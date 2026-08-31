"""Les 11 noeuds du graphe LangGraph du PM (Jalon M5, tache 5.2.2 — voir
`docs/specs/PLAN-ACTION-GLOBAL.md`, section 8.2, et
`docs/specs/05-devfactory-pm-engine.md`).

Chaque noeud est une fonction `async def node(state, config) -> dict` :
LangGraph fusionne le dictionnaire renvoye dans l'etat courant (pas de
mutation directe de `state`). Les dependances (`PmEngineDeps`) sont lues
via `config["configurable"]["deps"]`, jamais construites ici — voir
`pm_engine.deps` pour la justification (testabilite).
"""

from __future__ import annotations

import asyncio
import json
import logging
import re
import shlex

from langchain_core.runnables import RunnableConfig
from langgraph.types import interrupt

from .deps import PmEngineDeps
from .embeddings import embedding_literal as embeddings_embedding_literal
from .embeddings import pad_embedding
from .exec_client import wait_for_exec_completion
from .mcp_client import atelier_mcp_session, call_tool_json
from .state import PMWorkflowState, SubTask

logger = logging.getLogger(__name__)

# Attente maximale, dans `ProvisionWorkshop`, pour qu'un Workshop atteigne
# la phase `Running` (build de l'image devcontainer + boot de la microVM).
PROVISION_TIMEOUT_SECONDS = 900
PROVISION_POLL_SECONDS = 10

# Un modele encadre frequemment sa reponse JSON dans un bloc de code
# markdown (```json ... ```) malgre une consigne "UNIQUEMENT du JSON" —
# constate en pratique avec DeepSeek sur `PlanParallelTasks`, ou le repli
# "une seule tache" se declenchait donc systematiquement, annulant tout le
# decoupage en sous-taches paralleles. On retire ces delimiteurs avant
# de parser plutot que de durcir encore le prompt.
_CODE_FENCE_RE = re.compile(r"^\s*```(?:json)?\s*\n(.*?)\n\s*```\s*$", re.DOTALL)


def _strip_code_fences(text: str) -> str:
    match = _CODE_FENCE_RE.match(text.strip())
    return match.group(1) if match else text


def _deps(config: RunnableConfig) -> PmEngineDeps:
    return config["configurable"]["deps"]


# --------------------------------------------------------------------------
# 1. AnalyzeIssue
# --------------------------------------------------------------------------
async def analyze_issue(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)
    issue = await deps.git_provider.get_issue(state["repo"], state["issue_number"])

    analysis = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Tu es le Project Manager autonome d'Atelier. Analyse ce ticket et "
                    "resume en 2-3 phrases ce qu'il faut livrer, sans decouper en taches "
                    "(le decoupage est fait par un autre noeud)."
                ),
            },
            {"role": "user", "content": f"# {issue.title}\n\n{issue.body}"},
        ],
    )

    return {
        "issue_title": issue.title,
        "issue_body": issue.body,
        "issue_url": issue.url,
        "analysis": analysis,
        "phase": "AnalyzeIssue",
    }


# --------------------------------------------------------------------------
# 2. PlanParallelTasks
# --------------------------------------------------------------------------
async def plan_parallel_tasks(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)

    raw_plan = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Decoupe ce ticket en sous-taches paralleles SANS chevauchement de "
                    "fichiers (chaque sous-tache doit avoir un perimetre de fichiers "
                    "disjoint des autres, pour eviter tout conflit entre agents "
                    "paralleles). Reponds UNIQUEMENT avec un tableau JSON, un objet par "
                    'sous-tache : [{"id": "task-1", "title": "...", "scope": '
                    '["chemin/vers/fichiers/**"]}, ...].'
                ),
            },
            {"role": "user", "content": state.get("analysis", "")},
        ],
    )

    try:
        raw_tasks = json.loads(_strip_code_fences(raw_plan))
    except json.JSONDecodeError:
        logger.warning("PlanParallelTasks: reponse LLM non-JSON, repli sur une seule tache")
        raw_tasks = [{"id": "task-1", "title": state.get("issue_title", "task"), "scope": ["**"]}]

    plan: list[SubTask] = [
        SubTask(
            id=task["id"],
            title=task["title"],
            scope=task["scope"],
            workshop_name=f"pm-{state['issue_number']}-{task['id']}",
            branch_name=f"feature/{state['issue_number']}-{task['id']}",
        )
        for task in raw_tasks
    ]

    return {"plan": plan, "phase": "PlanParallelTasks"}


