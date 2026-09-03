"""Point d'entree FastAPI du PM Engine.

Jalon M5, tache 5.5.1/5.5.2 : cable le graphe LangGraph complet
(`pm_engine.graph`), le checkpointer PostgreSQL persistant (tache 5.3.3)
et la table de suivi HITL `pm_reviews` (`pm_engine.runner`) derriere trois
endpoints HTTP consommes par le Dashboard Next.js (BFF, voir
`dashboard/app/api/pm/*`, hors perimetre Python) :

- `POST /chat` (SSE) : "Ask Project Manager" — RAG sur `project_memories`
  (`pm_engine.rag`) + reponse LLM en streaming.
- `GET /reviews` : revues HITL en attente (`pm_engine.runner`).
- `POST /reviews/{thread_id}/decision` : approuve/rejette, reprend le
  graphe LangGraph exactement au point d'interruption.

Chaque requete est authentifiee independamment (`pm_engine.auth`, meme
fournisseur OIDC que `crates/api-server/src/auth.rs`) : le Dashboard
transmet le JWT de l'utilisateur, jamais un en-tete de confiance implicite.

Lancement local :

    uvicorn pm_engine.main:app --reload --port 8100
"""

from __future__ import annotations

import asyncio
import base64
import json
import logging
import os
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Literal

import asyncpg
from fastapi import Depends, FastAPI, Header, HTTPException, Request
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .auth import AuthState, Claims, make_require_auth
from .checkpointer import build_checkpointer
from .deps import PmEngineDeps
from .evidence_store import s3_config_from_env
from .git_providers import ForgejoProvider
from .graph import build_graph
from .llm_client import LlmClient
from .oidc import OidcTokenProvider
from .rag import search_memories
from .runner import list_pending_reviews, resume_review, start_workflow
from .workflows import get_workflow, list_workflows

logger = logging.getLogger(__name__)


def _jwt_subject(token: str) -> str:
    """Extrait `sub` d'un JWT sans verifier sa signature : usage interne
    uniquement, pour connaitre notre propre identite `atelier-pm-bot` a
    partir du jeton qu'on vient nous-memes d'obtenir aupres de Keycloak
    (pas un jeton externe non fiable)."""
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))["sub"]


def _qa_evidence_s3_config_or_none():
    """`s3_config_from_env` leve une `RuntimeError` si `S3_ENDPOINT` est
    present mais la configuration incomplete (bucket/region/cles absents)
    — un garde-fou utile pour `evidence_store` lui-meme, mais qui ne doit
    JAMAIS faire planter le demarrage de TOUT `pm-engine` pour une
    fonctionnalite censee etre optionnelle (`QAValidation` degrade deja
    proprement quand `qa_evidence_s3` est `None`, voir sa docstring) —
    contrairement aux variables de `required` ci-dessous, dont l'absence
    coupe reellement `/chat`/`/reviews`. Une erreur de configuration S3 ne
    doit couter que les preuves QA, jamais le reste du service."""
    try:
        return s3_config_from_env()
    except RuntimeError as exc:
        logger.warning(
            "configuration S3 incomplete (%s) : QAValidation degradera sans "
            "televerser de preuves",
            exc,
        )
        return None


