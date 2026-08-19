//! Test d'integration reel de la microVM "builder" : boot avec reseau (TAP,
//! voir `crate::network`), execution d'`envbuilder` a l'interieur du guest
//! contre un vrai depot public **via `net-proxy`** (pas d'acces reseau
//! direct/NAT — voir `crate::network` et `crates/builder-vm-init` pour le
//! detail de ce choix), verification que l'image attendue atterrit bien
//! sur le registre de dev reel — sans jamais passer par `CAP_SYS_ADMIN` sur
//! le process appelant : seul le guest (noyau dedie) a besoin de cette
//! capacite en interne, de facon triviale et sans risque puisqu'il n'a rien
//! d'autre a proteger.
//!
//! Necessite les memes variables que `tests/vm.rs`
//! (`ATELIER_TEST_FIRECRACKER_BIN`, `ATELIER_TEST_JAILER_BIN`,
//! `ATELIER_TEST_VM_KERNEL_PATH`) plus `ATELIER_TEST_BUILDER_ROOTFS_PATH`
//! (le `rootfs.ext4` produit depuis `crates/builder-vm-init`, voir
//! `deploy/dev/builder-vm/README.md`), `ATELIER_TEST_REGISTRY_ADDR`
//! (`host:port` d'un registre HTTP joignable depuis `net-proxy`) et
//! `ATELIER_TEST_NET_PROXY_ADDR` (`ip:port` ou un `atelier-net-proxy` deja
//! lance, allowlist large, ecoute — voir README).
//!
//! Necessite `CAP_NET_ADMIN` (creation du TAP) **dans le vrai espace de
//! noms reseau de la machine**, pas un `unshare --net` isole (contrairement
//! a `tests/network.rs`) : le guest doit atteindre `net-proxy`, qui doit
//! lui-meme atteindre Internet — les deux ne peuvent pas etre vrais a la
//! fois dans un netns isole sans route de sortie. Sur un poste de dev sans
//! root, ce test ne peut donc pas etre valide sans un acces root reel
//! (voir README pour la marche a suivre).
//!
//! Sans ces variables, le test est silencieusement ignore.

use atelier_firecracker::network::setup_link_local_tap;
use atelier_firecracker::vm::{Vm, VmConfig};
use std::path::PathBuf;
use std::time::Duration;

struct Fixtures {
    firecracker_bin: PathBuf,
    jailer_bin: PathBuf,
    snapshot_editor_bin: PathBuf,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
    registry_addr: String,
    net_proxy_addr: String,
}

fn fixtures() -> Option<Fixtures> {
    Some(Fixtures {
        firecracker_bin: std::env::var("ATELIER_TEST_FIRECRACKER_BIN").ok()?.into(),
        jailer_bin: std::env::var("ATELIER_TEST_JAILER_BIN").ok()?.into(),
        snapshot_editor_bin: std::env::var("ATELIER_TEST_SNAPSHOT_EDITOR_BIN")
            .unwrap_or_else(|_| "/bin/true".to_string())
            .into(),
        kernel_path: std::env::var("ATELIER_TEST_VM_KERNEL_PATH").ok()?.into(),
        rootfs_path: std::env::var("ATELIER_TEST_BUILDER_ROOTFS_PATH").ok()?.into(),
        registry_addr: std::env::var("ATELIER_TEST_REGISTRY_ADDR").ok()?,
        net_proxy_addr: std::env::var("ATELIER_TEST_NET_PROXY_ADDR").ok()?,
    })
}

