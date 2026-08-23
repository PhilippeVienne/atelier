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
//! `snapshot/create` et la restaurer via `snapshot/load` (voir
//! `crates/firecracker/src/vm.rs`, construit sur `fctools`), ce qui permet
//! de suspendre un Workshop (liberer le pod parent, ne garder que le
//! snapshot dans le cache) puis de le reprendre en quelques centaines de ms
//! sans rejouer le boot ni le setup du devcontainer.
//!
//! Canal de controle : petit serveur HTTP (`ATELIER_VM_CONTROL_ADDR`, pas de
//! vsock — vm-supervisor et le `controller` sont deux process dans deux pods
//! distincts, joignables via le reseau du cluster comme n'importe quel autre
//! service, contrairement au canal guest<->hote qui, lui, utilise `AF_VSOCK`,
//! voir `docs/ARCHITECTURE.md`). Le `controller` appelle `POST /snapshot`
//! avant de liberer le pod parent (mise en veille) ; ce process publie les
//! fichiers de snapshot dans `ATELIER_VM_SNAPSHOT_DIR` (sur le cache
//! partage, cf. `crates/controller/src/storage.rs`) puis s'arrete
//! proprement. Au demarrage suivant (reprise), si ce repertoire contient
//! deja un snapshot, il est charge via `Vm::restore_persisted` plutot que de
//! rebooter — voir ce constructeur pour le detail de comment un snapshot
//! survit a un redemarrage complet du process (l'API `fctools` de base ne
//! le permet pas directement).

use anyhow::Context;
use atelier_firecracker::network::{setup_link_local_tap, NetworkSetup};
use atelier_firecracker::vm::{Vm, VmConfig, VsockConfig};
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use axum::routing::post;
use axum::Router;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, Mutex};

type SnapshotResult = anyhow::Result<String>;

/// Demande envoyee par le serveur HTTP a la boucle principale (qui seule
/// possede la `Vm`) : un `mpsc` plutot qu'un `Arc<Mutex<Vm>>` partage,
/// puisque la reponse a un snapshot depend de l'arret complet de la VM qui
/// suit (la boucle principale doit reprendre la main derriere).
struct SnapshotRequest {
    respond_to: oneshot::Sender<SnapshotResult>,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-vm-supervisor");
    tracing::info!("atelier-vm-supervisor starting");