@asynccontextmanager
async def _lifespan(app: FastAPI) -> AsyncIterator[None]:
    auth_state = AuthState.from_env()
    app.state.require_auth_impl = make_require_auth(auth_state)
    app.state.deps = None
    app.state.graph = None

    database_url = os.environ.get("DATABASE_URL_PM")
    forgejo_url = os.environ.get("FORGEJO_URL")
    forgejo_token = os.environ.get("FORGEJO_TOKEN")
    keycloak_token_url = os.environ.get("KEYCLOAK_TOKEN_URL")
    keycloak_pm_bot_secret = os.environ.get("KEYCLOAK_PM_BOT_SECRET")
    litellm_url = os.environ.get("LITELLM_URL")
    litellm_master_key = os.environ.get("LITELLM_MASTER_KEY")
    atelier_api_url = os.environ.get("ATELIER_API_URL")

    required = {
        "DATABASE_URL_PM": database_url,
        "FORGEJO_URL": forgejo_url,
        "FORGEJO_TOKEN": forgejo_token,
        "KEYCLOAK_TOKEN_URL": keycloak_token_url,
        "KEYCLOAK_PM_BOT_SECRET": keycloak_pm_bot_secret,
        "LITELLM_URL": litellm_url,
        "LITELLM_MASTER_KEY": litellm_master_key,
        "ATELIER_API_URL": atelier_api_url,
    }
    missing = [name for name, value in required.items() if not value]
    if missing:
        logger.warning(
            "variables d'environnement absentes (%s) : /chat et /reviews "
            "repondront 503, seul /health fonctionnera",
            ", ".join(missing),
        )
        yield
        return

    git_provider = ForgejoProvider(forgejo_url, forgejo_token)
    llm_client = LlmClient(litellm_url, litellm_master_key)
    token_provider = OidcTokenProvider(keycloak_token_url, "atelier-pm-bot", keycloak_pm_bot_secret)
    pm_bot_subject = _jwt_subject(await token_provider.get_token())
    pool = await asyncpg.create_pool(database_url, min_size=1, max_size=10)

    app.state.deps = PmEngineDeps(
        git_provider=git_provider,
        llm_client=llm_client,
        atelier_api_url=atelier_api_url,
        mcp_token_provider=token_provider,
        db_pool=pool,
        pm_bot_subject=pm_bot_subject,
        # Allowlist egress des Workshops crees par le PM
        # (`ProvisionWorkshop`) : liste de domaines separee par des
        # virgules. Sans elle, ces Workshops naissent avec une allowlist
        # vide et leur build d'image echoue systematiquement — voir
        # `PmEngineDeps.workshop_egress_allowlist`.
        workshop_egress_allowlist=[
            d.strip()
            for d in os.environ.get("PM_ENGINE_WORKSHOP_EGRESS_ALLOWLIST", "").split(",")
            if d.strip()
        ],
        # `PM_ENGINE_CHAT_MODEL` : aucune cle payante reelle n'est
        # provisionnee dans cet environnement de dev (voir
        # docs/PROGRESS.md) — permet de pointer `/chat` vers un modele
        # `mock_response` LiteLLM (`atelier-budget-test`) pour les tests,
        # sans toucher au code. Defaut de production inchange.
        workshop_owner_group=os.environ.get("PM_ENGINE_WORKSHOP_OWNER_GROUP", ""),
        devcontainer_repo_template=os.environ.get("PM_ENGINE_DEVCONTAINER_REPO_TEMPLATE", ""),
        chat_model=os.environ.get("PM_ENGINE_CHAT_MODEL", "sonnet-premium"),
        opencode_model=os.environ.get(
            "PM_ENGINE_OPENCODE_MODEL", "atelier/atelier-workshop-agent"
        ),
        # Construite une seule fois ici, jamais reconstruite a chaque run
        # (`s3_config_from_env` — voir sa docstring pour la degradation
        # explicite si `S3_ENDPOINT` est absent : `QAValidation` produit
        # alors un verdict mais ne televerse aucune preuve).
        qa_evidence_s3=_qa_evidence_s3_config_or_none(),
        qa_workshop_devcontainer_repo=os.environ.get(
            "PM_ENGINE_QA_WORKSHOP_DEVCONTAINER_REPO", ""
        ),
    )

    async with build_checkpointer(database_url) as checkpointer:
        app.state.graph = build_graph(checkpointer)
        logger.info("PM Engine pret (tenant=%s)", pm_bot_subject)
        try:
            yield
        finally:
            await git_provider.aclose()
            await llm_client.aclose()
            await pool.close()


app = FastAPI(
    title="Atelier PM Engine",
    description=(
        "Moteur DevFactory & Project Manager autonome d'Atelier "
        "(voir docs/specs/05-devfactory-pm-engine.md)."
    ),
    version="0.1.0",
    lifespan=_lifespan,
)


