"""Les 12 noeuds du graphe LangGraph du PM (Jalon M5, tache 5.2.2 — voir
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
import base64
import fnmatch
import json
import logging
import re
import shlex

import httpx
from langchain_core.runnables import RunnableConfig
from langgraph.types import interrupt

from .deps import PmEngineDeps
from .embeddings import embedding_literal as embeddings_embedding_literal
from .embeddings import pad_embedding
from .evidence_store import upload_evidence
from .exec_client import ExecResult, wait_for_exec_completion
from .mcp_client import atelier_mcp_session, call_tool_json
from .state import PMWorkflowState, SubTask

logger = logging.getLogger(__name__)

# Attente maximale, dans `ProvisionWorkshop`, pour qu'un Workshop atteigne
# la phase `Running` (build de l'image devcontainer + boot de la microVM).
PROVISION_TIMEOUT_SECONDS = 900
PROVISION_POLL_SECONDS = 10
# Duree pendant laquelle une phase `Failed` est toleree avant d'abandonner :
# le Job de build retente (`backoffLimit`), et le Workshop repasse alors par
# `Running`. Large devant le delai de replanification d'un pod, court devant
# `PROVISION_TIMEOUT_SECONDS` — voir `_await_workshop_running`.
FAILED_GRACE_SECONDS = 300

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
                    "reformule-le en une job story (\"Quand <situation>, l'utilisateur "
                    "veut <motivation>, afin de <resultat>\") suivie de 2 a 5 criteres "
                    "d'acceptation verifiables (un par ligne, commencant par '- '). "
                    "Un ticket flou approxime silencieusement en sous-taches : le "
                    "decoupage (fait par un autre noeud) et l'agent qui livre ont besoin "
                    "d'un objectif explicite et testable, pas d'un resume vague. "
                    "Ne decoupe pas en taches ici — c'est le role d'un autre noeud."
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

# Entrees de racine qui ne constituent PAS un socle de projet : leur presence
# seule laisse le depot vierge du point de vue d'un agent qui doit produire du
# code executable.
_NOT_SCAFFOLDING = (
    "readme",
    "license",
    "licence",
    "copying",
    "changelog",
    "contributing",
    "code_of_conduct",
    "authors",
    "notice",
    "security",
)


def _is_greenfield(root_entries: list[str]) -> bool:
    """Le depot est-il vierge de tout socle de projet ?

    Un depot vide (ou ne contenant qu'un README et une licence) n'a ni
    manifeste, ni point d'entree, ni arborescence : chaque agent parti en
    parallele depuis `main` doit alors inventer ce socle POUR LUI, dans son
    propre Workshop, sans voir celui des autres. C'est le mecanisme exact qui
    produit deux serveurs concurrents pour la meme application (constate sur
    de vrais plans : une sous-tache prenant `index.js`, une autre
    `server.js`).

    Un depot deja pourvu, lui, se prete a un decoupage : les agents partagent
    le meme socle et n'ont plus qu'a se repartir des zones disjointes.
    """
    for entry in root_entries:
        name = entry.lower()
        if name.startswith("."):
            continue
        if any(name.startswith(prefix) for prefix in _NOT_SCAFFOLDING):
            continue
        return False
    return True


def _describe_root(root_entries: list[str] | None) -> str:
    """Ce qu'on dit au planificateur du depot. `None` (provider muet) et `[]`
    (depot vide) ne se disent PAS pareil : annoncer un depot vide alors qu'on
    n'en sait rien pousserait a tout reecrire."""
    if root_entries is None:
        return "inconnu (le contenu du depot n'a pas pu etre lu)"
    if not root_entries:
        return "depot VIDE (aucun fichier)"
    return ", ".join(sorted(root_entries))


def _plan_is_credible(plan: list[dict]) -> str | None:
    """Renvoie la raison pour laquelle un decoupage multi-taches n'est pas
    tenable, ou `None` s'il l'est.

    Le prompt DEMANDE des perimetres disjoints ; rien ne le verifiait. Une
    consigne non verifiee finit toujours par etre approximee.
    """
    if len(plan) < 2:
        return None
    seen: dict[str, str] = {}
    for task in plan:
        for entry in task.get("scope") or []:
            normalised = entry.strip().strip("/")
            # Un perimetre attrape-tout a cote d'autres sous-taches : elles se
            # marchent dessus par construction.
            if normalised in ("**", "*", ""):
                return f"la sous-tache {task['id']} prend tout le depot ({entry!r})"
            if normalised in seen and seen[normalised] != task["id"]:
                return (
                    f"{entry!r} est revendique par {seen[normalised]} ET par {task['id']}"
                )
            seen[normalised] = task["id"]
    return None


async def plan_parallel_tasks(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)

    # Etat REEL du depot sur la branche de base. Sans lui, le planificateur
    # decoupe a l'aveugle : il ne peut pas savoir si les agents partageront un
    # socle ou devront chacun en inventer un.
    root_entries: list[str] | None = None
    try:
        root_entries = await deps.git_provider.list_root_entries(state["repo"], "main")
    except Exception as exc:  # noqa: BLE001 - le decoupage doit rester possible sans
        logger.warning("PlanParallelTasks: contenu du depot illisible (%s)", exc)

    raw_plan = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Decoupe ce ticket en sous-taches paralleles SANS chevauchement de "
                    "fichiers (chaque sous-tache doit avoir un perimetre de fichiers "
                    "disjoint des autres, pour eviter tout conflit entre agents "
                    "paralleles).\n\n"
                    "Avant de figer le decoupage, trace mentalement le graphe de "
                    "dependances entre les sous-taches envisagees (qui a besoin du "
                    "resultat de qui pour tourner ou pour etre teste). Toute arete de "
                    "dependance entre deux sous-taches interdit de les paralleliser : "
                    "fusionne-les en une seule, ou renonce au decoupage.\n\n"
                    "CONTRAINTE ESSENTIELLE : chaque sous-tache est confiee a un agent "
                    "qui travaille SEUL, dans un environnement ou le code des AUTRES "
                    "sous-taches n'existe pas encore. Chaque sous-tache doit donc etre "
                    "realisable de bout en bout sans voir le travail des autres. En "
                    "particulier, ne cree JAMAIS une sous-tache dediee aux tests : les "
                    "tests d'un module font partie de la sous-tache qui ecrit ce module. "
                    "De meme, ne cree pas de sous-tache d'integration ou de "
                    "documentation transversale.\n\n"
                    "Ne decoupe JAMAIS selon des couches qui dependent l'une de "
                    "l'autre a l'execution (une sous-tache 'API' et une sous-tache "
                    "'logique metier' du meme service, par exemple) : aucune des deux "
                    "ne peut tourner sans l'autre. Un decoupage frontend/backend n'est "
                    "legitime que si chaque cote s'execute et se teste seul, le "
                    "frontend parlant au backend par HTTP.\n\n"
                    "Il ne doit exister qu'UN SEUL point d'entree pour l'application, "
                    "et un seul manifeste (package.json, pyproject.toml...) : s'ils "
                    "n'existent pas encore, ils appartiennent a une seule sous-tache, "
                    "et les autres ne peuvent donc pas tourner — c'est le signe que le "
                    "decoupage n'est pas possible.\n\n"
                    "S'il n'existe pas de decoupage reellement independant, renvoie une "
                    "SEULE sous-tache couvrant tout : mieux vaut une tache unique "
                    "coherente que plusieurs taches qui ne peuvent pas aboutir.\n\n"
                    "Reponds UNIQUEMENT avec un tableau JSON, un objet par "
                    'sous-tache : [{"id": "task-1", "title": "...", "scope": '
                    '["chemin/vers/fichiers/**"]}, ...].'
                ),
            },
            {
                "role": "user",
                "content": (
                    f"{state.get('analysis', '')}\n\n"
                    f"Contenu actuel de la racine du depot : {_describe_root(root_entries)}"
                ),
            },
        ],
    )

    try:
        raw_tasks = json.loads(_strip_code_fences(raw_plan))
        # Le JSON peut etre PARFAITEMENT valide sans avoir la forme attendue
        # (ex: `["task-1"]`, une liste de chaines plutot que d'objets) — un
        # ticket tres simple y pousse le modele en pratique (constate le
        # 2026-09-02, avec `claude-3-5-sonnet-20241022` reel, sur un ticket
        # d'une seule ligne). `json.JSONDecodeError` seul ne l'attrape pas :
        # le parsing reussit, c'est la lecture de `task["id"]` plus bas qui
        # levait `TypeError: string indices must be integers`. Meme doctrine
        # que le repli sur JSON invalide : une forme inattendue est traitee
        # comme une reponse inexploitable, jamais comme une exception qui
        # remonte.
        if not raw_tasks or not all(
            isinstance(task, dict) and {"id", "title", "scope"} <= task.keys()
            for task in raw_tasks
        ):
            raise ValueError("forme de plan inattendue (pas une liste d'objets id/title/scope)")
    except (json.JSONDecodeError, ValueError) as exc:
        logger.warning("PlanParallelTasks: reponse LLM inexploitable (%s), repli sur une seule tache", exc)
        raw_tasks = [{"id": "task-1", "title": state.get("issue_title", "task"), "scope": ["**"]}]

    # Garde-fous DETERMINISTES. Le prompt enonce ces regles, mais une consigne
    # qui n'est pas verifiee finit toujours par etre approximee — et le prix
    # d'un mauvais decoupage est plusieurs microVM qui produisent chacune leur
    # version de la meme application, donc du budget LLM brule pour du travail
    # jete.
    # Distingue des autres raisons de replier a une seule tache : c'est
    # cette condition, et elle seule, qui declenche `ExpandGreenfieldSpec`
    # (voir routage dans `graph.py`). Un decoupage juge incoherent
    # (`_plan_is_credible`) dans un depot deja pourvu n'a pas besoin de
    # spec — le socle existe deja.
    is_greenfield_repo = root_entries is not None and _is_greenfield(root_entries)

    single_task_reason: str | None = None
    if is_greenfield_repo and len(raw_tasks) > 1:
        single_task_reason = (
            "le depot est vierge : chaque agent devrait inventer son propre socle"
        )
    elif (incoherence := _plan_is_credible(raw_tasks)) is not None:
        single_task_reason = incoherence

    if single_task_reason is not None:
        logger.info(
            "PlanParallelTasks: %d sous-taches ramenees a une seule (%s)",
            len(raw_tasks),
            single_task_reason,
        )
        raw_tasks = [
            {
                "id": "task-1",
                "title": state.get("issue_title", "task"),
                "scope": ["**"],
            }
        ]

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

    return {"plan": plan, "greenfield": is_greenfield_repo, "phase": "PlanParallelTasks"}


