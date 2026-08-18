//! Construit un rootfs bootable par Firecracker a partir d'une source
//! devcontainer (`WorkshopSpec.devcontainer`), en deleguant la resolution du
//! devcontainer.json a `envbuilder` (github.com/coder/envbuilder) plutot que
//! de la reimplementer.
//!
//! Pipeline reel (verifie manuellement de bout en bout avant d'ecrire ce
//! code — boot Firecracker reussi sur le resultat) :
//! 1. `envbuilder` clone le repo, resout le devcontainer.json, construit
//!    l'image et la **pousse comme image OCI standard** vers un registre
//!    (`ENVBUILDER_PUSH_IMAGE`/`ENVBUILDER_CACHE_REPO`). Envbuilder ne
//!    produit *pas* de dossier d'export propre : il construit "en place"
//!    sur son propre `/`, donc la seule sortie exploitable est cette image
//!    poussee au registre.
//! 2. `crane export` (github.com/google/go-containerregistry) aplatit
//!    cette image OCI en tarball (pas de client OCI ecrit a la main : deux
//!    outils externes bien etablis, comme `envbuilder` lui-meme).
//! 3. Le tarball est extrait puis empaquete en image ext4 (`mke2fs -d`).
//! 4. Le digest sha256 du fichier ext4 sert de cle dans le cache
//!    content-addressed (aujourd'hui un repertoire monte depuis un PVC
//!    Kubernetes ; offload/reload vers S3 envisage plus tard, cf.
//!    docs/ARCHITECTURE.md).

use anyhow::{ensure, Context, Result};
use atelier_common::{patch_workshop_status, DevcontainerSource};
use kube::Client;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

#[tokio::main]
async fn main() -> Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-image-builder");

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
    let registry_addr =
        std::env::var("ATELIER_REGISTRY_ADDR").context("ATELIER_REGISTRY_ADDR manquant")?;
    let registry_insecure = std::env::var("ATELIER_REGISTRY_INSECURE")
        .map(|v| v == "true")
        .unwrap_or(false);
    let cache_dir = std::env::var("ATELIER_IMAGE_CACHE_DIR")
        .context("ATELIER_IMAGE_CACHE_DIR manquant (montage du PVC de cache)")?;
    let envbuilder_bin =
        std::env::var("ATELIER_ENVBUILDER_BIN").unwrap_or_else(|_| "envbuilder".to_string());
    let crane_bin = std::env::var("ATELIER_CRANE_BIN").unwrap_or_else(|_| "crane".to_string());

    let image_ref = format!("{registry_addr}/atelier-workshops/{workshop_name}:latest");

    tracing::info!(repo = %source.repo, revision = %source.revision, %image_ref, "building devcontainer via envbuilder");
    build_and_push(&envbuilder_bin, &source, &image_ref, registry_insecure).await?;

    let work_dir = PathBuf::from("/var/tmp/atelier-image-builder-work");
    tokio::fs::create_dir_all(&work_dir).await?;

    tracing::info!(%image_ref, "exporting image filesystem");
    let rootfs_dir = export_image_filesystem(&crane_bin, &image_ref, &work_dir, registry_insecure).await?;

    tracing::info!("packaging rootfs as ext4");
    let ext4_path = work_dir.join("rootfs.ext4");
    package_ext4(&rootfs_dir, &ext4_path).await?;

    let digest = sha256_file(&ext4_path).await?;
    tracing::info!(%digest, "publishing to content-addressed cache");
    let published_path = publish_to_cache(&cache_dir, &digest, &ext4_path).await?;

    tokio::fs::remove_dir_all(&rootfs_dir).await.ok();
    tokio::fs::remove_file(&ext4_path).await.ok();

    let client = Client::try_default().await?;
    patch_workshop_status(
        &client,
        &workshop_namespace,
        &workshop_name,
        serde_json::json!({ "imageDigest": digest }),
    )
    .await
    .context("echec de la mise a jour de status.imageDigest sur le Workshop")?;

    tracing::info!(%digest, path = ?published_path, "image published, workshop status updated");
    Ok(())
}

/// Invoque `envbuilder` pour resoudre le devcontainer.json et pousser
/// l'image resultante (build + `postCreateCommand` etc. deja executes)
/// vers `image_ref`. Envbuilder pousse toujours l'image finale au tag
/// `:latest` du repo qu'on lui donne, quel que soit le tag qu'on demande
/// nous-memes ensuite pour l'export — d'ou l'usage systematique de
/// `:latest` comme `image_ref`.
async fn build_and_push(
    envbuilder_bin: &str,
    source: &DevcontainerSource,
    image_ref: &str,
    registry_insecure: bool,
) -> Result<()> {
    let cache_repo = image_ref
        .rsplit_once(':')
        .map(|(repo, _tag)| repo)
        .unwrap_or(image_ref);

    // La revision est portee par la syntaxe `<url>#<ref>` d'envbuilder,
    // pas par une variable separee (verifie manuellement : `#main` est
    // bien interprete comme la branche a cloner).
    let git_url = if source.revision.is_empty() || source.revision == "HEAD" {
        source.repo.clone()
    } else {
        format!("{}#{}", source.repo, source.revision)
    };

    // `WorkshopSpec.devcontainer.config_path` est le chemin complet relatif
    // au depot (convention utilisateur, ex: ".devcontainer/devcontainer.json"),
    // mais `--devcontainer-json-path` d'envbuilder est relatif a
    // `--devcontainer-dir` (qui vaut deja ".devcontainer" par defaut) : les
    // passer tels quels double le chemin
    // (".devcontainer/.devcontainer/devcontainer.json", constate en
    // pratique). Il faut donc scinder repertoire et nom de fichier.
    let config_path = Path::new(&source.config_path);
    let devcontainer_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty());
    let devcontainer_json_filename = config_path
        .file_name()
        .context("chemin config_path invalide (pas de nom de fichier)")?;

    let mut cmd = Command::new(envbuilder_bin);
    cmd.env("ENVBUILDER_GIT_URL", git_url)
        .env("ENVBUILDER_DEVCONTAINER_JSON_PATH", devcontainer_json_filename)
        .env("ENVBUILDER_PUSH_IMAGE", "true")
        .env("ENVBUILDER_CACHE_REPO", cache_repo)
        .env("ENVBUILDER_EXIT_ON_BUILD_FAILURE", "true")
        .env("ENVBUILDER_INIT_COMMAND", "/bin/true")
        .env("ENVBUILDER_INSECURE", registry_insecure.to_string())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(devcontainer_dir) = devcontainer_dir {
        cmd.env("ENVBUILDER_DEVCONTAINER_DIR", devcontainer_dir);
    }

    let status = cmd
        .status()
        .await
        .with_context(|| format!("lancement du binaire envbuilder ({envbuilder_bin})"))?;

    ensure!(status.success(), "envbuilder a echoue avec le statut {status}");
    Ok(())
}

