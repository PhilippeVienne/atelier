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

    tracing::info!("injecting the opencode binary");
    inject_opencode_binary(&rootfs_dir).await?;

    tracing::info!("cloning target repository into the workspace");
    ensure_workspace_clone(&rootfs_dir, &source, git_credentials.as_ref()).await?;

    tracing::info!("installing the boot-time workspace refresh service");
    inject_workspace_refresh(&rootfs_dir, &source).await?;

    tracing::info!("installing terminal (ttyd) and web IDE (code-server)");
    inject_terminal_and_ide(&rootfs_dir, &source).await?;

    tracing::info!("installing sshd");
    inject_sshd(&rootfs_dir).await?;

    tracing::info!("checking for an init system, installing a minimal fallback if absent");
    ensure_init_system(&rootfs_dir).await?;

    tracing::info!("packaging rootfs as ext4");
    let ext4_path = work_dir.join("rootfs.ext4");
    package_ext4(&rootfs_dir, &ext4_path).await?;

    let digest = sha256_file(&ext4_path).await?;
    tracing::info!(%digest, "publishing to content-addressed cache");
    let published_path = publish_to_cache(&cache_dir, &digest, &ext4_path).await?;

    // Offload S3 best-effort (spec docs/specs/13-image-cache-offload.md,
    // tache 8.3) : ne bloque jamais le build ni la publication locale
    // ci-dessus, seule strictement necessaire pour que ce Workshop demarre.
    // `S3StorageBackend::from_env` renvoie `None` si `S3_ENDPOINT` est
    // absent, et `upload_image_cache_file` elle-meme ne fait rien si
    // `S3_BUCKET_IMAGE_CACHE` n'est pas configure (fonctionnalite
    // independamment optionnelle, voir `S3Config::bucket_image_cache`).
    match atelier_common::storage::S3StorageBackend::from_env() {
        Ok(Some(storage)) => {
            if let Err(err) = storage
                .upload_image_cache_file(&digest, &published_path)
                .await
            {
                tracing::warn!(%err, %digest, "televersement du cache d'images vers S3 echoue, ignore");
            }
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(%err, "configuration S3 invalide, offload du cache d'images ignore");
        }
    }

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
    // trop long fait echouer le boot en silence (voir
    // docs/architecture/pieges.md). Un Job = un pod = un netns dedie, donc
    // pas besoin d'unicite globale, seulement de brievete.
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
    let net_proxy_transparent_http_port: u16 =
        std::env::var("ATELIER_BUILDER_NET_PROXY_TRANSPARENT_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3180);
    let net_proxy_transparent_tls_port: u16 =
        std::env::var("ATELIER_BUILDER_NET_PROXY_TRANSPARENT_TLS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3181);
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
    // Jusqu'ici cette VM n'avait aucune regle iptables (protegee seulement
    // par l'absence de route par defaut, cf. `builder-vm-init::configure_network`) :
    // meme mecanisme de passerelle transparente que la VM agent
    // (`vm-supervisor`), pour que les etapes `RUN` d'un Dockerfile execute
    // par `envbuilder` (apt, etc.) fonctionnent sans jamais avoir besoin de
    // connaitre `HTTP_PROXY`/`HTTPS_PROXY` — voir
    // docs/architecture/network-security.md.
    network
        .enable_transparent_gateway(
            net_proxy_port
                .parse()
                .context("ATELIER_BUILDER_NET_PROXY_PORT invalide")?,
            net_proxy_transparent_http_port,
            net_proxy_transparent_tls_port,
            // Pas d'acces au serveur metadata : la VM builder execute
            // `envbuilder`, jamais les scripts de recuperation de
            // credentials du devcontainer (voir `enable_transparent_gateway`).
            None,
        )
        .await
        .context("pose des regles iptables de la passerelle transparente (VM builder)")?;

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
    // (SSH/terminal interactif, ex: code-server, un CLI d'agent) : on
    // complete le fichier existant plutot que de l'ecraser, une image de
    // base pouvant deja y definir d'autres variables.
    let environment_path = rootfs_dir.join("etc/environment");
    let mut environment = tokio::fs::read_to_string(&environment_path)
        .await
        .unwrap_or_default();
    if !environment.is_empty() && !environment.ends_with('\n') {
        environment.push('\n');
    }
    // `localhost,127.0.0.1` dans NO_PROXY : sans eux, tester son propre
    // serveur depuis l'interieur du guest (`curl http://localhost:3000/`,
    // reflexe le plus banal qui soit pour un agent qui vient d'ecrire un
    // service HTTP) part vers net-proxy, qui ne connait "localhost" que
    // comme lui-meme et repond 502 — l'agent croit alors son propre serveur
    // casse. Constate en Workshop reel le 2026-09-02 : un agent qui venait
    // de faire passer sa suite de tests (3/3) a conclu, sur la foi de ce
    // faux 502, que son serveur ne repondait pas.
    environment.push_str(&format!(
        "HTTP_PROXY={proxy_url}\nHTTPS_PROXY={proxy_url}\nhttp_proxy={proxy_url}\nhttps_proxy={proxy_url}\nNO_PROXY=169.254.0.1,localhost,127.0.0.1\nno_proxy=169.254.0.1,localhost,127.0.0.1\n"
    ));
    // LLM Proxy (service global du cluster, `deploy/dev/llm-proxy/`), route
    // vers `net-proxy` (alias `llm-proxy`, `crates/net-proxy/src/internal.rs`),
    // jamais un nom DNS reel — rien a ajouter a l'allowlist egress.
    // N'ecrit rien si le service n'est pas configure cote controller
    // (`ATELIER_LLM_PROXY_AUTH_TOKEN` absent) — meme convention que le
    // reste des fonctionnalites optionnelles.
    // Valeur VIDE traitee comme absente (voir la meme garde dans
    // `crates/controller/src/reconcile.rs`) : injecter un jeton vide donnait
    // un guest qui parlait a LiteLLM sans s'authentifier, sans erreur
    // visible — pire qu'une absence franche de configuration.
    if let Some(llm_proxy_auth_token) = std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty())
    {
        // `ANTHROPIC_API_KEY` vide desactive explicitement toute cle locale
        // eventuellement presente sur l'image de base, pour forcer le
        // passage par `ANTHROPIC_AUTH_TOKEN` — utile a un CLI Anthropic
        // qu'un developpeur choisirait d'installer lui-meme dans son
        // devcontainer (usage interactif, hors du chemin automatise
        // ci-dessous).
        environment.push_str(&format!(
            "ANTHROPIC_BASE_URL=http://llm-proxy\nANTHROPIC_AUTH_TOKEN={llm_proxy_auth_token}\nANTHROPIC_API_KEY=\n"
        ));
        // `opencode` (sst/opencode, licence MIT) : agent delegue par
        // `pm_engine.nodes.delegate_to_opencode` (remplace Claude Code le
        // 2026-09-01, voir docs/architecture/pieges.md — segfault
        // reproductible du binaire Bun `claude.exe`, sans rapport avec
        // cette infrastructure, et volonte de ne pas maintenir de
        // dependance de premier plan a un CLI en licence fermee dans un
        // outil qu'on veut entierement open source).
        // `OPENCODE_CONFIG_CONTENT` : config JSON inline plutot qu'un
        // fichier separe — evite de supposer un repertoire home fixe sur
        // une image de base arbitraire (meme raison que le choix
        // `/etc/environment` plutot qu'un profil utilisateur). Le
        // fournisseur `atelier` (`@ai-sdk/openai-compatible`) parle a
        // `llm-proxy` en API OpenAI standard — LiteLLM expose les deux
        // formes (`/v1/chat/completions` et `/v1/messages`) quel que soit
        // le fournisseur reel derriere l'alias `atelier-workshop-agent`
        // (voir `deploy/dev/llm-proxy/config.yaml`).
        let opencode_config = serde_json::json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": {
                "atelier": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "Atelier LLM Proxy",
                    "options": {
                        "baseURL": "http://llm-proxy/v1",
                        "apiKey": "{env:ATELIER_LLM_PROXY_AUTH_TOKEN}",
                    },
                    // `models` est OBLIGATOIRE, meme pour un fournisseur
                    // OpenAI-compatible : `opencode` ne decouvre pas les
                    // modeles en interrogeant `/v1/models`, il ne connait
                    // que ceux du catalogue models.dev et ceux declares
                    // ici. Sans cette section, `opencode models` ne liste
                    // rien pour `atelier` et `opencode run --model
                    // atelier/atelier-workshop-agent` se bloque sans le
                    // moindre message (constate en Workshop reel).
                    "models": {
                        "atelier-workshop-agent": {
                            "name": "Atelier Workshop Agent",
                        },
                    },
                },
            },
        })
        .to_string();
        environment.push_str(&format!(
            "ATELIER_LLM_PROXY_AUTH_TOKEN={llm_proxy_auth_token}\nOPENCODE_CONFIG_CONTENT={opencode_config}\n"
        ));
    }
    tokio::fs::write(&environment_path, &environment)
        .await
        .with_context(|| format!("ecriture de {environment_path:?}"))?;

    // Meme contenu, dans la forme qu'attend `sshd` : c'est lui, et non PAM,
    // qui doit exposer ces variables aux commandes de `exec_in_workshop`
    // (voir `inject_sshd`).
    write_ssh_environment_file(rootfs_dir, &environment).await?;

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