# --------------------------------------------------------------------------
# 2bis. ExpandGreenfieldSpec (uniquement si `greenfield` est vrai)
# --------------------------------------------------------------------------
async def expand_greenfield_spec(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Precise l'architecture d'un projet parti de zero AVANT de le deleguer.

    `PlanParallelTasks` a deja ecarte le risque de deux socles concurrents
    en repliant le decoupage a une seule sous-tache (voir `_is_greenfield`).
    Mais un seul agent face a un ticket vague choisit quand meme un point
    d'entree, un manifeste et une arborescence au hasard, faute d'une
    consigne explicite — la qualite du livrable en depend, pas seulement
    l'absence de conflit entre agents. S'inspire du gabarit `create-prd` de
    github.com/phuryn/pm-skills (section solution/architecture), reduit a
    l'essentiel pour ne pas gonfler le cout d'un run deja greenfield."""
    deps = _deps(config)

    spec = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Ce ticket vise un depot VIERGE : un seul agent va tout construire, "
                    "sans aucun code de reference existant. Fixe les decisions "
                    "d'architecture qu'il ne doit pas improviser, en Markdown concis "
                    "(10 lignes maximum) :\n"
                    "1. Point d'entree unique (chemin exact du fichier).\n"
                    "2. Manifeste de dependances (nom exact : package.json, "
                    "pyproject.toml, Cargo.toml...).\n"
                    "3. Arborescence des dossiers principaux.\n"
                    "4. Techno/langage retenu si le ticket ne le precise pas "
                    "(justifie en une phrase).\n"
                    "5. Point d'entree des tests : `.devcontainer/test.sh` DOIT exister "
                    "et lancer la suite complete (c'est la commande fixe que "
                    "l'integration continue execute ensuite — voir "
                    "`nodes.run_devcontainer_tests`).\n"
                    "N'invente aucune fonctionnalite absente du ticket."
                ),
            },
            {"role": "user", "content": state.get("analysis", "")},
        ],
    )

    return {"greenfield_spec": spec, "phase": "ExpandGreenfieldSpec"}


def route_after_plan(state: PMWorkflowState) -> str:
    return "ExpandGreenfieldSpec" if state.get("greenfield") else "ReviewArchitecture"


# --------------------------------------------------------------------------
# 2ter. ReviewArchitecture (docs/specs/08-equipe-it-consultative.md,
# section 5 — brique 5.6.3). Premier des quatre roles consultatifs, le
# seul a se prononcer AVANT toute creation de Workshop : un decoupage jugee
# malsain doit etre refait a la source (replanification), pas corrige en
# aval par les devs qui l'executent deja.
# --------------------------------------------------------------------------
def _parse_review_verdict(raw: str, role_name: str) -> dict:
    """Parsing commun aux quatre roles consultatifs (Architecte/QA/
    Securite/Ops) : reponse non-JSON ou verdict inattendu degrade TOUJOURS
    vers `"approve"`, jamais vers `"request_changes"` — un modele qui
    repond mal ne doit pas bloquer indefiniment un run par accident (meme
    doctrine que le repli sur une tache unique de `plan_parallel_tasks`
    face a une reponse non-JSON)."""
    try:
        verdict = json.loads(_strip_code_fences(raw))
        if verdict.get("verdict") not in ("approve", "request_changes"):
            raise ValueError("verdict inattendu")
    except (json.JSONDecodeError, ValueError, AttributeError):
        logger.warning("%s: reponse LLM non exploitable, repli sur approve", role_name)
        verdict = {"verdict": "approve", "comments": []}
    return verdict


async def review_architecture(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)

    raw_verdict = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Tu es l'architecte qui relit le decoupage en sous-taches "
                    "PROPOSE par le planificateur, avant que des devs ne se mettent au "
                    "travail dessus. Verifie en particulier :\n"
                    "1. Les perimetres de fichiers (`scope`) sont-ils VRAIMENT "
                    "disjoints, sans chevauchement ?\n"
                    "2. Existe-t-il une dependance cachee entre deux sous-taches "
                    "censees etre paralleles (l'une a besoin du resultat de l'autre "
                    "pour tourner ou etre testee) ?\n"
                    "3. Le decoupage est-il sur-decoupe (des sous-taches qui n'ont "
                    "aucune raison d'etre separees) ou sous-decoupe (une seule tache "
                    "qui melange des responsabilites sans rapport) ?\n"
                    "4. Sur un depot vierge, la specification d'architecture fournie "
                    "fixe-t-elle bien un point d'entree UNIQUE et un seul manifeste ?\n\n"
                    "Ne remets PAS en cause une tache unique legitime (un decoupage "
                    "vraiment independant n'existe pas toujours). Reponds UNIQUEMENT "
                    "avec un objet JSON : "
                    '{"verdict": "approve", "comments": []} ou '
                    '{"verdict": "request_changes", "comments": ["..."]}.'
                ),
            },
            {
                "role": "user",
                "content": (
                    f"## Ticket\n{state.get('analysis', '')}\n\n"
                    f"## Decoupage propose\n{json.dumps(state.get('plan', []), indent=2)}\n\n"
                    + (
                        f"## Specification d'architecture (depot vierge)\n"
                        f"{state.get('greenfield_spec', '')}\n\n"
                        if state.get("greenfield")
                        else ""
                    )
                ),
            },
        ],
    )

    verdict = _parse_review_verdict(raw_verdict, "ReviewArchitecture")

    return {
        "architecture_review": verdict,
        "phase": "ReviewArchitecture",
    }


def route_after_architecture_review(state: PMWorkflowState) -> str:
    review = state.get("architecture_review") or {}
    if review.get("verdict") != "request_changes":
        return "ProvisionWorkshop"
    attempts = state.get("architecture_review_attempts", 0) + 1
    if attempts >= state.get("max_architecture_review_attempts", 3):
        # Budget de revue epuise : on avance quand meme, un humain tranchera
        # via AwaitHitlApproval — meme doctrine que `route_after_tests`.
        logger.info(
            "ReviewArchitecture: budget de revue epuise (%d tentatives), "
            "passage en force vers ProvisionWorkshop",
            attempts,
        )
        return "ProvisionWorkshop"
    return "ArchitectureReconsideration"


async def prepare_architecture_reconsideration(
    state: PMWorkflowState, config: RunnableConfig
) -> dict:
    """Injecte les objections de l'architecte dans `analysis` avant de
    rebafouiller `PlanParallelTasks` — meme mecanisme que
    `auto_correction_loop` pour les echecs de tests, applique ici a un
    decoupage juge malsain plutot qu'a un code qui ne compile pas."""
    review = state.get("architecture_review") or {}
    attempts = state.get("architecture_review_attempts", 0) + 1
    comments = "\n".join(f"- {c}" for c in review.get("comments", []))
    return {
        "architecture_review_attempts": attempts,
        "analysis": (
            f"{state.get('analysis', '')}\n\n"
            f"## Revue d'architecture, tentative {attempts}\n"
            "Le decoupage precedent a ete rejete par la revue d'architecture :\n"
            f"{comments}\n"
            "Reponds a ces objections dans le nouveau decoupage."
        ),
        "phase": "ReviewArchitecture",
    }


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
            # Idempotent : LangGraph rejoue un noeud qui a echoue (reprise
            # d'un thread depuis son checkpoint), et la branche de la
            # sous-tache existe alors deja. Sans ce filet, toute reprise de
            # `ProvisionWorkshop` mourait sur un `409 Conflict` — y compris
            # les reprises apres une panne dont le cluster s'etait remis tout
            # seul (constate le 2026-08-31). Une branche deja creee est
            # exactement l'etat vise : il n'y a rien a corriger.
            try:
                await deps.git_provider.create_branch(
                    state["repo"], task["branch_name"], base_branch="main"
                )
            except httpx.HTTPStatusError as exc:
                if exc.response.status_code != 409:
                    raise
                logger.info(
                    "ProvisionWorkshop: la branche %s existe deja, reprise",
                    task["branch_name"],
                )
            # Meme raison d'idempotence que la branche ci-dessus. On teste
            # l'existence plutot que de rattraper une erreur de creation :
            # cela ne depend d'aucun texte de message d'erreur, et le
            # Workshop deja present est justement l'etat recherche.
            if await _workshop_exists(deps, task["workshop_name"]):
                logger.info(
                    "ProvisionWorkshop: le Workshop %s existe deja, reprise",
                    task["workshop_name"],
                )
            else:
                await call_tool_json(
                    session,
                    "create_workshop",
                    {
                        "name": task["workshop_name"],
                        "devcontainerRepo": state["devcontainer_repo"],
                        "devcontainerRevision": task["branch_name"],
                        "cpu": "2",
                        "memory": "4Gi",
                        # Sans allowlist explicite, `create_workshop` en cree
                        # une VIDE et le build d'image du Workshop ne peut
                        # jamais aboutir — voir
                        # `PmEngineDeps.workshop_egress_allowlist`.
                        "egressAllowlist": deps.workshop_egress_allowlist,
                        # Omis si non configure : l'api-server retient alors
                        # le groupe unique de l'appelant. C'est seulement
                        # quand il y en a plusieurs qu'il faut trancher
                        # explicitement.
                        **(
                            {"ownerGroup": deps.workshop_owner_group}
                            if deps.workshop_owner_group
                            else {}
                        ),
                    },
                )
            # Hors du `if`/`else` ci-dessus, y compris sur reprise d'un
            # Workshop deja existant : une reprise LangGraph peut retomber
            # ici alors que `create_workshop` a reussi mais que le process a
            # crashe (ou qu'OpenBao etait injoignable) juste avant l'appel
            # `set_workshop_git_credential` qui suit. Le sauter dans ce cas
            # laissait le Workshop sans credential git POUR TOUJOURS (aucune
            # autre reprise ne le retente, `_workshop_exists` etant alors
            # vrai) — reproduisant exactement le bug plus bas que ce depot
            # sert a corriger. L'appel est idempotent (ecriture KV v2, donc
            # sans effet de bord a le repeter).
            #
            # Sans ce depot, `delegate_to_opencode` echouait plus tard avec
            # `fatal: could not read Username ... No such device or address`
            # (constate en Workshop reel le 2026-09-02) : `create_workshop`
            # n'avait aucun moyen de transmettre un identifiant git en
            # ecriture au Workshop, alors que ce provider en detient DEJA
            # un — le meme jeton qui lui sert a creer des branches/ouvrir
            # des PR donne acces en ecriture au depot. `git_push_credential`
            # renvoie `None` pour un provider qui ne sait pas en fournir
            # (voir sa docstring) : dans ce cas, on n'appelle simplement pas
            # cet outil, l'agent devra s'authentifier lui-meme si son
            # `git push` en a besoin.
            git_credential = deps.git_provider.git_push_credential()
            if git_credential is not None:
                git_username, git_password = git_credential
                await call_tool_json(
                    session,
                    "set_workshop_git_credential",
                    {
                        "name": task["workshop_name"],
                        "username": git_username,
                        "password": git_password,
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
    # microVM. Sans elle, `DelegateToOpencode` partait aussitot et
    # echouait systematiquement ("le Workshop n'a pas de pod parent actif"),
    # bloquant le graphe a ce noeud. Provisionner, c'est rendre pret.
    for task in state.get("plan", []):
        await _await_workshop_running(deps, task["workshop_name"])

    return {"phase": "ProvisionWorkshop"}


async def _workshop_exists(deps: PmEngineDeps, workshop_name: str) -> bool:
    """`get_workshop_status` aboutit-il pour ce Workshop ? Toute erreur vaut
    « absent » : ce test ne sert qu'a eviter une creation en double lors
    d'une reprise, et se tromper cote « absent » reproduit simplement le
    comportement d'avant (la creation echouera bruyamment si le Workshop
    existait bel et bien)."""
    try:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            await call_tool_json(session, "get_workshop_status", {"name": workshop_name})
        return True
    except Exception:  # noqa: BLE001 - se tromper cote « absent » est sans danger
        return False


async def _await_workshop_running(deps: PmEngineDeps, workshop_name: str) -> None:
    """Sonde `get_workshop_status` jusqu'a la phase `Running`.

    `PROVISION_TIMEOUT_SECONDS` est volontairement large : le premier build
    d'une image devcontainer donnee est long (clone + `envbuilder` + push
    registre), les suivants profitent du cache d'images partage."""
    deadline = asyncio.get_running_loop().time() + PROVISION_TIMEOUT_SECONDS
    failed_since: float | None = None
    while True:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            status = await call_tool_json(
                session, "get_workshop_status", {"name": workshop_name}
            )
        phase = (status or {}).get("phase")
        now = asyncio.get_running_loop().time()
        if phase == "Running":
            logger.info("ProvisionWorkshop: %s est Running", workshop_name)
            return
        # `Failed` n'est PAS definitif : le Job de build a un `backoffLimit`,
        # et un pod de build qui echoue fait passer le Workshop par `Failed`
        # avant que la tentative suivante ne reussisse. Abandonner au premier
        # `Failed` faisait donc echouer tout le workflow sur une panne dont
        # Kubernetes se remettait seul quelques minutes plus tard (constate le
        # 2026-08-31 : premier pod de build en `Error`, retentative
        # `Completed`, Workshop finalement `Running`). On n'abandonne que si
        # l'echec PERSISTE — ce qui garde l'interet du fail-fast (ne pas
        # attendre le timeout complet sur un Workshop reellement mort) sans
        # sacrifier les reprises normales.
        if phase == "Failed":
            failed_since = failed_since if failed_since is not None else now
            if now - failed_since > FAILED_GRACE_SECONDS:
                raise RuntimeError(
                    f"le Workshop {workshop_name} est en echec (phase Failed) "
                    f"depuis plus de {FAILED_GRACE_SECONDS}s, voir ses "
                    "evenements Kubernetes"
                )
        else:
            failed_since = None
        if asyncio.get_running_loop().time() > deadline:
            raise TimeoutError(
                f"le Workshop {workshop_name} n'est pas Running apres "
                f"{PROVISION_TIMEOUT_SECONDS}s (phase={phase})"
            )
        logger.info("ProvisionWorkshop: %s en phase %s, attente...", workshop_name, phase)
        await asyncio.sleep(PROVISION_POLL_SECONDS)


# --------------------------------------------------------------------------
# 4. DelegateToOpencode
# --------------------------------------------------------------------------
async def delegate_to_opencode(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Lance `opencode` (sst/opencode, licence MIT) dans chaque microVM
    (`exec_in_workshop`, MCP) avec le perimetre de fichiers de sa sous-tache
    injecte dans le prompt — c'est ce qui garantit l'absence de
    chevauchement (voir docs/specs/05-devfactory-pm-engine.md, section 1).

    Remplace `claude` (Claude Code, CLI proprietaire Anthropic) le
    2026-09-01 : (1) un segfault reproductible du binaire Bun `claude.exe`,
    sans rapport avec l'infrastructure d'atelier (isole hors microVM, sur le
    noeud lui-meme — voir docs/architecture/pieges.md) a montre qu'un CLI
    proprietaire distribue comme executable compile est un point de
    fragilite qu'atelier ne maitrise pas ; (2) atelier vise une plateforme
    entierement open source — y maintenir une dependance de premier plan a
    un outil en licence fermee expose a des changements de conditions
    d'utilisation hors de notre controle. `opencode` reste compatible avec
    l'ecosysteme MCP/skills existant."""
    deps = _deps(config)

    # Une session MCP COURTE par appel, jamais une seule gardee ouverte
    # pendant toute la duree des attentes : la meme classe de defaut que
    # celui deja corrige cote net-proxy/identity-proxy (voir
    # docs/architecture/pieges.md), un cran plus haut dans la chaine.
    # `exec_in_workshop` ne fait que SOUMETTRE la commande et rend la main
    # immediatement (l'attente reelle se fait via `wait_for_exec_completion`,
    # sur un flux SSE totalement separe) — la session MCP n'a donc besoin de
    # vivre que quelques secondes. La garder ouverte pendant l'attente (qui
    # peut durer 10-20+ minutes le temps qu'`opencode` reflechisse) l'exposait
    # a l'idle-timeout d'un hop intermediaire (Traefik, `http://api.atelier.local`) :
    # `MCPError: Session terminated` au moment du DEUXIEME appel (le commit
    # automatique), constate en Workshop reel le 2026-09-02. Meme convention
    # deja en place dans `run_devcontainer_tests` (session fermee avant
    # d'attendre) — ce noeud-ci derogeait seul a la regle.
    for task in state.get("plan", []):
        scope = " ".join(task["scope"])
        # Le commit/push fait partie de la consigne : sans lui, le
        # travail de l'agent reste dans le systeme de fichiers de la
        # microVM et n'atteint jamais la branche de la sous-tache —
        # `OpenPullRequest` ouvrait alors une PR systematiquement VIDE
        # (0 fichier modifie), sans que rien ne signale l'anomalie.
        # Constate en executant le graphe complet pour la premiere fois
        # (2026-08-30).
        greenfield_spec = state.get("greenfield_spec")
        spec_section = (
            f"\n\nArchitecture a suivre (depot vierge, decide en amont pour "
            f"eviter tout choix arbitraire) :\n{greenfield_spec}"
            if greenfield_spec
            else ""
        )
        prompt = (
            f"{task['title']}\n\n{state.get('analysis', '')}{spec_section}\n\n"
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
        # directory`, `fatal: not a git repository`...), et l'agent
        # recevait un prompt tronque. Bug reel constate le 2026-08-30 en
        # executant le graphe complet (avec Claude Code, meme risque
        # avec n'importe quel CLI).
        #
        # Au-dela du dysfonctionnement, c'est une injection de commande :
        # le corps d'un ticket est une entree non fiable, et il finissait
        # interprete par le shell du Workshop. `shlex.quote` produit des
        # guillemets SIMPLES, ou plus rien n'est interprete.
        # `--auto` (opencode) : approuve automatiquement les permissions
        # non explicitement refusees — l'equivalent de
        # `--permission-mode bypassPermissions` cote Claude Code, requis
        # pour la meme raison : en mode non-interactif (`opencode run`),
        # il n'y a personne pour approuver `git add`/`commit`/`push`
        # autrement, et l'agent ecrivait alors un travail complet et
        # correct qui restait en fichiers NON SUIVIS dans la microVM —
        # `OpenPullRequest` ouvrait une PR vide (constate avec Claude
        # Code le 2026-08-31, meme mecanisme applicable ici).
        #
        # Deleguer les permissions est ici sans danger, et c'est meme la
        # raison d'etre d'Atelier : l'agent s'execute dans une microVM
        # Firecracker jetable, sans acces reseau hors allowlist. La
        # frontiere de securite est la microVM, pas l'invite de
        # confirmation d'un CLI.
        # `< /dev/null` : sans stdin ferme, `opencode run` n'ecrit
        # STRICTEMENT RIEN sur sa sortie — pas meme ses messages
        # d'erreur — et reste bloque jusqu'au timeout. Constate le
        # 2026-09-02 en Workshop reel : la meme commande, au caractere
        # pres, passe de "aucune sortie, aucun code d'erreur" a une
        # reponse complete selon que stdin est ferme ou non. Ce n'est
        # pas cosmetique : c'est la difference entre un echec
        # diagnosticable et un blocage muet.
        command = (
            "opencode run --auto "
            f"--model {shlex.quote(deps.opencode_model)} "
            f"{shlex.quote(prompt)} < /dev/null"
        )
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            execution = await call_tool_json(
                session, "exec_in_workshop", {"name": task["workshop_name"], "command": command}
            )
        delegate_result = await wait_for_exec_completion(
            deps.atelier_api_url,
            deps.mcp_token_provider,
            task["workshop_name"],
            execution["executionId"],
        )
        # Un agent qui crashe AVANT d'ecrire quoi que ce soit (constate
        # en pratique avec `claude --print` : segfault Bun au demarrage,
        # exit_code `None`, `stdout` vide — voir
        # docs/architecture/pieges.md) laissait ce noeud continuer comme
        # si de rien n'etait. Le symptome ne remontait alors qu'a
        # `RunDevcontainerTests`, sous une forme trompeuse
        # (`.devcontainer/test.sh: No such file or directory`, qui
        # ressemble a un oubli de l'agent) — et `AutoCorrectionLoop`
        # rappelait le meme binaire casse jusqu'a epuiser tout le budget
        # de correction sans qu'une seule ligne de code soit ecrite. Une
        # erreur d'ENVIRONNEMENT ne se corrige pas en reformulant le
        # prompt : echouer immediatement, sans passer par la boucle de
        # correction.
        if delegate_result.exit_code != 0:
            raise RuntimeError(
                "DelegateToOpencode: opencode n'a pas termine normalement dans "
                f"{task['workshop_name']} — "
                f"{format_test_trace(task['workshop_name'], delegate_result)}"
            )

        # Le commit/push n'est plus laisse a la SEULE diligence de
        # l'agent : la consigne dans le prompt ci-dessus reste (elle
        # aide l'agent a laisser un historique lisible s'il y pense),
        # mais ce n'est plus elle qui garantit quoi que ce soit.
        # Constate en Workshop reel le 2026-09-02 : un agent peut
        # terminer avec exit_code 0, ecrire un code entierement correct,
        # faire passer sa propre suite de tests (5/5) — et neanmoins
        # ne JAMAIS executer `git commit`/`git push`, le dernier geste
        # d'une longue session agentique etant precisement celui qu'un
        # LLM (comme un humain) est le plus susceptible d'oublier.
        # `OpenPullRequest` ouvrait alors une PR au diff vide malgre un
        # travail par ailleurs correct et teste, sans qu'aucun message
        # d'erreur clair ne remonte a l'humain charge de l'approuver.
        #
        # `shlex.quote` sur le titre, jamais une f-string interpolee
        # directement dans le message de commit : meme raisonnement que
        # pour le prompt lui-meme, le titre peut porter des caracteres
        # que le shell interpreterait.
        # `git push` s'execute TOUJOURS a la fin, meme quand il n'y avait
        # rien a committer : un agent qui commite localement mais oublie
        # le `push` (l'autre moitie du meme oubli) laisserait sinon son
        # travail invisible cote Forgejo sans qu'aucune erreur ne le
        # signale — `git diff --cached --quiet` n'aurait alors plus rien
        # a se mettre sous la dent (deja commite), le `||` sauterait le
        # commit, et sans ce `push` inconditionnel, la branche distante
        # resterait figee au commit initial.
        commit_command = (
            "git add -A && "
            f"(git diff --cached --quiet || git commit -m {shlex.quote(task['title'])}) "
            "&& git push origin HEAD"
        )
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            commit_execution = await call_tool_json(
                session,
                "exec_in_workshop",
                {"name": task["workshop_name"], "command": commit_command},
            )
        commit_result = await wait_for_exec_completion(
            deps.atelier_api_url,
            deps.mcp_token_provider,
            task["workshop_name"],
            commit_execution["executionId"],
        )
        if commit_result.exit_code != 0:
            raise RuntimeError(
                "DelegateToOpencode: le commit/push automatique a echoue dans "
                f"{task['workshop_name']} — "
                f"{format_test_trace(task['workshop_name'], commit_result)}"
            )

    return {"phase": "DelegateToOpencode"}


# --------------------------------------------------------------------------
# 5. RunDevcontainerTests
# --------------------------------------------------------------------------
async def integrate_sub_tasks(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Fusionne les branches des sous-taches dans celle de la premiere, dans
    la microVM de cette premiere sous-tache.

    Sans cette etape, le travail parallele n'etait JAMAIS reuni : chaque
    agent poussait sur sa propre branche, `OpenPullRequest` ouvrait la PR
    depuis la branche de la premiere sous-tache seulement, et tout le reste
    restait abandonne sur ses branches (constate le 2026-08-31 sur le ticket
    14 : l'interface web de `task-2` n'apparaissait nulle part dans la PR).

    C'est aussi ce qui rendait `RunDevcontainerTests` incapable de dire quoi
    que ce soit d'utile : il lancait la suite de tests du projet dans CHAQUE
    Workshop, alors que chacun ne contient que sa propre part. Le Workshop
    qui n'avait pas ecrit `test/` echouait sur un `node --test test/` sans
    repertoire — un `exit 1` structurel, sans rapport avec la qualite du
    code. Une suite de tests ne veut dire quelque chose que sur l'ensemble
    reuni.
    """
    deps = _deps(config)
    plan = state.get("plan", [])
    if len(plan) < 2:
        return {"phase": "IntegrateSubTasks", "integration_conflicts": []}

    target = plan[0]
    others = [t["branch_name"] for t in plan[1:]]
    # `--no-edit` : jamais d'editeur interactif. Chaque fusion est tentee
    # separement pour pouvoir nommer precisement celle qui bloque.
    # `-c user.*` plutot que de compter sur l'identite du depot : une image
    # de devcontainer construite avant que `image-builder` ne pose cette
    # identite (ou tiree du cache d'images) n'en a pas, et `git merge` echoue
    # alors sur "Committer identity unknown" — un echec que ce noeud
    # rapportait comme un CONFLIT, ce qui envoyait sur une fausse piste
    # (constate le 2026-08-31). `-c` ne modifie rien dans le depot.
    identity = '-c user.name="Atelier PM" -c user.email="pm@atelier.local"'
    merges = " ".join(
        f'echo "== {b} =="; git {identity} merge --no-edit "origin/{b}" 2>&1 || echo "CONFLIT:{b}";'
        for b in others
    )
    command = (
        f'git fetch --all --quiet; git checkout "{target["branch_name"]}" 2>&1 | tail -1; '
        f"{merges} "
        "git push origin HEAD 2>&1 | tail -2"
    )

    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        execution = await call_tool_json(
            session,
            "exec_in_workshop",
            {"name": target["workshop_name"], "command": command},
        )
    result = await wait_for_exec_completion(
        deps.atelier_api_url,
        deps.mcp_token_provider,
        target["workshop_name"],
        execution["executionId"],
    )
    output = f"{result.stdout}\n{result.stderr}"
    conflicts = [line.split("CONFLIT:", 1)[1].strip() for line in output.splitlines() if "CONFLIT:" in line]
    if conflicts:
        # On n'echoue pas : une PR partielle assortie d'un avertissement
        # explicite vaut mieux qu'un workflow mort, la revue humaine restant
        # de toute facon le dernier mot.
        logger.error(
            "IntegrateSubTasks: fusion impossible pour %s dans %s",
            conflicts,
            target["branch_name"],
        )
    else:
        logger.info(
            "IntegrateSubTasks: %d branche(s) fusionnee(s) dans %s",
            len(others),
            target["branch_name"],
        )
    return {"phase": "IntegrateSubTasks", "integration_conflicts": conflicts}


def format_test_trace(workshop_name: str, result: ExecResult) -> str:
    """Trace de test d'une sous-tache, telle qu'elle sera re-injectee dans le
    prompt de l'agent par `AutoCorrectionLoop`.

    Le code de sortie en fait partie, et une sortie vide est signalee comme
    telle. Sans ca, l'echec d'une sous-tache dont la suite de tests ne produit
    rien (typiquement : elle n'ecrit aucun test, la commande sort en erreur
    sans afficher un mot) donnait un `error_trace` reduit a "Les tests ont
    echoue :" suivi de rien du tout. L'agent rappele pour corriger n'avait
    alors strictement rien sur quoi travailler, et le budget de corrections se
    consommait a vide — constate le 2026-08-31, deux tours de correction
    perdus sur une trace vide.
    """
    body = f"{result.stdout}\n{result.stderr}".strip()
    if not body:
        body = (
            "(aucune sortie : la commande de test n'a rien affiche — "
            "suite de tests absente ou non executee)"
        )
    return f"## {workshop_name} (exit code {result.exit_code})\n{body}\n\n"


async def run_devcontainer_tests(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Execute la suite de tests declaree par le devcontainer
    (convention : `.devcontainer/devcontainer.json` -> champ
    `postStartCommand`/script `test.sh` du projet cible, resolu par le
    guest lui-meme — ce noeud ne fait qu'executer une commande fixe,
    `bash .devcontainer/test.sh`, voir la limite documentee dans
    docs/PROGRESS.md)."""
    deps = _deps(config)
    plan = state.get("plan", [])
    if not plan:
        return {
            "test_output": "",
            "test_passed": False,
            "error_trace": "aucune sous-tache a tester",
            "phase": "RunDevcontainerTests",
        }

    # Un SEUL Workshop, celui qui porte l'integration (voir
    # `IntegrateSubTasks`). Lancer la suite dans chaque Workshop n'avait pas
    # de sens : chacun ne contient que sa propre sous-tache, donc un projet
    # incomplet par construction — celui qui n'avait pas ecrit `test/`
    # echouait sur `node --test test/` faute de repertoire, un `exit 1`
    # structurel sans rapport avec la qualite du code. Une suite de tests ne
    # dit quelque chose que sur l'ensemble reuni.
    target = plan[0]
    all_passed = True
    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        execution = await call_tool_json(
            session,
            "exec_in_workshop",
            {"name": target["workshop_name"], "command": "bash .devcontainer/test.sh"},
        )
    result = await wait_for_exec_completion(
        deps.atelier_api_url,
        deps.mcp_token_provider,
        target["workshop_name"],
        execution["executionId"],
    )
    combined_output = format_test_trace(target["workshop_name"], result)
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
    `DelegateToOpencode` (traces d'erreur re-injectees dans le prompt du
    prochain passage) ou de continuer vers `OpenPullRequest` est prise par
    l'arete conditionnelle `route_after_correction` (voir `graph.py`), pas
    ici : ce noeud ne fait qu'avancer l'etat borne (jamais de boucle
    infinie, meme si les tests echouent indefiniment)."""
    attempts = state.get("correction_attempts", 0) + 1
    return {
        "correction_attempts": attempts,
        # « Corrige ces echecs » tout court laissait l'agent libre de repartir
        # de zero : au 2e tour il reimplementait la fonctionnalite dans une
        # arborescence differente, si bien que la PR finale contenait DEUX
        # implementations completes de la meme chose (constate le 2026-08-31,
        # PR 13 : `src/**` et `api/**` en parallele). On lui dit donc
        # explicitement de partir de l'existant.
        "analysis": (
            f"{state.get('analysis', '')}\n\n"
            f"## Tentative de correction {attempts}\nLes tests ont echoue :\n"
            f"{state.get('error_trace', '')}\n"
            "Corrige ces echecs en MODIFIANT les fichiers deja presents dans le "
            "depot. Ne recree pas l'arborescence et ne reimplemente pas ce qui "
            "existe deja : commence par lire l'etat courant du depot, puis "
            "corrige au plus juste."
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
# 6bis. Detection deterministe des chemins sensibles/infra
# (docs/specs/08-equipe-it-consultative.md, section 4.2 — brique 5.6.2)
# --------------------------------------------------------------------------
# Volontairement generiques : le PM Engine pilote des depots CIBLES
# quelconques (le run de validation de reference portait sur
# `pm-validation-url-shortener`, une app Node.js sans aucun rapport avec ce
# depot), jamais son propre code. Ces motifs ne doivent donc JAMAIS nommer
# un composant interne d'Atelier — seulement des conventions de nommage
# qu'on retrouve dans n'importe quel projet (Node.js, Rust, Python, Go...).
SECURITY_SENSITIVE_PATTERNS = [
    "**/*auth*",
    "**/*credential*",
    "**/*secret*",
    "**/*password*",
    "**/*token*",
    "**/*session*",
    "**/*.pem",
    "**/*.key",
    "**/.env*",
    "**/*oauth*",
    "**/*jwt*",
]
OPS_SENSITIVE_PATTERNS = [
    "**/*.tf",
    "**/migrations/**",
    "Dockerfile*",
    "docker-compose*",
    "**/*.yaml",
    "**/*.yml",
    ".devcontainer/**",
    "**/Chart.yaml",
]

# Capture le chemin CIBLE (« b/... ») de chaque fichier touche. Format
# commun aux trois implementations de `BaseGitProvider.get_diff` : Forgejo
# et GitHub emettent nativement cette ligne, et `GitLabProvider.get_diff` la
# synthetise (voir sa docstring) precisement pour que ce motif reste unique
# quelle que soit la forge.
_DIFF_GIT_HEADER_RE = re.compile(r"^diff --git a/(?:.+) b/(.+)$", re.MULTILINE)


def _diff_file_paths(diff: str) -> list[str]:
    return [match.group(1) for match in _DIFF_GIT_HEADER_RE.finditer(diff)]


def _path_matches(path: str, pattern: str) -> bool:
    """`fnmatch` traite `*` comme un caractere generique qui traverse aussi
    les `/` (ce n'est PAS un glob de chemin comme celui d'un shell) : un
    motif `**/*.tf` exige donc un `/` REELLEMENT present dans le chemin
    compare (`src/main.tf` correspond, `main.tf` a la racine non). On
    retente alors sans le prefixe `**/`, qui matche `*.tf` — couvrant le cas
    racine sans jamais faire regresser le cas imbrique deja couvert par le
    motif complet."""
    if fnmatch.fnmatch(path, pattern):
        return True
    if pattern.startswith("**/"):
        return fnmatch.fnmatch(path, pattern[3:])
    return False


def diff_matches_any_pattern(diff: str, patterns: list[str]) -> bool:
    """Le diff touche-t-il au moins un fichier correspondant a l'un de ces
    motifs ? Deterministe, sans appel LLM — voir la doctrine de
    `_plan_is_credible` : une decision verifiable par du code ne doit
    jamais dependre de l'approximation d'un modele."""
    paths = _diff_file_paths(diff)
    return any(_path_matches(path, pattern) for path in paths for pattern in patterns)


# Un diff volumineux (une sous-tache entiere) gonflerait le prompt de
# revue bien au-dela de ce qui est exploitable — tronque plutot que de
# risquer un depassement de contexte ou une facture LLM disproportionnee.
_REVIEW_DIFF_MAX_CHARS = 20000


def _truncate_diff_for_review(diff: str) -> str:
    if len(diff) <= _REVIEW_DIFF_MAX_CHARS:
        return diff
    return diff[:_REVIEW_DIFF_MAX_CHARS] + "\n... (diff tronque, trop volumineux pour la revue)"


async def _review_diff(deps: PmEngineDeps, state: PMWorkflowState) -> str:
    """Diff de la sous-tache de tete, meme simplification assumee que
    `open_pull_request` (une seule PR/branche de tete dans cette premiere
    version — voir sa docstring). Chaine vide si `plan` est vide ou si le
    provider ne sait pas repondre : les roles de revue degradent alors
    vers `approve` faute de matiere a examiner, plutot que d'echouer."""
    plan = state.get("plan") or []
    if not plan:
        return ""
    diff = await deps.git_provider.get_diff(state["repo"], "main", plan[0]["branch_name"])
    return diff or ""


# --------------------------------------------------------------------------
# 6ter. ReviewCode / ReviewSecurity / ReviewOps (docs/specs/
# 08-equipe-it-consultative.md, section 5 — brique 5.6.4). ReviewCode est
# systematique ; ReviewSecurity/ReviewOps ne se declenchent que si le diff
# touche des chemins sensibles/infra (detection deterministe ci-dessus,
# section 6bis), et s'executent alors EN PARALLELE (fan-out natif
# LangGraph via `route_after_code_review`, qui peut renvoyer les deux cles
# a la fois).
# --------------------------------------------------------------------------
async def review_code(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)
    diff = await _review_diff(deps, state)

    raw_verdict = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Tu es le relecteur QA qui examine le CODE PRODUIT avant "
                    "l'ouverture de la Pull Request. Verifie que le diff repond "
                    "reellement au ticket, sans regression evidente ni code mort "
                    "(fichiers dupliques, implementation abandonnee en cours de "
                    "route). Ne remets PAS en cause des choix de style mineurs. "
                    "Reponds UNIQUEMENT avec un objet JSON : "
                    '{"verdict": "approve", "comments": []} ou '
                    '{"verdict": "request_changes", "comments": ["..."]}.'
                ),
            },
            {
                "role": "user",
                "content": (
                    f"## Ticket\n{state.get('analysis', '')}\n\n"
                    f"## Diff\n{_truncate_diff_for_review(diff)}"
                ),
            },
        ],
    )
    verdict = _parse_review_verdict(raw_verdict, "ReviewCode")

    return {
        "code_review": verdict,
        "security_review_needed": diff_matches_any_pattern(diff, SECURITY_SENSITIVE_PATTERNS),
        "ops_review_needed": diff_matches_any_pattern(diff, OPS_SENSITIVE_PATTERNS),
        "phase": "ReviewCode",
    }


def route_after_code_review(state: PMWorkflowState) -> list[str]:
    """Fan-out natif LangGraph : une liste de plusieurs cles declenche
    l'execution de chacun des noeuds correspondants EN PARALLELE (pas
    besoin de `Send` explicite ici, les branches ne se recouvrent pas).
    Aucune des deux ne se declenche -> saute directement a `ReviewGate`,
    seule facon d'y parvenir sans passer par les deux roles conditionnels."""
    targets = []
    if state.get("security_review_needed"):
        targets.append("ReviewSecurity")
    if state.get("ops_review_needed"):
        targets.append("ReviewOps")
    return targets or ["ReviewGate"]


async def review_security(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)
    diff = await _review_diff(deps, state)

    raw_verdict = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Tu es le relecteur SECURITE. Ce diff touche a des chemins "
                    "sensibles (authentification, secrets, identifiants, jetons, "
                    "sessions...). Verifie qu'aucun secret n'est ecrit en clair, "
                    "qu'aucune verification d'authentification/autorisation n'est "
                    "affaiblie ou contournee, et qu'aucune donnee sensible ne fuite "
                    "(logs, messages d'erreur, reponses HTTP). Reponds UNIQUEMENT "
                    "avec un objet JSON : "
                    '{"verdict": "approve", "comments": []} ou '
                    '{"verdict": "request_changes", "comments": ["..."]}.'
                ),
            },
            {
                "role": "user",
                "content": (
                    f"## Ticket\n{state.get('analysis', '')}\n\n"
                    f"## Diff\n{_truncate_diff_for_review(diff)}"
                ),
            },
        ],
    )
    verdict = _parse_review_verdict(raw_verdict, "ReviewSecurity")

    # Pas de `"phase"` ici : ReviewSecurity peut s'executer EN PARALLELE de
    # ReviewOps (`route_after_code_review`), et LangGraph rejette deux
    # ecritures concurrentes differentes sur la meme cle dans le meme
    # superstep (`InvalidUpdateError: At key 'phase'...`, reproduit en
    # pratique le 2026-09-02 lors du run de validation du ticket #27 des
    # que les deux roles se declenchaient ensemble). `ReviewGate`, seul
    # point de convergence des deux, ecrit la phase pour tout le monde —
    # coherent avec `PIPELINE_PHASES` (`pm_engine.workflows`), qui exclut
    # deja ces deux roles conditionnels de la barre de progression.
    return {"security_review": verdict}


