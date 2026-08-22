//! Serveur MCP expose a l'agent : point d'entree unique pour demander des
//! reglages a l'atelier plutot que d'agir en direct (elargir l'egress,
//! obtenir un credential, a terme activer un simulateur).
//!
//! Transport : HTTP/SSE (streamable HTTP, SDK officiel `rmcp`) via l'alias
//! interne `mcp-gateway` de `net-proxy` (`crates/net-proxy/src/internal.rs`),
//! jamais joint directement par la VM (meme garantie que pour
//! `identity-proxy`, voir `docs/architecture/network-security.md`). Le
//! design cible documente aussi un transport `vsock` natif, non construit
//! pour l'instant — limite assumee, voir `docs/PROGRESS.md`.

mod gateway;

use std::collections::HashSet;
use std::sync::Arc;

use atelier_common::OpenBaoClient;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use tokio::net::TcpListener;

use gateway::{Gateway, GatewayConfig};

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3130";
const DEFAULT_NET_PROXY_ADMIN_ADDR: &str = "127.0.0.1:9001";

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