/// Aplatit l'image poussee en tarball (`crane export`) et l'extrait dans un
/// repertoire de travail. Pas de client OCI ecrit a la main : `crane` est un
/// outil etabli de l'ecosysteme (google/go-containerregistry) pour
/// exactement cet usage.
async fn export_image_filesystem(
    crane_bin: &str,
    image_ref: &str,
    work_dir: &Path,
    registry_insecure: bool,
) -> Result<PathBuf> {
    let tar_path = work_dir.join("rootfs.tar");
    let mut cmd = Command::new(crane_bin);
    if registry_insecure {
        cmd.arg("--insecure");
    }
    let status = cmd
        .arg("export")
        .arg(image_ref)
        .arg(&tar_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .with_context(|| format!("lancement du binaire crane ({crane_bin})"))?;
    ensure!(status.success(), "crane export a echoue avec le statut {status}");

    let rootfs_dir = work_dir.join("rootfs");
    tokio::fs::create_dir_all(&rootfs_dir).await?;
    let status = Command::new("tar")
        .arg("xf")
        .arg(&tar_path)
        .arg("-C")
        .arg(&rootfs_dir)
        .status()
        .await
        .context("extraction du tarball de l'image")?;
    ensure!(status.success(), "extraction tar a echoue avec le statut {status}");

    tokio::fs::remove_file(&tar_path).await.ok();
    Ok(rootfs_dir)
}

/// Empaquette un repertoire en image ext4 (`mke2fs -d`), dimensionnee sur le
/// contenu reel avec une marge.
async fn package_ext4(rootfs_dir: &Path, ext4_path: &Path) -> Result<()> {
    let du_output = Command::new("du")
        .arg("-sk")
        .arg(rootfs_dir)
        .output()
        .await
        .context("mesure de la taille du rootfs")?;
    ensure!(du_output.status.success(), "du a echoue");
    let size_kb: u64 = String::from_utf8_lossy(&du_output.stdout)
        .split_whitespace()
        .next()
        .context("sortie de du inattendue")?
        .parse()
        .context("taille de rootfs invalide")?;
    // Marge de 512 Mio pour l'espace libre et les metadonnees ext4.
    let size_mb = size_kb / 1024 + 512;

    let status = Command::new("truncate")
        .arg("-s")
        .arg(format!("{size_mb}M"))
        .arg(ext4_path)
        .status()
        .await
        .context("allocation du fichier ext4")?;
    ensure!(status.success(), "truncate a echoue avec le statut {status}");

    let status = Command::new("mke2fs")
        .args(["-F", "-t", "ext4", "-d"])
        .arg(rootfs_dir)
        .arg(ext4_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("lancement de mke2fs")?;
    ensure!(status.success(), "mke2fs a echoue avec le statut {status}");

    Ok(())
}

async fn sha256_file(path: &Path) -> Result<String> {
    let bytes = tokio::fs::read(path).await.context("lecture du fichier ext4 pour digest")?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// Publie l'image dans le cache content-addressed. Aujourd'hui : un
/// repertoire monte depuis un PVC Kubernetes partage entre le Job
/// image-builder (lecture-ecriture) et les pods parents (lecture seule).
/// TODO: offload/reload vers S3 (ou autre object storage) une fois le PVC
/// trop rempli — cf. docs/ARCHITECTURE.md, hors scope de cette iteration.
async fn publish_to_cache(cache_dir: &str, digest: &str, ext4_path: &Path) -> Result<PathBuf> {
    let digest_dir_name = digest.replace(':', "_");
    let dest_dir = PathBuf::from(cache_dir).join(&digest_dir_name);
    tokio::fs::create_dir_all(&dest_dir)
        .await
        .with_context(|| format!("creation du repertoire de cache {dest_dir:?}"))?;
    let dest_path = dest_dir.join("rootfs.ext4");

    // Deja present (build precedent avec le meme contenu resolu) : rien a
    // refaire, le cache est content-addressed donc idempotent.
    if dest_path.exists() {
        return Ok(dest_path);
    }

    tokio::fs::copy(ext4_path, &dest_path)
        .await
        .with_context(|| format!("publication de l'image dans le cache ({dest_path:?})"))?;
    Ok(dest_path)
}
