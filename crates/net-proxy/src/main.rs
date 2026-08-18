//! Proxy de sortie reseau pour la microVM : n'autorise que les domaines/IP
//! listes dans `Workshop.spec.egress_allowlist`, journalise chaque requete.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("atelier-net-proxy starting");
    // TODO: proxy HTTP(S) CONNECT avec allowlist de domaines
    // TODO: journal d'audit des appels sortants (destination, taille, resultat)
    Ok(())
}