@app.get("/health")
async def health() -> dict[str, str]:
    """Sonde de sante liveness/readiness (pas de dependance externe verifiee ici)."""
    return {"status": "ok"}


async def require_auth(request: Request, authorization: str | None = Header(default=None)) -> Claims:
    """Delegue a `pm_engine.auth.make_require_auth` (le meme callable que
    `tests/test_auth.py` valide contre le vrai Keycloak de dev), construit
    une seule fois au demarrage dans `_lifespan` — un `AuthState` a besoin
    du JWKS charge une fois, pas a chaque requete."""
    return await request.app.state.require_auth_impl(authorization=authorization)


def _require_deps(request: Request) -> PmEngineDeps:
    deps = request.app.state.deps
    if deps is None:
        raise HTTPException(
            status_code=503,
            detail="PM Engine non configure (variables d'environnement absentes)",
        )
    return deps


class ChatMessage(BaseModel):
    role: Literal["user", "assistant"]
    content: str


class ChatRequest(BaseModel):
    # Optionnel : le Dashboard laisse poser une question sans cibler de
    # projet (fonctionnement general, "importe tel depot"...). Exiger un
    # depot rendait le bouton d'envoi inerte sans rien expliquer, alors que
    # beaucoup de questions n'en dependent pas.
    repo: str = ""
    query: str
    # Tour(s) precedents de CETTE conversation, tels qu'affiches par le
    # Dashboard (`dashboard/app/pm/pm-chat.tsx`) : sans ca, chaque appel est
    # traite comme une toute premiere conversation (bug constate en
    # pratique — l'agent repond "je n'ai aucune memoire de nos echanges
    # passes" des le deuxieme message). Seul le texte final est rejoue, pas
    # les `tool_calls` intermediaires d'un tour precedent (le Dashboard ne
    # les stocke pas non plus) : un `setup_mirror_project` deja execute
    # reste donc visible dans le texte de la reponse assistante qui suit,
    # ce qui suffit au LLM pour ne pas le refaire si on le lui redemande.
    history: list[ChatMessage] = []


# Outil expose au LLM (Jalon M5, "Projets") : importer un depot GitHub/GitLab
# (prive ou public) comme miroir Forgejo interne, directement depuis une
# demande en langage naturel ("importe github.com/acme/widgets"). Le jeton
# d'acces d'un depot prive, s'il est fourni, transite alors par le message
# utilisateur puis par ce meme appel LLM avant d'atteindre cet outil —
# compromis assume du choix "conversationnel" plutot qu'un formulaire dedie
# (`dashboard/app/projects/new`, toujours disponible pour eviter ce transit
# quand ce n'est pas souhaite). Jamais journalise ni persiste par ce module.
SETUP_MIRROR_PROJECT_TOOL = {
    "type": "function",
    "function": {
        "name": "setup_mirror_project",
        "description": (
            "Importe un depot GitHub ou GitLab (prive ou public) comme miroir "
            "Forgejo interne, resynchronise automatiquement toutes les 10 "
            "minutes. A appeler uniquement quand l'utilisateur demande "
            "explicitement d'importer/mirrorer/configurer un nouveau projet "
            "depuis une URL externe — jamais pour une simple question sur un "
            "projet deja configure."
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "nom court du projet (slug Forgejo), ex: widgets",
                },
                "source_url": {
                    "type": "string",
                    "description": "URL HTTPS du depot source, ex: https://github.com/acme/widgets",
                },
                "private": {
                    "type": "boolean",
                    "description": "vrai si le depot source est prive",
                },
                "token": {
                    "type": "string",
                    "description": (
                        "jeton d'acces personnel (PAT) pour un depot prive ; "
                        "omis pour un depot public. Ne jamais inventer de "
                        "valeur : demander a l'utilisateur de le fournir si "
                        "private=true et qu'aucun jeton n'a ete donne."
                    ),
                },
            },
            "required": ["name", "source_url", "private"],
        },
    },
}


