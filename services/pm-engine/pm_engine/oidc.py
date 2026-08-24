"""Jeton OIDC du PM Engine (Jalon M5, tache 5.2.2) : le PM pilote Atelier
en tant qu'identite de service propre (`atelier-pm-bot`, client
confidentiel OIDC avec compte de service — voir
`deploy/dev/keycloak/realm-export.json`), pas au nom de l'utilisateur qui a
ouvert le ticket. C'est cette identite qui devient `owner_subject` des
Workshops crees par le PM (`crates/api-server/src/mcp_server.rs::create_workshop`)
— le PM est le veritable steward de ces environnements ephemeres, jamais
l'humain, qui approuve/rejette via un mecanisme separe (tache 5.5.2) plutot
que par la propriete Kubernetes du Workshop.
"""

from __future__ import annotations

import time

import httpx


class OidcTokenProvider:
    """Grant `client_credentials`, jeton mis en cache et rafraichi un peu
    avant expiration (meme marge de securite que
    `crates/api-server/src/session_auth.rs::TOKEN_TTL_MARGIN` cote Rust,
    adaptee au TTL court typique d'un jeton OIDC plutot qu'un jeton
    Kubernetes-auth de 15 minutes)."""

    def __init__(
        self,
        token_url: str,
        client_id: str,
        client_secret: str,
        *,
        expiry_margin_s: float = 30.0,
    ) -> None:
        self._token_url = token_url
        self._client_id = client_id
        self._client_secret = client_secret
        self._expiry_margin_s = expiry_margin_s
        self._cached_token: str | None = None
        self._expires_at: float = 0.0

    async def get_token(self) -> str:
        if self._cached_token is not None and time.monotonic() < self._expires_at:
            return self._cached_token

        async with httpx.AsyncClient(timeout=10.0) as client:
            response = await client.post(
                self._token_url,
                data={
                    "client_id": self._client_id,
                    "client_secret": self._client_secret,
                    "grant_type": "client_credentials",
                },
            )
            response.raise_for_status()
            data = response.json()

        self._cached_token = data["access_token"]
        self._expires_at = time.monotonic() + data["expires_in"] - self._expiry_margin_s
        return self._cached_token
