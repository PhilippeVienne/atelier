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

import json
import logging

from langchain_core.runnables import RunnableConfig
from langgraph.types import interrupt

from .deps import PmEngineDeps
from .embeddings import embedding_literal as embeddings_embedding_literal
from .embeddings import pad_embedding
from .exec_client import wait_for_exec_completion
from .mcp_client import atelier_mcp_session, call_tool_json
from .state import PMWorkflowState, SubTask

logger = logging.getLogger(__name__)


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
        raw_tasks = json.loads(raw_plan)
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
                },
            )

    return {"phase": "ProvisionWorkshop"}


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
            prompt = (
                f"{task['title']}\n\n{state.get('analysis', '')}\n\n"
                f"IMPORTANT: ne modifie QUE les fichiers sous {scope} — un autre agent "
                "travaille en parallele sur le reste de ce depot."
            )
            command = f"claude --print --permission-mode acceptEdits {json.dumps(prompt)}"
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
