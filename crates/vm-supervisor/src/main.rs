//! Tourne dans le pod parent. Pilote le cycle de vie de la microVM Firecracker
//! (jailer, socket API, montage rootfs, vsock) qui heberge l'agent de code.
//!
//! Le rootfs monte est celui construit en amont par `image-builder` a partir
//! du devcontainer du `Workshop` (reference par `WorkshopStatus.image_digest`),
//! pas une image ad hoc : l'environnement de l'agent est donc un devcontainer
//! standard (VS Code Dev Containers) boote comme microVM plutot que comme
//! conteneur Docker.
//!
//! Mise en veille : Firecracker sait figer une microVM (etat + memoire) via
//! `PUT /snapshot/create` et la restaurer via `PUT /snapshot/load`, ce qui
//! permet de suspendre un Workshop (liberer le pod parent, ne garder que le
//! snapshot dans le cache) puis de le reprendre en quelques centaines de ms
//! sans rejouer le boot ni le setup du devcontainer. C'est ce mecanisme, pas
//! un simple arret/redemarrage, qui gere `WorkshopSpec.desired_state`.
//!
//! Responsabilites :
//! - recuperer le rootfs construit (cache content-addressed) via son digest,
//!   ou un snapshot existant (`status.snapshot_digest`) en cas de reprise
//! - demarrer/arreter la microVM via l'API Firecracker (unix socket)
//! - sur demande de mise en veille : `snapshot/create`, publier le snapshot
//!   dans le cache content-addressed, rendre le digest au control plane
//! - sur demande de reprise : recuperer le snapshot, `snapshot/load`
//! - exposer un canal vsock entre le pod parent et l'agent dans la VM
//! - relayer les logs/metriques de la VM vers le control plane

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("atelier-vm-supervisor starting");
    // TODO: lancer le jailer Firecracker avec la config issue du Workshop CR
    //       (depuis image_digest, ou restauration depuis snapshot_digest)
    // TODO: exposer un canal de controle (vsock) pour boot/shutdown/status
    // TODO: gerer les commandes suspend/resume (snapshot/create, snapshot/load)
    Ok(())
}
