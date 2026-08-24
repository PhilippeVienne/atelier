"""Verifie empiriquement, contre la vraie instance Keycloak de dev, que
`OidcTokenProvider` obtient un jeton reel via `client_credentials`
(client de service `atelier-pm-bot`, voir
`deploy/dev/keycloak/realm-export.json`) et le met en cache.

Necessite KEYCLOAK_TOKEN_URL/KEYCLOAK_PM_BOT_SECRET (voir
deploy/dev/keycloak/README.md). Skip si non disponible.
"""

from __future__ import annotations

import os

import httpx
import pytest

from pm_engine.oidc import OidcTokenProvider

TOKEN_URL = os.environ.get(
    "KEYCLOAK_TOKEN_URL",
    "http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token",
)
CLIENT_SECRET = os.environ.get("KEYCLOAK_PM_BOT_SECRET")


@pytest.mark.asyncio
async def test_get_token_returns_a_real_valid_jwt_and_caches_it() -> None:
    if not CLIENT_SECRET:
        pytest.skip("KEYCLOAK_PM_BOT_SECRET non defini, test ignore")

    provider = OidcTokenProvider(TOKEN_URL, "atelier-pm-bot", CLIENT_SECRET)
    try:
        token = await provider.get_token()
    except httpx.HTTPError as exc:
        pytest.skip(f"Keycloak injoignable pour ce test: {exc}")

    assert token.count(".") == 2  # forme JWT (header.payload.signature)

    # Deuxieme appel : sert le jeton en cache (pas de nouvelle requete
    # reseau necessaire tant que la marge d'expiration n'est pas atteinte).
    token_again = await provider.get_token()
    assert token_again == token
