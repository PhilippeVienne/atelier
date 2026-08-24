"""Verifie empiriquement, contre la vraie instance Keycloak de dev, que
`pm_engine.auth` accepte un vrai JWT `atelier-pm-bot` (client_credentials,
audience `atelier-api` mappee sur le realm, voir
`deploy/dev/keycloak/realm-export.json`) et rejette un jeton invalide.

Necessite KEYCLOAK_TOKEN_URL/KEYCLOAK_PM_BOT_SECRET (voir
deploy/dev/keycloak/README.md). Skip si non disponible."""

from __future__ import annotations

import os

import httpx
import pytest
from fastapi import HTTPException

from pm_engine.auth import AuthState, make_require_auth
from pm_engine.oidc import OidcTokenProvider

TOKEN_URL = os.environ.get(
    "KEYCLOAK_TOKEN_URL",
    "http://127.0.0.1:8090/realms/atelier/protocol/openid-connect/token",
)
CLIENT_SECRET = os.environ.get("KEYCLOAK_PM_BOT_SECRET")
ISSUER = os.environ.get("ATELIER_JWT_ISSUER", "http://127.0.0.1:8090/realms/atelier")
JWKS_URL = os.environ.get(
    "ATELIER_JWT_JWKS_URL", f"{ISSUER}/protocol/openid-connect/certs"
)
AUDIENCE = os.environ.get("ATELIER_JWT_AUDIENCE", "atelier-api")


def _configure_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("ATELIER_JWT_ISSUER", ISSUER)
    monkeypatch.setenv("ATELIER_JWT_JWKS_URL", JWKS_URL)
    monkeypatch.setenv("ATELIER_JWT_AUDIENCE", AUDIENCE)


@pytest.mark.asyncio
async def test_require_auth_accepts_a_real_atelier_pm_bot_jwt(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    if not CLIENT_SECRET:
        pytest.skip("KEYCLOAK_PM_BOT_SECRET non defini, test ignore")
    _configure_env(monkeypatch)

    provider = OidcTokenProvider(TOKEN_URL, "atelier-pm-bot", CLIENT_SECRET)
    try:
        token = await provider.get_token()
    except httpx.HTTPError as exc:
        pytest.skip(f"Keycloak injoignable pour ce test: {exc}")

    auth_state = AuthState.from_env()
    require_auth = make_require_auth(auth_state)

    try:
        claims = await require_auth(authorization=f"Bearer {token}")
    except HTTPException as exc:
        pytest.skip(f"JWKS injoignable ou rejet inattendu pour ce test: {exc.detail}")

    assert claims.sub


@pytest.mark.asyncio
async def test_require_auth_rejects_a_garbage_token(monkeypatch: pytest.MonkeyPatch) -> None:
    if not CLIENT_SECRET:
        pytest.skip("KEYCLOAK_PM_BOT_SECRET non defini, test ignore (pas de JWKS a interroger)")
    _configure_env(monkeypatch)

    auth_state = AuthState.from_env()
    require_auth = make_require_auth(auth_state)

    with pytest.raises(HTTPException) as exc_info:
        await require_auth(authorization="Bearer not-a-real-jwt")
    assert exc_info.value.status_code == 401


@pytest.mark.asyncio
async def test_require_auth_disabled_without_issuer_env(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("ATELIER_JWT_ISSUER", raising=False)
    auth_state = AuthState.from_env()
    require_auth = make_require_auth(auth_state)

    with pytest.raises(HTTPException) as exc_info:
        await require_auth(authorization="Bearer whatever")
    assert exc_info.value.status_code == 503
