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
    /// Confinement de securite (tache 4.2.4) plutot qu'une mise en veille
    /// ordinaire.
    ///
    /// Deux differences, toutes deux voulues : l'egress du guest est GELE
    /// avant le snapshot (sans quoi une exfiltration en cours continuerait
    /// pendant qu'on l'archive), et la microVM n'est PAS eteinte ensuite —
    /// une mise en veille libere le pod, un confinement doit laisser
    /// l'incident analysable.
    lockdown: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-vm-supervisor");
    tracing::info!("atelier-vm-supervisor starting");

    let net_proxy_port: u16 = std::env::var("ATELIER_NET_PROXY_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3128);
    let net_proxy_transparent_http_port: u16 =
        std::env::var("ATELIER_NET_PROXY_TRANSPARENT_HTTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3180);
    let net_proxy_transparent_tls_port: u16 =
        std::env::var("ATELIER_NET_PROXY_TRANSPARENT_TLS_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3181);
    // Serveur metadata de `net-proxy` (`DEFAULT_METADATA_ADDR`,
    // crates/net-proxy/src/main.rs) : c'est par la que le guest recupere son
    // mot de passe de session (ttyd/code-server) et sa cle publique SSH
    // autorisee, au boot, via `atelier-fetch-session-auth.sh` /
    // `atelier-fetch-ssh-authorized-key.sh` (depot atelier-workspace).
    //
    // Bug reel trouve en testant (2026-08-30, premier vrai Workshop
    // Firecracker de ce depot) : ce port n'etait PAS transmis a
    // `enable_transparent_gateway`, donc jamais accepte par la chaine
    // dediee, dont la derniere regle est un `DROP` — tout le trafic
    // guest -> 169.254.0.1:3132 etait silencieusement jete. Les deux
    // scripts du guest epuisaient alors systematiquement leur budget de
    // retry puis basculaient sur leur repli (mot de passe aleatoire / pas
    // d'`authorized_keys`), rendant ttyd/code-server (401) et sshd
    // (`Permission denied (publickey)`) definitivement inaccessibles,
    // alors meme que net-proxy servait deja la bonne valeur et que le
    // Workshop atteignait `Running`. `DROP` (et non `REJECT`) rendait le
    // symptome trompeur : chaque tentative expirait sur le timeout de
    // `curl` au lieu d'echouer immediatement, ce qui ressemblait a une
    // lenteur de boot plutot qu'a un blocage franc.
    let net_proxy_metadata_port: u16 = std::env::var("ATELIER_NET_PROXY_METADATA_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3132);

    // TAP link-local, net-proxy (dans le meme pod, meme netns) comme seule
    // passerelle joignable — verrouille au niveau paquet par
    // `enable_transparent_gateway` : la microVM a deja une route par defaut
    // vers net-proxy via l'autoconfiguration IP du kernel (`ip=` plus bas),
    // donc son trafic HTTP/HTTPS/DNS y arrive naturellement, redirige de
    // maniere transparente sans qu'aucune configuration ne soit necessaire
    // a l'interieur du guest (pas de HTTP_PROXY, pas de resolveur DNS
    // particulier a connaitre — le devcontainer de l'agent est arbitraire,
    // fourni par l'utilisateur du Workshop). Voir
    // docs/architecture/network-security.md pour le detail complet.
    let network = setup_link_local_tap("atelier-vm", 0)
        .await
        .context("creation du TAP pour la microVM de l'agent (CAP_NET_ADMIN requis)")?;
    network
        .enable_transparent_gateway(
            net_proxy_port,
            net_proxy_transparent_http_port,
            net_proxy_transparent_tls_port,
            Some(net_proxy_metadata_port),
        )
        .await
        .context("pose des regles iptables de la passerelle transparente")?;

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

    // Repli S3 (spec docs/specs/13-image-cache-offload.md, tache 8.4) :
    // le PVC local est un cache a eviction (8.5), pas la source de verite —
    // si les fichiers de snapshot en ont ete evinces mais qu'une copie
    // existe sur S3 (televersee par `snapshot_and_publish` ci-dessous a la
    // suspension precedente), la retelecharger AVANT le `match` qui suit,
    // pour qu'il la trouve comme si elle n'avait jamais quitte le disque.
    // Best effort et jamais bloquant : un echec ici (S3 non configure,
    // injoignable, ou simplement aucun snapshot a restaurer) laisse le
    // `match` suivant retomber sur son comportement actuel (boot a froid).
    if let (Some(state), Some(mem), Some(prefix)) = (
        &snapshot_state_path,
        &snapshot_mem_path,
        std::env::var("ATELIER_VM_SNAPSHOT_S3_PREFIX").ok(),
    ) {
        if !state.exists() || !mem.exists() {
            match atelier_common::storage::S3StorageBackend::from_env() {
                Ok(Some(storage)) => {
                    let state_ok = storage
                        .download_snapshot_to_file(&prefix, "snapshot.state", state)
                        .await;
                    let mem_ok = storage
                        .download_snapshot_to_file(&prefix, "snapshot.mem", mem)
                        .await;
                    match (state_ok, mem_ok) {
                        (Ok(()), Ok(())) => {
                            tracing::info!(%prefix, "snapshot files restored from S3 after local cache eviction");
                        }
                        (state_res, mem_res) => {
                            // Partiel = inutilisable : un `snapshot.state`
                            // sans son `snapshot.mem` (ou l'inverse) ferait
                            // echouer `Vm::restore_persisted` de toute
                            // facon — supprime les deux pour retomber
                            // proprement sur le boot a froid ci-dessous.
                            if let Err(err) = state_res {
                                tracing::warn!(%err, %prefix, "telechargement S3 de snapshot.state echoue");
                            }
                            if let Err(err) = mem_res {
                                tracing::warn!(%err, %prefix, "telechargement S3 de snapshot.mem echoue");
                            }
                            tokio::fs::remove_file(state).await.ok();
                            tokio::fs::remove_file(mem).await.ok();
                        }
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(%err, "configuration S3 invalide, repli sur snapshot ignore");
                }
            }
        }
    }

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

    // Etat de confinement, expose en lecture : c'est le seul moyen pour le
    // controller de savoir qu'un Workshop a ete confine. Sans lui, un
    // operateur verrait un Workshop `Running` alors que son reseau est
    // coupe et son etat archive — un mensonge par omission, et exactement le
    // genre d'etat silencieux qui coute cher sur ce projet.
    let locked_down = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (tx, mut rx) = mpsc::channel::<SnapshotRequest>(1);
    let control_addr =
        std::env::var("ATELIER_VM_CONTROL_ADDR").unwrap_or_else(|_| "0.0.0.0:8081".to_string());
    let control_state = ControlState {
        tx: Arc::new(Mutex::new(tx)),
        locked_down: Arc::clone(&locked_down),
    };
    let app = Router::new()
        .route("/snapshot", post(snapshot_handler))
        .route(
            "/lockdown",
            post(lockdown_handler).get(lockdown_state_handler),
        )
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
                let Some(SnapshotRequest { respond_to, lockdown }) = request else {
                    // Serveur HTTP arrete (ne devrait pas arriver avant la
                    // fin du process) : rien de plus a faire ici.
                    break;
                };
                if lockdown {
                    // L'egress d'abord : le snapshot prend plusieurs
                    // secondes, pendant lesquelles une exfiltration en cours
                    // continuerait tranquillement.
                    if let Err(err) = network.lockdown_egress().await {
                        tracing::error!(%err, "gel de l'egress echoue, le snapshot d'urgence est pris malgre tout");
                    }
                }
                let result = snapshot_and_publish(&mut vm, snapshot_dir.as_deref()).await;
                let succeeded = result.is_ok();
                let _ = respond_to.send(result);
                if lockdown {
                    locked_down.store(true, std::sync::atomic::Ordering::SeqCst);
                    // On s'arrete la : ni `shutdown`, ni `teardown`. La
                    // microVM reste figee et le pod debout, pour que
                    // l'incident puisse etre examine.
                    tracing::error!("CONFINEMENT DE SECURITE actif : egress gele, etat archive, microVM conservee");
                    continue;
                }
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

    // Offload S3 best-effort (spec docs/specs/13-image-cache-offload.md,
    // tache 8.4) : ne bloque jamais la suspension, seule la publication
    // locale ci-dessus est strictement necessaire pour une reprise
    // immediate. `ATELIER_VM_SNAPSHOT_S3_PREFIX` absente = pas de prefixe
    // calculable, offload simplement saute (meme garde que `image-builder`,
    // tache 8.3).
    if let Ok(prefix) = std::env::var("ATELIER_VM_SNAPSHOT_S3_PREFIX") {
        match atelier_common::storage::S3StorageBackend::from_env() {
            Ok(Some(storage)) => {
                if let Err(err) = storage
                    .upload_snapshot_file(&prefix, "snapshot.state", &published_state)
                    .await
                {
                    tracing::warn!(%err, %prefix, "televersement S3 de snapshot.state echoue, ignore");
                }
                if let Err(err) = storage
                    .upload_snapshot_file(&prefix, "snapshot.mem", &published_mem)
                    .await
                {
                    tracing::warn!(%err, %prefix, "televersement S3 de snapshot.mem echoue, ignore");
                }
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(%err, "configuration S3 invalide, offload du snapshot ignore");
            }
        }
    }

    let mut hasher = Sha256::new();
    hasher.update(tokio::fs::read(&published_state).await?);
    hasher.update(tokio::fs::read(&published_mem).await?);
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[derive(Clone)]
struct ControlState {
    tx: Arc<Mutex<mpsc::Sender<SnapshotRequest>>>,
    locked_down: Arc<std::sync::atomic::AtomicBool>,
}

async fn snapshot_handler(State(state): State<ControlState>) -> impl IntoResponse {
    control_request(state.tx, false).await
}

/// Etat de confinement, lu par le controller a chaque reconciliation.
async fn lockdown_state_handler(State(state): State<ControlState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "lockdown": state.locked_down.load(std::sync::atomic::Ordering::SeqCst)
    }))
}

/// Confinement de securite, demande par `net-proxy` quand il detecte une
/// anomalie reseau (`crates/net-proxy/src/anomaly.rs`). Meme canal que le
/// snapshot : c'est `vm-supervisor` qui pilote le TAP et la microVM, lui
/// seul peut couper et figer.
async fn lockdown_handler(State(state): State<ControlState>) -> impl IntoResponse {
    control_request(state.tx, true).await
}

async fn control_request(
    tx: Arc<Mutex<mpsc::Sender<SnapshotRequest>>>,
    lockdown: bool,
) -> axum::response::Response {
    let (respond_to, rx) = oneshot::channel();
    let send_result = tx
        .lock()
        .await
        .send(SnapshotRequest {
            respond_to,
            lockdown,
        })
        .await;
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
