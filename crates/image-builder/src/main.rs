//! Construit un rootfs bootable par Firecracker a partir d'une source
//! devcontainer (`WorkshopSpec.devcontainer`), en deleguant la resolution du
//! devcontainer.json a `envbuilder` (github.com/coder/envbuilder) plutot que
//! de la reimplementer.
//!
//! Pipeline reel (valide de bout en bout, voir `docs/PROGRESS.md` section
//! "Builder microVM") :
//! 1. `envbuilder` clone le repo, resout le devcontainer.json, construit
//!    l'image et la **pousse comme image OCI standard** vers un registre
//!    (`ENVBUILDER_PUSH_IMAGE`/`ENVBUILDER_CACHE_REPO`) — pas dans **ce**
//!    conteneur, mais a l'interieur d'une **microVM Firecracker jetable**
//!    (`crates/builder-vm-init`), demarree par ce process via
//!    `atelier_firecracker::vm::Vm::boot_with_network`. Isolation
//!    deliberee : envbuilder remonte tous les points de montage existants
//!    apres avoir vide le systeme de fichiers de son propre conteneur pour
//!    y extraire l'image cible, ce qui necessite `CAP_SYS_ADMIN` — capacite
//!    qu'on refuse d'accorder a un process qui execute des instructions de
//!    build (`RUN`, `postCreateCommand`) issues du **depot cible du
//!    Workshop**, potentiellement non fiable. Dans son propre noyau
//!    (microVM), ce remount est sans risque, equivalent a n'importe quel
//!    process root sur une machine dediee. Le seul chemin de sortie reseau
//!    de cette microVM est un `net-proxy` sidecar du meme pod (allowlist
//!    `Workshop.spec.egress_allowlist`), jamais un acces direct/NAT — voir
//!    `crates/firecracker::network` et `crates/builder-vm-init`.
//! 2. `crane export` (github.com/google/go-containerregistry) aplatit
//!    cette image OCI en tarball (pas de client OCI ecrit a la main : deux
//!    outils externes bien etablis, comme `envbuilder` lui-meme).
//! 3. Le tarball est extrait puis empaquete en image ext4 (`mke2fs -d`).
//! 4. Le digest sha256 du fichier ext4 sert de cle dans le cache
//!    content-addressed (aujourd'hui un repertoire monte depuis un PVC
//!    Kubernetes ; offload/reload vers S3 envisage plus tard, cf.
//!    docs/ARCHITECTURE.md).

use anyhow::{ensure, Context, Result};
use atelier_common::{patch_workshop_status, DevcontainerSource, OpenBaoClient};
use atelier_firecracker::network::setup_link_local_tap;
use atelier_firecracker::vm::{Vm, VmConfig};
use kube::Client;
use sha2::{Digest, Sha256};
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
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
    let crane_bin = std::env::var("ATELIER_CRANE_BIN").unwrap_or_else(|_| "crane".to_string());

    let image_ref = format!("{registry_addr}/atelier-workshops/{workshop_name}:latest");

    let work_dir = PathBuf::from("/var/tmp/atelier-image-builder-work");
    tokio::fs::create_dir_all(&work_dir).await?;

    let git_credentials = resolve_git_credentials(&workshop_name).await;

    tracing::info!(repo = %source.repo, revision = %source.revision, %image_ref, git_auth = git_credentials.is_some(), "building devcontainer via envbuilder (microVM builder)");
    build_via_microvm(
        &source,
        &image_ref,
        registry_insecure,
        &work_dir,
        git_credentials.as_ref(),
    )
    .await?;

    tracing::info!(%image_ref, "exporting image filesystem");
    let rootfs_dir =
        export_image_filesystem(&crane_bin, &image_ref, &work_dir, registry_insecure).await?;

    tracing::info!("injecting net-proxy network configuration (HTTP_PROXY/DNS)");
    inject_net_proxy_config(&rootfs_dir).await?;

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