async def review_ops(state: PMWorkflowState, config: RunnableConfig) -> dict:
    deps = _deps(config)
    diff = await _review_diff(deps, state)

    raw_verdict = await deps.llm_client.chat(
        deps.chat_model,
        [
            {
                "role": "system",
                "content": (
                    "Tu es le relecteur OPS/SRE. Ce diff touche a de l'infrastructure "
                    "(manifestes de deploiement, migrations de base de donnees, image "
                    "de conteneur...). Verifie que l'impact deploiement est maitrise : "
                    "une migration irreversible ou destructive sans etape de repli, une "
                    "image qui tourne en root sans raison, un manifeste qui casse un "
                    "environnement existant. Reponds UNIQUEMENT avec un objet JSON : "
                    '{"verdict": "approve", "comments": []} ou '
                    '{"verdict": "request_changes", "comments": ["..."]}.'
                ),
            },
            {
                "role": "user",
                "content": (
                    f"## Ticket\n{state.get('analysis', '')}\n\n"
                    f"## Diff\n{_truncate_diff_for_review(diff)}"
                ),
            },
        ],
    )
    verdict = _parse_review_verdict(raw_verdict, "ReviewOps")

    # Pas de `"phase"` ici — meme raison que `review_security` (voir sa
    # docstring/commentaire) : ecriture concurrente rejetee par LangGraph
    # quand les deux roles tournent en parallele.
    return {"ops_review": verdict}


