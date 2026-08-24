"""Verifie empiriquement, contre un vrai `atelier-api-server` (Jalon M4)
authentifie par la vraie instance Keycloak de dev, que
`pm_engine.mcp_client` pilote reellement le cycle de vie d'un Workshop via
le serveur MCP externe.

Necessite ATELIER_API_URL (un vrai `atelier-api-server` en cours
d'execution, avec ATELIER_JWT_ISSUER/JWKS_URL/AUDIENCE pointant vers la
meme instance Keycloak — voir docs/PROGRESS.md pour la commande de
lancement utilisee lors de la validation de ce module) et
KEYCLOAK_PM_BOT_SECRET. Skip si non disponible.
"""

from __future__ import annotations

import os
import uuid

import httpx
import pytest

from pm_engine.mcp_client import atelier_mcp_session, call_tool_json, call_tool_text
from pm_engine.oidc import OidcTokenProvider

ATELIER_API_URL = os.environ.get("ATELIER_API_URL", "http://127.0.0.1:8080")
KEYCLOAK_TOKEN_URL = os.environ.get(
    "KEYCLOAK_TOKEN_URL",
    "http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token",
)
KEYCLOAK_PM_BOT_SECRET = os.environ.get("KEYCLOAK_PM_BOT_SECRET")


@pytest.mark.asyncio
async def test_mcp_session_drives_a_real_workshop_lifecycle() -> None:
    if not KEYCLOAK_PM_BOT_SECRET:
        pytest.skip("KEYCLOAK_PM_BOT_SECRET non defini, test ignore")

    token_provider = OidcTokenProvider(KEYCLOAK_TOKEN_URL, "atelier-pm-bot", KEYCLOAK_PM_BOT_SECRET)
    name = f"test-pm-mcp-{uuid.uuid4().hex[:8]}"

    try:
        async with atelier_mcp_session(ATELIER_API_URL, token_provider) as session:
            tools = await session.list_tools()
            tool_names = {t.name for t in tools.tools}
            assert {
                "create_workshop",
                "list_workshops",
                "get_workshop_status",
                "suspend_workshop",
                "resume_workshop",
                "delete_workshop",
                "exec_in_workshop",
            } <= tool_names

            created = await call_tool_json(
                session,
                "create_workshop",
                {
                    "name": name,
                    "devcontainerRepo": "https://example.invalid/repo.git",
                    "cpu": "1",
                    "memory": "1Gi",
                },
            )
            # `ownerSubject` = la claim `sub` du jeton (un UUID Keycloak
            # pour un compte de service, pas son nom lisible) — voir
            # `crates/api-server/src/auth.rs::AuthenticatedUser`.
            assert created["spec"]["ownerSubject"]

            listed = await call_tool_json(session, "list_workshops", {})
            assert any(w["metadata"]["name"] == name for w in listed)

            await call_tool_text(session, "delete_workshop", {"name": name})
    except httpx.HTTPError as exc:
        pytest.skip(f"atelier-api-server/Keycloak injoignable pour ce test: {exc}")