    let net_proxy_port: u16 = std::env::var("ATELIER_NET_PROXY_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3128);

    // TAP link-local, pas de NAT/route de sortie directe — net-proxy (dans
    // le meme pod, meme netns) est le seul voisin joignable, verrouille
    // ensuite au niveau paquet par `restrict_to_net_proxy` en defense en
    // profondeur de l'allowlist applicative. Voir
    // docs/architecture/network-security.md pour le detail complet.
    let network = setup_link_local_tap("atelier-vm", 0)
        .await
        .context("creation du TAP pour la microVM de l'agent (CAP_NET_ADMIN requis)")?;
    network
        .restrict_to_net_proxy(net_proxy_port)
        .await
        .context("pose des regles iptables de restriction du TAP")?;

    let base_boot_args = std::env::var("ATELIER_VM_BOOT_ARGS")
        .unwrap_or_else(|_| "console=ttyS0 reboot=k panic=1 pci=off".to_string());
    let boot_args = format!("{base_boot_args} {}", kernel_ip_boot_arg(&network));

    let config = VmConfig {
        firecracker_bin: env_path("ATELIER_FIRECRACKER_BIN", "firecracker"),
        jailer_bin: env_path("ATELIER_JAILER_BIN", "jailer"),
        snapshot_editor_bin: env_path("ATELIER_SNAPSHOT_EDITOR_BIN", "snapshot-editor"),
        chroot_base_dir: env_path("ATELIER_VM_CHROOT_BASE_DIR", "/srv/jailer"),
        jail_id: std::env::var("ATELIER_VM_JAIL_ID").unwrap_or_else(|_| "atelier-vm".to_string()),
        uid: env_u32("ATELIER_VM_UID", 0),
        gid: env_u32("ATELIER_VM_GID", 0),
        vcpu_count: std::env::var("ATELIER_VM_VCPU_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
        mem_mib: std::env::var("ATELIER_VM_MEM_MIB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256),
        boot_args,
        // Toujours active : sans emplacement partage avec `mcp-gateway`
        // (`ATELIER_VM_CHROOT_BASE_DIR` sur un volume commun), ce device
        // reste simplement inutilise, cout nul. `guest_cid` >= 3 requis
        // (0/1/2 reserves, 2 = l'hote).
        vsock: Some(VsockConfig {
            guest_cid: env_u32("ATELIER_VM_VSOCK_GUEST_CID", 3),
            uds_relative_path: format!(
                "/{}",
                std::env::var("ATELIER_VM_VSOCK_UDS_FILENAME")
                    .unwrap_or_else(|_| "vsock.sock".to_string())
            ),
        }),
    };
    let kernel_path = env_path("ATELIER_VM_KERNEL_PATH", "");
    let rootfs_path = env_path("ATELIER_VM_ROOTFS_PATH", "");
    anyhow::ensure!(
        !kernel_path.as_os_str().is_empty() && !rootfs_path.as_os_str().is_empty(),
        "ATELIER_VM_KERNEL_PATH et ATELIER_VM_ROOTFS_PATH sont requis"
    );

    // Repertoire (sur le cache partage) ou publier/lire les fichiers de
    // snapshot de CE Workshop. Vide/non fourni : jamais de restauration
    // possible, comportement degrade mais explicite plutot qu'un echec
    // silencieux (un Workshop qui n'a jamais ete suspendu n'a simplement pas
    // encore de snapshot a restaurer).
    let snapshot_dir = std::env::var("ATELIER_VM_SNAPSHOT_DIR")
        .ok()
        .map(PathBuf::from);
    let snapshot_state_path = snapshot_dir.as_ref().map(|d| d.join("snapshot.state"));
    let snapshot_mem_path = snapshot_dir.as_ref().map(|d| d.join("snapshot.mem"));

    let mut vm = match (&snapshot_state_path, &snapshot_mem_path) {
        (Some(state), Some(mem)) if state.exists() && mem.exists() => {
            tracing::info!(?state, ?mem, "restoring microVM from persisted snapshot");
            Vm::restore_persisted(
                &config,
                &kernel_path,
                &rootfs_path,
                Some(&network),
                state,
                mem,
            )
            .await
            .context("restauration de la microVM depuis un snapshot persiste")?
        }
        _ => {
            tracing::info!(?kernel_path, ?rootfs_path, "booting microVM");
            Vm::boot_with_network(&config, &kernel_path, &rootfs_path, &network).await?
        }
    };
    tracing::info!("microVM running");

    // TODO: recuperer kernel/rootfs depuis le cache content-addressed via
    //       status.image_digest plutot que des chemins fournis directement
    // TODO: relayer logs/metriques de la VM vers le control plane

    let (tx, mut rx) = mpsc::channel::<SnapshotRequest>(1);
    let control_addr =
        std::env::var("ATELIER_VM_CONTROL_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
    let control_state = Arc::new(Mutex::new(tx));
    let app = Router::new()
        .route("/snapshot", post(snapshot_handler))
        .with_state(control_state);
    let listener = tokio::net::TcpListener::bind(&control_addr)
        .await
        .with_context(|| format!("ecoute du serveur de controle sur {control_addr}"))?;
    tracing::info!(%control_addr, "control server listening");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(listener, app).await {
            tracing::error!(%err, "control server stopped unexpectedly");
        }
    });

    loop {
        tokio::select! {
            request = rx.recv() => {
                let Some(SnapshotRequest { respond_to }) = request else {
                    // Serveur HTTP arrete (ne devrait pas arriver avant la
                    // fin du process) : rien de plus a faire ici.
                    break;
                };
                let result = snapshot_and_publish(&mut vm, snapshot_dir.as_deref()).await;
                let succeeded = result.is_ok();
                let _ = respond_to.send(result);
                if succeeded {
                    tracing::info!("snapshot published, shutting down microVM for suspend");
                    let shutdown_result = vm.shutdown().await;
                    network.teardown().await;
                    shutdown_result?;
                    return Ok(());
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                match vm.is_running().await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::warn!("microVM en pause ou injoignable");
                        break;
                    }
                    Err(err) => {
                        tracing::error!(%err, "impossible d'interroger l'etat de la microVM");
                        break;
                    }
                }
            }
        }
    }

    let shutdown_result = vm.shutdown().await;
    network.teardown().await;
    shutdown_result
}

/// Parametre de boot noyau `ip=` (autoconfiguration IP standard du noyau
/// Linux, cf. `Documentation/admin-guide/nfs/nfsroot.rst`) : configure
/// l'interface et la route par defaut du guest **avant meme que son init ne
/// demarre**, sans cooperation necessaire de l'image (contrairement a
/// `atelier-builder-vm-init`, qui a son propre init personnalise,
/// `vm-supervisor` boote le devcontainer construit par `image-builder` tel
/// quel — son init n'est pas le notre).
fn kernel_ip_boot_arg(network: &NetworkSetup) -> String {
    let netmask = prefix_to_netmask(network.network_length);
    format!(
        "ip={}::{}:{netmask}::{}:off",
        network.guest_ip, network.host_ip, network.iface_id
    )
}

fn prefix_to_netmask(prefix_len: u8) -> std::net::Ipv4Addr {
    let mask: u32 = if prefix_len == 0 {
        0
    } else {
        u32::MAX << (32 - prefix_len)
    };
    std::net::Ipv4Addr::from(mask)
}

/// Prend un snapshot de la VM en cours et publie ses fichiers dans
/// `snapshot_dir` (le cache partage). Renvoie un digest sha256 des deux
/// fichiers concatenes — informatif (`WorkshopStatus.snapshot_digest`), pas
/// une cle de stockage content-addressed : contrairement au cache d'images
/// (partage entre Workshops, dedupe utile), un snapshot est intrinsequement
/// scope a un seul Workshop a un instant donne, stocke sous son propre
/// repertoire.
async fn snapshot_and_publish(vm: &mut Vm, snapshot_dir: Option<&Path>) -> SnapshotResult {
    let snapshot_dir = snapshot_dir.ok_or_else(|| {
        anyhow::anyhow!("ATELIER_VM_SNAPSHOT_DIR non configure, impossible de publier le snapshot")
    })?;
    tokio::fs::create_dir_all(snapshot_dir).await?;

    let snapshot = vm.snapshot().await?;

    let published_state = snapshot_dir.join("snapshot.state");
    let published_mem = snapshot_dir.join("snapshot.mem");
    // Fichiers temporaires puis rename atomique : un lecteur concurrent
    // (reprise en cours pendant qu'une autre suspend republie, ne devrait
    // pas arriver en pratique vu le cycle de vie d'un seul Workshop, mais
    // bon marche a se premunir) ne voit jamais un fichier partiellement
    // ecrit.
    let tmp_state = snapshot_dir.join("snapshot.state.tmp");
    let tmp_mem = snapshot_dir.join("snapshot.mem.tmp");
    tokio::fs::copy(&snapshot.snapshot_path, &tmp_state).await?;
    tokio::fs::copy(&snapshot.mem_file_path, &tmp_mem).await?;
    tokio::fs::rename(&tmp_state, &published_state).await?;
    tokio::fs::rename(&tmp_mem, &published_mem).await?;

    let mut hasher = Sha256::new();
    hasher.update(tokio::fs::read(&published_state).await?);
    hasher.update(tokio::fs::read(&published_mem).await?);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

async fn snapshot_handler(
    State(tx): State<Arc<Mutex<mpsc::Sender<SnapshotRequest>>>>,
) -> impl IntoResponse {
    let (respond_to, rx) = oneshot::channel();
    let send_result = tx.lock().await.send(SnapshotRequest { respond_to }).await;
    if send_result.is_err() {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "vm-supervisor is shutting down".to_string(),
        )
            .into_response();
    }
    match rx.await {
        Ok(Ok(digest)) => (
            StatusCode::OK,
            Json(serde_json::json!({ "snapshotDigest": digest })),
        )
            .into_response(),
        Ok(Err(err)) => (StatusCode::INTERNAL_SERVER_ERROR, format!("{err:#}")).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "reponse perdue (process en cours d'arret)".to_string(),
        )
            .into_response(),
    }
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var)
        .unwrap_or_else(|_| default.to_string())
        .into()
}

fn env_u32(var: &str, default: u32) -> u32 {
    std::env::var(var)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
