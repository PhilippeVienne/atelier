//! Serveur MCP expose a l'agent (via vsock) depuis le pod parent : point
//! d'entree unique pour demander l'acces a des outils/simulateurs et pour
//! negocier des reglages (ex: elargir l'allowlist reseau, activer un simulateur AWS).

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-mcp-gateway");
    tracing::info!("atelier-mcp-gateway starting");
    // TODO: serveur MCP (stdio ou socket vsock) expose a l'agent dans la VM
    // TODO: outils MCP : list_tools, request_egress, enable_simulator, request_credential
    Ok(())
}