/// Recopie `/etc/environment` dans `~vscode/.ssh/environment`, seul canal par
/// lequel le `sshd` injecte (`UsePAM no`, donc pas de `pam_env`) transmet un
/// environnement aux commandes non interactives de `exec_in_workshop`.
///
/// Le format attendu par `sshd` est strictement `NOM=valeur`, sans guillemets
/// autour de la valeur : `PATH="/usr/bin"` y deviendrait litteralement
/// `"/usr/bin"`, guillemets compris. Seuls les guillemets ENGLOBANTS sont
/// retires — `OPENCODE_CONFIG_CONTENT` est du JSON et porte les siens, qui
/// doivent survivre intacts.
async fn write_ssh_environment_file(rootfs_dir: &Path, environment: &str) -> Result<()> {
    let rendered = render_ssh_environment(environment);

    let ssh_dir = rootfs_dir.join("home/vscode/.ssh");
    tokio::fs::create_dir_all(&ssh_dir).await?;
    let path = ssh_dir.join("environment");
    tokio::fs::write(&path, rendered)
        .await
        .with_context(|| format!("ecriture de {path:?}"))?;
    Ok(())
}

/// Partie purement textuelle de [`write_ssh_environment_file`], isolee pour
/// etre testable : c'est exactement la transformation qui, ecrite en `sed`
/// dans un script shell genere, s'etait revelee fausse sans que rien ne le
/// signale.
fn render_ssh_environment(environment: &str) -> String {
    environment
        .lines()
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            if name.is_empty()
                || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                || name.starts_with(|c: char| c.is_ascii_digit())
            {
                return None;
            }
            let value = value
                .strip_prefix('"')
                .and_then(|v| v.strip_suffix('"'))
                .unwrap_or(value);
            Some(format!("{name}={value}\n"))
        })
        .collect()
}

/// Copie le binaire `opencode` (baque dans CETTE image `atelier-image-builder`,
/// voir `Dockerfile`) dans le rootfs du devcontainer construit.
///
/// Decision (2026-09-01) : ne JAMAIS compter sur le devcontainer.json du
/// depot cible (ni sur une Feature, ni sur un `postCreateCommand`) pour
/// installer `opencode` — un telechargement depuis l'interieur de la
/// microVM builder passe par `net-proxy`, dont le tunnel CONNECT reste
/// bloque sans erreur sur un gros binaire externe
/// (`release-assets.githubusercontent.com`, plusieurs minutes sans
/// avancer, cause non identifiee — voir docs/architecture/pieges.md). CE
/// conteneur (`atelier-image-builder`), lui, a un acces reseau normal AU
/// MOMENT DU `docker build` (image Dockerfile, jamais au runtime) : le
/// binaire est deja present sur disque quand ce process tourne, aucun
/// reseau requis ici. Meme garantie que Claude Code, qui n'avait jamais
/// ete installe par atelier mais se trouvait deja, par chance, dans
/// l'image de base Microsoft — sauf qu'ici la presence est GARANTIE, plus
/// besoin de chance.
///
/// `ATELIER_OPENCODE_BIN` absent ou fichier introuvable : best-effort, ne
/// bloque pas le build (meme convention que le reste de ce module) — utile
/// en dev quand cette image n'a pas ete rebuild avec le binaire baque.
async fn inject_opencode_binary(rootfs_dir: &Path) -> Result<()> {
    let Ok(source_path) = std::env::var("ATELIER_OPENCODE_BIN") else {
        tracing::warn!(
            "ATELIER_OPENCODE_BIN absent, opencode ne sera pas injecte dans ce devcontainer"
        );
        return Ok(());
    };
    if !tokio::fs::try_exists(&source_path).await.unwrap_or(false) {
        tracing::warn!(
            source_path,
            "binaire opencode introuvable, injection ignoree"
        );
        return Ok(());
    }

    let dest_path = rootfs_dir.join("usr/local/bin/opencode");
    tokio::fs::copy(&source_path, &dest_path)
        .await
        .with_context(|| format!("copie de {source_path} vers {dest_path:?}"))?;

    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(&dest_path, std::fs::Permissions::from_mode(0o755))
        .await
        .with_context(|| format!("chmod +x de {dest_path:?}"))?;

    Ok(())
}

