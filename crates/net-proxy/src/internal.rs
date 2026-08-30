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

#[derive(Debug, Clone, Default)]
pub struct InternalRoutes {
    routes: HashMap<&'static str, (String, u16)>,
}

impl InternalRoutes {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(
            std::env::var("ATELIER_IDENTITY_PROXY_ADDR").ok(),
            std::env::var("ATELIER_MCP_GATEWAY_ADDR").ok(),
            std::env::var("ATELIER_REGISTRY_ALIAS_ADDR").ok(),
            std::env::var("ATELIER_LLM_PROXY_ADDR").ok(),
            std::env::var("ATELIER_GIT_ALIAS_ADDR").ok(),
        )
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
        Ok(Self { routes })
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
}