/// Identifiants git optionnels pour un depot prive, lus au mieux depuis
/// OpenBao (`workshops/<name>/git`, champs `username`/`password`) — aucun
/// champ dedie dans le CRD `Workshop` : le secret est simplement absent pour
/// un depot public (cas le plus courant jusqu'ici), l'utilisateur le
/// provisionne lui-meme dans OpenBao s'il en a besoin, meme convention que
/// `workshops/<name>/mcp` pour `mcp-gateway`. `password` seul (sans
/// `username`) est accepte : convention GitHub/GitLab courante pour un token
/// d'acces personnel, ou `x-access-token` sert de nom d'utilisateur
/// generique.
struct GitCredentials {
    username: String,
    password: String,
}

async fn resolve_git_credentials(workshop_name: &str) -> Option<GitCredentials> {
    let addr = std::env::var("OPENBAO_ADDR").ok()?;
    let client = OpenBaoClient::from_env(addr, workshop_name.to_string());
    let token = client
        .login()
        .await
        .inspect_err(
            |err| tracing::debug!(%err, "login OpenBao echoue (pas de secret git provisionne ?)"),
        )
        .ok()?;
    let password = client.read_field(&token, "git", "password").await.ok()?;
    let username = client
        .read_field(&token, "git", "username")
        .await
        .unwrap_or_else(|_| "x-access-token".to_string());
    tracing::info!("identifiants git lus depuis OpenBao (workshops/<name>/git)");
    Some(GitCredentials { username, password })
}

