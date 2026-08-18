//! Test d'integration : necessite le binaire `firecracker`, un noyau et un
//! rootfs de test, et l'acces a /dev/kvm. Cf. deploy/dev/firecracker/README.md
//! pour les recuperer. Variables d'environnement :
//!
//!   ATELIER_TEST_FIRECRACKER_BIN, ATELIER_TEST_VM_KERNEL_PATH,
//!   ATELIER_TEST_VM_ROOTFS_PATH
//!
//! Sans ces variables, le test est silencieusement ignore : Firecracker a
//! besoin de KVM, indisponible dans beaucoup d'environnements CI.

use atelier_vm_supervisor::vm::{Vm, VmConfig};
use std::path::PathBuf;

struct TestFixtures {
    firecracker_bin: PathBuf,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
}

fn fixtures() -> Option<TestFixtures> {
    Some(TestFixtures {
        firecracker_bin: std::env::var("ATELIER_TEST_FIRECRACKER_BIN").ok()?.into(),
        kernel_path: std::env::var("ATELIER_TEST_VM_KERNEL_PATH").ok()?.into(),
        rootfs_path: std::env::var("ATELIER_TEST_VM_ROOTFS_PATH").ok()?.into(),
    })
}

/// Chemin de socket court : `sun_path` (chemin d'un socket Unix) est limite
/// a ~108 octets sur Linux, un chemin sous le repertoire du test (souvent
/// profond, ex: target/debug/...) le depasserait facilement.
fn short_socket_path(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "atelier-vm-test-{label}-{}.sock",
        std::process::id()
    ))
}

#[tokio::test]
async fn boot_snapshot_and_restore_real_microvm() {
    let Some(fixtures) = fixtures() else {
        eprintln!(
            "ATELIER_TEST_FIRECRACKER_BIN/VM_KERNEL_PATH/VM_ROOTFS_PATH non definis, test ignore (voir deploy/dev/firecracker/README.md)"
        );
        return;
    };

    // Le rootfs est monte en lecture-ecriture par Firecracker : on travaille
    // sur une copie pour ne pas modifier le fixture partage.
    let work_dir = std::env::temp_dir().join(format!("atelier-vm-test-{}", std::process::id()));
    tokio::fs::create_dir_all(&work_dir).await.unwrap();
    let rootfs_copy = work_dir.join("rootfs.ext4");
    tokio::fs::copy(&fixtures.rootfs_path, &rootfs_copy)
        .await
        .expect("copie du rootfs de test");

    let config = VmConfig {
        firecracker_bin: fixtures.firecracker_bin.clone(),
        socket_path: short_socket_path("boot"),
        vcpu_count: 1,
        mem_mib: 256,
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
    };

    let vm = Vm::boot(&config, &fixtures.kernel_path, &rootfs_copy)
        .await
        .expect("le boot de la microVM doit reussir");
    assert!(
        vm.is_running().await.expect("lecture de l'etat de la VM"),
        "la microVM doit etre en etat Running apres le boot"
    );

    let state_path = work_dir.join("snapshot.state");
    let mem_path = work_dir.join("snapshot.mem");
    vm.snapshot(&state_path, &mem_path)
        .await
        .expect("le snapshot de la microVM doit reussir");
    assert!(state_path.exists(), "le fichier d'etat du snapshot doit exister");
    assert!(mem_path.exists(), "le fichier memoire du snapshot doit exister");

    // Le process source du snapshot doit etre arrete avant de restaurer
    // ailleurs (deux VM ne peuvent pas tourner sur le meme rootfs a la fois,
    // et ca valide le vrai cas d'usage : suspend = arret complet du pod).
    vm.kill().await.expect("arret de la microVM source");

    let restore_config = VmConfig {
        socket_path: short_socket_path("restore"),
        ..config
    };
    let restored = Vm::restore(&restore_config, &state_path, &mem_path)
        .await
        .expect("la restauration depuis le snapshot doit reussir");
    assert!(
        restored.is_running().await.expect("lecture de l'etat de la VM restauree"),
        "la microVM restauree doit etre en etat Running"
    );
    restored.kill().await.expect("arret de la microVM restauree");

    tokio::fs::remove_dir_all(&work_dir).await.ok();
    tokio::fs::remove_file(short_socket_path("boot")).await.ok();
    tokio::fs::remove_file(short_socket_path("restore")).await.ok();
}
