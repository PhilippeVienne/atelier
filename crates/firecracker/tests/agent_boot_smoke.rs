//! Test d'integration (spike) : verifie si un devcontainer arbitraire
//! (aplatit en `rootfs.ext4`, meme procedure que `image-builder` — voir
//! `demo/ministack-workshop/README.md`) demarre reellement ses services une
//! fois boote **exactement comme `vm-supervisor` boote la microVM de
//! l'agent** : boot_args par defaut, **sans** `init=` personnalise
//! (contrairement a la microVM "builder", qui utilise
//! `atelier-builder-vm-init`) — voir `crates/vm-supervisor/src/main.rs`.
//!
//! Question ouverte : le PID 1 par defaut de l'image (systemd ou non)
//! demarre-t-il seulement, et va-t-il faire tourner nos services
//! (dockerd/ministack/code-server, normalement demarres par
//! `postStartCommand`, un concept du CLI `devcontainer`, pas du systeme
//! init de l'image) ? Ce test ne suppose rien : il boote reellement et
//! observe (console + connexion TCP reelle vers le guest).
//!
//! Necessite les memes variables que `tests/vm.rs`
//! (`ATELIER_TEST_FIRECRACKER_BIN`, `ATELIER_TEST_JAILER_BIN`,
//! `ATELIER_TEST_VM_KERNEL_PATH`) plus `ATELIER_TEST_AGENT_ROOTFS_PATH`
//! (le `rootfs.ext4` du devcontainer demo, voir
//! `demo/ministack-workshop/README.md` pour la procedure de construction
//! manuelle). Sans ces variables, le test est silencieusement ignore.

use atelier_firecracker::network::setup_link_local_tap;
use atelier_firecracker::vm::{Vm, VmConfig};
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::time::Duration;
use tokio::net::TcpStream;

struct Fixtures {
    firecracker_bin: PathBuf,
    jailer_bin: PathBuf,
    snapshot_editor_bin: PathBuf,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
}

fn fixtures() -> Option<Fixtures> {
    Some(Fixtures {
        firecracker_bin: std::env::var("ATELIER_TEST_FIRECRACKER_BIN").ok()?.into(),
        jailer_bin: std::env::var("ATELIER_TEST_JAILER_BIN").ok()?.into(),
        snapshot_editor_bin: std::env::var("ATELIER_TEST_SNAPSHOT_EDITOR_BIN")
            .unwrap_or_else(|_| "/bin/true".to_string())
            .into(),
        kernel_path: std::env::var("ATELIER_TEST_VM_KERNEL_PATH").ok()?.into(),
        rootfs_path: std::env::var("ATELIER_TEST_AGENT_ROOTFS_PATH").ok()?.into(),
    })
}

fn prefix_to_netmask(prefix_len: u8) -> Ipv4Addr {
    let mask: u32 = if prefix_len == 0 { 0 } else { u32::MAX << (32 - prefix_len) };
    Ipv4Addr::from(mask)
}

/// Essaie une connexion TCP reelle vers `(ip, port)`, avec un court timeout
/// et quelques tentatives espacees (le service peut mettre un moment a
/// demarrer, meme une fois le boot du noyau termine).
async fn probe_tcp(ip: Ipv4Addr, port: u16, attempts: u32, delay: Duration) -> bool {
    for i in 0..attempts {
        if tokio::time::timeout(Duration::from_secs(2), TcpStream::connect((ip, port)))
            .await
            .map(|r| r.is_ok())
            .unwrap_or(false)
        {
            return true;
        }
        if i + 1 < attempts {
            tokio::time::sleep(delay).await;
        }
    }
    false
}

