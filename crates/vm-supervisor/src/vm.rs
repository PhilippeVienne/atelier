//! Cycle de vie d'une microVM Firecracker : boot depuis un kernel+rootfs,
//! snapshot, restauration depuis un snapshot.
//!
//! Iteration actuelle : pilote le binaire `firecracker` directement (pas de
//! jailer — pas de chroot/cgroups/seccomp dedies), suffisant pour valider le
//! mecanisme lui-meme. Le jailer reste necessaire avant toute utilisation en
//! production (isolation reelle de la microVM), cf. TODO dans
//! `docs/ARCHITECTURE.md`.

use crate::firecracker::{wait_for_socket, FirecrackerClient};
use anyhow::{ensure, Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::process::{Child, Command};

pub struct VmConfig {
    pub firecracker_bin: PathBuf,
    pub socket_path: PathBuf,
    pub vcpu_count: u32,
    pub mem_mib: u32,
    pub boot_args: String,
}

/// Une microVM en cours d'execution : le process `firecracker` qui la
/// porte doit rester en vie tant que la VM tourne (le tuer arrete la VM).
pub struct Vm {
    process: Child,
    client: FirecrackerClient,
}

impl Vm {
    /// Demarre une nouvelle microVM depuis un kernel et un rootfs.
    pub async fn boot(config: &VmConfig, kernel_path: &Path, rootfs_path: &Path) -> Result<Self> {
        let process = spawn_firecracker(&config.firecracker_bin, &config.socket_path).await?;
        let client = FirecrackerClient::new(&config.socket_path);
        wait_for_socket(&config.socket_path, Duration::from_secs(5)).await?;

        client
            .put(
                "/boot-source",
                &serde_json::json!({
                    "kernel_image_path": kernel_path,
                    "boot_args": config.boot_args,
                }),
            )
            .await
            .context("configuration du boot-source")?;

        client
            .put(
                "/drives/rootfs",
                &serde_json::json!({
                    "drive_id": "rootfs",
                    "path_on_host": rootfs_path,
                    "is_root_device": true,
                    "is_read_only": false,
                }),
            )
            .await
            .context("configuration du drive rootfs")?;

        client
            .put(
                "/machine-config",
                &serde_json::json!({
                    "vcpu_count": config.vcpu_count,
                    "mem_size_mib": config.mem_mib,
                }),
            )
            .await
            .context("configuration machine-config")?;

        client
            .put(
                "/actions",
                &serde_json::json!({ "action_type": "InstanceStart" }),
            )
            .await
            .context("demarrage de l'instance")?;

        let vm = Self { process, client };
        vm.wait_until_state(Duration::from_secs(5), "Running").await?;
        Ok(vm)
    }

    /// Restaure une microVM depuis un snapshot pris precedemment par
    /// [`Vm::snapshot`]. Un nouveau process `firecracker` est lance (le
    /// snapshot ne peut pas etre charge dans le process qui l'a cree).
    pub async fn restore(
        config: &VmConfig,
        snapshot_state_path: &Path,
        snapshot_mem_path: &Path,
    ) -> Result<Self> {
        let process = spawn_firecracker(&config.firecracker_bin, &config.socket_path).await?;
        let client = FirecrackerClient::new(&config.socket_path);
        wait_for_socket(&config.socket_path, Duration::from_secs(5)).await?;

        client
            .put(
                "/snapshot/load",
                &serde_json::json!({
                    "snapshot_path": snapshot_state_path,
                    "mem_backend": {
                        "backend_type": "File",
                        "backend_path": snapshot_mem_path,
                    },
                    "resume_vm": true,
                }),
            )
            .await
            .context("restauration du snapshot")?;

        let vm = Self { process, client };
        vm.wait_until_state(Duration::from_secs(5), "Running").await?;
        Ok(vm)
    }

    /// Fige la VM (pause) et ecrit son etat + sa memoire complete sur disque.
    /// La VM reste en pause apres l'appel ; le process `firecracker` doit
    /// ensuite etre arrete par l'appelant si le pod parent va etre libere
    /// (cf. `Vm::into_child` / la logique d'appel dans `main.rs`).
    pub async fn snapshot(&self, state_path: &Path, mem_path: &Path) -> Result<()> {
        self.client
            .patch("/vm", &serde_json::json!({ "state": "Paused" }))
            .await
            .context("mise en pause avant snapshot")?;

        self.client
            .put(
                "/snapshot/create",
                &serde_json::json!({
                    "snapshot_path": state_path,
                    "mem_file_path": mem_path,
                    "snapshot_type": "Full",
                }),
            )
            .await
            .context("creation du snapshot")?;

        Ok(())
    }

    pub async fn is_running(&self) -> Result<bool> {
        let state = self.client.get("/").await?;
        Ok(state["state"] == "Running")
    }

    async fn wait_until_state(&self, timeout: Duration, expected: &str) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            let state = self.client.get("/").await?;
            if state["state"] == expected {
                return Ok(());
            }
            ensure!(
                tokio::time::Instant::now() < deadline,
                "timeout en attendant l'etat {expected} (actuel: {})",
                state["state"]
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Arrete le process `firecracker` (et donc la VM). A appeler avant de
    /// liberer le pod parent lors d'une suspension, une fois le snapshot
    /// pris.
    pub async fn kill(mut self) -> Result<()> {
        self.process.kill().await.context("arret du process firecracker")
    }
}

async fn spawn_firecracker(firecracker_bin: &Path, socket_path: &Path) -> Result<Child> {
    // Le socket precedent doit etre absent, Firecracker refuse de demarrer
    // si le fichier existe deja.
    let _ = tokio::fs::remove_file(socket_path).await;

    Command::new(firecracker_bin)
        .arg("--api-sock")
        .arg(socket_path)
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("lancement de {firecracker_bin:?}"))
}
