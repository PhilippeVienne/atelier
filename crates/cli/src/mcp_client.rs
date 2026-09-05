//! Connexion cliente au serveur MCP externe deja expose par `api-server`
//! (`/v1/mcp`, memes outils lifecycle que
//! `crates/api-server/src/mcp_server.rs`) : reutilise pour `atelier mcp
//! serve` (tache 9.9) le meme client `rmcp` (transport Streamable HTTP,
//! `bearer_auth`) que `crates/api-server/tests/mcp.rs` (test d'integration
//! reel deja existant contre ce meme endpoint).

use anyhow::{Context, Result};
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::RoleClient;

pub type UpstreamService = rmcp::service::RunningService<RoleClient, ()>;

pub async fn connect(api_url: &str, access_token: &str) -> Result<UpstreamService> {
    let mut config = StreamableHttpClientTransportConfig::default();
    config.uri = format!("{}/v1/mcp", api_url.trim_end_matches('/')).into();
    config.auth_header = Some(access_token.to_string());
    let transport =
        StreamableHttpClientTransport::with_client(reqwest013::Client::default(), config);
    rmcp::ServiceExt::serve((), transport)
        .await
        .context("connexion/handshake MCP vers api-server (/v1/mcp)")
}