/// S'assure que `/workspaces` et le repertoire cible `/workspaces/<repo>`
/// existent et appartiennent a l'utilisateur non-root (typiquement `vscode` /
/// uid 1000) dans le rootfs final, pour que `ttyd` et `code-server`
/// demarrent directement dans le bon workspace sans dependre d'un `mkdir`
/// prealable.
/// Place dans le rootfs un vrai clone du depot cible, a la revision
/// demandee, plus le necessaire pour le rafraichir a chaque demarrage de la
/// microVM.
///
/// Jusqu'ici ce repertoire n'etait qu'un dossier vide avec un `.keep` :
/// `envbuilder` clone bien le depot pour CONSTRUIRE l'image, mais ce clone
/// vit dans la microVM "builder" et ne se retrouve pas dans l'image poussee.
/// L'agent bootait donc dans un workspace vide, sans code ni depot git — il
/// ne pouvait ni travailler sur les sources, ni commiter, ni pousser. Cause
/// racine des "PR vides" du PM (constate le 2026-08-30).
///
/// Le clone est fait ici, cote hote, et embarque dans le rootfs : la microVM
/// demarre donc avec les sources immediatement disponibles, sans dependre du
/// reseau au boot. Un service systemd (voir `inject_workspace_refresh`) le
/// remet ensuite a jour a chaque demarrage, pour qu'un Workshop repris
/// apres une longue veille ne reparte pas d'un etat perime.
async fn ensure_workspace_clone(
    rootfs_dir: &Path,
    source: &DevcontainerSource,
    credentials: Option<&GitCredentials>,
) -> Result<()> {
    let ws_dir = rootfs_dir.join("workspaces").join(workspace_name(source));
    tokio::fs::create_dir_all(&ws_dir).await?;

    // L'URL peut porter les identifiants d'un depot prive : jamais
    // journalisee, contrairement a `source.repo`.
    let clone_url = authenticated_url(&source.repo, credentials);
    let status = Command::new("git")
        .args([
            "clone",
            "--no-single-branch",
            &clone_url,
            &ws_dir.to_string_lossy(),
        ])
        // `net-proxy` tourne en sidecar du meme pod, donc dans le meme
        // netns : joignable en loopback, comme pour la microVM builder.
        //
        // Les MINUSCULES ne sont pas une redondance de confort : libcurl,
        // que `git` utilise, ignore deliberement `HTTP_PROXY` en majuscules
        // pour les URL `http://` (protection historique contre l'en-tete
        // CGI `Proxy:`) et ne lit que `http_proxy`. Avec les seules
        // majuscules, `git clone` court-circuitait donc le proxy et tentait
        // de resoudre `git.atelier.internal` lui-meme — un nom qui n'existe
        // que dans `net-proxy` : `Could not resolve host`, clone echoue,
        // workspace livre vide, et l'agent bootait sans code ni depot git.
        // Constate en run PM reel le 2026-09-02.
        .env("HTTP_PROXY", net_proxy_local_url())
        .env("HTTPS_PROXY", net_proxy_local_url())
        .env("http_proxy", net_proxy_local_url())
        .env("https_proxy", net_proxy_local_url())
        .status()
        .await
        .context("lancement de `git clone`")?;

    if !status.success() {
        // Non bloquant : un depot injoignable ou prive sans identifiants ne
        // doit pas faire echouer tout le build d'image. Le workspace reste
        // alors vide, et le service de rafraichissement retentera au boot.
        tracing::warn!(
            repo = %source.repo,
            "clone du depot dans le workspace echoue, workspace laisse vide"
        );
    } else {
        let revision = source.revision.trim();
        if !revision.is_empty() && revision != "HEAD" {
            let checkout = Command::new("git")
                .args(["-C", &ws_dir.to_string_lossy(), "checkout", revision])
                .status()
                .await
                .context("lancement de `git checkout`")?;
            if !checkout.success() {
                tracing::warn!(%revision, "revision introuvable, clone laisse sur la branche par defaut");
            }
        }
        // Identite git par defaut du depot clone : sans elle, `git commit`
        // echoue ("Please tell me who you are") pour tout agent qui essaie
        // de commiter son travail dans la microVM. Locale au depot (pas
        // `--global`), donc triviale a surcharger par l'utilisateur ou par
        // un agent qui connait une meilleure identite.
        for (key, value) in [
            ("user.name", "Atelier Workshop"),
            ("user.email", "workshop@atelier.local"),
        ] {
            let _ = Command::new("git")
                .args(["-C", &ws_dir.to_string_lossy(), "config", key, value])
                .status()
                .await;
        }
        tracing::info!(repo = %source.repo, revision = %source.revision, "depot clone dans le workspace");
    }

    // Un `.keep` reste utile quand le clone a echoue : sans lui, `mke2fs -d`
    // ne materialise pas un repertoire vide.
    if !ws_dir.join(".git").exists() {
        tokio::fs::write(ws_dir.join(".keep"), "atelier workspace\n")
            .await
            .ok();
    }

    chown_recursive(&rootfs_dir.join("workspaces"));
    Ok(())
}