async def review_gate(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Point de convergence apres `ReviewCode`/`ReviewSecurity`/`ReviewOps` :
    LangGraph n'execute ce noeud qu'une fois toutes les branches declenchees
    par `route_after_code_review` terminees (semantique BSP native, aucune
    synchronisation manuelle necessaire). Ne fait rien d'autre que marquer
    la phase — l'agregation des verdicts est la responsabilite de
    `route_after_review`, l'arete conditionnelle qui suit."""
    return {"phase": "ReviewGate"}


def route_after_review(state: PMWorkflowState) -> str:
    reviews = (state.get("code_review"), state.get("security_review"), state.get("ops_review"))
    if all(review is None or review.get("verdict") != "request_changes" for review in reviews):
        return "OpenPullRequest"
    attempts = state.get("review_attempts", 0) + 1
    if attempts >= state.get("max_review_attempts", 3):
        # Budget de revue epuise : on avance quand meme, un humain tranchera
        # via AwaitHitlApproval — meme doctrine que `route_after_tests`.
        logger.info(
            "ReviewGate: budget de revue epuise (%d tentatives), "
            "passage en force vers OpenPullRequest",
            attempts,
        )
        return "OpenPullRequest"
    return "ReviewReconsideration"


async def prepare_review_reconsideration(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Injecte les objections cumulees de ReviewCode/Security/Ops dans
    `analysis` avant de rebafouiller `DelegateToOpencode` — meme mecanisme
    que `auto_correction_loop` pour les echecs de tests, avec un compteur
    DISTINCT (`review_attempts`) : un code rejete par la revue et un code
    qui ne compile pas sont des echecs de nature differente (voir
    docs/specs/08-equipe-it-consultative.md, section 4.4)."""
    attempts = state.get("review_attempts", 0) + 1
    comments: list[str] = []
    for label, review in (
        ("Code", state.get("code_review")),
        ("Securite", state.get("security_review")),
        ("Ops", state.get("ops_review")),
    ):
        if review and review.get("verdict") == "request_changes":
            comments.extend(f"[{label}] {c}" for c in review.get("comments", []))
    comments_text = "\n".join(f"- {c}" for c in comments)
    return {
        "review_attempts": attempts,
        "analysis": (
            f"{state.get('analysis', '')}\n\n"
            f"## Revue avant PR, tentative {attempts}\n"
            "Le code produit a ete rejete par la revue :\n"
            f"{comments_text}\n"
            "Corrige ces points en MODIFIANT les fichiers deja presents dans le "
            "depot. Ne recree pas l'arborescence et ne reimplemente pas ce qui "
            "existe deja."
        ),
        "phase": "ReviewCode",
    }


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
    # Les conflits d'integration sont dits dans la PR : le relecteur humain
    # doit savoir qu'il ne regarde qu'une partie du travail, sans quoi une PR
    # amputee de la moitie des sous-taches ressemble a une PR complete.
    conflicts = state.get("integration_conflicts") or []
    integration_line = (
        f"⚠️ Branches NON fusionnees (conflit) : {', '.join(conflicts)} — "
        "cette PR ne contient pas leur travail.\n\n"
        if conflicts
        else ""
    )
    body = (
        f"Resout #{state['issue_number']}.\n\n{state.get('analysis', '')}\n\n"
        f"{integration_line}"
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
    # Garde-fou : une PR sans aucun fichier modifie signifie que le travail
    # de l'agent n'a jamais atteint la branche. Ce n'etait pas toujours vrai
    # (le commit/push pouvait auparavant echouer a l'insu de tous, voir
    # `delegate_to_opencode`) — mais depuis que celui-ci garantit lui-meme le
    # commit ET le push, une PR encore vide ICI ne peut plus signifier
    # "l'agent a oublie de committer" : elle signifie que la sous-tache n'a
    # RIEN produit du tout, un vrai echec.
    #
    # Ancien comportement (jusqu'au 2026-09-02) : avertir en log puis laisser
    # le graphe continuer vers la revue humaine, "seule juge" — mais le
    # paquet transmis a `AwaitHitlApproval` (`interrupt`) ne porte que
    # `question`/`pr_url`, jamais `pr_changed_files` : un relecteur qui
    # approuve sans re-verifier la PR lui-meme ne voit jamais qu'elle est
    # vide. Constate en pratique : un run entierement propre par ailleurs
    # (planificateur, tests 5/5) a neanmoins abouti a une PR vide passee
    # inapercue jusqu'a verification manuelle. Meme doctrine que
    # `delegate_to_opencode` : echouer immediatement, sans passer par
    # `AutoCorrectionLoop` (reformuler le prompt ne corrige rien ici) ni par
    # une revue humaine qui n'a rien a approuver.
    changed_files = await deps.git_provider.changed_file_count(state["repo"], pr.number)
    if changed_files == 0:
        raise RuntimeError(
            f"OpenPullRequest: la PR {pr.url} ne contient AUCUN fichier "
            "modifie — la sous-tache n'a rien produit malgre le commit/push "
            f"garanti par DelegateToOpencode, dans les Workshops "
            f"{[t['workshop_name'] for t in plan]}"
        )

    return {
        "pr_number": pr.number,
        "pr_url": pr.url,
        "pr_changed_files": changed_files,
        "phase": "OpenPullRequest",
    }


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
def _outstanding_review_concerns(state: PMWorkflowState) -> list[str]:
    """Objections des quatre roles consultatifs (docs/specs/
    08-equipe-it-consultative.md, section 4.5 — brique 5.6.5) encore
    actives au moment d'atteindre `AwaitHitlApproval`.

    Aucun flag dedie "budget epuise" n'est necessaire : le SEUL moyen
    d'atteindre `ProvisionWorkshop`/`OpenPullRequest` avec un verdict
    encore a `"request_changes"` est que `route_after_architecture_review`/
    `route_after_review` ait laisse passer un budget de revue epuise (un
    verdict `"approve"` ne bloque jamais, et un rejet avec budget restant
    reboucle vers une replanification/redelegation qui REMPLACE ce verdict
    par un nouveau avant de revenir ici). Lire l'etat final suffit donc a
    detecter un passage en force, sans suivre separement le fait qu'il ait
    eu lieu."""
    concerns: list[str] = []
    for label, review in (
        ("Architecture", state.get("architecture_review")),
        ("Code", state.get("code_review")),
        ("Securite", state.get("security_review")),
        ("Ops", state.get("ops_review")),
    ):
        if review and review.get("verdict") == "request_changes":
            concerns.extend(f"[{label}] {c}" for c in review.get("comments", []))
    return concerns


async def await_hitl_approval(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Point d'arret du graphe (`interrupt`, LangGraph) : l'execution se
    suspend ICI et le checkpoint PostgreSQL (tache 5.3.3) persiste l'etat
    complet — reprend exactement a ce noeud, sur n'importe quel worker,
    quand un humain resume le graphe avec sa decision (voir
    `pm_engine.graph.resume_with_decision` et la tache 5.5.2, interface
    Dashboard, hors perimetre de cette session).

    `outstanding_concerns` (toujours present, eventuellement vide) rend
    visible AU RELECTEUR HUMAIN LUI-MEME un passage en force d'un role
    consultatif par epuisement de budget — meme raison que le garde-fou
    deja en place sur `pr_changed_files` dans `open_pull_request` : une
    anomalie que seul l'etat interne du graphe connait, jamais montree
    dans le payload d'interruption, est une anomalie qu'un relecteur qui
    approuve sans re-verifier tout le run ne voit jamais."""
    decision = interrupt(
        {
            "question": "Approuver la fusion de cette Pull Request ?",
            "pr_url": state.get("pr_url"),
            "outstanding_concerns": _outstanding_review_concerns(state),
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


# --------------------------------------------------------------------------
# 12. QAValidation (docs/specs/09-qa-validation-post-merge.md, tache 5.7.3)
# --------------------------------------------------------------------------
QA_EVIDENCE_DIR = ".qa-evidence"


def _qa_workshop_name(issue_number: int) -> str:
    # Disjoint des Workshops de sous-taches (`pm-<issue>-task-N`) : jamais
    # de confusion ni de conflit de nom, meme convention de prefixe.
    return f"pm-{issue_number}-qa"


def _qa_prompt(state: PMWorkflowState) -> str:
    return (
        "Tu es le validateur QA post-merge : le code a DEJA ete fusionne sur "
        "main, ton role est d'en apporter la PREUVE de bon fonctionnement en "
        "executant REELLEMENT l'application — pas une relecture statique du "
        "code, ce role existe deja (ReviewCode).\n\n"
        f"## Ticket resolu\n{state.get('analysis', '')}\n\n"
        "Demarre l'application (installe ses dependances si besoin), puis :\n"
        "- Si elle sert une interface HTML, capture une preuve visuelle : "
        "installe toi-meme un outil de capture d'ecran headless si besoin "
        f"(le devcontainer n'en fournit pas forcement un), et ecris le(s) "
        f"fichier(s) PNG obtenus dans {QA_EVIDENCE_DIR}/.\n"
        "- Sinon (API pure, sans interface web), exerce-la par de VRAIES "
        "requetes HTTP couvrant les criteres d'acceptation du ticket "
        f"ci-dessus, et consigne les reponses obtenues dans des fichiers "
        f"texte sous {QA_EVIDENCE_DIR}/.\n\n"
        "Ne commite JAMAIS ces fichiers de preuve : ils n'appartiennent pas "
        "a l'historique git du projet, seulement a ce run de validation.\n\n"
        "Termine ta reponse par UN SEUL bloc JSON (rien d'autre apres) : "
        '{"verdict": "pass", "comments": [], "evidence_files": '
        f'["{QA_EVIDENCE_DIR}/exemple.png"]}} ou {{"verdict": "fail", '
        '"comments": ["..."], "evidence_files": [...]}.'
    )


# Bug REEL constate le 2026-09-02 (run de validation, ticket #29) : une
# premiere version isolait le verdict par une simple expression reguliere
# interdisant toute accolade INTERNE (`\{[^{}]*"verdict"[^{}]*\}`), en
# supposant qu'un objet {verdict, comments, evidence_files} n'en contient
# jamais. Fausse en pratique — RIEN n'empeche un COMMENTAIRE (texte libre
# redige par l'agent) de citer litteralement une reponse JSON de
# l'application testee, ex: `"corps exact {\"status\":\"ok\"}"` : ces
# accolades, a l'interieur d'une chaine, ne sont pas structurelles, mais
# une regex naive ne le sait pas et rate l'objet entier — l'agent avait
# pourtant produit un verdict parfaitement valide, silencieusement pris
# pour une reponse inexploitable. `_find_json_objects` scanne caractere
# par caractere en suivant si on est DANS une chaine (guillemets non
# echappes), pour ne compter que les accolades reellement structurelles.
def _find_json_objects(text: str) -> list[str]:
    """Sous-chaines correspondant a des objets JSON de plus haut niveau
    (accolades equilibrees), dans l'ordre d'apparition. Ignore les
    accolades a l'interieur des chaines — voir le commentaire ci-dessus."""
    objects: list[str] = []
    depth = 0
    start = 0
    in_string = False
    escaped = False
    for i, char in enumerate(text):
        if in_string:
            if escaped:
                escaped = False
            elif char == "\\":
                escaped = True
            elif char == '"':
                in_string = False
            continue
        if char == '"':
            in_string = True
        elif char == "{":
            if depth == 0:
                start = i
            depth += 1
        elif char == "}" and depth > 0:
            depth -= 1
            if depth == 0:
                objects.append(text[start : i + 1])
    return objects


def _parse_qa_verdict(raw: str) -> dict:
    """Repli INVERSE de `_parse_review_verdict` : une reponse non
    exploitable degrade vers `"fail"`, jamais `"pass"` — ce noeud terminal
    ne bloque plus rien (spec 09, section 6), un repli optimiste
    masquerait une incertitude reelle plutot que de la rendre visible.

    Essaie les objets JSON de plus haut niveau du DERNIER au premier (le
    verdict est cense etre le dernier message de l'agent, mais un objet
    JSON cite plus tot dans son raisonnement — ex: la sortie d'un `cat
    package.json` — ne doit jamais etre pris pour le verdict, voir le
    test correspondant) et retient le premier qui parse ET porte un
    `verdict` reconnu."""
    fallback = {
        "verdict": "fail",
        "comments": ["reponse de l'agent QA non exploitable"],
        "evidence_files": [],
    }
    for candidate in reversed(_find_json_objects(raw)):
        try:
            verdict = json.loads(candidate)
        except json.JSONDecodeError:
            continue
        if isinstance(verdict, dict) and verdict.get("verdict") in ("pass", "fail"):
            verdict.setdefault("comments", [])
            verdict.setdefault("evidence_files", [])
            return verdict
    logger.warning("QAValidation: aucun verdict JSON exploitable dans la reponse de l'agent")
    return fallback


async def _exec_and_wait(deps: PmEngineDeps, workshop_name: str, command: str) -> ExecResult:
    async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
        execution = await call_tool_json(
            session, "exec_in_workshop", {"name": workshop_name, "command": command}
        )
    return await wait_for_exec_completion(
        deps.atelier_api_url, deps.mcp_token_provider, workshop_name, execution["executionId"]
    )


async def _provision_qa_workshop(
    deps: PmEngineDeps, state: PMWorkflowState, workshop_name: str
) -> None:
    """Meme filet d'idempotence que `provision_workshop` (une reprise
    LangGraph peut retomber ici alors que le Workshop existe deja), pointe
    sur `main` — pas sur une branche de sous-tache — puisque c'est
    precisement le code FUSIONNE qu'on veut exercer.

    Reprend explicitement un Workshop deja `Suspended` (contrairement a
    `provision_workshop`, dont le Workshop de sous-tache ne peut jamais
    l'etre avant que CE noeud n'ait fini) : `QAValidation` suspend elle-
    meme son Workshop en fin de course (`_finish_qa_validation`), donc une
    reprise de CE noeud precis (crash entre la suspension et le retour du
    noeud, LangGraph rejoue depuis le debut) peut plausiblement retomber
    sur son propre Workshop deja mis en veille — `_await_workshop_running`
    seule attendrait alors indefiniment (jusqu'au timeout) une transition
    qui ne viendra jamais toute seule."""
    exists = await _workshop_exists(deps, workshop_name)
    if not exists:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            await call_tool_json(
                session,
                "create_workshop",
                {
                    "name": workshop_name,
                    "devcontainerRepo": deps.qa_workshop_devcontainer_repo
                    or state["devcontainer_repo"],
                    "devcontainerRevision": "main",
                    "cpu": "2",
                    "memory": "4Gi",
                    "egressAllowlist": deps.workshop_egress_allowlist,
                    **(
                        {"ownerGroup": deps.workshop_owner_group}
                        if deps.workshop_owner_group
                        else {}
                    ),
                },
            )
    else:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            status = await call_tool_json(session, "get_workshop_status", {"name": workshop_name})
        if (status or {}).get("phase") == "Suspended":
            async with atelier_mcp_session(
                deps.atelier_api_url, deps.mcp_token_provider
            ) as session:
                await call_tool_json(session, "resume_workshop", {"name": workshop_name})
    await _await_workshop_running(deps, workshop_name)


async def _collect_qa_evidence(
    deps: PmEngineDeps, state: PMWorkflowState, workshop_name: str, evidence_files: list[str]
) -> list[str]:
    """Recupere chaque fichier de preuve par le canal EXISTANT
    (`exec_in_workshop`, base64) — voir la section 2 de la spec pour la
    raison de fond (l'authentification S3 signe chaque requete, une
    injection d'en-tete statique comme pour Git/LiteLLM ne peut pas la
    satisfaire) — puis le televerse. Un fichier illisible ou une entree
    S3 non configuree degradent (log + fichier ignore), jamais une
    exception qui ferait echouer tout le noeud pour UNE preuve manquante."""
    if not evidence_files:
        return []
    if deps.qa_evidence_s3 is None:
        logger.info(
            "QAValidation: S3 non configure (`S3_ENDPOINT` absent), %d preuve(s) NON televersee(s)",
            len(evidence_files),
        )
        return []

    prefix = f"qa/{state['repo']}/{state['issue_number']}"
    keys: list[str] = []
    for relative_path in evidence_files:
        # Chaque preuve est independante : une erreur sur L'UNE d'elles
        # (exec MCP en erreur, base64 invalide, S3 injoignable...) ne doit
        # JAMAIS remonter et faire perdre le verdict REEL de l'agent (deja
        # etabli avant cet appel) au profit d'un "en erreur" generique —
        # capture large et deliberee (`Exception`, pas seulement
        # `binascii.Error`), le prix d'une preuve manquante est sans
        # commune mesure avec celui d'un verdict authentique ecrase.
        try:
            result = await _exec_and_wait(
                deps, workshop_name, f"base64 {shlex.quote(relative_path)} | tr -d '\\n'"
            )
            if result.exit_code != 0:
                raise RuntimeError(result.stderr or result.stdout or "exit_code != 0")
            content = base64.b64decode(result.stdout.strip())
            key = f"{prefix}/{relative_path.rsplit('/', 1)[-1]}"
            await upload_evidence(deps.qa_evidence_s3, key, content)
        except Exception as exc:  # noqa: BLE001 - voir ci-dessus
            logger.warning(
                "QAValidation: preuve %s non televersee, ignoree (%s)", relative_path, exc
            )
            continue
        keys.append(key)
    return keys


async def _finish_qa_validation(
    deps: PmEngineDeps,
    state: PMWorkflowState,
    workshop_name: str,
    verdict: dict,
    evidence_keys: list[str],
) -> None:
    """Mise en veille du Workshop + commentaire de PR : purement
    accessoire au verdict deja etabli (calcule AVANT cet appel) — ne doit
    donc jamais faire echouer le noeud si l'un des deux echoue (Workshop
    deja disparu, PR fermee entre-temps...). Meme doctrine que le reste de
    ce noeud terminal (section 6 de la spec)."""
    try:
        async with atelier_mcp_session(deps.atelier_api_url, deps.mcp_token_provider) as session:
            await call_tool_json(session, "suspend_workshop", {"name": workshop_name})
    except Exception as exc:  # noqa: BLE001 - accessoire, voir docstring
        logger.warning("QAValidation: mise en veille de %s echouee (%s)", workshop_name, exc)

    verdict_label = "✅ reussie" if verdict.get("verdict") == "pass" else "⚠️ echouee"
    comments = "\n".join(f"- {c}" for c in verdict.get("comments", []))
    evidence_line = (
        f"\nPreuves : {', '.join(evidence_keys)}" if evidence_keys else "\nAucune preuve televersee."
    )
    body = f"Validation QA post-merge {verdict_label}.\n{comments}{evidence_line}"
    # Sur la PR elle-meme si connue (numero potentiellement different du
    # ticket, comme observe en pratique — issue #27, PR #28), a defaut sur
    # le ticket : meme repli que `merge_and_close`.
    target_number = state.get("pr_number") or state["issue_number"]
    try:
        await deps.git_provider.post_comment(state["repo"], target_number, body)
    except Exception as exc:  # noqa: BLE001 - accessoire, voir docstring
        logger.warning("QAValidation: commentaire de PR echoue (%s)", exc)


async def run_qa_validation(state: PMWorkflowState, config: RunnableConfig) -> dict:
    """Noeud TERMINAL et NON BLOQUANT (spec 09, section 6) : la fusion a
    deja eu lieu (`MergeAndClose`), rien ne peut plus etre defait depuis ce
    point du graphe. Toute erreur ici degrade vers un `qa_verdict` explicite
    plutot que de remonter et faire echouer un run par ailleurs entierement
    reussi (ticket resolu, code fusionne) — voir aussi
    `docs/architecture/pieges.md` pour la meme doctrine deja appliquee a
    `route_after_tests`/`route_after_review` (budget epuise -> on avance
    quand meme), ici poussee a son terme logique : plus aucune arete
    conditionnelle n'existe en sortie, ce noeud est le dernier du graphe."""
    deps = _deps(config)
    workshop_name = _qa_workshop_name(state["issue_number"])
    evidence_keys: list[str] = []
    try:
        await _provision_qa_workshop(deps, state, workshop_name)
        command = (
            "opencode run --auto "
            f"--model {shlex.quote(deps.opencode_model)} "
            f"{shlex.quote(_qa_prompt(state))} < /dev/null"
        )
        result = await _exec_and_wait(deps, workshop_name, command)
        verdict = _parse_qa_verdict(result.stdout)
        evidence_keys = await _collect_qa_evidence(
            deps, state, workshop_name, verdict.get("evidence_files", [])
        )
    except Exception as exc:  # noqa: BLE001 - noeud terminal, ne doit jamais faire echouer le workflow
        logger.warning("QAValidation: erreur d'infrastructure, verdict degrade en echec (%s)", exc)
        verdict = {
            "verdict": "fail",
            "comments": [f"QAValidation en erreur: {exc}"],
            "evidence_files": [],
        }

    await _finish_qa_validation(deps, state, workshop_name, verdict, evidence_keys)
    return {"qa_verdict": verdict, "qa_evidence_keys": evidence_keys, "phase": "QAValidation"}
