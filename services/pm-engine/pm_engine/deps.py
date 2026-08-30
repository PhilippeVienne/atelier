"""Dependances injectees dans chaque noeud du graphe (Jalon M5, tache
5.2.2), via `config["configurable"]["deps"]` a l'appel de
`graph.ainvoke`/`graph.astream` — jamais construites a l'interieur d'un
noeud (testabilite : chaque noeud peut recevoir de vraies dependances de
test, ex: un `BaseGitProvider` pointant vers un depot Forgejo jetable)."""

from __future__ import annotations

from dataclasses import dataclass, field

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

    workshop_egress_allowlist: list[str] = field(default_factory=list)
    """Allowlist egress des Workshops crees par `ProvisionWorkshop`.

    Bug reel constate en testant le graphe complet de bout en bout pour la
    premiere fois (2026-08-30) : `create_workshop` (MCP) laisse ce champ
    vide par defaut, et `ProvisionWorkshop` ne le renseignait pas — les
    Workshops du PM naissaient donc avec une allowlist VIDE, ce qui rend
    leur build d'image systematiquement impossible (`net-proxy` bloque
    l'acces au registre de l'image de base, aux features, a apt/npm...).
    Le PM etant le createur de ces Workshops, c'est a lui de fournir cette
    liste ; laissee configurable (jamais codee en dur dans le noeud) parce
    qu'elle depend entierement du devcontainer du depot cible.
    Renseignee depuis `PM_ENGINE_WORKSHOP_EGRESS_ALLOWLIST` (voir
    `pm_engine.main`)."""

    claude_code_model: str = "claude-3-5-sonnet-20241022"
    """Modele passe explicitement a Claude Code (`--model`) dans les
    Workshops.

    Ne PAS dependre du modele par defaut du CLI : son nom evolue a chaque
    version (`claude-opus-4-8[1m]` constate le 2026-08-30) et Claude Code
    refuse alors de demarrer derriere LiteLLM ("issue with the selected
    model"), sortant en erreur sans ecrire le moindre fichier — le PM
    ouvrait des PR vides. Epingler le modele releve de toute facon du PM
    (cout, reproductibilite), pas du CLI.

    Renseigne depuis `PM_ENGINE_CLAUDE_CODE_MODEL` (voir `pm_engine.main`)."""

    chat_model: str = "sonnet-premium"
    embedding_model: str = "embedding-dev-local"
    pm_bot_subject: str = ""
    """`sub` du jeton `atelier-pm-bot` (Jalon M5) : renseigne au demarrage
    du service, sert a scoper les ecritures RLS dans `IndexKnowledge` (voir
    `crate::exec` cote atelier pour la meme convention RLS
    `app.current_tenant`, cote Rust)."""