async def _run_tool_call(deps: PmEngineDeps, call: dict) -> str:
    """Execute un `tool_call` renvoye par le LLM et renvoie le contenu JSON
    du message `role: tool` correspondant — jamais d'exception qui
    remonterait jusqu'au flux SSE : un echec (nom de projet deja pris, URL
    invalide, jeton refuse...) doit rester une reponse que le LLM peut lire
    et reformuler pour l'utilisateur, pas une coupure brutale du chat."""
    name = call["function"]["name"]
    if name != "setup_mirror_project":
        return json.dumps({"status": "error", "message": f"outil inconnu: {name}"})
    try:
        args = json.loads(call["function"]["arguments"])
    except json.JSONDecodeError as exc:
        return json.dumps({"status": "error", "message": f"arguments invalides: {exc}"})
    if not isinstance(deps.git_provider, ForgejoProvider):
        return json.dumps({"status": "error", "message": "miroir Forgejo non disponible"})
    try:
        project = await deps.git_provider.create_mirror(
            name=args["name"],
            source_url=args["source_url"],
            private=bool(args.get("private", False)),
            token=args.get("token"),
        )
    except Exception as exc:  # noqa: BLE001 - traduit en resultat d'outil, voir docstring
        logger.warning("echec setup_mirror_project(%s): %s", args.get("name"), exc)
        return json.dumps({"status": "error", "message": str(exc)})
    return json.dumps(
        {
            "status": "ok",
            "full_name": project.full_name,
            "clone_url": project.clone_url,
            "private": project.private,
        }
    )


async def _save_chat_turn(
    deps: PmEngineDeps,
    user_sub: str,
    repo: str,
    query: str,
    answer: str,
    tool_calls: list[dict],
) -> None:
    """Persiste le tour user+assistant dans `pm_chat_messages` (tache
    5.5.1, historique persistant), `tool_calls` inclus (tache suivante,
    "elements interactifs" : sans ca, la carte d'appel d'outil affichee en
    direct disparaissait a chaque rechargement de page alors que le texte
    final qui la suit restait visible — un tour visuellement incoherent
    avec lui-meme). Appele apres coup, jamais sur le chemin critique du
    streaming SSE — une panne d'ecriture ne doit jamais empecher
    l'utilisateur de lire la reponse deja recue, seulement lui couter la
    persistance de CE tour (journalise, pas leve)."""
    if not answer:
        # Reponse vide (erreur en cours de stream, connexion coupee...) :
        # rien de coherent a rejouer au tour suivant.
        return
    try:
        async with deps.db_pool.acquire() as conn:  # type: ignore[attr-defined]
            async with conn.transaction():
                await conn.execute(
                    "INSERT INTO pm_chat_messages (user_sub, repo, role, content) VALUES ($1, $2, 'user', $3)",
                    user_sub, repo, query,
                )
                await conn.execute(
                    "INSERT INTO pm_chat_messages (user_sub, repo, role, content, tool_calls) "
                    "VALUES ($1, $2, 'assistant', $3, $4)",
                    user_sub, repo, answer, json.dumps(tool_calls),
                )
    except Exception:  # voir docstring : jamais fatal, seulement journalise
        logger.warning("echec de persistance du tour de chat PM (user=%s)", user_sub, exc_info=True)


