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
//! `PUT /snapshot/create` et la restaurer via `PUT /snapshot/load` (voir
//! `crates/vm-supervisor/src/vm.rs`), ce qui permet de suspendre un Workshop
//! (liberer le pod parent, ne garder que le snapshot dans le cache) puis de
//! le reprendre en quelques centaines de ms sans rejouer le boot ni le setup
//! du devcontainer.
//!
//! Iteration actuelle : boot direct depuis un kernel/rootfs fournis par
//! l'environnement (pas encore de recuperation depuis le cache
//! content-addressed `image_digest`/`snapshot_digest`, ni de pilotage par le
//! `controller` via vsock — cf. TODO plus bas). Le mecanisme Firecracker
//! lui-meme (boot, snapshot, restore) est reel et teste, voir
//! `crates/vm-supervisor/tests/vm.rs`.

use atelier_vm_supervisor::vm::{Vm, VmConfig};
use std::path::PathBuf;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-vm-supervisor");
    tracing::info!("atelier-vm-supervisor starting");

    let config = VmConfig {
        firecracker_bin: env_path("ATELIER_FIRECRACKER_BIN", "firecracker"),
        socket_path: env_path("ATELIER_VM_SOCKET_PATH", "/run/firecracker.sock"),
        vcpu_count: std::env::var("ATELIER_VM_VCPU_COUNT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1),
        mem_mib: std::env::var("ATELIER_VM_MEM_MIB")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(256),
        boot_args: std::env::var("ATELIER_VM_BOOT_ARGS")
            .unwrap_or_else(|_| "console=ttyS0 reboot=k panic=1 pci=off".to_string()),
    };
    let kernel_path = env_path("ATELIER_VM_KERNEL_PATH", "");
    let rootfs_path = env_path("ATELIER_VM_ROOTFS_PATH", "");
    anyhow::ensure!(
        !kernel_path.as_os_str().is_empty() && !rootfs_path.as_os_str().is_empty(),
        "ATELIER_VM_KERNEL_PATH et ATELIER_VM_ROOTFS_PATH sont requis"
    );

    tracing::info!(?kernel_path, ?rootfs_path, "booting microVM");
    let vm = Vm::boot(&config, &kernel_path, &rootfs_path).await?;
    tracing::info!("microVM running");

    // TODO: recuperer kernel/rootfs depuis le cache content-addressed via
    //       status.image_digest (ou status.snapshot_digest + Vm::restore en
    //       cas de reprise) plutot que des chemins fournis directement
    // TODO: canal de controle (vsock) expose au controller/api-server pour
    //       les commandes suspend (Vm::snapshot puis arret) / status
    // TODO: relayer logs/metriques de la VM vers le control plane
    // TODO: jailer (chroot/cgroups/seccomp) plutot que firecracker nu

    // En attendant le canal de controle, le process reste vivant tant que
    // la VM tourne (le tuer arrete la VM, cf. Vm::boot/kill_on_drop).
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        match vm.is_running().await {
            Ok(true) => {}
            Ok(false) => {
                tracing::warn!("microVM n'est plus en etat Running");
                break;
            }
            Err(err) => {
                tracing::error!(%err, "impossible d'interroger l'etat de la microVM");
                break;
            }
        }
    }

    Ok(())
}

fn env_path(var: &str, default: &str) -> PathBuf {
    std::env::var(var).unwrap_or_else(|_| default.to_string()).into()
}