/// Construit et pousse l'image devcontainer en demarrant la microVM
/// "builder" (`crates/builder-vm-init`), qui execute `envbuilder` dans son
/// propre noyau plutot que dans ce conteneur (voir commentaire de module).
/// `image_ref` est l'adresse **cote hote** (celle que ce process utilisera
/// ensuite pour `crane export`) ; le guest recoit une reference construite a
/// partir de l'IP hote du lien point-a-point si `image_ref` pointe sur
/// `localhost`/loopback (voir [`image_ref_for_guest`]).
///
/// `git_credentials`, s'ils sont fournis, transitent par les `boot_args` du
/// kernel (`atelier.git_username`/`atelier.git_password`, meme mecanisme que
/// le reste des parametres de `builder-vm-init`) — **limite assumee** :
/// Firecracker journalise les `boot_args` tels quels dans la console du
/// guest au demarrage (`The API server received a Put request on
/// "/boot-source" with body ...`), que `Vm::boot_with_network` draine vers
/// `tracing::debug!` (jamais `info!`/`warn!`, donc invisible avec le niveau
/// de log par defaut en production). Alternative ecartee pour cette
/// iteration : faire lire le secret directement par `builder-vm-init` via un
/// nouvel alias `net-proxy`/OpenBao depuis l'interieur du guest, plus sur
/// mais plus complexe — a reconsiderer si ce niveau d'exposition (debug logs
/// uniquement) s'avere insuffisant.
async fn build_via_microvm(
    source: &DevcontainerSource,
    image_ref: &str,
    registry_insecure: bool,
    work_dir: &Path,
    git_credentials: Option<&GitCredentials>,
) -> Result<()> {
    // `WorkshopSpec.devcontainer.config_path` est le chemin complet relatif
    // au depot (convention utilisateur, ex: ".devcontainer/devcontainer.json"),
    // mais `ENVBUILDER_DEVCONTAINER_JSON_PATH` est relatif a
    // `ENVBUILDER_DEVCONTAINER_DIR` (qui vaut deja ".devcontainer" par
    // defaut) : les passer tels quels double le chemin
    // (".devcontainer/.devcontainer/devcontainer.json", constate en
    // pratique dans `crates/builder-vm-init`). Il faut donc scinder
    // repertoire et nom de fichier.
    let config_path = Path::new(&source.config_path);
    let devcontainer_dir = config_path.parent().filter(|p| !p.as_os_str().is_empty());
    let devcontainer_json_filename = config_path
        .file_name()
        .and_then(|f| f.to_str())
        .context("chemin config_path invalide (pas de nom de fichier)")?;

    // Nom de jail/TAP court et deterministe (hash des variables d'identite
    // du build) : `sockaddr_un.sun_path` est limite a 108 octets sur Linux
    // et IFNAMSIZ a 15 caracteres pour un nom d'interface — un nom de jail
    // trop long fait echouer le boot en silence (voir docs/PROGRESS.md,
    // "Lecons retenues"). Un Job = un pod = un netns dedie, donc pas besoin
    // d'unicite globale, seulement de brievete.
    let mut hasher = Sha256::new();
    hasher.update(source.repo.as_bytes());
    hasher.update(source.revision.as_bytes());
    hasher.update(image_ref.as_bytes());
    let digest = hasher.finalize();
    let short_id: String = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
    let jail_id = format!("ib{short_id}");
    let tap_name = format!("ib{short_id}");

    let firecracker_bin = env_path("ATELIER_BUILDER_FIRECRACKER_BIN", "firecracker");
    let jailer_bin = env_path("ATELIER_BUILDER_JAILER_BIN", "jailer");
    let kernel_path = env_path("ATELIER_BUILDER_VM_KERNEL_PATH", "");
    ensure!(
        !kernel_path.as_os_str().is_empty(),
        "ATELIER_BUILDER_VM_KERNEL_PATH est requis"
    );
    let builder_rootfs_path = resolve_builder_rootfs(work_dir).await?;
    let chroot_base_dir = env_path("ATELIER_BUILDER_VM_CHROOT_BASE_DIR", "/srv/builder-jailer");
    let net_proxy_port =
        std::env::var("ATELIER_BUILDER_NET_PROXY_PORT").unwrap_or_else(|_| "3128".to_string());
    let vcpu_count: u8 = std::env::var("ATELIER_BUILDER_VM_VCPU_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    let mem_mib: usize = std::env::var("ATELIER_BUILDER_VM_MEM_MIB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2048);
    let boot_timeout_secs: u64 = std::env::var("ATELIER_BUILDER_VM_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1800);

    let network = setup_link_local_tap(&tap_name, 1)
        .await
        .context("creation du TAP pour la microVM builder (CAP_NET_ADMIN requis)")?;

    let builder_rootfs_path_cleanup = builder_rootfs_path.clone();
    let result = run_builder_vm(RunBuilderVmArgs {
        firecracker_bin,
        jailer_bin,
        kernel_path,
        builder_rootfs_path,
        chroot_base_dir,
        jail_id,
        source,
        devcontainer_dir: devcontainer_dir.and_then(|p| p.to_str()),
        devcontainer_json_filename,
        image_ref: &image_ref_for_guest(image_ref, network.host_ip),
        registry_insecure,
        net_proxy_port: &net_proxy_port,
        vcpu_count,
        mem_mib,
        boot_timeout_secs,
        network: &network,
        git_credentials,
    })
    .await;

    network.teardown().await;
    tokio::fs::remove_file(&builder_rootfs_path_cleanup)
        .await
        .ok();
    result
}

/// Fournit un rootfs.ext4 pour la microVM builder, avec assez de marge pour
/// un vrai build devcontainer (paquets apt/pip/npm) : le rootfs baque dans
/// l'image `image-builder` (`ATELIER_BUILDER_VM_ROOTFS_BASE_PATH`, voir
/// `Dockerfile`) ne contient que le contenu de base
/// (`atelier-builder-vm-init` + `envbuilder`) avec une marge minimale — un
/// `rootfs.ext4` de cette taille tel quel echoue en "no space left on
/// device" en plein build (constate en pratique, voir docs/PROGRESS.md).
/// Copie donc ce rootfs de base dans le repertoire de travail puis
/// l'agrandit (`truncate` + `resize2fs`) avant chaque build.
///
/// `ATELIER_BUILDER_VM_ROOTFS_PATH`, s'il est defini, court-circuite tout
/// ca et est utilise tel quel (deja pre-dimensionne) — pratique pour du
/// test manuel (voir `deploy/dev/builder-vm/README.md`), sans devoir
/// reconstruire l'image `image-builder` a chaque fois.
async fn resolve_builder_rootfs(work_dir: &Path) -> Result<PathBuf> {
    if let Ok(explicit) = std::env::var("ATELIER_BUILDER_VM_ROOTFS_PATH") {
        return Ok(explicit.into());
    }

    let base_path = env_path("ATELIER_BUILDER_VM_ROOTFS_BASE_PATH", "");
    ensure!(
        !base_path.as_os_str().is_empty(),
        "ATELIER_BUILDER_VM_ROOTFS_PATH ou ATELIER_BUILDER_VM_ROOTFS_BASE_PATH est requis"
    );
    let margin_mb: u64 = std::env::var("ATELIER_BUILDER_VM_ROOTFS_MARGIN_MB")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4096);

    let rootfs_path = work_dir.join("builder-vm-rootfs.ext4");
    tokio::fs::copy(&base_path, &rootfs_path)
        .await
        .with_context(|| {
            format!("copie du rootfs de base de la microVM builder ({base_path:?})")
        })?;

    let base_size_mb = tokio::fs::metadata(&rootfs_path).await?.len() / 1024 / 1024;
    let status = Command::new("truncate")
        .arg("-s")
        .arg(format!("{}M", base_size_mb + margin_mb))
        .arg(&rootfs_path)
        .status()
        .await
        .context("agrandissement du fichier rootfs de la microVM builder")?;
    ensure!(
        status.success(),
        "truncate a echoue avec le statut {status}"
    );

    let status = Command::new("resize2fs")
        .arg(&rootfs_path)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await
        .context("lancement de resize2fs")?;
    ensure!(
        status.success(),
        "resize2fs a echoue avec le statut {status}"
    );

    Ok(rootfs_path)
}

