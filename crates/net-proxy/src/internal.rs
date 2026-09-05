//! Alias internes joignables via net-proxy sans passer par l'allowlist
//! egress : `identity-proxy`, `mcp-gateway` et `registry`, les composants de
//! confiance auxquels une VM doit pouvoir parler en HTTP (CONNECT ou
//! requete en clair), au meme titre qu'un nom de domaine ordinaire, mais
//! sans que le proprietaire du `Workshop` ait besoin de les lister dans
//! `Workshop.spec.egress_allowlist` — ce ne sont pas de l'egress vers
//! l'exterieur, ce sont des composants de confiance du pod lui-meme.
//! `registry` sert a la microVM "builder" (`crates/image-builder`) pour
//! joindre le registre interne (ou `envbuilder` pousse l'image construite)
//! sans que l'utilisateur ait besoin d'ajouter ce detail d'implementation a
//! sa propre allowlist, pensee pour l'usage runtime de l'agent — voir
//! `docs/PROGRESS.md`, section "Builder microVM".
//!
//! Route resolue *avant* l'allowlist et *sans* chainage vers un eventuel
//! proxy parent (`ATELIER_UPSTREAM_PROXY`) : ces destinations restent
//! toujours joignables quelle que soit la politique egress ou reseau
//! environnante.
//!
//! `git.atelier.internal` (`atelier_common::GIT_ALIAS_HOST`) suit le meme
//! principe pour la forge Git ciblee par l'agent (Jalon M2, section 5.2) :
//! voir le commentaire dedie plus bas ainsi que
//! `crates/controller/src/git_identity.rs`.

use std::collections::HashMap;

const IDENTITY_PROXY_ALIAS: &str = "identity-proxy";
const MCP_GATEWAY_ALIAS: &str = "mcp-gateway";
const REGISTRY_ALIAS: &str = "registry";
/// Service global du cluster (meme niveau qu'OpenBao, pas un sidecar par
/// pod — voir `deploy/dev/llm-proxy/`) : un seul LiteLLM partage par tous
/// les Workshops, traduit les appels Anthropic Messages API de Claude Code
/// vers le fournisseur bon marche configure (DeepSeek par defaut). Alias
/// fixe comme les trois precedents, toujours actif des que configure —
/// contrairement a l'alias `simulator` (`crate::proxy::EgressConfig`), qui
/// lui est mutable et gate par un appel MCP explicite.
const LLM_PROXY_ALIAS: &str = "llm-proxy";
/// Alias de la forge Git ciblee par l'agent (voir
/// `crates/controller/src/git_identity.rs` et `atelier_common::GIT_ALIAS_HOST`,
/// meme constante partagee pour eviter toute divergence entre le controller
/// et net-proxy) : resout, comme les quatre alias precedents, directement
/// vers `identity-proxy` (pas vers la forge elle-meme) — c'est identity-proxy
/// qui, une fois la requete recue, injecte le PAT puis se connecte a la
/// vraie destination. Cette derniere connexion resout `git.atelier.internal`
/// via `/etc/hosts` du pod (`hostAliases`, poses par le controller), jamais
/// par ce module : net-proxy n'a besoin d'aucune resolution DNS ici, un
/// simple aiguillage vers l'adresse locale d'identity-proxy.
///
/// Sans cet alias, `git.atelier.internal` suivrait le chemin normal de
/// l'egress (allowlist puis, si autorise, chainage obligatoire vers
/// identity-proxy — voir le commentaire de tete de `main.rs`) : l'utilisateur
/// devrait alors explicitement ajouter la forge Git a
/// `Workshop.spec.egress_allowlist`, alors que ce n'est pas de l'egress vers
/// l'exterieur au sens du modele de securite du Workshop (comme `registry`
/// pour l'image-builder), mais un composant de confiance que l'agent doit
/// toujours pouvoir joindre pour cloner/pousser sur ses propres depots.
const GIT_ALIAS: &str = atelier_common::GIT_ALIAS_HOST;

/// Suffixe des alias de simulateurs sidecars declares dans
/// `Workshop.spec.simulators` (spec `docs/specs/14-devex-cli-simulateurs-hitl.md`
/// §4.3, tache 9.3) : `<name>.atelier.internal`, meme convention de nommage
/// que [`GIT_ALIAS`] — mais un ensemble DYNAMIQUE (un nom par Workshop, pas
/// fixe a la compilation), d'ou une table separee de [`InternalRoutes::routes`]
/// (cles `&'static str`).
const SIMULATOR_ALIAS_SUFFIX: &str = ".atelier.internal";