@app.post("/chat")
async def chat(
    body: ChatRequest,
    request: Request,
    claims: Claims = Depends(require_auth),
) -> StreamingResponse:
    """"Ask Project Manager" (tache 5.5.1) : RAG sur `project_memories`
    scope par tenant `atelier-pm-bot` (voir `pm_engine.rag`), puis reponse
    LLM en streaming SSE — le Dashboard consomme ce flux directement
    (`EventSource`/`fetch` + `ReadableStream`, voir
    `dashboard/app/workshops/[name]/events/route.ts` pour le pattern SSE
    deja etabli cote BFF). Le LLM dispose aussi de l'outil
    `setup_mirror_project` ci-dessus ("Projets") : un premier appel
    non-streamant decide s'il l'invoque, seule la reponse finale en langage
    naturel est ensuite streamee — voir `LlmClient.chat_with_tools`. Un
    appel d'outil produit aussi deux evenements SSE dedies (`tool_call` puis
    `tool_result`, "elements interactifs") pour que le Dashboard l'affiche
    en direct plutot que de le laisser invisible jusqu'a ce que le LLM le
    mentionne lui-meme dans son texte — voir `dashboard/app/pm/pm-chat.tsx`.
    Le tour complet (question, reponse finale, ET les tool_calls executes)
    est persiste dans `pm_chat_messages` une fois le flux termine, rejouable
    via `GET /chat/history` (colonne `tool_calls`, toujours en etat "done" a
    la relecture — un tour interrompu en cours d'outil n'ecrit jamais de
    reponse, voir `_save_chat_turn`)."""
    deps: PmEngineDeps = _require_deps(request)

    async def event_stream() -> AsyncIterator[bytes]:
        matches = await search_memories(
            deps.db_pool, deps.llm_client, deps.embedding_model, deps.pm_bot_subject,
            body.query, limit=5,
        )
        context = "\n\n".join(f"- ({m.repo}) {m.content}" for m in matches)
        messages = [
            {
                "role": "system",
                "content": (
                    "Tu es le Project Manager autonome d'Atelier"
                    + (f" pour le depot {body.repo}." if body.repo else
                       ", aucun projet n'est cible par cette conversation.")
                    + " Reponds en te basant sur ces memoires passees "
                    f"si elles sont pertinentes :\n\n{context or '(aucune memoire pertinente)'}"
                    "\n\nTu disposes de l'outil setup_mirror_project pour importer "
                    "un nouveau projet GitHub/GitLab comme miroir interne, sur "
                    "demande explicite de l'utilisateur uniquement."
                ),
            },
            *[{"role": m.role, "content": m.content} for m in body.history],
            {"role": "user", "content": body.query},
        ]
        answer = ""
        # Accumule les tool_calls du tour pour `_save_chat_turn` : la meme
        # forme que les evenements SSE `tool_call`/`tool_result`, sauf que
        # celle-ci survit a un rechargement de page (`GET /chat/history`).
        persisted_tool_calls: list[dict] = []
        try:
            message = await deps.llm_client.chat_with_tools(
                deps.chat_model, messages, tools=[SETUP_MIRROR_PROJECT_TOOL]
            )
            tool_calls = message.get("tool_calls") or []
            if not tool_calls:
                content = message.get("content") or ""
                if content:
                    answer = content
                    yield f"data: {json.dumps({'delta': content})}\n\n".encode()
            else:
                messages.append(message)
                for call in tool_calls:
                    # Evenement SSE dedie (Jalon M5, "elements interactifs") :
                    # avant cette carte, un appel d'outil restait invisible
                    # tant que le LLM ne le racontait pas lui-meme dans son
                    # texte final — le Dashboard ne peut afficher que ce
                    # qu'il recoit explicitement, pas deviner un tool_call
                    # execute silencieusement cote serveur.
                    try:
                        args = json.loads(call["function"]["arguments"])
                    except json.JSONDecodeError:
                        args = {}
                    # `id` (pas seulement `name`) identifie la carte cote
                    # Dashboard : deux appels au MEME outil dans un seul tour
                    # (rare mais permis par le protocole tool_calls) ne
                    # doivent pas se faire ecraser l'un l'autre au moment de
                    # rattacher le `tool_result` correspondant.
                    yield (
                        f"data: {json.dumps({'tool_call': {'id': call['id'], 'name': call['function']['name'], 'arguments': args}})}\n\n"
                    ).encode()
                    result = await _run_tool_call(deps, call)
                    result_obj = json.loads(result)
                    yield (
                        f"data: {json.dumps({'tool_result': {'id': call['id'], 'name': call['function']['name'], 'result': result_obj}})}\n\n"
                    ).encode()
                    persisted_tool_calls.append(
                        {
                            "id": call["id"],
                            "name": call["function"]["name"],
                            "arguments": args,
                            "result": result_obj,
                        }
                    )
                    messages.append(
                        {"role": "tool", "tool_call_id": call["id"], "content": result}
                    )
                async for delta in deps.llm_client.chat_stream(
                    deps.chat_model, messages, tools=[SETUP_MIRROR_PROJECT_TOOL]
                ):
                    answer += delta
                    yield f"data: {json.dumps({'delta': delta})}\n\n".encode()
        except Exception as exc:  # noqa: BLE001 - traduit en evenement SSE d'erreur
            logger.warning("erreur pendant le streaming du chat PM: %s", exc)
            yield f"data: {json.dumps({'error': str(exc)})}\n\n".encode()
        await _save_chat_turn(deps, claims.sub, body.repo, body.query, answer, persisted_tool_calls)
        yield b"data: [DONE]\n\n"

    return StreamingResponse(event_stream(), media_type="text/event-stream")


