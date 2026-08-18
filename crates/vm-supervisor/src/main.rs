//! Tourne dans le pod parent. Pilote le cycle de vie de la microVM Firecracker
//! (jailer, socket API, montage rootfs, vsock) qui heberge l'agent de code.
//!
//! Responsabilites :
//! - demarrer/arreter la microVM via l'API Firecracker (unix socket)
//! - exposer un canal vsock entre le pod parent et l'agent dans la VM
//! - relayer les logs/metriques de la VM vers le control plane

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("atelier-vm-supervisor starting");
    // TODO: lancer le jailer Firecracker avec la config issue du Workshop CR
    // TODO: exposer un canal de controle (vsock) pour boot/shutdown/status
    Ok(())
}
