//! Tourne dans le pod parent. Pilote le cycle de vie de la microVM Firecracker
//! (jailer, socket API, montage rootfs, vsock) qui heberge l'agent de code.
//!
//! Le rootfs monte est celui construit en amont par `image-builder` a partir
//! du devcontainer du `Workshop` (reference par `WorkshopStatus.image_digest`),
//! pas une image ad hoc : l'environnement de l'agent est donc un devcontainer
//! standard (VS Code Dev Containers) boote comme microVM plutot que comme
//! conteneur Docker.
//!
//! Responsabilites :
//! - recuperer le rootfs construit (cache content-addressed) via son digest
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