@app.get("/chat/history")
async def chat_history(
    request: Request,
    repo: str = "",
    claims: Claims = Depends(require_auth),
) -> list[dict]:
    """Historique persiste des tours de chat PM de L'UTILISATEUR COURANT
    pour `repo` (tache 5.5.1) : consomme au montage de
    `dashboard/app/pm/pm-chat.tsx` pour ne plus perdre la conversation a
    chaque rechargement de page. Scope par `user_sub` (JWT), pas par
    `pm_bot_subject` — voir la docstring de la migration
    `pm_chat_messages` pour la raison (donnees personnelles, pas
    partagees comme `project_memories`/`pm_reviews`)."""
    deps: PmEngineDeps = _require_deps(request)
    async with deps.db_pool.acquire() as conn:  # type: ignore[attr-defined]
        rows = await conn.fetch(
            "SELECT role, content, tool_calls, created_at FROM pm_chat_messages "
            "WHERE user_sub = $1 AND repo = $2 ORDER BY created_at ASC",
            claims.sub,
            repo,
        )
    return [
        {
            "role": row["role"],
            "content": row["content"],
            # asyncpg ne decode pas nativement `jsonb` sans codec dedie sur
            # le pool (aucun configure ici, meme convention que
            # `pm_engine.nodes.index_knowledge` pour `metadata`) : `row[...]`
            # est donc le texte JSON brut, pas encore une valeur Python.
            "tool_calls": json.loads(row["tool_calls"]),
            "created_at": row["created_at"].isoformat(),
        }
        for row in rows
    ]


@app.get("/reviews")
async def reviews(
    request: Request, _claims: Claims = Depends(require_auth)
) -> list[dict]:
    """Revues HITL en attente (tache 5.5.2) : partagees entre tous les
    utilisateurs authentifies de cette instance Atelier, voir
    `pm_engine.runner`/`pm_engine.rag` pour la meme justification de
    perimetre `tenant_id`."""
    deps: PmEngineDeps = _require_deps(request)
    rows = await list_pending_reviews(deps)
    return [
        {
            "thread_id": row["thread_id"],
            "repo": row["repo"],
            "issue_number": row["issue_number"],
            "pr_url": row["pr_url"],
            "created_at": row["created_at"].isoformat(),
        }
        for row in rows
    ]


class DecisionRequest(BaseModel):
    decision: str


@app.post("/reviews/{thread_id}/decision")
async def decide_review(
    thread_id: str,
    body: DecisionRequest,
    request: Request,
    _claims: Claims = Depends(require_auth),
) -> dict:
    """Approuve/rejette une PR ouverte par le bot (tache 5.5.2) : reprend
    le graphe LangGraph exactement au noeud `AwaitHitlApproval`, sur l'etat
    complet restaure depuis le checkpoint PostgreSQL (tache 5.3.3, pas
    seulement la decision elle-meme)."""
    deps: PmEngineDeps = _require_deps(request)
    graph = request.app.state.graph
    if body.decision not in ("approved", "rejected"):
        raise HTTPException(status_code=400, detail="decision doit etre 'approved' ou 'rejected'")
    try:
        result = await resume_review(graph, deps, thread_id, body.decision)
    except Exception as exc:  # remonte comme 404/500 explicite au BFF
        logger.warning("echec de reprise du thread %s: %s", thread_id, exc)
        raise HTTPException(status_code=404, detail=f"thread {thread_id} introuvable ou invalide") from exc
    return {"thread_id": thread_id, "status": result.get("status", "unknown")}


