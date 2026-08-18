//! Construit un rootfs bootable par Firecracker a partir d'une source
//! devcontainer (`WorkshopSpec.devcontainer`), sans dependre d'un daemon
//! Docker dans le pod (a la maniere de coder/envbuilder).
//!
//! Etapes prevues :
//! 1. cloner le depot a la revision demandee
//! 2. resoudre le devcontainer.json (image de base, build.dockerfile, features)
//! 3. construire le systeme de fichiers final (buildkit rootless ou equivalent)
//! 4. l'empaqueter en image ext4 et le publier dans le cache content-addressed
//!    (cle = digest du contenu resolu), reference ensuite dans
//!    `WorkshopStatus.image_digest`

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("atelier-image-builder starting");
    // TODO: cloner le repo + parser devcontainer.json (image/build/features)
    // TODO: construire le rootfs (buildkit rootless) et le convertir en ext4
    // TODO: publier dans le cache content-addressed, retourner le digest
    Ok(())
}
