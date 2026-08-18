//! Injecte des identites/tokens (ex: credentials cloud, tokens d'API) dans
//! les appels sortants de l'agent, sans jamais exposer le secret brut a la VM.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("atelier-identity-proxy starting");
    // TODO: recuperer les secrets via un backend externe (Vault, k8s Secret projete)
    // TODO: signer/injecter les credentials a la volee dans les requetes proxiees
    Ok(())
}