#[derive(Debug, Clone, Default)]
pub struct InternalRoutes {
    routes: HashMap<&'static str, (String, u16)>,
    /// Alias de simulateurs sidecars, cle = alias complet en minuscules
    /// (ex: `postgres.atelier.internal`) — voir [`SIMULATOR_ALIAS_SUFFIX`].
    simulators: HashMap<String, (String, u16)>,
    /// Cibles inter-Workshops explicitement autorisees (spec docs/specs/16-
    /// escouades-multi-agents-swarms-mesh.md §3.2, tache 12.1) — cle = alias
    /// complet en minuscules (ex: `api.ws-backend.atelier.internal`), meme
    /// mecanisme que [`Self::simulators`] mais table SEPAREE : une regle
    /// d'un Workshop client ne doit jamais accidentellement matcher un
    /// simulateur portant le meme nom qu'un service exporte d'un AUTRE
    /// Workshop (deux origines de configuration completement distinctes —
    /// `Workshop.spec.simulators` du Workshop lui-meme contre
    /// `Workshop.spec.allowed_internal_targets`, resolu par le controller).
    /// "Zero Wildcard" par construction : une recherche de cle exacte dans
    /// cette table, jamais un suffixe/motif.
    squad: HashMap<String, (String, u16)>,
}

impl InternalRoutes {
    pub fn from_env() -> anyhow::Result<Self> {
        let mut routes = Self::parse(
            std::env::var("ATELIER_IDENTITY_PROXY_ADDR").ok(),
            std::env::var("ATELIER_MCP_GATEWAY_ADDR").ok(),
            std::env::var("ATELIER_REGISTRY_ALIAS_ADDR").ok(),
            std::env::var("ATELIER_LLM_PROXY_ADDR").ok(),
            std::env::var("ATELIER_GIT_ALIAS_ADDR").ok(),
        )?;
        if let Ok(raw) = std::env::var("ATELIER_SIMULATORS") {
            routes.add_simulators(&raw)?;
        }
        if let Ok(raw) = std::env::var("ATELIER_ALLOWED_INTERNAL_TARGETS") {
            routes.add_squad_targets(&raw)?;
        }
        Ok(routes)
    }

    /// Ajoute les cibles inter-Workshops decrites par `raw` : liste separee
    /// par des virgules d'entrees `alias=host:port` (posee par le
    /// `controller` a partir de `Workshop.spec.allowed_internal_targets`
    /// UNE FOIS resolues en adresses reelles — le ClusterIP du Service
    /// `<workshop-cible>-<service>`, jamais l'alias `*.atelier.internal`
    /// lui-meme, qui n'existe que du point de vue de net-proxy). Meme
    /// format textuel que [`Self::add_simulators`], table separee (voir la
    /// doc du champ [`Self::squad`]).
    pub fn add_squad_targets(&mut self, raw: &str) -> anyhow::Result<()> {
        for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (alias, addr) = entry.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("cible inter-Workshop invalide {entry:?}, attendu alias=host:port")
            })?;
            self.squad
                .insert(alias.to_ascii_lowercase(), parse_addr(addr)?);
        }
        Ok(())
    }

    /// Ajoute les alias de simulateurs decrits par `raw` : liste separee par
    /// des virgules d'entrees `nom=host:port` (posee par le `controller` a
    /// partir de `Workshop.spec.simulators`, un sidecar du meme pod donc
    /// toujours `127.0.0.1:<port>` en pratique — voir
    /// `crates/controller/src/reconcile.rs`).
    pub fn add_simulators(&mut self, raw: &str) -> anyhow::Result<()> {
        for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            let (name, addr) = entry.split_once('=').ok_or_else(|| {
                anyhow::anyhow!("entree de simulateur invalide {entry:?}, attendu nom=host:port")
            })?;
            let alias = format!("{}{SIMULATOR_ALIAS_SUFFIX}", name.to_ascii_lowercase());
            self.simulators.insert(alias, parse_addr(addr)?);
        }
        Ok(())
    }

    fn parse(
        identity_proxy_addr: Option<String>,
        mcp_gateway_addr: Option<String>,
        registry_addr: Option<String>,
        llm_proxy_addr: Option<String>,
        git_alias_addr: Option<String>,
    ) -> anyhow::Result<Self> {
        let mut routes = HashMap::new();
        if let Some(addr) = identity_proxy_addr.filter(|s| !s.trim().is_empty()) {
            routes.insert(IDENTITY_PROXY_ALIAS, parse_addr(&addr)?);
        }
        if let Some(addr) = mcp_gateway_addr.filter(|s| !s.trim().is_empty()) {
            routes.insert(MCP_GATEWAY_ALIAS, parse_addr(&addr)?);
        }
        if let Some(addr) = registry_addr.filter(|s| !s.trim().is_empty()) {
            routes.insert(REGISTRY_ALIAS, parse_addr(&addr)?);
        }
        if let Some(addr) = llm_proxy_addr.filter(|s| !s.trim().is_empty()) {
            routes.insert(LLM_PROXY_ALIAS, parse_addr(&addr)?);
        }
        if let Some(addr) = git_alias_addr.filter(|s| !s.trim().is_empty()) {
            routes.insert(GIT_ALIAS, parse_addr(&addr)?);
        }
        Ok(Self {
            routes,
            simulators: HashMap::new(),
            squad: HashMap::new(),
        })
    }

    /// Enregistre un alias a la main — reserve aux tests, qui n'ont pas a
    /// passer par des variables d'environnement pour verifier le routage.
    #[cfg(test)]
    pub fn insert_for_test(&mut self, alias: &'static str, addr: (String, u16)) {
        self.routes.insert(alias, addr);
    }

    /// Adresse reelle vers laquelle relayer si `host` est un alias interne
    /// configure — comparaison insensible a la casse, pas de wildcard (ce
    /// sont des noms fixes, pas des domaines).
    pub fn resolve(&self, host: &str) -> Option<(String, u16)> {
        let host = host.to_ascii_lowercase();
        self.routes
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(&host))
            .map(|(_, addr)| addr.clone())
            .or_else(|| self.simulators.get(&host).cloned())
            .or_else(|| self.squad.get(&host).cloned())
    }
}