/// Installe dans le rootfs un service systemd qui remet le clone du
/// workspace a jour a chaque demarrage de la microVM.
///
/// Le clone embarque dans l'image (voir `ensure_workspace_clone`) fige les
/// sources a l'instant du build. Un Workshop repris apres plusieurs jours de
/// veille, ou dont la branche a avance entre-temps, repartirait sinon d'un
/// etat perime. Ce service rattrape l'ecart au boot.
///
/// Best-effort par conception (`|| true`, jamais `Restart=`) : sans reseau,
/// sans identifiants pour un depot prive, ou sur un depot dont la branche a
/// disparu, la microVM doit demarrer quand meme avec les sources telles
/// qu'elles etaient au build. Un workspace legerement en retard vaut mieux
/// qu'un Workshop qui ne demarre pas.
///
/// `git reset --hard` et non `pull` : le travail local d'un agent n'a pas a
/// survivre a un redemarrage — il est cense avoir ete commite et pousse.
/// Les modifications non commitees sont donc volontairement ecrasees, ce que
/// le message du service annonce explicitement.
async fn inject_workspace_refresh(rootfs_dir: &Path, source: &DevcontainerSource) -> Result<()> {
    let ws_path = format!("/workspaces/{}", workspace_name(source));
    let revision = {
        let r = source.revision.trim();
        if r.is_empty() || r == "HEAD" {
            "HEAD".to_string()
        } else {
            r.to_string()
        }
    };

    let script = format!(
        r#"#!/usr/bin/env bash
# Remet le clone du workspace a jour au demarrage de la microVM (installe
# par atelier-image-builder). Best-effort : ne fait jamais echouer le boot.
set -u
WS="{ws_path}"
REV="{revision}"

[ -d "$WS/.git" ] || {{ echo "atelier: $WS n'est pas un depot git, rien a rafraichir" >&2; exit 0; }}
cd "$WS" || exit 0

# `git fetch` sort en erreur sans reseau ni identifiants : c'est un cas
# normal (Workshop hors ligne, depot prive), on garde alors les sources
# embarquees dans l'image.
if ! git fetch --prune origin 2>/dev/null; then
    echo "atelier: fetch impossible, sources du build conservees" >&2
    exit 0
fi

if [ "$REV" = "HEAD" ]; then
    TARGET="origin/$(git symbolic-ref --short HEAD 2>/dev/null || echo main)"
else
    TARGET="origin/$REV"
fi

if git rev-parse --verify --quiet "$TARGET" >/dev/null; then
    echo "atelier: mise a jour du workspace sur $TARGET (les modifications non commitees sont ecrasees)" >&2
    git reset --hard "$TARGET" >/dev/null 2>&1 || true
else
    echo "atelier: $TARGET introuvable, sources du build conservees" >&2
fi
exit 0
"#
    );

    let bin_dir = rootfs_dir.join("usr/local/bin");
    tokio::fs::create_dir_all(&bin_dir).await?;
    let script_path = bin_dir.join("atelier-refresh-workspace.sh");
    tokio::fs::write(&script_path, script).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).await?;
    }

    // `User=vscode` : le clone appartient a l'uid 1000, un `git` lance en
    // root y refuserait de travailler ("dubious ownership") et laisserait
    // en plus des fichiers root dans le workspace.
    let unit = "[Unit]\nDescription=Rafraichit le clone du workspace (atelier)\nAfter=network.target\n\n[Service]\nType=oneshot\nRemainAfterExit=yes\nUser=vscode\nGroup=vscode\nExecStart=/usr/local/bin/atelier-refresh-workspace.sh\n\n[Install]\nWantedBy=multi-user.target\n";

    let unit_dir = rootfs_dir.join("etc/systemd/system");
    tokio::fs::create_dir_all(&unit_dir).await?;
    tokio::fs::write(unit_dir.join("atelier-refresh-workspace.service"), unit).await?;

    // `systemctl enable` est inoperant sur un rootfs hors ligne (et l'image
    // de base intercepte parfois `systemctl`) : on cree le lien du
    // `multi-user.target.wants` a la main, meme methode que le devcontainer
    // de demo pour ses propres unites.
    let wants_dir = unit_dir.join("multi-user.target.wants");
    tokio::fs::create_dir_all(&wants_dir).await?;
    let link = wants_dir.join("atelier-refresh-workspace.service");
    if !link.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            "/etc/systemd/system/atelier-refresh-workspace.service",
            &link,
        )
        .ok();
    }
    Ok(())
}

