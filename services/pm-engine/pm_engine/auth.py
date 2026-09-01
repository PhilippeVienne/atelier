"""Verification des JWT entrants sur les endpoints HTTP du PM Engine
(Jalon M5, taches 5.5.1/5.5.2) : miroir Python de
`crates/api-server/src/auth.rs` — meme fournisseur OIDC de confiance
(`ATELIER_JWT_ISSUER`/`ATELIER_JWT_JWKS_URL`/`ATELIER_JWT_AUDIENCE`), meme
principe (JWKS RFC 7517 mis en cache, refetch immediat sur `kid` inconnu).

Le Dashboard transmet le JWT de l'utilisateur authentifie (deja verifie une
premiere fois cote `atelier-api-server` pour les appels `/v1/mcp`, voir
`dashboard/lib/session.ts`) dans l'en-tete `Authorization` de ses appels au
PM Engine (`/chat`, `/reviews`) : ce module le revalide independamment —
jamais de confiance aveugle en un en-tete provenant d'un autre service."""

from __future__ import annotations

import logging
import os
from dataclasses import dataclass, field

import httpx
import jwt
from fastapi import Header, HTTPException
from jwt import PyJWKClient

logger = logging.getLogger(__name__)

JWKS_REFRESH_INTERVAL_SECONDS = 10 * 60
"""Meme intervalle que `crates/api-server/src/auth.rs::JWKS_REFRESH_INTERVAL` :
couvre une rotation de cles planifiee cote fournisseur OIDC sans jamais
avoir besoin de redemarrer le process."""


@dataclass
class TrustedIssuer:
    issuer: str
    jwks_url: str
    audience: str

    @classmethod
    def from_env(cls) -> TrustedIssuer | None:
        issuer = os.environ.get("ATELIER_JWT_ISSUER")
        if not issuer:
            return None
        jwks_url = os.environ.get("ATELIER_JWT_JWKS_URL")
        audience = os.environ.get("ATELIER_JWT_AUDIENCE")
        if not jwks_url or not audience:
            raise RuntimeError(
                "ATELIER_JWT_ISSUER est defini mais ATELIER_JWT_JWKS_URL "
                "ou ATELIER_JWT_AUDIENCE est absent"
            )
        return cls(issuer=issuer, jwks_url=jwks_url, audience=audience)


@dataclass
class AuthState:
    """`None` (pas de fournisseur OIDC configure) fait refuser
    systematiquement toute requete authentifiee : evite de demarrer
    silencieusement en mode "tout est autorise", meme comportement que
    `AuthState::Disabled` cote Rust."""

    trusted: TrustedIssuer | None
    _jwk_client: PyJWKClient | None = field(default=None, repr=False)

    @classmethod
    def from_env(cls) -> AuthState:
        trusted = TrustedIssuer.from_env()
        if trusted is None:
            logger.warning(
                "ATELIER_JWT_ISSUER absent : authentification desactivee, "
                "toutes les requetes protegees seront refusees"
            )
            return cls(trusted=None)
        # `PyJWKClient` fait son propre cache + refetch-sur-kid-inconnu en
        # interne (`lifespan`/`cache_keys`), equivalent du couple
        # `Arc<RwLock<JwkSet>>` + refetch immediat de la version Rust.
        jwk_client = PyJWKClient(
            trusted.jwks_url,
            lifespan=JWKS_REFRESH_INTERVAL_SECONDS,
            cache_keys=True,
        )
        logger.info("JWKS configure (issuer=%s, audience=%s)", trusted.issuer, trusted.audience)
        return cls(trusted=trusted, _jwk_client=jwk_client)


@dataclass
class Claims:
    sub: str
    preferred_username: str | None = None
    email: str | None = None
    groups: list[str] | None = None


def _validate_token(token: str, trusted: TrustedIssuer, jwk_client: PyJWKClient) -> Claims:
    signing_key = jwk_client.get_signing_key_from_jwt(token)
    payload = jwt.decode(
        token,
        signing_key.key,
        algorithms=[
            "RS256", "RS384", "RS512",
            "ES256", "ES384",
            "PS256", "PS384", "PS512",
            "EdDSA",
        ],
        issuer=trusted.issuer,
        audience=trusted.audience,
    )
    return Claims(
        sub=payload["sub"],
        preferred_username=payload.get("preferred_username"),
        email=payload.get("email"),
        groups=payload.get("groups"),
    )


def make_require_auth(auth_state: AuthState):
    """Fabrique une dependance FastAPI `require_auth(authorization)` liee a
    un `AuthState` donne — pattern necessaire car `AuthState` n'est connu
    qu'au demarrage de l'app (lecture de l'environnement), contrairement a
    `Depends` qui a besoin d'un callable importable a froid."""

    async def require_auth(authorization: str | None = Header(default=None)) -> Claims:
        if auth_state.trusted is None or auth_state._jwk_client is None:
            raise HTTPException(status_code=503, detail="authentification non configuree")

        if not authorization or not authorization.startswith("Bearer "):
            raise HTTPException(
                status_code=401, detail="en-tete Authorization: Bearer <jwt> requis"
            )
        token = authorization.removeprefix("Bearer ")

        try:
            return _validate_token(token, auth_state.trusted, auth_state._jwk_client)
        except (jwt.PyJWTError, httpx.HTTPError, KeyError) as err:
            logger.warning("JWT refuse: %s", err)
            raise HTTPException(status_code=401, detail="JWT invalide") from err

    return require_auth