#[tokio::test]
async fn agent_devcontainer_boots_without_custom_init() {
    // Console du guest draine vers `tracing::debug!` (voir
    // `crates/firecracker/src/vm.rs`) : seul canal de diagnostic sans vsock.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "atelier_firecracker=debug".into()),
        )
        .with_test_writer()
        .try_init();

    let Some(fixtures) = fixtures() else {
        eprintln!(
            "ATELIER_TEST_FIRECRACKER_BIN/JAILER_BIN/VM_KERNEL_PATH/AGENT_ROOTFS_PATH non definis, test ignore (voir demo/ministack-workshop/README.md)"
        );
        return;
    };

    let work_dir = PathBuf::from("/var/tmp").join(format!("atelier-abs-{}", std::process::id()));
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let tap_name = format!("fc-a{}", std::process::id() % 10000);
    let network = setup_link_local_tap(&tap_name, 2)
        .await
        .expect("la creation du TAP doit reussir (CAP_NET_ADMIN requis)");

    // Boot_args : exactement ceux par defaut de `vm-supervisor`
    // (`crates/vm-supervisor/src/main.rs`, `ATELIER_VM_BOOT_ARGS` par
    // defaut + `kernel_ip_boot_arg`) — **aucun** `init=` personnalise,
    // contrairement a la microVM "builder".
    let netmask = prefix_to_netmask(network.network_length);
    let boot_args = format!(
        "console=ttyS0 reboot=k panic=1 pci=off ip={}::{}:{netmask}::{}:off",
        network.guest_ip, network.host_ip, network.iface_id
    );

    let config = VmConfig {
        firecracker_bin: fixtures.firecracker_bin,
        jailer_bin: fixtures.jailer_bin,
        snapshot_editor_bin: fixtures.snapshot_editor_bin,
        chroot_base_dir: work_dir.join("jails"),
        jail_id: format!("abs-{}", std::process::id()),
        uid: nix::unistd::Uid::current().as_raw(),
        gid: nix::unistd::Gid::current().as_raw(),
        vcpu_count: 2,
        mem_mib: 2048,
        boot_args,
        vsock: None,
    };

    eprintln!("[diag] avant boot_with_network, t={:?}", std::time::Instant::now());
    let vm = Vm::boot_with_network(&config, &fixtures.kernel_path, &fixtures.rootfs_path, &network).await;

    let mut vm = match vm {
        Ok(vm) => vm,
        Err(err) => {
            network.teardown().await;
            tokio::fs::remove_dir_all(&work_dir).await.ok();
            panic!("le boot jaile du devcontainer agent a echoue : {err:#}");
        }
    };
    eprintln!("[diag] apres boot_with_network (VM demarree)");

    // Laisser le temps au noyau + PID 1 + (eventuellement) nos services de
    // demarrer, en verifiant regulierement qu'aucun crash/panique n'a
    // arrete la VM plus tot que prevu.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    loop {
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        match vm.is_running().await {
            Ok(true) => tokio::time::sleep(Duration::from_secs(2)).await,
            Ok(false) => {
                eprintln!("[diag] la VM s'est arretee toute seule avant la fin du delai d'observation");
                break;
            }
            Err(err) => {
                eprintln!("[diag] is_running() erreur : {err:#}");
                break;
            }
        }
    }

    eprintln!("[diag] sondage TCP reel vers le guest (code-server:8080, ministack:4566)");
    let code_server_up = probe_tcp(network.guest_ip, 8080, 5, Duration::from_secs(3)).await;
    let ministack_up = probe_tcp(network.guest_ip, 4566, 5, Duration::from_secs(3)).await;
    eprintln!("[diag] code_server_up={code_server_up} ministack_up={ministack_up}");

    let still_running = vm.is_running().await.unwrap_or(false);
    eprintln!("[diag] VM encore en cours d'execution apres la fenetre d'observation : {still_running}");

    if still_running {
        let _ = vm.shutdown().await;
    }
    network.teardown().await;
    tokio::fs::remove_dir_all(&work_dir).await.ok();

    assert!(
        code_server_up || ministack_up,
        "ni code-server (8080) ni ministack (4566) ne repondent dans le guest : le PID 1 par \
         defaut de l'image ne demarre probablement pas nos services automatiquement (postStartCommand \
         est un concept du CLI `devcontainer`, pas du systeme init de l'image) — voir la console \
         ci-dessus (RUST_LOG=atelier_firecracker=debug) pour diagnostiquer le PID 1 reellement \
         atteint."
    );
}