/// Installe `ttyd` (terminal web) et `code-server` (IDE web) — jusqu'ici
/// fournis UNIQUEMENT par le devcontainer de demo externe
/// (`github.com/PhilippeVienne/atelier-workspace`), jamais par
/// `image-builder`. Sans ca, un Workshop sur n'importe quel autre depot
/// cible ne repond jamais sur `ttyd:7681` (sonde de readiness, voir
/// `crates/controller/src/reconcile.rs::GUEST_TERMINAL_PORT`) et reste
/// bloque hors `Running` — ni terminal, ni IDE. Meme mecanisme que
/// `inject_opencode_binary` : binaires deja baques dans CETTE image
/// (`Dockerfile`), simple copie, aucun reseau requis dans la microVM.
///
/// Mot de passe recupere via `GET http://169.254.0.1:3132/session-auth`
/// (503 tant que le controller ne l'a pas provisionne — contrat documente
/// dans `crates/net-proxy/src/metadata.rs`) : `ttyd --credential` (Basic
/// Auth reel) pour le terminal ; `PASSWORD=... code-server --auth
/// password` pour l'IDE — `code-server` IGNORE le Basic Auth (mesure le
/// 2026-09-01, voir docs/architecture/pieges.md), c'est sa propre variable
/// d'environnement qui compte.
async fn inject_terminal_and_ide(rootfs_dir: &Path, source: &DevcontainerSource) -> Result<()> {
    let (Ok(ttyd_bin), Ok(code_server_dir)) = (
        std::env::var("ATELIER_TTYD_BIN"),
        std::env::var("ATELIER_CODE_SERVER_DIR"),
    ) else {
        tracing::warn!(
            "ATELIER_TTYD_BIN/ATELIER_CODE_SERVER_DIR absents, terminal et IDE web non installes"
        );
        return Ok(());
    };

    // `User=vscode` est deja suppose par `inject_workspace_refresh`
    // ci-dessus, et par les deux unites installees plus bas — mais rien ne
    // le garantissait sur une image de base qui n'est pas de la famille
    // `mcr.microsoft.com/devcontainers/*`. `systemd` refuserait sinon de
    // demarrer ces unites (utilisateur inconnu de `/etc/passwd`), en
    // silence.
    ensure_vscode_user(rootfs_dir).await?;

    let bin_dir = rootfs_dir.join("usr/local/bin");
    tokio::fs::create_dir_all(&bin_dir).await?;

    let ttyd_dest = bin_dir.join("ttyd");
    tokio::fs::copy(&ttyd_bin, &ttyd_dest)
        .await
        .with_context(|| format!("copie de {ttyd_bin} vers {ttyd_dest:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&ttyd_dest, std::fs::Permissions::from_mode(0o755)).await?;
    }

    let code_server_dest = rootfs_dir.join("opt/atelier-code-server");
    let status = Command::new("cp")
        .args(["-a", &code_server_dir, &code_server_dest.to_string_lossy()])
        .status()
        .await
        .context("copie de code-server")?;
    ensure!(status.success(), "cp -a code-server a echoue");
    chown_recursive(&code_server_dest);

    let ws_path = format!("/workspaces/{}", workspace_name(source));

    // Boucle de retry : le secret `session_auth` (OpenBao) n'est
    // provisionne par le controller qu'apres la creation du pod parent,
    // pas garanti pret au premier `systemctl start` — `net-proxy` renvoie
    // `503` jusque-la (meme convention que `atelier-fetch-session-auth.sh`
    // du devcontainer de demo).
    let fetch_password = "PASSWORD=\"\"\nfor i in $(seq 1 60); do\n    PASSWORD=$(curl -fsS http://169.254.0.1:3132/session-auth 2>/dev/null) && [ -n \"$PASSWORD\" ] && break\n    sleep 2\ndone\n";

    let ttyd_script = format!(
        "#!/usr/bin/env bash\nset -u\n{fetch_password}\nexec /usr/local/bin/ttyd --writable --credential \"atelier:$PASSWORD\" -p 7681 bash\n"
    );
    write_executable(&bin_dir.join("atelier-start-ttyd.sh"), &ttyd_script).await?;

    let code_server_script = format!(
        "#!/usr/bin/env bash\nset -u\n{fetch_password}\nexport PASSWORD\nexec /opt/atelier-code-server/bin/code-server --auth password --bind-addr 0.0.0.0:8080 {ws_path}\n"
    );
    write_executable(
        &bin_dir.join("atelier-start-code-server.sh"),
        &code_server_script,
    )
    .await?;

    install_and_enable_unit(
        rootfs_dir,
        "atelier-terminal.service",
        "[Unit]\nDescription=Terminal web atelier (ttyd)\nAfter=network.target\n\n\
         [Service]\nType=simple\nRestart=on-failure\nRestartSec=2\nUser=vscode\nGroup=vscode\n\
         ExecStart=/usr/local/bin/atelier-start-ttyd.sh\n\n[Install]\nWantedBy=multi-user.target\n",
    )
    .await?;

    install_and_enable_unit(
        rootfs_dir,
        "atelier-code-server.service",
        "[Unit]\nDescription=IDE web atelier (code-server)\nAfter=network.target\n\n\
         [Service]\nType=simple\nRestart=on-failure\nRestartSec=2\nUser=vscode\nGroup=vscode\n\
         ExecStart=/usr/local/bin/atelier-start-code-server.sh\n\n[Install]\nWantedBy=multi-user.target\n",
    )
    .await?;

    Ok(())
}

/// `vscode`/uid 1000 est une convention des images `mcr.microsoft.com/
/// devcontainers/*` (et du devcontainer de demo), jamais garantie par la
/// spec devcontainer elle-meme. Ecrit directement les entrees minimales
/// dans le rootfs plutot que d'invoquer `useradd` (qui n'existe pas sur
/// toutes les images de base, et qu'on ne peut de toute facon pas executer
/// DANS un rootfs etranger sans y chrooter).
async fn ensure_vscode_user(rootfs_dir: &Path) -> Result<()> {
    ensure_system_user(
        rootfs_dir,
        "vscode",
        1000,
        1000,
        "/home/vscode",
        "/bin/bash",
    )
    .await
}

/// `sshd` (OpenSSH >= 7.5, y compris la version Debian bookworm embarquee
/// ici) exige un compte systeme dedie pour sa separation de privileges
/// MEME AVEC `UsePrivilegeSeparation no` dans la config — cette directive
/// est desormais un no-op silencieux (`Deprecated option
/// UsePrivilegeSeparation`, constate en pratique le 2026-09-01), pas un
/// vrai interrupteur. Sans ce compte, `sshd -t` echoue immediatement avec
/// `Privilege separation user sshd does not exist`, avant meme d'ecouter
/// sur un port.
async fn ensure_sshd_user(rootfs_dir: &Path) -> Result<()> {
    // `/nonexistent`, PAS `/run/sshd` : ce dernier est le repertoire de
    // separation de privileges de `sshd` lui-meme, qui EXIGE qu'il
    // appartienne a root et ne soit ni group- ni world-writable — le
    // confondre avec le "home" de l'utilisateur `sshd` le fait chown vers
    // 101:101, que `sshd -t` refuse alors de demarrer (constate en
    // pratique). `/run/sshd` est cree par le script de demarrage
    // (`atelier-start-sshd.sh`, execute en root avant l'abandon de
    // privileges), jamais ici.
    ensure_system_user(
        rootfs_dir,
        "sshd",
        101,
        101,
        "/nonexistent",
        "/usr/sbin/nologin",
    )
    .await
}

/// Cree un utilisateur/groupe directement dans `/etc/passwd`,
/// `/etc/group`, `/etc/shadow` du rootfs, si absent — sans invoquer
/// `useradd` (indisponible sur toutes les images de base, et de toute
/// facon inexecutable DANS un rootfs etranger sans y chrooter).
async fn ensure_system_user(
    rootfs_dir: &Path,
    name: &str,
    uid: u32,
    gid: u32,
    home: &str,
    shell: &str,
) -> Result<()> {
    let has_entry = |content: &str| content.lines().any(|l| l.split(':').next() == Some(name));

    let passwd_path = rootfs_dir.join("etc/passwd");
    let mut passwd = tokio::fs::read_to_string(&passwd_path)
        .await
        .unwrap_or_default();
    let already_existed = has_entry(&passwd);

    if !already_existed {
        tracing::warn!(
            name,
            "utilisateur absent de l'image de base, creation directe dans le rootfs"
        );
        if !passwd.is_empty() && !passwd.ends_with('\n') {
            passwd.push('\n');
        }
        passwd.push_str(&format!("{name}:x:{uid}:{gid}:{name}:{home}:{shell}\n"));
        tokio::fs::write(&passwd_path, passwd)
            .await
            .with_context(|| format!("ecriture de {passwd_path:?}"))?;

        let group_path = rootfs_dir.join("etc/group");
        let mut group = tokio::fs::read_to_string(&group_path)
            .await
            .unwrap_or_default();
        if !has_entry(&group) {
            if !group.is_empty() && !group.ends_with('\n') {
                group.push('\n');
            }
            group.push_str(&format!("{name}:x:{gid}:\n"));
            tokio::fs::write(&group_path, group)
                .await
                .with_context(|| format!("ecriture de {group_path:?}"))?;
        }

        let home_dir = rootfs_dir.join(home.trim_start_matches('/'));
        tokio::fs::create_dir_all(&home_dir).await.ok();
        let _ = Command::new("chown")
            .args([
                format!("{uid}:{gid}"),
                home_dir.to_string_lossy().into_owned(),
            ])
            .status()
            .await;
    }

    // Applique QUE le compte vienne d'etre cree ou existait deja dans
    // l'image de base (ex: `vscode` sur `mcr.microsoft.com/devcontainers/*`) :
    // ce dernier cas porte aussi frequemment un `!` en `/etc/shadow`, et
    // `sshd` le traite comme un compte ADMINISTRATIVEMENT VERROUILLE
    // ("User vscode not allowed because account is locked", constate en
    // pratique le 2026-09-01) — un blocage qui s'applique a TOUTE methode
    // d'authentification, y compris par cle publique, pas seulement au mot
    // de passe. `*` reste "aucun mot de passe ne peut authentifier ce
    // compte" sans declencher ce verrou.
    unlock_shadow_password(rootfs_dir, name).await;
    Ok(())
}

/// Reecrit le champ mot de passe de `/etc/shadow` pour `name` de `!` vers
/// `*` (voir `ensure_system_user`), ou ajoute une entree `*` si absente.
/// Best-effort silencieux : un `/etc/shadow` illisible ou absent ne doit
/// jamais faire echouer le build.
async fn unlock_shadow_password(rootfs_dir: &Path, name: &str) {
    let shadow_path = rootfs_dir.join("etc/shadow");
    let Ok(shadow) = tokio::fs::read_to_string(&shadow_path).await else {
        return;
    };
    let prefix = format!("{name}:");
    let mut changed = false;
    let mut lines: Vec<String> = shadow
        .lines()
        .map(|line| {
            if let Some(rest) = line.strip_prefix(&prefix) {
                if let Some(rest_after_field) = rest.strip_prefix('!') {
                    changed = true;
                    return format!("{prefix}*{rest_after_field}");
                }
            }
            line.to_string()
        })
        .collect();
    if !changed {
        if lines.iter().any(|l| l.starts_with(&prefix)) {
            return;
        }
        lines.push(format!("{name}:*:19000:0:99999:7:::"));
    }
    let mut new_shadow = lines.join("\n");
    new_shadow.push('\n');
    let _ = tokio::fs::write(&shadow_path, new_shadow).await;
}

async fn write_executable(path: &Path, content: &str) -> Result<()> {
    tokio::fs::write(path, content)
        .await
        .with_context(|| format!("ecriture de {path:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).await?;
    }
    Ok(())
}

/// Ecrit une unite systemd et l'active a la main (symlink dans
/// `multi-user.target.wants`) : `systemctl enable` est inoperant sur un
/// rootfs hors ligne, meme methode que `inject_workspace_refresh` et le
/// devcontainer de demo pour leurs propres unites.
async fn install_and_enable_unit(rootfs_dir: &Path, name: &str, unit_content: &str) -> Result<()> {
    let unit_dir = rootfs_dir.join("etc/systemd/system");
    tokio::fs::create_dir_all(&unit_dir).await?;
    tokio::fs::write(unit_dir.join(name), unit_content).await?;

    let wants_dir = unit_dir.join("multi-user.target.wants");
    tokio::fs::create_dir_all(&wants_dir).await?;
    let link = wants_dir.join(name);
    if !link.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(format!("/etc/systemd/system/{name}"), &link).ok();
    }
    Ok(())
}

/// Installe `sshd` — meme raisonnement que `inject_terminal_and_ide` : ce
/// n'etait jusqu'ici fourni QUE par le devcontainer de demo externe.
///
/// `sshd` (contrairement a `ttyd`/`code-server`) n'a pas de distribution
/// statique officielle : le `Dockerfile` l'installe via apt dans l'image
/// `atelier-image-builder` (meme famille glibc que la grande majorite des
/// devcontainers reels) puis l'embarque avec ses bibliotheques dynamiques
/// resolues par `ldd` — execute ici via son propre interprete
/// (`ld.so --library-path ...`), sans jamais toucher au `ld.so.cache` de
/// l'image CIBLE ni supposer que ses bibliotheques systeme sont compatibles.
/// `UsePAM no` dans la config generee ci-dessous evite d'avoir a embarquer
/// toute la pile PAM (modules charges par `dlopen`, jamais vus par `ldd`).
async fn inject_sshd(rootfs_dir: &Path) -> Result<()> {
    let Ok(sshd_dir) = std::env::var("ATELIER_SSHD_DIR") else {
        tracing::warn!("ATELIER_SSHD_DIR absent, sshd non installe");
        return Ok(());
    };

    let dest = rootfs_dir.join("opt/atelier-sshd");
    let status = Command::new("cp")
        .args(["-a", &sshd_dir, &dest.to_string_lossy()])
        .status()
        .await
        .context("copie de sshd")?;
    ensure!(status.success(), "cp -a sshd a echoue");

    // Voir `ensure_sshd_user` : indispensable, `UsePrivilegeSeparation no`
    // ne suffit plus sur les versions recentes d'OpenSSH.
    ensure_sshd_user(rootfs_dir).await?;

    let sshd_config = "Port 2222\n\
         HostKey /etc/atelier-sshd/ssh_host_rsa_key\n\
         HostKey /etc/atelier-sshd/ssh_host_ed25519_key\n\
         PermitRootLogin no\n\
         PasswordAuthentication no\n\
         PubkeyAuthentication yes\n\
         UsePAM no\n\
         AuthorizedKeysFile /home/vscode/.ssh/authorized_keys\n\
         PermitUserEnvironment yes\n\
         PidFile /run/atelier-sshd.pid\n";
    tokio::fs::create_dir_all(rootfs_dir.join("etc/atelier-sshd")).await?;
    tokio::fs::write(rootfs_dir.join("etc/atelier-sshd/sshd_config"), sshd_config).await?;

    let bin_dir = rootfs_dir.join("usr/local/bin");
    tokio::fs::create_dir_all(&bin_dir).await?;

    // Boucle de retry : meme contrat que `atelier-start-ttyd.sh`, la cle
    // publique (`ssh_authorized_key`, OpenBao) n'est pas garantie prete au
    // premier demarrage — voir `crates/net-proxy/src/metadata.rs`.
    //
    // `~/.ssh/environment` (+ `PermitUserEnvironment yes`) est ce qui rend
    // `/etc/environment` visible a une commande lancee par `exec_in_workshop`.
    // Sans PAM — et `UsePAM no` est deliberé ici, pour ne pas avoir a
    // embarquer toute la pile PAM — c'est `pam_env` qui manque, et lui seul
    // lit `/etc/environment`. Une session SSH non interactive n'ouvre par
    // ailleurs ni `/etc/profile` ni `~/.bashrc`. L'agent demarrait donc SANS
    // `OPENCODE_CONFIG_CONTENT` ni `ATELIER_LLM_PROXY_AUTH_TOKEN` :
    // `opencode` ne connaissait aucun fournisseur et mourait sur un
    // laconique `Unexpected server error`. Le devcontainer de demo n'avait
    // jamais montre le probleme puisqu'il utilise le `sshd` systeme, avec
    // PAM. Le `sed` recopie les paires `CLE=valeur` en retirant les
    // guillemets, que le format de `~/.ssh/environment` n'accepte pas.
    // `PermitUserEnvironment` elargit en principe la surface d'attaque (un
    // utilisateur pouvant ecrire ce fichier peut injecter `LD_PRELOAD`) :
    // sans objet ici, ou le compte EST l'agent et la microVM est jetable.
    // Le fichier lui-meme est ecrit par `write_ssh_environment_file`, au
    // moment ou l'on compose deja `/etc/environment` — le deriver ici par un
    // `sed` dans le script obligeait a echapper une expression reguliere a
    // travers une chaine Rust ET un script shell, ce qui a effectivement
    // produit un `\\(` la ou il fallait `\(` : fichier vide, agent sans
    // configuration, et aucun message d'erreur.
    // `sshd` se re-execute lui-meme (`execve`) pour chaque connexion
    // entrante, en repartant du chemin binaire brut — pas via notre wrapper
    // `ld.so --library-path` — et ce re-exec echoue alors a charger ses
    // bibliotheques embarquees (`libwrap.so.0: cannot open shared object
    // file`, constate en pratique le 2026-09-01). `LD_LIBRARY_PATH`, en
    // revanche, est une variable d'environnement normale : elle survit au
    // re-exec et s'applique donc aussi au process enfant.
    //
    // `-r` (ne pas se re-executer a chaque connexion) est ce qui rend ce
    // montage viable. Par defaut, `sshd` relance son propre binaire par
    // connexion entrante : le re-exec repart du chemin brut, donc avec
    // l'interpreteur ELF de l'image CIBLE, tout en heritant d'un
    // `LD_LIBRARY_PATH` qui pointe vers NOS bibliotheques bookworm. Editeur
    // de liens recent et glibc plus ancienne ne s'accordent pas : le
    // processus meurt aussitot et la connexion est coupee avant meme
    // l'echange de versions (`kex_exchange_identification: Connection closed
    // by remote host`), alors que `sshd` annonce paisiblement
    // `Server listening on 0.0.0.0 port 2222`. Symptome trompeur s'il en
    // est : le port repond, donc tout semble en place.
    //
    // `LD_LIBRARY_PATH` est pose UNIQUEMENT sur `sshd` et `ssh-keygen`, via `env`, et
    // surtout jamais `export`ee pour tout le script : nos bibliotheques
    // viennent de bookworm, alors que l'image CIBLE peut etre bien plus
    // recente. Un `export` global faisait charger notre glibc a toutes les
    // commandes suivantes — `mkdir`, `chmod`, `chown`, `seq`, `curl` — qui
    // mouraient toutes en `stack smashing detected` /
    // `version GLIBC_2.38 not found`. Le script n'allait alors pas plus loin
    // que sa premiere ligne utile : ni `~/.ssh`, ni cles d'hote, et un
    // `sshd` relance en boucle par `guest-init` qui coupait chaque
    // connexion (`Disconnected`). Constate en Workshop reel le 2026-09-02
    // sur une image `devcontainers/javascript-node:20`.
    let script = "#!/usr/bin/env bash\n\
         set -u\n\
         SSHD=/opt/atelier-sshd/bin/sshd\n\
         SSHKEYGEN=/opt/atelier-sshd/bin/ssh-keygen\n\
         RUN=\"env LD_LIBRARY_PATH=/opt/atelier-sshd/lib \
             /opt/atelier-sshd/bin/ld.so --library-path /opt/atelier-sshd/lib\"\n\
         \n\
         mkdir -p /home/vscode/.ssh /run/sshd\n\
         PUBKEY=\"\"\n\
         for i in $(seq 1 60); do\n    \
             PUBKEY=$(curl -fsS http://169.254.0.1:3132/ssh-authorized-key 2>/dev/null) && [ -n \"$PUBKEY\" ] && break\n    \
             sleep 2\n\
         done\n\
         echo \"$PUBKEY\" > /home/vscode/.ssh/authorized_keys\n\
         touch /home/vscode/.ssh/environment\n\
         chmod 700 /home/vscode/.ssh\n\
         chmod 600 /home/vscode/.ssh/authorized_keys /home/vscode/.ssh/environment\n\
         chown -R vscode:vscode /home/vscode/.ssh\n\
         \n\
         [ -f /etc/atelier-sshd/ssh_host_rsa_key ] || $RUN \"$SSHKEYGEN\" -q -t rsa -f /etc/atelier-sshd/ssh_host_rsa_key -N \"\"\n\
         [ -f /etc/atelier-sshd/ssh_host_ed25519_key ] || $RUN \"$SSHKEYGEN\" -q -t ed25519 -f /etc/atelier-sshd/ssh_host_ed25519_key -N \"\"\n\
         \n\
         exec $RUN \"$SSHD\" -D -e -r -f /etc/atelier-sshd/sshd_config\n";
    write_executable(&bin_dir.join("atelier-start-sshd.sh"), script).await?;

    // `sshd` gere lui-meme la bascule vers l'utilisateur authentifie
    // (`vscode`, seul compte cree par `ensure_vscode_user`) via la
    // separation de privileges — contrairement a `ttyd`/`code-server`, cette
    // unite tourne donc en root.
    install_and_enable_unit(
        rootfs_dir,
        "atelier-sshd.service",
        "[Unit]\nDescription=sshd atelier\nAfter=network.target\n\n\
         [Service]\nType=simple\nRestart=on-failure\nRestartSec=2\n\
         ExecStart=/usr/local/bin/atelier-start-sshd.sh\n\n[Install]\nWantedBy=multi-user.target\n",
    )
    .await?;

    Ok(())
}

/// `image-builder` suppose partout ailleurs (`inject_workspace_refresh`,
/// `inject_terminal_and_ide`, `inject_sshd`) que l'image de base demarre
/// `systemd` en PID 1 pour activer les unites injectees — vrai pour la
/// famille `mcr.microsoft.com/devcontainers/*` et le devcontainer de demo,
/// jamais garanti par la spec devcontainer elle-meme. Si `systemd` est
/// absent, ces unites ne seraient JAMAIS executees (silencieusement) :
/// bascule alors `/sbin/init` vers `atelier-guest-init`
/// (`crates/guest-init`), qui lance les memes scripts directement.
///
/// Le reseau n'a pas besoin d'etre reconfigure dans ce cas : c'est le
/// noyau qui le fait au boot (`ip=`, voir `crates/vm-supervisor`), pas
/// l'init — systemd ou non.
async fn ensure_init_system(rootfs_dir: &Path) -> Result<()> {
    let has_systemd = tokio::fs::try_exists(rootfs_dir.join("lib/systemd/systemd"))
        .await
        .unwrap_or(false)
        || tokio::fs::try_exists(rootfs_dir.join("usr/lib/systemd/systemd"))
            .await
            .unwrap_or(false);
    if has_systemd {
        return Ok(());
    }

    let Ok(init_bin) = std::env::var("ATELIER_GUEST_INIT_BIN") else {
        tracing::warn!(
            "systemd absent de l'image de base et ATELIER_GUEST_INIT_BIN non defini : \
             aucun service atelier (terminal, IDE, sshd, rafraichissement du workspace) \
             ne demarrera dans ce Workshop"
        );
        return Ok(());
    };
    tracing::warn!(
        "systemd absent de l'image de base, installation d'un init minimal (atelier-guest-init)"
    );

    let dest = rootfs_dir.join("sbin/init");
    tokio::fs::create_dir_all(rootfs_dir.join("sbin")).await?;
    // Peut deja exister (busybox init, symlink...) : sans effet ici de
    // toute facon puisque systemd est absent, donc rien qui l'active.
    tokio::fs::remove_file(&dest).await.ok();
    tokio::fs::copy(&init_bin, &dest)
        .await
        .with_context(|| format!("copie de {init_bin} vers {dest:?}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755)).await?;
    }
    Ok(())
}

/// Nom du repertoire de travail dans `/workspaces`, derive du depot.
fn workspace_name(source: &DevcontainerSource) -> String {
    source
        .repo
        .rsplit('/')
        .next()
        .unwrap_or("workspace")
        .trim_end_matches(".git")
        .to_string()
}

fn net_proxy_local_url() -> String {
    let port =
        std::env::var("ATELIER_BUILDER_NET_PROXY_PORT").unwrap_or_else(|_| "3128".to_string());
    format!("http://127.0.0.1:{port}")
}

/// Insere les identifiants dans l'URL quand le depot en exige — forme
/// acceptee par git pour HTTP(S), et le seul moyen de les passer a un
/// `git clone` non interactif sans ecrire de fichier de credentials.
fn authenticated_url(repo: &str, credentials: Option<&GitCredentials>) -> String {
    let Some(creds) = credentials else {
        return repo.to_string();
    };
    match repo.split_once("://") {
        Some((scheme, rest)) if !rest.contains('@') => {
            format!("{scheme}://{}:{}@{rest}", creds.username, creds.password)
        }
        _ => repo.to_string(),
    }
}

fn chown_recursive(path: &Path) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("chown")
            .args(["-R", "1000:1000", &path.to_string_lossy()])
            .status();
    }
    #[cfg(not(unix))]
    let _ = path;
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

#[cfg(test)]
mod tests {
    use super::render_ssh_environment;

    #[test]
    fn les_guillemets_englobants_sautent_mais_pas_ceux_du_json() {
        // `PATH` est cite dans `/etc/environment` de toute image Debian ;
        // `sshd` prendrait les guillemets pour une partie de la valeur.
        // `OPENCODE_CONFIG_CONTENT`, lui, EST du JSON : ses guillemets
        // interieurs doivent traverser intacts, faute de quoi `opencode`
        // ne voit aucun fournisseur et meurt sur un `Unexpected server
        // error` sans autre explication.
        let rendered = render_ssh_environment(concat!(
            "PATH=\"/usr/local/bin:/usr/bin\"\n",
            "HTTP_PROXY=http://169.254.0.1:3128\n",
            "OPENCODE_CONFIG_CONTENT={\"provider\":{\"atelier\":{\"npm\":\"x\"}}}\n",
        ));

        assert_eq!(
            rendered,
            concat!(
                "PATH=/usr/local/bin:/usr/bin\n",
                "HTTP_PROXY=http://169.254.0.1:3128\n",
                "OPENCODE_CONFIG_CONTENT={\"provider\":{\"atelier\":{\"npm\":\"x\"}}}\n",
            )
        );
    }

    #[test]
    fn les_lignes_qui_ne_sont_pas_des_affectations_sont_ignorees() {
        // `sshd` refuse le fichier entier des la premiere ligne mal formee :
        // commentaires et lignes vides doivent disparaitre ici.
        let rendered =
            render_ssh_environment("# un commentaire\n\nDEBIAN_FRONTEND=noninteractive\n");
        assert_eq!(rendered, "DEBIAN_FRONTEND=noninteractive\n");
    }
}