#[tokio::test]
async fn boots_builder_vm_and_pushes_image_to_registry() {
    let Some(fixtures) = fixtures() else {
        eprintln!(
            "ATELIER_TEST_FIRECRACKER_BIN/JAILER_BIN/VM_KERNEL_PATH/BUILDER_ROOTFS_PATH/REGISTRY_ADDR non definis, test ignore (voir deploy/dev/builder-vm/README.md)"
        );
        return;
    };

    let work_dir =
        PathBuf::from("/var/tmp").join(format!("atelier-builder-vm-test-{}", std::process::id()));
    tokio::fs::create_dir_all(&work_dir).await.unwrap();

    let tap_name = format!("fc-b{}", std::process::id() % 10000);
    let network = setup_link_local_tap(&tap_name, 1)
        .await
        .expect("la creation du TAP doit reussir (CAP_NET_ADMIN requis)");

    // Port de `net-proxy` : deduit de ATELIER_TEST_NET_PROXY_ADDR, deja
    // demarre par l'appelant et suppose a l'ecoute sur `0.0.0.0` (donc
    // atteignable aussi via l'IP hote du TAP, voir README) — c'est cette
    // IP-la, pas l'adresse de la fixture, que le guest utilise reellement.
    let net_proxy_port = fixtures
        .net_proxy_addr
        .rsplit_once(':')
        .map(|(_, port)| port)
        .unwrap_or("3128");

    let image_ref = format!(
        "{}/atelier-workshops/builder-vm-test-{}:latest",
        fixtures.registry_addr,
        std::process::id()
    );

    let boot_args = format!(
        "console=ttyS0 reboot=k panic=1 pci=off init=/sbin/atelier-builder-vm-init \
         atelier.repo=https://github.com/microsoft/vscode-remote-try-python \
         atelier.revision=main \
         atelier.devcontainer_json_filename=devcontainer.json \
         atelier.image_ref={image_ref} \
         atelier.registry_insecure=true \
         atelier.guest_ip={} atelier.host_ip={} atelier.prefix={} \
         atelier.net_proxy_port={net_proxy_port}",
        network.guest_ip, network.host_ip, network.network_length,
    );

    let config = VmConfig {
        firecracker_bin: fixtures.firecracker_bin,
        jailer_bin: fixtures.jailer_bin,
        snapshot_editor_bin: fixtures.snapshot_editor_bin,
        chroot_base_dir: work_dir.join("jails"),
        jail_id: format!("atelier-builder-vm-test-{}", std::process::id()),
        uid: nix::unistd::Uid::current().as_raw(),
        gid: nix::unistd::Gid::current().as_raw(),
        vcpu_count: 2,
        mem_mib: 1024,
        boot_args,
    };

    let mut vm = Vm::boot_with_network(&config, &fixtures.kernel_path, &fixtures.rootfs_path, &network)
        .await
        .expect("le boot jaile de la microVM builder doit reussir");

    // La VM s'eteint d'elle-meme (reboot(RB_POWER_OFF) dans
    // atelier-builder-vm-init) une fois envbuilder termine : on attend
    // qu'is_running() devienne false plutot que d'appeler shutdown() nous-
    // memes, le clone+build+push pouvant prendre plusieurs dizaines de
    // secondes.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
    loop {
        assert!(
            tokio::time::Instant::now() < deadline,
            "la microVM builder ne s'est pas eteinte a temps (build trop long ou echec silencieux)"
        );
        match vm.is_running().await {
            Ok(true) => tokio::time::sleep(Duration::from_secs(2)).await,
            Ok(false) => break,
            Err(_) => break, // process eteint : get_info() echoue, c'est attendu
        }
    }

    network.teardown().await;
    tokio::fs::remove_dir_all(&work_dir).await.ok();

    // Le hote determine le succes en interrogeant le registre plutot que
    // via un canal de controle explicite (pas de vsock dans ce MVP, voir
    // docs/PROGRESS.md) : si l'image attendue y est presente, envbuilder a
    // clone + construit + pousse avec succes a l'interieur du guest.
    let crane_status = std::process::Command::new(
        std::env::var("ATELIER_TEST_CRANE_BIN").unwrap_or_else(|_| "crane".to_string()),
    )
    .args(["manifest", "--insecure", &image_ref])
    .stdout(std::process::Stdio::null())
    .status()
    .expect("lancement de crane manifest");
    assert!(
        crane_status.success(),
        "l'image {image_ref} doit avoir ete poussee au registre par envbuilder dans le guest"
    );
}
