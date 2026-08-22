//! Serveur MCP expose a l'agent : point d'entree unique pour demander des
//! reglages a l'atelier plutot que d'agir en direct (elargir l'egress,
//! obtenir un credential, a terme activer un simulateur).
//!
//! Deux transports actifs en parallele :
//! - HTTP/SSE (streamable HTTP, SDK officiel `rmcp`) via l'alias interne
//!   `mcp-gateway` de `net-proxy` (`crates/net-proxy/src/internal.rs`),
//!   jamais joint directement par la VM (meme garantie que pour
//!   `identity-proxy`, voir `docs/architecture/network-security.md`).
//! - `vsock` natif (`ATELIER_MCP_GATEWAY_VSOCK_UDS_PATH`) : canal guest<->hote
//!   plus direct (pas de TAP/iptables/allowlist a traverser), pour un client
//!   MCP a l'interieur du guest capable de parler `AF_VSOCK` directement.
//!   Reutilise le meme `Gateway` (meme `ServerHandler`) sur un flux brut via
//!   `rmcp::transport::async_rw` (`Gateway::serve(unix_stream)`) plutot que
//!   `StreamableHttpService`, specifique a HTTP. Convention Firecracker pour
//!   les connexions **initiees par le guest** : ce process doit lier un UDS
//!   a `<uds_path>_<port>` (le fichier `<uds_path>` lui-meme est cree par
//!   Firecracker, cote `vm-supervisor`, voir `crates/firecracker/src/vm.rs`).

mod gateway;

use std::collections::HashSet;
use std::sync::Arc;

use atelier_common::OpenBaoClient;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::ServiceExt;
use tokio::net::{TcpListener, UnixListener};

use gateway::{Gateway, GatewayConfig};

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3130";
const DEFAULT_NET_PROXY_ADMIN_ADDR: &str = "127.0.0.1:9001";
const DEFAULT_VSOCK_PORT: u32 = 10000;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-mcp-gateway");

    let listen_addr = std::env::var("ATELIER_MCP_GATEWAY_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string());
    let workshop_name = std::env::var("ATELIER_WORKSHOP_NAME").unwrap_or_default();
    let enabled_tools: HashSet<String> = std::env::var("ATELIER_TOOLS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    let net_proxy_admin_addr = std::env::var("ATELIER_NET_PROXY_ADMIN_ADDR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| Some(DEFAULT_NET_PROXY_ADMIN_ADDR.to_string()));
    let openbao = std::env::var("OPENBAO_ADDR")
        .ok()
        .map(|addr| OpenBaoClient::from_env(addr, workshop_name.clone()));

    if openbao.is_none() {
        tracing::warn!(
            "OPENBAO_ADDR absent : le tool request_credential echouera systematiquement"
        );
    }
    tracing::info!(
        count = enabled_tools.len(),
        tools = ?enabled_tools,
        "atelier-mcp-gateway demarre"
    );

    let config = Arc::new(GatewayConfig {
        enabled_tools,
        openbao,
        net_proxy_admin_addr,
        http: reqwest::Client::new(),
    });

    if let Some(uds_path) = std::env::var("ATELIER_MCP_GATEWAY_VSOCK_UDS_PATH")
        .ok()
        .filter(|s| !s.trim().is_empty())
    {
        let vsock_port: u32 = std::env::var("ATELIER_MCP_GATEWAY_VSOCK_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_VSOCK_PORT);
        let listen_path = format!("{uds_path}_{vsock_port}");
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            if let Err(err) = run_vsock_listener(&listen_path, config).await {
                tracing::error!(%err, %listen_path, "serveur MCP vsock arrete en erreur");
            }
        });
    } else {
        tracing::info!("ATELIER_MCP_GATEWAY_VSOCK_UDS_PATH absent : transport vsock desactive");
    }

    // `allowed_hosts` par defaut de rmcp ne couvre que localhost/127.0.0.1 —
    // net-proxy relaie la requete telle quelle recue de la VM (voir
    // `crate::proxy::forward` dans net-proxy), Host header compris, donc
    // potentiellement "mcp-gateway" (le nom de l'alias) plutot qu'une IP.
    let http_config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(["mcp-gateway", "127.0.0.1", "localhost"]);

    let service: StreamableHttpService<Gateway, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(Gateway::new(Arc::clone(&config))),
        Default::default(),
        http_config,
    );

    let router = axum::Router::new().nest_service("/mcp", service);
    let listener = TcpListener::bind(&listen_addr).await?;
    tracing::info!(%listen_addr, "serveur MCP en ecoute (/mcp)");
    axum::serve(listener, router).await?;
    Ok(())
}

/// Accepte les connexions `AF_VSOCK` initiees par le guest (relayees par
/// Firecracker vers ce socket Unix, cf. commentaire de module) : une session
/// MCP complete par connexion, meme `Gateway` que le transport HTTP. Le
/// fichier peut deja exister d'un process precedent (redemarrage du
/// conteneur) : supprime avant de re-lier, sans quoi `bind` echoue en
/// `AddrInUse`.
async fn run_vsock_listener(listen_path: &str, config: Arc<gateway::GatewayConfig>) -> anyhow::Result<()> {
    let _ = tokio::fs::remove_file(listen_path).await;
    let listener = UnixListener::bind(listen_path)?;
    tracing::info!(%listen_path, "serveur MCP vsock en ecoute");
    loop {
        let (stream, _addr) = listener.accept().await?;
        let config = Arc::clone(&config);
        tokio::spawn(async move {
            let gateway = Gateway::new(config);
            match gateway.serve(stream).await {
                Ok(running) => {
                    if let Err(err) = running.waiting().await {
                        tracing::warn!(%err, "session MCP vsock terminee en erreur");
                    }
                }
                Err(err) => tracing::warn!(%err, "echec d'initialisation d'une session MCP vsock"),
            }
        });
    }
}
