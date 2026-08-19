//! Proxy de sortie reseau pour la microVM : n'autorise que les domaines/IP
//! listes dans `Workshop.spec.egress_allowlist`, journalise chaque requete.
//! Fournit aussi le sens inverse : le port-forward de la microVM vers
//! l'exterieur (voir `portforward`).
//!
//! Deux serveurs distincts, deux audiences :
//! - le proxy egress (`ATELIER_NET_PROXY_LISTEN_ADDR`) est configure comme
//!   `HTTP_PROXY`/`HTTPS_PROXY` cote microVM : requetes HTTP en clair
//!   relayees telles quelles, HTTPS tunnele via `CONNECT` sans
//!   dechiffrement. Peut lui-meme chainer vers un proxy parent impose par
//!   le reseau environnant (`ATELIER_UPSTREAM_PROXY`), sauf pour les
//!   destinations listees dans `ATELIER_NO_PROXY`.
//! - le serveur de controle (`ATELIER_NET_PROXY_CONTROL_ADDR`) expose
//!   `/portforward`, destine uniquement a `api-server` (jamais a un client
//!   final direct — voir `crates/net-proxy/src/portforward.rs`).
//!
//! net-proxy sert aussi de resolveur DNS pour la microVM
//! (`ATELIER_DNS_LISTEN_ADDR`, UDP+TCP) : meme allowlist que le proxy
//! egress, une requete pour un nom hors allowlist recoit `REFUSED` sans
//! jamais atteindre l'upstream (voir `crates/net-proxy/src/dns.rs`).
//!
//! Deux alias internes toujours joignables en HTTP(S) via ce meme proxy
//! egress, hors allowlist : `identity-proxy` (`ATELIER_IDENTITY_PROXY_ADDR`)
//! et `mcp-gateway` (`ATELIER_MCP_GATEWAY_ADDR`) — voir
//! `crates/net-proxy/src/internal.rs`.
//!
//! Decision de design : net-proxy est le **seul** point d'entree reseau
//! joignable par la VM (voir le pare-feu TAP dans `docs/ARCHITECTURE.md`,
//! section "Isolation reseau de la microVM") — la VM ne configure jamais
//! `identity-proxy` directement comme `HTTP_PROXY`. Consequence : si
//! `ATELIER_IDENTITY_PROXY_ADDR` est configure, net-proxy y chaine
//! *tout* l'egress autorise (pas seulement l'alias `identity-proxy`
//! adresse explicitement par nom) — c'est identity-proxy, en aval, qui
//! decide au cas par cas d'injecter un credential ou de relayer tel quel.
//! Ce chainage remplace alors le proxy parent externe
//! (`ATELIER_UPSTREAM_PROXY`) pour la duree du saut.

mod allowlist;
mod dns;
mod forward;
mod http;
mod internal;
mod portforward;
mod proxy;
mod upstream;

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;

use internal::InternalRoutes;
use proxy::EgressConfig;
use upstream::UpstreamProxy;

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3128";
const DEFAULT_CONTROL_ADDR: &str = "0.0.0.0:9000";
const DEFAULT_DNS_LISTEN_ADDR: &str = "0.0.0.0:53";
const DEFAULT_VM_ADDR: &str = "127.0.0.1";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-net-proxy");

    let listen_addr = std::env::var("ATELIER_NET_PROXY_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string());
    let control_addr = std::env::var("ATELIER_NET_PROXY_CONTROL_ADDR")
        .unwrap_or_else(|_| DEFAULT_CONTROL_ADDR.to_string());
    let vm_addr: Arc<str> =
        std::env::var("ATELIER_VM_ADDR").unwrap_or_else(|_| DEFAULT_VM_ADDR.to_string()).into();

    let allowlist: Arc<Vec<String>> = Arc::new(parse_csv_env("ATELIER_EGRESS_ALLOWLIST"));
    let no_proxy: Arc<Vec<String>> = Arc::new(upstream::no_proxy_from_env());
    let upstream_proxy = UpstreamProxy::from_env().map(Arc::new);
    let internal_routes = Arc::new(InternalRoutes::from_env()?);
    // Meme variable que l'alias interne "identity-proxy" (`internal_routes`) :
    // ici pour le chainage obligatoire de tout l'egress autorise, pas pour
    // l'adressage explicite par nom — voir `EgressConfig::identity_proxy`.
    let identity_proxy = std::env::var("ATELIER_IDENTITY_PROXY_ADDR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(|addr| {
            Arc::new(UpstreamProxy {
                addr,
                auth_header: None,
            })
        });

    if allowlist.is_empty() {
        tracing::warn!(
            "ATELIER_EGRESS_ALLOWLIST absente ou vide : tout le trafic sortant sera refuse"
        );
    } else {
        tracing::info!(allowlist = ?allowlist, "atelier-net-proxy demarre");
    }
    if let Some(proxy) = &upstream_proxy {
        tracing::info!(
            upstream_addr = %proxy.addr,
            auth = proxy.auth_header.is_some(),
            no_proxy = ?no_proxy,
            "proxy parent configure"
        );
    }
    tracing::info!(
        identity_proxy_alias = internal_routes.resolve("identity-proxy").is_some(),
        mcp_gateway_alias = internal_routes.resolve("mcp-gateway").is_some(),
        identity_proxy_mandatory_hop = identity_proxy.is_some(),
        "routes internes (identity-proxy/mcp-gateway) configurees"
    );

    let egress_config = EgressConfig {
        allowlist,
        upstream: upstream_proxy,
        no_proxy,
        internal: internal_routes,
        identity_proxy,
    };

    let control_router = portforward::router(portforward::PortForwardState {
        vm_addr: Arc::clone(&vm_addr),
    });
    let control_listener = TcpListener::bind(&control_addr).await?;
    tracing::info!(%control_addr, "serveur de controle (port-forward) en ecoute");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(control_listener, control_router).await {
            tracing::error!(%err, "serveur de controle arrete en erreur");
        }
    });

    let dns_listen_addr = std::env::var("ATELIER_DNS_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_DNS_LISTEN_ADDR.to_string());
    let dns_upstream =
        std::env::var("ATELIER_DNS_UPSTREAM").unwrap_or_else(|_| dns::default_upstream());
    let dns_config = dns::DnsConfig {
        listen_addr: dns_listen_addr,
        upstream: dns_upstream,
        allowlist: Arc::clone(&egress_config.allowlist),
    };
    tokio::spawn(async move {
        if let Err(err) = dns::run(dns_config).await {
            tracing::error!(%err, "proxy DNS arrete en erreur");
        }
    });

    let egress_listener = TcpListener::bind(&listen_addr).await?;
    tracing::info!(%listen_addr, "proxy egress en ecoute");

    loop {
        let (socket, peer): (_, SocketAddr) = egress_listener.accept().await?;
        let config = egress_config.clone();
        tokio::spawn(async move {
            if let Err(err) = proxy::handle_connection(socket, peer, config).await {
                tracing::warn!(%peer, %err, "connexion terminee en erreur");
            }
        });
    }
}

fn parse_csv_env(name: &str) -> Vec<String> {
    std::env::var(name)
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}