struct RunBuilderVmArgs<'a> {
    firecracker_bin: PathBuf,
    jailer_bin: PathBuf,
    kernel_path: PathBuf,
    builder_rootfs_path: PathBuf,
    chroot_base_dir: PathBuf,
    jail_id: String,
    source: &'a DevcontainerSource,
    devcontainer_dir: Option<&'a str>,
    devcontainer_json_filename: &'a str,
    image_ref: &'a str,
    registry_insecure: bool,
    net_proxy_port: &'a str,
    vcpu_count: u8,
    mem_mib: usize,
    boot_timeout_secs: u64,
    git_credentials: Option<&'a GitCredentials>,
    network: &'a atelier_firecracker::network::NetworkSetup,
}

async fn run_builder_vm(args: RunBuilderVmArgs<'_>) -> Result<()> {
    let git_url = if args.source.revision.is_empty() || args.source.revision == "HEAD" {
        args.source.repo.clone()
    } else {
        format!("{}#{}", args.source.repo, args.source.revision)
    };

    let mut boot_args = format!(
        "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/atelier-builder-vm-init \
         atelier.repo={git_url} \
         atelier.devcontainer_json_filename={} \
         atelier.image_ref={} \
         atelier.registry_insecure={} \
         atelier.guest_ip={} atelier.host_ip={} atelier.prefix={} \
         atelier.net_proxy_port={}",
        args.devcontainer_json_filename,
        args.image_ref,
        args.registry_insecure,
        args.network.guest_ip,
        args.network.host_ip,
        args.network.network_length,
        args.net_proxy_port,
    );
    if let Some(devcontainer_dir) = args.devcontainer_dir {
        boot_args.push_str(&format!(" atelier.devcontainer_dir={devcontainer_dir}"));
    }
    if let Some(creds) = args.git_credentials {
        // Voir le commentaire de `build_via_microvm` pour la limite assumee
        // (visible en clair dans les logs debug de la console guest).
        boot_args.push_str(&format!(
            " atelier.git_username={} atelier.git_password={}",
            creds.username, creds.password
        ));
    }

    let config = VmConfig {
        firecracker_bin: args.firecracker_bin,
        jailer_bin: args.jailer_bin,
        snapshot_editor_bin: "/bin/true".into(),
        chroot_base_dir: args.chroot_base_dir,
        jail_id: args.jail_id,
        uid: 0,
        gid: 0,
        vcpu_count: args.vcpu_count,
        mem_mib: args.mem_mib,
        boot_args,
        vsock: None,
    };

    tracing::info!("booting builder microVM");
    let mut vm = Vm::boot_with_network(
        &config,
        &args.kernel_path,
        &args.builder_rootfs_path,
        args.network,
    )
    .await
    .context("boot de la microVM builder")?;

    // La VM s'eteint d'elle-meme (reboot(RB_AUTOBOOT) dans
    // atelier-builder-vm-init) une fois envbuilder termine : pas de canal de
    // controle vsock dans ce MVP, donc on attend qu'is_running() devienne
    // faux plutot que d'appeler shutdown() nous-memes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.boot_timeout_secs);
    loop {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "la microVM builder ne s'est pas eteinte a temps (build trop long ou echec silencieux)"
        );
        match vm.is_running().await {
            Ok(true) => tokio::time::sleep(Duration::from_secs(2)).await,
            Ok(false) => break,
            Err(err) => {
                tracing::warn!(%err, "impossible d'interroger l'etat de la microVM builder (VM probablement eteinte)");
                break;
            }
        }
    }
    tracing::info!("builder microVM exited");
    Ok(())
}

