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

    workshop_owner_group: str = ""
    """Groupe proprietaire des Workshops crees par le PM.

    Obligatoire des lors que le compte de service appartient a plusieurs
    groupes : l'api-server refuse alors de choisir a sa place (`400`), pour
    ne pas placer un environnement — et sa depense — dans un groupe au
    hasard. Renseigne depuis `PM_ENGINE_WORKSHOP_OWNER_GROUP`.
    Voir `docs/specs/07-groupes.md`."""

    devcontainer_repo_template: str = ""
    """Gabarit d'URL de clone du depot, telle qu'un GUEST doit la voir.

    `{repo}` y est remplace par l'identifiant `owner/nom` du depot. Existe
    parce que cette URL n'a rien a voir avec celle de la forge vue depuis
    l'hote : les microVM n'ont ni le `/etc/hosts` ni le DNS de l'hote, et ne
    sortent que sur les ports 80 et 443. C'est donc un parametre de
    deploiement, pas quelque chose que le Dashboard puisse deviner — et le
    renseigner ici evite surtout de faire transiter des identifiants par
    l'interface.

    Exemple (dev) :
    `http://atelier-forgejo-dev.default.svc.cluster.local/{repo}.git`.
    Renseigne depuis `PM_ENGINE_DEVCONTAINER_REPO_TEMPLATE`."""

    opencode_model: str = "atelier/atelier-workshop-agent"
    """Modele passe explicitement a `opencode` (`--model provider/model`)
    dans les Workshops.

    Choisir le modele releve du PM (cout, reproductibilite d'un run a
    l'autre) plutot que du defaut d'un CLI. Le provider `atelier` est defini
    dans le `opencode.json` injecte par `image-builder`
    (`inject_net_proxy_config`, `crates/image-builder`) : un fournisseur
    `@ai-sdk/openai-compatible` pointant vers `llm-proxy` — voir
    `deploy/dev/llm-proxy/config.yaml` pour l'alias LiteLLM
    `atelier-workshop-agent` correspondant.

    Ancien champ `claude_code_model` (Claude Code, retire le 2026-09-01
    apres un segfault reproductible du binaire Bun `claude.exe`, sans
    rapport avec l'infrastructure d'atelier — voir
    `docs/architecture/pieges.md`). `opencode` etant open-source (licence
    MIT), l'invocation ne depend plus d'un CLI proprietaire pour executer le
    code confie a chaque sous-tache.

    Renseigne depuis `PM_ENGINE_OPENCODE_MODEL` (voir `pm_engine.main`)."""

    chat_model: str = "sonnet-premium"
    embedding_model: str = "embedding-dev-local"
    pm_bot_subject: str = ""
    """`sub` du jeton `atelier-pm-bot` (Jalon M5) : renseigne au demarrage
    du service, sert a scoper les ecritures RLS dans `IndexKnowledge` (voir
    `crate::exec` cote atelier pour la meme convention RLS
    `app.current_tenant`, cote Rust)."""