# --------------------------------------------------------------------------
# 3. ProvisionWorkshop
# --------------------------------------------------------------------------
async def provision_workshop(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Cree un Workshop par sous-tache (`create_workshop`, MCP). Les
    appels sont emis sequentiellement dans CE noeud (une boucle, pas un
    fan-out LangGraph natif via `Send` — simplification assumee de cette
    premiere version, voir docs/PROGRESS.md) : les Workshops eux-memes
    tournent bel et bien en parallele dans le cluster une fois crees, seule
    l'emission des appels `create_workshop` est sequentielle ici."""
    deps = _deps(config)
    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        for task in state.get("plan", []):
            await deps.git_provider.create_branch(
                state["repo"], task["branch_name"], base_branch="main"
            )
            await call_tool_json(
                session,
                "create_workshop",
                {
                    "name": task["workshop_name"],
                    "devcontainerRepo": state["devcontainer_repo"],
                    "devcontainerRevision": task["branch_name"],
                    "cpu": "2",
                    "memory": "4Gi",
                    # Sans allowlist explicite, `create_workshop` en cree une
                    # VIDE et le build d'image du Workshop ne peut jamais
                    # aboutir — voir `PmEngineDeps.workshop_egress_allowlist`.
                    "egressAllowlist": deps.workshop_egress_allowlist,
                },
            )


    # Attente hors de la session MCP de creation : elle dure plusieurs
    # minutes, bien au-dela de la duree de vie du jeton OIDC fige a
    # l'ouverture d'une session (voir
    # `pm_engine.mcp_client.atelier_mcp_session`) — chaque sondage rouvre
    # donc sa propre session courte, sans quoi le jeton expire en cours
    # d'attente et l'API repond `ExpiredSignature` (constate en pratique).
    #
    # Cette attente est indispensable : `create_workshop` est asynchrone (il
    # cree la ressource Kubernetes et rend la main immediatement), mais le
    # Workshop met ensuite ~1 min a construire son image puis a booter sa
    # microVM. Sans elle, `DelegateToClaudeCode` partait aussitot et
    # echouait systematiquement ("le Workshop n'a pas de pod parent actif"),
    # bloquant le graphe a ce noeud. Provisionner, c'est rendre pret.
    for task in state.get("plan", []):
        await _await_workshop_running(deps, task["workshop_name"])

    return {"phase": "ProvisionWorkshop"}


async def _await_workshop_running(deps: PmEngineDeps, workshop_name: str) -> None:
    """Sonde `get_workshop_status` jusqu'a la phase `Running`.

    `PROVISION_TIMEOUT_SECONDS` est volontairement large : le premier build
    d'une image devcontainer donnee est long (clone + `envbuilder` + push
    registre), les suivants profitent du cache d'images partage."""
    deadline = asyncio.get_running_loop().time() + PROVISION_TIMEOUT_SECONDS
    while True:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            status = await call_tool_json(
                session, "get_workshop_status", {"name": workshop_name}
            )
        phase = (status or {}).get("phase")
        if phase == "Running":
            logger.info("ProvisionWorkshop: %s est Running", workshop_name)
            return
        if phase == "Failed":
            raise RuntimeError(
                f"le Workshop {workshop_name} est en echec (phase Failed), "
                "voir ses evenements Kubernetes"
            )
        if asyncio.get_running_loop().time() > deadline:
            raise TimeoutError(
                f"le Workshop {workshop_name} n'est pas Running apres "
                f"{PROVISION_TIMEOUT_SECONDS}s (phase={phase})"
            )
        logger.info("ProvisionWorkshop: %s en phase %s, attente...", workshop_name, phase)
        await asyncio.sleep(PROVISION_POLL_SECONDS)


# --------------------------------------------------------------------------
# 4. DelegateToClaudeCode
# --------------------------------------------------------------------------
async def delegate_to_claude_code(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Lance Claude Code dans chaque microVM (`exec_in_workshop`, MCP) avec
    le perimetre de fichiers de sa sous-tache injecte dans le prompt —
    c'est ce qui garantit l'absence de chevauchement (voir
    docs/specs/05-devfactory-pm-engine.md, section 1)."""
    deps = _deps(config)
    token = await deps.mcp_token_provider.get_token()

    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        for task in state.get("plan", []):
            scope = " ".join(task["scope"])
            # Le commit/push fait partie de la consigne : sans lui, le
            # travail de l'agent reste dans le systeme de fichiers de la
            # microVM et n'atteint jamais la branche de la sous-tache —
            # `OpenPullRequest` ouvrait alors une PR systematiquement VIDE
            # (0 fichier modifie), sans que rien ne signale l'anomalie.
            # Constate en executant le graphe complet pour la premiere fois
            # (2026-08-30).
            prompt = (
                f"{task['title']}\n\n{state.get('analysis', '')}\n\n"
                f"IMPORTANT: ne modifie QUE les fichiers sous {scope} — un autre agent "
                "travaille en parallele sur le reste de ce depot.\n\n"
                "Quand ton travail est termine, commite-le puis pousse-le sur la "
                f"branche courante ({task['branch_name']}) : "
                "`git add -A && git commit -m \"<message>\" && git push origin HEAD`."
            )
            # `shlex.quote`, JAMAIS `json.dumps` : ce dernier produit une
            # chaine entre guillemets DOUBLES, dans lesquels bash interprete
            # encore les backticks, `$(...)` et `$VAR`. Or ce prompt contient
            # du texte genere par un LLM a partir du ticket (`analysis`) et
            # des chemins entoures de backticks — bash executait donc des
            # fragments du prompt comme des commandes (`api/: No such file or
            # directory`, `fatal: not a git repository`...), et Claude Code
            # recevait un prompt tronque. Bug reel constate le 2026-08-30 en
            # executant le graphe complet.
            #
            # Au-dela du dysfonctionnement, c'est une injection de commande :
            # le corps d'un ticket est une entree non fiable, et il finissait
            # interprete par le shell du Workshop. `shlex.quote` produit des
            # guillemets SIMPLES, ou plus rien n'est interprete.
            # `bypassPermissions`, et NON `acceptEdits` : ce dernier
            # auto-approuve les editions de fichiers mais PAS les commandes
            # `Bash`. En mode `--print` (non interactif), il n'y a personne
            # pour approuver : `git add`/`commit`/`push` etaient donc refuses
            # en silence. L'agent ecrivait un travail complet et correct, qui
            # restait en fichiers NON SUIVIS dans la microVM — et
            # `OpenPullRequest` ouvrait une PR vide. Constate le 2026-08-31 :
            # `git status` dans le Workshop montrait `?? api/`, `?? server.js`,
            # `?? test/` avec un `git log` intact.
            #
            # Deleguer les permissions est ici sans danger, et c'est meme la
            # raison d'etre d'Atelier : l'agent s'execute dans une microVM
            # Firecracker jetable, sans acces reseau hors allowlist. La
            # frontiere de securite est la microVM, pas l'invite de
            # confirmation d'un CLI.
            command = (
                "claude --print --permission-mode bypassPermissions "
                f"--model {shlex.quote(deps.claude_code_model)} "
                f"{shlex.quote(prompt)}"
            )
            execution = await call_tool_json(
                session, "exec_in_workshop", {"name": task["workshop_name"], "command": command}
            )
            await wait_for_exec_completion(
                deps.atelier_api_url, token, task["workshop_name"], execution["executionId"]
            )

    return {"phase": "DelegateToClaudeCode"}


# --------------------------------------------------------------------------
# 5. RunDevcontainerTests
# --------------------------------------------------------------------------
async def run_devcontainer_tests(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Execute la suite de tests declaree par le devcontainer
    (convention : `.devcontainer/devcontainer.json` -> champ
    `postStartCommand`/script `test.sh` du projet cible, resolu par le
    guest lui-meme — ce noeud ne fait qu'executer une commande fixe,
    `bash .devcontainer/test.sh`, voir la limite documentee dans
    docs/PROGRESS.md)."""
    deps = _deps(config)
    token = await deps.mcp_token_provider.get_token()

    all_passed = True
    combined_output = ""
    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        for task in state.get("plan", []):
            execution = await call_tool_json(
                session,
                "exec_in_workshop",
                {"name": task["workshop_name"], "command": "bash .devcontainer/test.sh"},
            )
            result = await wait_for_exec_completion(
                deps.atelier_api_url, token, task["workshop_name"], execution["executionId"]
            )
            combined_output += f"## {task['workshop_name']}\n{result.stdout}\n{result.stderr}\n"
            if result.exit_code != 0:
                all_passed = False

    return {
        "test_output": combined_output,
        "test_passed": all_passed,
        "error_trace": "" if all_passed else combined_output,
        "phase": "RunDevcontainerTests",
    }


# --------------------------------------------------------------------------
# 6. AutoCorrectionLoop
# --------------------------------------------------------------------------
async def auto_correction_loop(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Incremente le compteur de tentatives — la decision de reboucler vers
    `DelegateToClaudeCode` (traces d'erreur re-injectees dans le prompt du
    prochain passage) ou de continuer vers `OpenPullRequest` est prise par
    l'arete conditionnelle `route_after_correction` (voir `graph.py`), pas
    ici : ce noeud ne fait qu'avancer l'etat borne (jamais de boucle
    infinie, meme si les tests echouent indefiniment)."""
    attempts = state.get("correction_attempts", 0) + 1
    return {
        "correction_attempts": attempts,
        "analysis": (
            f"{state.get('analysis', '')}\n\n"
            f"## Tentative de correction {attempts}\nLes tests ont echoue :\n"
            f"{state.get('error_trace', '')}\nCorrige ces echecs."
        ),
        "phase": "AutoCorrectionLoop",
    }


def route_after_tests(state: PMWorkflowState) -> str:
    if state.get("test_passed"):
        return "OpenPullRequest"
    if state.get("correction_attempts", 0) >= state.get("max_correction_attempts", 3):
        # Budget de corrections epuise : on ouvre quand meme la PR, en
        # l'etat, plutot que de bloquer indefiniment — un humain tranchera
        # via AwaitHitlApproval.
        return "OpenPullRequest"
    return "AutoCorrectionLoop"


# --------------------------------------------------------------------------
# 7. OpenPullRequest
# --------------------------------------------------------------------------
async def open_pull_request(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)
    plan = state.get("plan", [])
    if not plan:
        return {"phase": "OpenPullRequest", "error": "aucune sous-tache a fusionner"}

    # Une seule PR pour l'ensemble des sous-taches de ce ticket dans cette
    # premiere version (voir la meme simplification que ProvisionWorkshop) :
    # la premiere branche de sous-tache sert de tete de PR.
    head_task = plan[0]
    body = (
        f"Resout #{state['issue_number']}.\n\n{state.get('analysis', '')}\n\n"
        f"Tests : {'✅ passes' if state.get('test_passed') else '⚠️ echec, voir logs'}\n\n"
        "PR ouverte automatiquement par atelier-pm-bot."
    )
    pr = await deps.git_provider.create_pr(
        state["repo"],
        title=state.get("issue_title", f"Resout #{state['issue_number']}"),
        body=body,
        head_branch=head_task["branch_name"],
        base_branch="main",
    )
    return {"pr_number": pr.number, "pr_url": pr.url, "phase": "OpenPullRequest"}


# --------------------------------------------------------------------------
# 8. SuspendWhileWaitingReview
# --------------------------------------------------------------------------
async def suspend_while_waiting_review(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Libere CPU/RAM (snapshot Firecracker vers S3) des l'ouverture de la
    PR : plus besoin des microVMs tant qu'un humain n'a pas approuve —
    voir docs/specs/05-devfactory-pm-engine.md, section 1.5."""
    deps = _deps(config)
    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        for task in state.get("plan", []):
            await call_tool_json(session, "suspend_workshop", {"name": task["workshop_name"]})
    return {"phase": "SuspendWhileWaitingReview"}


# --------------------------------------------------------------------------
# 9. AwaitHitlApproval
# --------------------------------------------------------------------------
async def await_hitl_approval(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Point d'arret du graphe (`interrupt`, LangGraph) : l'execution se
    suspend ICI et le checkpoint PostgreSQL (tache 5.3.3) persiste l'etat
    complet — reprend exactement a ce noeud, sur n'importe quel worker,
    quand un humain resume le graphe avec sa decision (voir
    `pm_engine.graph.resume_with_decision` et la tache 5.5.2, interface
    Dashboard, hors perimetre de cette session)."""
    decision = interrupt(
        {
            "question": "Approuver la fusion de cette Pull Request ?",
            "pr_url": state.get("pr_url"),
        }
    )
    return {"hitl_decision": decision, "phase": "AwaitHitlApproval"}


def route_after_hitl(state: PMWorkflowState) -> str:
    return "MergeAndClose" if state.get("hitl_decision") == "approved" else "__end__"


# --------------------------------------------------------------------------
# 10. MergeAndClose
# --------------------------------------------------------------------------
async def merge_and_close(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)
    if "pr_number" in state:
        await deps.git_provider.merge_pr(state["repo"], state["pr_number"])
    await deps.git_provider.post_comment(
        state["repo"],
        state["issue_number"],
        f"Resolu par atelier-pm-bot, voir {state.get('pr_url', '')}.",
    )
    return {"status": "merged", "phase": "MergeAndClose"}


# --------------------------------------------------------------------------
# 11. IndexKnowledge
# --------------------------------------------------------------------------
async def index_knowledge(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Extrait le pattern de resolution de ce ticket et l'indexe dans
    `project_memories` (pgvector, RLS par `tenant_id` — voir
    `services/pm-engine/migrations/20260824000000_init_pm_engine.sql` et
    `deploy/dev/ollama`/tache 5.0.2 pour le modele d'embedding local).
    Complement de dimension (384 -> 1536) : voir `pm_engine.embeddings`,
    partage avec `pm_engine.rag` (5.5.1) qui interroge cette meme table."""
    deps = _deps(config)
    content = (
        f"# {state.get('issue_title', '')}\n\n{state.get('analysis', '')}\n\n"
        f"PR: {state.get('pr_url', '')}"
    )
    embedding = pad_embedding(await deps.llm_client.embed(deps.embedding_model, content))
    embedding_literal = embeddings_embedding_literal(embedding)

    async with deps.db_pool.acquire() as conn:
        async with conn.transaction():
            await conn.execute(
                "SELECT set_config('app.current_tenant', $1, true)", deps.pm_bot_subject
            )
            await conn.execute(
                "INSERT INTO project_memories (tenant_id, project_id, content, metadata, embedding) "
                "VALUES ($1, $2, $3, $4, $5)",
                deps.pm_bot_subject,
                state["repo"],
                content,
                json.dumps({"issue_number": state["issue_number"], "pr_url": state.get("pr_url")}),
                embedding_literal,
            )

    return {"knowledge_indexed": True, "phase": "IndexKnowledge", "status": "done"}
