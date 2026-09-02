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
from collections.abc import AsyncIterator
from contextlib import asynccontextmanager
from typing import Any

import httpx2
from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client

from .oidc import OidcTokenProvider


class _OidcAuth(httpx2.Auth):
    """Pose l'en-tete `Authorization` a CHAQUE requete HTTP de la session,
    en demandant son jeton au provider (qui le met en cache et le renouvelle
    avant expiration — voir `OidcTokenProvider`).

    Un en-tete fige a l'ouverture de la session ne tient pas : une session
    Streamable HTTP emet plusieurs requetes HTTP au fil de sa vie (POST de
    l'appel d'outil, flux SSE, DELETE de fermeture), et un noeud comme
    `DelegateToOpencode` vit bien plus longtemps qu'un jeton OIDC — le
    jeton expirait donc EN COURS de session et l'api-server repondait
    `ExpiredSignature`. Rafraichir par requete est la bonne granularite :
    elle ne depend d'aucune hypothese sur la duree des appels d'outils.
    """

    def __init__(self, token_provider: OidcTokenProvider) -> None:
        self._token_provider = token_provider

    async def async_auth_flow(self, request):  # type: ignore[no-untyped-def]
        request.headers["Authorization"] = f"Bearer {await self._token_provider.get_token()}"
        yield request


@asynccontextmanager
async def atelier_mcp_session(
    base_url: str, token_provider: OidcTokenProvider
) -> AsyncIterator[ClientSession]:
    """Ouvre une session MCP authentifiee vers `{base_url}/v1/mcp`, dont le
    jeton est renouvele a chaque requete HTTP (voir `_OidcAuth`) : la session
    peut donc vivre plus longtemps qu'un jeton OIDC.

    `httpx2` (pas `httpx`) : le SDK MCP officiel (`mcp`) requiert
    precisement le client HTTP de sa propre dependance `httpx2` pour
    `streamable_http_client` — constate a l'usage (le type attendu par
    cette fonction n'est pas `httpx.AsyncClient`)."""
    async with httpx2.AsyncClient(auth=_OidcAuth(token_provider)) as http_client:
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
