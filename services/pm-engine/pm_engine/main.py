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

import base64
import json
import logging
import os
from contextlib import asynccontextmanager
from typing import AsyncIterator, Literal

import asyncpg
from fastapi import Depends, FastAPI, Header, HTTPException, Request
from fastapi.responses import StreamingResponse
from pydantic import BaseModel

from .auth import AuthState, Claims, make_require_auth
from .checkpointer import build_checkpointer
from .deps import PmEngineDeps
from .git_providers import ForgejoProvider
from .graph import build_graph
from .llm_client import LlmClient
from .oidc import OidcTokenProvider
from .rag import search_memories
from .runner import list_pending_reviews, resume_review

logger = logging.getLogger(__name__)


def _jwt_subject(token: str) -> str:
    """Extrait `sub` d'un JWT sans verifier sa signature : usage interne
    uniquement, pour connaitre notre propre identite `atelier-pm-bot` a
    partir du jeton qu'on vient nous-memes d'obtenir aupres de Keycloak
    (pas un jeton externe non fiable)."""
    payload = token.split(".")[1]
    payload += "=" * (-len(payload) % 4)
    return json.loads(base64.urlsafe_b64decode(payload))["sub"]


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
        chat_model=os.environ.get("PM_ENGINE_CHAT_MODEL", "sonnet-premium"),
        claude_code_model=os.environ.get(
            "PM_ENGINE_CLAUDE_CODE_MODEL", "claude-3-5-sonnet-20241022"
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
    repo: str
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


@app.post("/chat")
async def chat(
    body: ChatRequest,
    request: Request,
    _claims: Claims = Depends(require_auth),
) -> StreamingResponse:
    """"Ask Project Manager" (tache 5.5.1) : RAG sur `project_memories`
    scope par tenant `atelier-pm-bot` (voir `pm_engine.rag`), puis reponse
    LLM en streaming SSE — le Dashboard consomme ce flux directement
    (`EventSource`/`fetch` + `ReadableStream`, voir
    `dashboard/app/workshops/[name]/events/route.ts` pour le pattern SSE
    deja etabli cote BFF). Le LLM dispose aussi de l'outil
    `setup_mirror_project` ci-dessus ("Projets") : un premier appel
    non-streamant decide s'il l'invoque, seule la reponse finale en langage
    naturel est ensuite streamee — voir `LlmClient.chat_with_tools`."""
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
                    "Tu es le Project Manager autonome d'Atelier pour le depot "
                    f"{body.repo}. Reponds en te basant sur ces memoires passees "
                    f"si elles sont pertinentes :\n\n{context or '(aucune memoire pertinente)'}"
                    "\n\nTu disposes de l'outil setup_mirror_project pour importer "
                    "un nouveau projet GitHub/GitLab comme miroir interne, sur "
                    "demande explicite de l'utilisateur uniquement."
                ),
            },
            *[{"role": m.role, "content": m.content} for m in body.history],
            {"role": "user", "content": body.query},
        ]
        try:
            message = await deps.llm_client.chat_with_tools(
                deps.chat_model, messages, tools=[SETUP_MIRROR_PROJECT_TOOL]
            )
            tool_calls = message.get("tool_calls") or []
            if not tool_calls:
                content = message.get("content") or ""
                if content:
                    yield f"data: {json.dumps({'delta': content})}\n\n".encode()
            else:
                messages.append(message)
                for call in tool_calls:
                    result = await _run_tool_call(deps, call)
                    messages.append(
                        {"role": "tool", "tool_call_id": call["id"], "content": result}
                    )
                async for delta in deps.llm_client.chat_stream(deps.chat_model, messages):
                    yield f"data: {json.dumps({'delta': delta})}\n\n".encode()
        except Exception as exc:  # noqa: BLE001 - traduit en evenement SSE d'erreur
            logger.warning("erreur pendant le streaming du chat PM: %s", exc)
            yield f"data: {json.dumps({'error': str(exc)})}\n\n".encode()
        yield b"data: [DONE]\n\n"

    return StreamingResponse(event_stream(), media_type="text/event-stream")


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
    except Exception as exc:  # noqa: BLE001 - remonte comme 404/500 explicite au BFF
        logger.warning("echec de reprise du thread %s: %s", thread_id, exc)
        raise HTTPException(status_code=404, detail=f"thread {thread_id} introuvable ou invalide") from exc
    return {"thread_id": thread_id, "status": result.get("status", "unknown")}
