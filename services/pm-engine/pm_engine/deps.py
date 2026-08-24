"""Dependances injectees dans chaque noeud du graphe (Jalon M5, tache
5.2.2), via `config["configurable"]["deps"]` a l'appel de
`graph.ainvoke`/`graph.astream` — jamais construites a l'interieur d'un
noeud (testabilite : chaque noeud peut recevoir de vraies dependances de
test, ex: un `BaseGitProvider` pointant vers un depot Forgejo jetable)."""

from __future__ import annotations

from dataclasses import dataclass

from .git_providers import BaseGitProvider
from .llm_client import LlmClient
from .oidc import OidcTokenProvider


@dataclass
class PmEngineDeps:
    git_provider: BaseGitProvider
    llm_client: LlmClient
    atelier_api_url: str
    mcp_token_provider: OidcTokenProvider
    db_pool: object  # asyncpg.Pool — type precis evite en tete de module
    # pour ne pas imposer `asyncpg` a tout importeur de ce fichier
    # (`pm_engine.graph` l'importe seul, voir ce module).

    chat_model: str = "sonnet-premium"
    embedding_model: str = "embedding-dev-local"
    pm_bot_subject: str = ""
    """`sub` du jeton `atelier-pm-bot` (Jalon M5) : renseigne au demarrage
    du service, sert a scoper les ecritures RLS dans `IndexKnowledge` (voir
    `crate::exec` cote atelier pour la meme convention RLS
    `app.current_tenant`, cote Rust)."""