# --------------------------------------------------------------------------
# Suivi des workflows (« mission control » du Dashboard)
# --------------------------------------------------------------------------
class WorkflowRequest(BaseModel):
    repo: str
    issue_number: int
    # Optionnel : par defaut, deduit de `PM_ENGINE_DEVCONTAINER_REPO_TEMPLATE`
    # (voir `PmEngineDeps.devcontainer_repo_template`). Ce gabarit peut porter
    # des identifiants, qui n'ont alors aucune raison de transiter par
    # l'interface — le Dashboard n'envoie que `owner/nom`.
    devcontainer_repo: str | None = None


@app.post("/workflows")
async def launch_workflow(
    body: WorkflowRequest,
    request: Request,
    _claims: Claims = Depends(require_auth),
) -> dict:
    """Demarre un workflow PM sur un ticket, et rend la main immediatement.

    Le graphe tourne une dizaine de minutes : le tenir dans la requete HTTP
    condamnerait l'appelant a une connexion ouverte aussi longtemps, pour
    rien — l'etat est de toute facon persiste a chaque noeud dans le
    checkpointer, et c'est `GET /workflows/{thread_id}` qui le donne. La
    tache de fond n'est donc pas un detail d'implementation : elle est le
    pendant naturel d'un graphe qui sait deja reprendre ou il en etait.

    LIMITE ASSUMEE : cette tache vit dans le processus pm-engine. Le
    redemarrer pendant un run l'interrompt — l'etat reste intact dans le
    checkpointer et un nouvel appel sur le meme `thread_id` reprend ou on en
    etait, mais RIEN ne le fait automatiquement. Un workflow orphelin reste
    donc fige sur sa derniere phase jusqu'a ce qu'on le relance. Le rendre
    reellement resistant demanderait une file de travaux persistante et un
    reprise au demarrage, ce qui depasse ce jalon.
    """
    deps: PmEngineDeps = _require_deps(request)
    graph = request.app.state.graph
    thread_id = f"{body.repo}#{body.issue_number}"

    devcontainer_repo = body.devcontainer_repo or (
        deps.devcontainer_repo_template.replace("{repo}", body.repo)
        if deps.devcontainer_repo_template
        else ""
    )
    if not devcontainer_repo:
        raise HTTPException(
            status_code=400,
            detail=(
                "devcontainer_repo absent et PM_ENGINE_DEVCONTAINER_REPO_TEMPLATE "
                "non configure : impossible de savoir comment un guest clone ce depot"
            ),
        )

    async def _run() -> None:
        try:
            await start_workflow(
                graph, deps, thread_id, body.repo, body.issue_number, devcontainer_repo
            )
        except Exception as exc:  # journalise, jamais avale en silence
            logger.exception("workflow %s interrompu: %s", thread_id, exc)

    asyncio.create_task(_run())
    return {"thread_id": thread_id, "repo": body.repo, "issue_number": body.issue_number}


@app.get("/workflows")
async def workflows(
    request: Request, _claims: Claims = Depends(require_auth)
) -> list[dict]:
    """Workflows connus, du plus recemment actif au plus ancien."""
    deps: PmEngineDeps = _require_deps(request)
    return await list_workflows(request.app.state.graph, deps)


@app.get("/workflows/{thread_id:path}")
async def workflow_state(
    thread_id: str,
    request: Request,
    _claims: Claims = Depends(require_auth),
) -> dict:
    """Etat courant d'un workflow, relu depuis son checkpoint.

    `{thread_id:path}` et non `{thread_id}` : un identifiant de thread vaut
    `owner/repo#42` et contient donc une barre oblique.
    """
    deps: PmEngineDeps = _require_deps(request)
    state = await get_workflow(request.app.state.graph, deps, thread_id)
    if state is None:
        raise HTTPException(status_code=404, detail=f"workflow {thread_id} introuvable")
    return state
