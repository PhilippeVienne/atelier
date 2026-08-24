"""Client MCP vers le serveur externe d'Atelier (Jalon M5, tache 5.2.2) :
c'est par ce canal, et JAMAIS par un raccourci interne (appel direct a
l'API Kubernetes, etc.), que le PM pilote le cycle de vie des Workshops
(`create_workshop`, `exec_in_workshop`, `suspend_workshop`...) — memes
outils, meme protocole (SDK officiel `mcp`, transport Streamable HTTP) et
memes regles de visibilite qu'un client MCP externe (Claude Desktop/
Cursor), voir `crates/api-server/src/mcp_server.rs` (Jalon M4) cote
atelier.
"""

from __future__ import annotations

import json
from contextlib import asynccontextmanager
from typing import Any, AsyncIterator

import httpx2
from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client

from .oidc import OidcTokenProvider


@asynccontextmanager
async def atelier_mcp_session(
    base_url: str, token_provider: OidcTokenProvider
) -> AsyncIterator[ClientSession]:
    """Ouvre une session MCP authentifiee vers `{base_url}/v1/mcp`. Une
    session par appelant (le jeton est fixe pour la duree de la session
    Streamable HTTP — pas de rafraichissement en cours de session, coherent
    avec la duree de vie courte d'une session MCP au sein d'un noeud de
    graphe LangGraph).

    `httpx2` (pas `httpx`) : le SDK MCP officiel (`mcp`) requiert
    precisement le client HTTP de sa propre dependance `httpx2` pour
    `streamable_http_client` — constate a l'usage (le type attendu par
    cette fonction n'est pas `httpx.AsyncClient`)."""
    token = await token_provider.get_token()
    async with httpx2.AsyncClient(headers={"Authorization": f"Bearer {token}"}) as http_client:
        async with streamable_http_client(
            f"{base_url.rstrip('/')}/v1/mcp", http_client=http_client
        ) as (read, write):
            async with ClientSession(read, write) as session:
                await session.initialize()
                yield session


async def call_tool_text(session: ClientSession, name: str, arguments: dict[str, Any]) -> str:
    """Appelle un outil MCP et renvoie son resultat texte brut. Leve
    `RuntimeError` (message du serveur inclus) si l'appel echoue cote MCP
    (erreur JSON-RPC de niveau outil, ex: Workshop introuvable,
    Fast-Fail...)."""
    result = await session.call_tool(name, arguments)
    text = result.content[0].text if result.content else ""
    if result.is_error:
        raise RuntimeError(f"appel MCP {name!r} echoue: {text}")
    return text


async def call_tool_json(session: ClientSession, name: str, arguments: dict[str, Any]) -> Any:
    """Comme [`call_tool_text`], mais deserialise le resultat en JSON —
    `create_workshop`/`list_workshops`/`get_workshop_status`/
    `suspend_workshop`/`resume_workshop`/`exec_in_workshop` renvoient tous
    du JSON. `delete_workshop`, seul, renvoie un texte libre de
    confirmation (voir `call_tool_text`), pas du JSON."""
    text = await call_tool_text(session, name, arguments)
    return json.loads(text)