/// Construit la reference d'image passee au guest. Deux strategies :
///
/// - si `ATELIER_BUILDER_REGISTRY_ALIAS` (ex: `registry:5000`) est defini
///   (production : le controller le cable sur le net-proxy sidecar du Job,
///   voir `crates/net-proxy::internal` et `crates/controller/src/reconcile.rs`),
///   on l'utilise tel quel — l'alias `registry` de net-proxy resout vers le
///   vrai registre **sans** que l'utilisateur ait besoin de l'ajouter a
///   `Workshop.spec.egress_allowlist` (detail d'implementation interne, pas
///   de l'egress au sens du modele de securite du Workshop) ;
/// - sinon (test manuel sans net-proxy sidecar, cf.
///   `deploy/dev/builder-vm/README.md`), on retombe sur une heuristique :
///   `envbuilder` (client HTTP Go) exclut inconditionnellement `localhost`
///   et les IP loopback du proxy configure via `HTTP_PROXY`/`HTTPS_PROXY`
///   (`golang.org/x/net/http/httpproxy`), meme sans `NO_PROXY` — comportement
///   non desactivable depuis l'environnement. Le guest n'a pas de route par
///   defaut (voir `crates/builder-vm-init::configure_network`), donc une
///   reference d'image en `localhost:<port>/...` y echoue toujours en
///   "network is unreachable" ; on la reecrit alors avec l'IP hote du lien
///   point-a-point (sinon, nom DNS reel, on la laisse telle quelle).
fn image_ref_for_guest(image_ref: &str, host_ip: Ipv4Addr) -> String {
    let Some((host_port, path)) = image_ref.split_once('/') else {
        return image_ref.to_string();
    };
    if let Ok(alias) = std::env::var("ATELIER_BUILDER_REGISTRY_ALIAS") {
        if !alias.trim().is_empty() {
            return format!("{alias}/{path}");
        }
    }
    let (host, port) = match host_port.split_once(':') {
        Some((h, p)) => (h, Some(p)),
        None => (host_port, None),
    };
    let is_loopback = host == "localhost"
        || host
            .parse::<std::net::IpAddr>()
            .map(|ip| ip.is_loopback())
            .unwrap_or(false);
    if !is_loopback {
        return image_ref.to_string();
    }
    match port {
        Some(port) => format!("{host_ip}:{port}/{path}"),
        None => format!("{host_ip}/{path}"),
    }
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var)
        .unwrap_or_else(|_| default.to_string())
        .into()
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
    ensure!(
        status.success(),
        "crane export a echoue avec le statut {status}"
    );

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
    ensure!(
        status.success(),
        "extraction tar a echoue avec le statut {status}"
    );

    tokio::fs::remove_file(&tar_path).await.ok();
    Ok(rootfs_dir)
}

