//! Construit un rootfs bootable par Firecracker a partir d'une source
//! devcontainer (`WorkshopSpec.devcontainer`), en deleguant la resolution du
//! devcontainer.json a `envbuilder` (github.com/coder/envbuilder) plutot que
//! de la reimplementer : envbuilder sait deja construire un devcontainer
//! sans daemon Docker (buildkit rootless embarque), ce qui convient a un
//! job de build lance depuis le control plane.
//!
//! Ce binaire est le point d'entree du `Job` que le `controller` cree pour
//! un `Workshop` dont `status.imageDigest` est absent : il attend le binaire
//! `envbuilder` present dans son image, l'invoque, empaquette le resultat en
//! image ext4 publiee dans le cache content-addressed, puis patche
//! lui-meme `status.imageDigest` sur le `Workshop` (le controller n'a donc
//! pas besoin de lire les logs/la sortie du Job pour recuperer le digest).

use anyhow::{ensure, Context, Result};
use atelier_common::{patch_workshop_status, DevcontainerSource};
use kube::Client;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // MVP : la source et l'identite du Workshop sont injectees via
    // l'environnement par le Job que le `controller` declenche.
    let source = DevcontainerSource {
        repo: std::env::var("ATELIER_DEVCONTAINER_REPO")
            .context("ATELIER_DEVCONTAINER_REPO manquant")?,
        revision: std::env::var("ATELIER_DEVCONTAINER_REVISION").unwrap_or_else(|_| "HEAD".into()),
        config_path: std::env::var("ATELIER_DEVCONTAINER_CONFIG_PATH")
            .unwrap_or_else(|_| ".devcontainer/devcontainer.json".into()),
    };
    let workshop_name =
        std::env::var("ATELIER_WORKSHOP_NAME").context("ATELIER_WORKSHOP_NAME manquant")?;
    let workshop_namespace = std::env::var("ATELIER_WORKSHOP_NAMESPACE")
        .context("ATELIER_WORKSHOP_NAMESPACE manquant")?;

    let export_dir = PathBuf::from("/var/lib/atelier/image-builder/export");
    tokio::fs::create_dir_all(&export_dir).await?;

    tracing::info!(repo = %source.repo, revision = %source.revision, "invoking envbuilder");
    build_with_envbuilder(&source, &export_dir).await?;

    let digest = package_and_publish(&export_dir).await?;
    tracing::info!(digest = %digest, "image published, reporting to workshop status");

    let client = Client::try_default().await?;
    patch_workshop_status(
        &client,
        &workshop_namespace,
        &workshop_name,
        serde_json::json!({ "imageDigest": digest }),
    )
    .await
    .context("echec de la mise a jour de status.imageDigest sur le Workshop")?;

    Ok(())
}

/// Invoque le binaire `envbuilder` pour resoudre le devcontainer.json et
/// produire le filesystem final dans `export_dir`.
///
/// Noms de variables a verifier/ajuster contre la reference envbuilder au
/// moment de l'implementation (elles evoluent d'une version a l'autre) :
/// https://github.com/coder/envbuilder/blob/main/docs/env-variables.md
async fn build_with_envbuilder(source: &DevcontainerSource, export_dir: &Path) -> Result<()> {
    let status = Command::new("envbuilder")
        .env("ENVBUILDER_GIT_URL", &source.repo)
        .env("ENVBUILDER_GIT_CLONE_REF", &source.revision)
        .env("ENVBUILDER_DEVCONTAINER_JSON_PATH", &source.config_path)
        .env("ENVBUILDER_ROOT_DIR", export_dir)
        .env("ENVBUILDER_EXIT_ON_BUILD_FAILURE", "true")
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context(
            "echec de lancement du binaire envbuilder (doit etre present dans l'image du job de build)",
        )?;

    ensure!(status.success(), "envbuilder a echoue avec le statut {status}");
    Ok(())
}

/// Empaquette `export_dir` en image ext4 et la publie dans le cache
/// content-addresse consomme par `vm-supervisor`, retourne son digest.
async fn package_and_publish(_export_dir: &Path) -> Result<String> {
    // TODO: `mkfs.ext4 -d <export_dir> <out.img>` (ou equivalent), calcul du
    // digest de contenu, publication dans le cache (registre OCI ou blob
    // store) sous cette cle, retour du digest
    anyhow::bail!("empaquetage/publication non implementes")
}