fn parse_addr(addr: &str) -> anyhow::Result<(String, u16)> {
    let (host, port) = addr
        .rsplit_once(':')
        .ok_or_else(|| anyhow::anyhow!("adresse invalide {addr:?}, attendu host:port"))?;
    let port: u16 = port
        .parse()
        .map_err(|_| anyhow::anyhow!("port invalide dans {addr:?}"))?;
    Ok((host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_configured_aliases() {
        let routes = InternalRoutes::parse(
            Some("127.0.0.1:3129".to_string()),
            Some("127.0.0.1:3130".to_string()),
            Some("127.0.0.1:3131".to_string()),
            Some("127.0.0.1:3132".to_string()),
            Some("127.0.0.1:3129".to_string()),
        )
        .unwrap();
        assert_eq!(
            routes.resolve("identity-proxy"),
            Some(("127.0.0.1".to_string(), 3129))
        );
        assert_eq!(
            routes.resolve("mcp-gateway"),
            Some(("127.0.0.1".to_string(), 3130))
        );
        assert_eq!(
            routes.resolve("registry"),
            Some(("127.0.0.1".to_string(), 3131))
        );
        assert_eq!(
            routes.resolve("llm-proxy"),
            Some(("127.0.0.1".to_string(), 3132))
        );
        assert_eq!(
            routes.resolve("git.atelier.internal"),
            Some(("127.0.0.1".to_string(), 3129))
        );
    }

    #[test]
    fn case_insensitive() {
        let routes =
            InternalRoutes::parse(Some("127.0.0.1:3129".to_string()), None, None, None, None)
                .unwrap();
        assert!(routes.resolve("Identity-Proxy").is_some());
    }

    #[test]
    fn unrelated_host_not_resolved() {
        let routes =
            InternalRoutes::parse(Some("127.0.0.1:3129".to_string()), None, None, None, None)
                .unwrap();
        assert_eq!(routes.resolve("github.com"), None);
    }

    #[test]
    fn unset_env_disables_the_route() {
        let routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        assert_eq!(routes.resolve("identity-proxy"), None);
        assert_eq!(routes.resolve("mcp-gateway"), None);
        assert_eq!(routes.resolve("registry"), None);
        assert_eq!(routes.resolve("llm-proxy"), None);
        assert_eq!(routes.resolve("git.atelier.internal"), None);
    }

    #[test]
    fn rejects_malformed_address() {
        assert!(
            InternalRoutes::parse(Some("no-port-here".to_string()), None, None, None, None)
                .is_err()
        );
    }

    /// Distingue cet alias de `llm-proxy` (fixe, toujours actif des que
    /// configure) de l'alias mutable `simulator` (`crate::proxy`), gate par
    /// un appel MCP explicite : `llm-proxy` ne depend d'aucun etat runtime.
    #[test]
    fn llm_proxy_alias_is_independent_of_others() {
        let routes =
            InternalRoutes::parse(None, None, None, Some("10.0.0.5:4000".to_string()), None)
                .unwrap();
        assert_eq!(
            routes.resolve("llm-proxy"),
            Some(("10.0.0.5".to_string(), 4000))
        );
        assert_eq!(routes.resolve("identity-proxy"), None);
    }

    /// L'alias Git bypasse l'allowlist au meme titre que les autres (voir
    /// commentaire de tete de ce module) : sans lui, `git.atelier.internal`
    /// suivrait le chemin normal de l'egress (allowlist puis, si autorise,
    /// chainage `identity-proxy`), ce qui obligerait l'utilisateur a l'ajouter
    /// explicitement a `Workshop.spec.egress_allowlist`.
    #[test]
    fn git_alias_resolves_independently_and_is_case_insensitive() {
        let routes =
            InternalRoutes::parse(None, None, None, None, Some("127.0.0.1:3129".to_string()))
                .unwrap();
        assert_eq!(
            routes.resolve("git.atelier.internal"),
            Some(("127.0.0.1".to_string(), 3129))
        );
        assert_eq!(
            routes.resolve("GIT.ATELIER.INTERNAL"),
            Some(("127.0.0.1".to_string(), 3129))
        );
        assert_eq!(routes.resolve("llm-proxy"), None);
    }

    /// Alias de simulateurs sidecars (tache 9.3) : declaratifs, un par
    /// `Workshop.spec.simulators`, poses par le controller via
    /// `ATELIER_SIMULATORS` — independants des alias fixes ci-dessus.
    #[test]
    fn simulator_aliases_resolve_by_name_and_are_case_insensitive() {
        let mut routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        routes
            .add_simulators("postgres=127.0.0.1:5432,localstack=127.0.0.1:4566")
            .unwrap();
        assert_eq!(
            routes.resolve("postgres.atelier.internal"),
            Some(("127.0.0.1".to_string(), 5432))
        );
        assert_eq!(
            routes.resolve("LOCALSTACK.ATELIER.INTERNAL"),
            Some(("127.0.0.1".to_string(), 4566))
        );
        assert_eq!(routes.resolve("wiremock.atelier.internal"), None);
    }

    #[test]
    fn malformed_simulator_entry_is_rejected() {
        let mut routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        assert!(routes.add_simulators("postgres-no-equals-sign").is_err());
    }

    /// Cibles inter-Workshops (tache 12.1) : meme mecanisme textuel que les
    /// simulateurs, mais table separee pour eviter qu'un simulateur et un
    /// service exporte d'un autre Workshop portant le meme nom ne se
    /// confondent.
    #[test]
    fn squad_targets_resolve_by_alias_and_are_case_insensitive() {
        let mut routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        routes
            .add_squad_targets("api.ws-backend.atelier.internal=10.96.1.5:8080")
            .unwrap();
        assert_eq!(
            routes.resolve("api.ws-backend.atelier.internal"),
            Some(("10.96.1.5".to_string(), 8080))
        );
        assert_eq!(
            routes.resolve("API.WS-BACKEND.ATELIER.INTERNAL"),
            Some(("10.96.1.5".to_string(), 8080))
        );
    }

    /// "Zero Wildcard" (spec 16 §3.2) : une cible NON explicitement listee
    /// reste inaccessible, meme si elle ressemble a un alias inter-Workshop
    /// valide.
    #[test]
    fn squad_target_not_listed_is_not_resolved() {
        let mut routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        routes
            .add_squad_targets("api.ws-backend.atelier.internal=10.96.1.5:8080")
            .unwrap();
        assert_eq!(routes.resolve("other.ws-backend.atelier.internal"), None);
        assert_eq!(routes.resolve("api.ws-other.atelier.internal"), None);
    }

    #[test]
    fn malformed_squad_entry_is_rejected() {
        let mut routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        assert!(routes
            .add_squad_targets("api.ws-backend.atelier.internal-no-equals-sign")
            .is_err());
    }

    /// Table separee de `simulators` : un simulateur nomme "api" dans CE
    /// Workshop et un service exporte nomme "api" dans un AUTRE Workshop
    /// (meme nom, sources de configuration totalement distinctes) ne
    /// doivent jamais s'ecraser l'un l'autre.
    #[test]
    fn simulator_and_squad_target_with_the_same_alias_coexist_independently() {
        let mut routes = InternalRoutes::parse(None, None, None, None, None).unwrap();
        routes.add_simulators("api=127.0.0.1:9999").unwrap();
        routes
            .add_squad_targets("api.ws-backend.atelier.internal=10.96.1.5:8080")
            .unwrap();
        assert_eq!(
            routes.resolve("api.atelier.internal"),
            Some(("127.0.0.1".to_string(), 9999))
        );
        assert_eq!(
            routes.resolve("api.ws-backend.atelier.internal"),
            Some(("10.96.1.5".to_string(), 8080))
        );
    }
}