/// Ecrit `/etc/environment` et `/etc/resolv.conf` directement dans
/// l'arborescence exportee, pour que le devcontainer utilise reellement
/// `net-proxy` comme seule sortie reseau une fois booted : le TAP + les
/// regles iptables de `vm-supervisor` (voir `docs/PROGRESS.md`, "Reseau de
/// l'agent") n'autorisent deja que ce chemin, mais rien ne configurait
/// jusqu'ici `HTTP_PROXY`/`HTTPS_PROXY` ni un resolveur DNS a l'interieur de
/// l'image construite — un devcontainer qui l'ignore tente une connexion
/// directe, silencieusement rejetee par ces regles. Doit s'ecrire dans le
/// filesystem lui-meme (pas passe en variable au build) : l'export brut
/// (`crane export` + `mke2fs`) perd toute metadonnee OCI `ENV`, et
/// `vm-supervisor` boote le PID 1 du devcontainer tel quel, sans init
/// personnalise capable de recevoir des parametres au boot (contrairement a
/// la microVM "builder"). `net-proxy` est toujours joignable a l'adresse
/// fixe du lien point-a-point (`169.254.0.1`, cf.
/// `crates/firecracker::network`), le port `3128` est une constante partagee
/// avec `vm-supervisor`/`controller` (`crates/controller/src/reconcile.rs`),
/// pas encore configurable par Workshop.
async fn inject_net_proxy_config(rootfs_dir: &Path) -> Result<()> {
    let net_proxy_port =
        std::env::var("ATELIER_NET_PROXY_PORT").unwrap_or_else(|_| "3128".to_string());
    let proxy_url = format!("http://169.254.0.1:{net_proxy_port}");

    // /etc/environment est lu par pam_env pour toute session de login
    // (SSH/terminal interactif, ex: code-server, Claude Code) : on complete
    // le fichier existant plutot que de l'ecraser, une image de base pouvant
    // deja y definir d'autres variables.
    let environment_path = rootfs_dir.join("etc/environment");
    let mut environment = tokio::fs::read_to_string(&environment_path)
        .await
        .unwrap_or_default();
    if !environment.is_empty() && !environment.ends_with('\n') {
        environment.push('\n');
    }
    environment.push_str(&format!(
        "HTTP_PROXY={proxy_url}\nHTTPS_PROXY={proxy_url}\nhttp_proxy={proxy_url}\nhttps_proxy={proxy_url}\nNO_PROXY=169.254.0.1\nno_proxy=169.254.0.1\n"
    ));
    // LLM Proxy (service global du cluster, `deploy/dev/llm-proxy/`) :
    // route les appels Anthropic Messages API de Claude Code vers
    // `net-proxy` (alias `llm-proxy`, `crates/net-proxy/src/internal.rs`),
    // jamais un nom DNS reel — rien a ajouter a l'allowlist egress.
    // `ANTHROPIC_API_KEY` vide desactive explicitement toute cle locale
    // eventuellement presente sur l'image de base, pour forcer le passage
    // par `ANTHROPIC_AUTH_TOKEN`. N'ecrit rien si le service n'est pas
    // configure cote controller (`ATELIER_LLM_PROXY_AUTH_TOKEN` absent) —
    // meme convention que le reste des fonctionnalites optionnelles.
    if let Ok(llm_proxy_auth_token) = std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN") {
        environment.push_str(&format!(
            "ANTHROPIC_BASE_URL=http://llm-proxy\nANTHROPIC_AUTH_TOKEN={llm_proxy_auth_token}\nANTHROPIC_API_KEY=\n"
        ));
    }
    tokio::fs::write(&environment_path, environment)
        .await
        .with_context(|| format!("ecriture de {environment_path:?}"))?;

    // Peut etre un symlink (ex: vers systemd-resolved) dans l'image de base
    // — sans effet ici puisque rien ne fait tourner systemd-resolved dans
    // cette microVM, remplace donc par un fichier statique.
    let resolv_conf_path = rootfs_dir.join("etc/resolv.conf");
    tokio::fs::remove_file(&resolv_conf_path).await.ok();
    tokio::fs::write(&resolv_conf_path, "nameserver 169.254.0.1\n")
        .await
        .with_context(|| format!("ecriture de {resolv_conf_path:?}"))?;

    Ok(())
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
    ensure!(
        status.success(),
        "truncate a echoue avec le statut {status}"
    );

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
    let bytes = tokio::fs::read(path)
        .await
        .context("lecture du fichier ext4 pour digest")?;
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
