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

mod admin;
mod allowlist;
mod dns;
mod forward;
mod http;
mod internal;
mod metadata;
mod portforward;
mod proxy;
mod session_auth;
mod tls_sni;
mod upstream;

use std::net::SocketAddr;
use std::sync::Arc;

use atelier_common::OpenBaoClient;
use tokio::net::TcpListener;
use tokio::sync::RwLock;

use internal::InternalRoutes;
use proxy::EgressConfig;
use upstream::UpstreamProxy;

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3128";
const DEFAULT_CONTROL_ADDR: &str = "0.0.0.0:9000";
const DEFAULT_DNS_LISTEN_ADDR: &str = "0.0.0.0:53";
const DEFAULT_VM_ADDR: &str = "127.0.0.1";
/// Lie a `127.0.0.1` uniquement (voir `crate::admin`) : jamais `0.0.0.0`.
const DEFAULT_ADMIN_ADDR: &str = "127.0.0.1:9001";
/// Joignable par la VM comme le reste des ports cote guest (voir
/// `crate::metadata`) : jamais `127.0.0.1`.
const DEFAULT_METADATA_ADDR: &str = "0.0.0.0:3132";
/// Cibles des regles `iptables -t nat ... REDIRECT` posees par
/// `crates/firecracker::network::NetworkSetup::enable_transparent_gateway` :
/// la VM n'a jamais besoin de connaitre ces ports (ni meme l'existence de
/// net-proxy), le trafic y arrive deja reecrit par le noyau avant meme
/// d'atteindre l'application. Voir `docs/architecture/network-security.md`.
const DEFAULT_TRANSPARENT_HTTP_ADDR: &str = "0.0.0.0:3180";
const DEFAULT_TRANSPARENT_TLS_ADDR: &str = "0.0.0.0:3181";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-net-proxy");

    let listen_addr = std::env::var("ATELIER_NET_PROXY_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string());
    let control_addr = std::env::var("ATELIER_NET_PROXY_CONTROL_ADDR")
        .unwrap_or_else(|_| DEFAULT_CONTROL_ADDR.to_string());
    let vm_addr: Arc<str> = std::env::var("ATELIER_VM_ADDR")
        .unwrap_or_else(|_| DEFAULT_VM_ADDR.to_string())
        .into();

    let admin_addr = std::env::var("ATELIER_NET_PROXY_ADMIN_ADDR")
        .unwrap_or_else(|_| DEFAULT_ADMIN_ADDR.to_string());

    let allowlist: Arc<RwLock<Vec<String>>> =
        Arc::new(RwLock::new(parse_csv_env("ATELIER_EGRESS_ALLOWLIST")));
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

    {
        let initial = allowlist.read().await;
        if initial.is_empty() {
            tracing::warn!(
                "ATELIER_EGRESS_ALLOWLIST absente ou vide : tout le trafic sortant sera refuse"
            );
        } else {
            tracing::info!(allowlist = ?*initial, "atelier-net-proxy demarre");
        }
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
        registry_alias = internal_routes.resolve("registry").is_some(),
        llm_proxy_alias = internal_routes.resolve("llm-proxy").is_some(),
        git_alias = internal_routes
            .resolve(atelier_common::GIT_ALIAS_HOST)
            .is_some(),
        identity_proxy_mandatory_hop = identity_proxy.is_some(),
        "routes internes (identity-proxy/mcp-gateway/registry/llm-proxy/git) configurees"
    );

    // Sidecar `simulator` du pod (LocalStack), voir `crates/mcp-gateway`
    // (tool `enable_simulator`) et `crates/controller/src/reconcile.rs` :
    // present seulement si `Workshop.spec.tools` le demande. Contrairement
    // aux alias de `internal_routes` (toujours actifs), reste `None` (donc
    // non joignable, l'allowlist normale le rejette) tant que l'agent n'a
    // pas explicitement appele `enable_simulator`.
    let simulator_target = std::env::var("ATELIER_SIMULATOR_ADDR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|addr| {
            let (host, port) = addr.rsplit_once(':')?;
            Some((host.to_string(), port.parse::<u16>().ok()?))
        });
    let simulator: Arc<RwLock<Option<(String, u16)>>> = Arc::new(RwLock::new(None));

    let egress_config = EgressConfig {
        allowlist,
        upstream: upstream_proxy,
        no_proxy,
        internal: internal_routes,
        identity_proxy,
        simulator: Arc::clone(&simulator),
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

    // Reserve a mcp-gateway (outil MCP `request_egress`) : lie a
    // 127.0.0.1 uniquement, injoignable par la microVM (voir `crate::admin`).
    let admin_router = admin::router(admin::AdminState {
        allowlist: Arc::clone(&egress_config.allowlist),
        simulator_target,
        simulator,
    });
    let admin_listener = TcpListener::bind(&admin_addr).await?;
    tracing::info!(%admin_addr, "serveur d'administration (allowlist) en ecoute");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(admin_listener, admin_router).await {
            tracing::error!(%err, "serveur d'administration arrete en erreur");
        }
    });

    // Mot de passe de session (Basic Auth guest), voir `crate::session_auth`
    // et `crate::metadata` : desactive (le guest recoit `503` en boucle) si
    // OpenBao n'est pas configure, meme convention que le reste des
    // fonctionnalites optionnelles.
    let session_auth_cache: session_auth::SessionAuthCache = Arc::new(RwLock::new(None));
    match (
        std::env::var("OPENBAO_ADDR"),
        std::env::var("ATELIER_WORKSHOP_NAME"),
    ) {
        (Ok(openbao_addr), Ok(workshop_name)) => {
            let client = OpenBaoClient::from_env(openbao_addr, workshop_name);
            tokio::spawn(session_auth::refresh_loop(
                client,
                Arc::clone(&session_auth_cache),
            ));
        }
        (Ok(_), Err(_)) => {
            // Configuration incoherente (ne devrait pas arriver en
            // production : le controller pose toujours les deux ensemble,
            // voir `crates/controller/src/reconcile.rs`) : on degrade
            // seulement cette fonctionnalite plutot que de faire echouer
            // tout net-proxy (egress/DNS restent utiles sans Basic Auth
            // guest).
            tracing::warn!(
                "OPENBAO_ADDR present mais ATELIER_WORKSHOP_NAME absent, mot de passe de session desactive"
            );
        }
        (Err(_), _) => {
            tracing::warn!(
                "OPENBAO_ADDR absent, net-proxy demarre sans mot de passe de session (Basic Auth guest desactive)"
            );
        }
    }
    let metadata_addr = std::env::var("ATELIER_NET_PROXY_METADATA_ADDR")
        .unwrap_or_else(|_| DEFAULT_METADATA_ADDR.to_string());
    let metadata_router = metadata::router(metadata::MetadataState {
        session_auth: session_auth_cache,
    });
    let metadata_listener = TcpListener::bind(&metadata_addr).await?;
    tracing::info!(%metadata_addr, "serveur metadata guest (session-auth) en ecoute");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(metadata_listener, metadata_router).await {
            tracing::error!(%err, "serveur metadata guest arrete en erreur");
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

    // Ports d'ecoute "transparents", cibles des redirections iptables
    // posees cote hote (voir `enable_transparent_gateway`) : contrairement
    // au port egress classique ci-dessous, ni `HTTP_PROXY` ni `CONNECT` ne
    // sont necessaires cote guest — c'est le seul chemin qui satisfait la
    // contrainte "aucune configuration interne a la microVM".
    let transparent_http_addr = std::env::var("ATELIER_NET_PROXY_TRANSPARENT_HTTP_ADDR")
        .unwrap_or_else(|_| DEFAULT_TRANSPARENT_HTTP_ADDR.to_string());
    let transparent_http_listener = TcpListener::bind(&transparent_http_addr).await?;
    tracing::info!(%transparent_http_addr, "proxy egress HTTP transparent en ecoute");
    {
        let config = egress_config.clone();
        tokio::spawn(async move {
            loop {
                let (socket, peer): (_, SocketAddr) = match transparent_http_listener.accept().await
                {
                    Ok(accepted) => accepted,
                    Err(err) => {
                        tracing::error!(%err, "accept() HTTP transparent a echoue");
                        continue;
                    }
                };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(err) = proxy::handle_connection(socket, peer, config).await {
                        tracing::warn!(%peer, %err, "connexion HTTP transparente terminee en erreur");
                    }
                });
            }
        });
    }

    let transparent_tls_addr = std::env::var("ATELIER_NET_PROXY_TRANSPARENT_TLS_ADDR")
        .unwrap_or_else(|_| DEFAULT_TRANSPARENT_TLS_ADDR.to_string());
    let transparent_tls_listener = TcpListener::bind(&transparent_tls_addr).await?;
    tracing::info!(%transparent_tls_addr, "proxy egress TLS transparent (SNI) en ecoute");
    {
        let config = egress_config.clone();
        tokio::spawn(async move {
            loop {
                let (socket, peer): (_, SocketAddr) = match transparent_tls_listener.accept().await
                {
                    Ok(accepted) => accepted,
                    Err(err) => {
                        tracing::error!(%err, "accept() TLS transparent a echoue");
                        continue;
                    }
                };
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(err) =
                        proxy::handle_transparent_tls_connection(socket, peer, config).await
                    {
                        tracing::warn!(%peer, %err, "connexion TLS transparente terminee en erreur");
                    }
                });
            }
        });
    }

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
