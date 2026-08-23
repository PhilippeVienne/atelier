//! Test d'integration : necessite les binaires `firecracker`/`jailer`, un
//! noyau et un rootfs de test, l'acces a /dev/kvm, et une regle sudoers
//! NOPASSWD scopee au binaire jailer (le jailer doit tourner en root). Cf.
//! deploy/dev/firecracker/README.md. Variables d'environnement :
//!
//!   ATELIER_TEST_FIRECRACKER_BIN, ATELIER_TEST_JAILER_BIN,
//!   ATELIER_TEST_SNAPSHOT_EDITOR_BIN, ATELIER_TEST_VM_KERNEL_PATH,
//!   ATELIER_TEST_VM_ROOTFS_PATH
//!
//! Sans ces variables, le test est silencieusement ignore.

use atelier_firecracker::vm::{Vm, VmConfig};
use std::path::PathBuf;

struct TestFixtures {
    firecracker_bin: PathBuf,
    jailer_bin: PathBuf,
    snapshot_editor_bin: PathBuf,
    kernel_path: PathBuf,
    rootfs_path: PathBuf,
}

fn fixtures() -> Option<TestFixtures> {
    Some(TestFixtures {
        firecracker_bin: std::env::var("ATELIER_TEST_FIRECRACKER_BIN").ok()?.into(),
        jailer_bin: std::env::var("ATELIER_TEST_JAILER_BIN").ok()?.into(),
        snapshot_editor_bin: std::env::var("ATELIER_TEST_SNAPSHOT_EDITOR_BIN")
            .unwrap_or_else(|_| "/bin/true".to_string())
            .into(),
        kernel_path: std::env::var("ATELIER_TEST_VM_KERNEL_PATH").ok()?.into(),
        rootfs_path: std::env::var("ATELIER_TEST_VM_ROOTFS_PATH").ok()?.into(),
    })
}

/// `Vm::restore` prend `&mut self` sur la VM source (son `ResourceSystem`
/// porte les ressources `Moved` d'origine, kernel/rootfs, a recopier dans
/// le nouveau jail) : dans ce test, la VM source reste donc vivante jusqu'a
/// ce que la restauration soit demarree, puis on l'arrete. En production, le
/// resume se ferait dans un tout autre process (bien plus tard) : la vraie
/// question de comment reconstituer ce `ResourceSystem` source a partir du
/// seul `WorkshopStatus.snapshotDigest` persiste, cf. TODO dans
/// docs/ARCHITECTURE.md.
#[tokio::test]
async fn boot_snapshot_and_restore_real_jailed_microvm() {
    let Some(fixtures) = fixtures() else {
        eprintln!(
            "ATELIER_TEST_FIRECRACKER_BIN/JAILER_BIN/VM_KERNEL_PATH/VM_ROOTFS_PATH non definis, test ignore (voir deploy/dev/firecracker/README.md)"
        );
        return;
    };

    // Pas `std::env::temp_dir()` (`/tmp`, souvent `tmpfs,nodev`) : le jailer
    // cree ses propres device nodes (/dev/kvm etc.) dans le jail, inertes
    // sur un systeme de fichiers monte `nodev` (KVM refuse de s'ouvrir avec
    // `Permission denied`, meme avec les bonnes permissions/capabilities —
    // constate en pratique). `/var/tmp` est generalement sur le systeme de
    // fichiers racine, sans `nodev`.
    let work_dir =
        PathBuf::from("/var/tmp").join(format!("atelier-vm-test-{}", std::process::id()));
    tokio::fs::create_dir_all(&work_dir).await.unwrap();
    let chroot_base_dir = work_dir.join("jails");

    let base_config = VmConfig {
        firecracker_bin: fixtures.firecracker_bin.clone(),
        jailer_bin: fixtures.jailer_bin.clone(),
        snapshot_editor_bin: fixtures.snapshot_editor_bin.clone(),
        chroot_base_dir,
        jail_id: format!("atelier-test-boot-{}", std::process::id()),
        // Downgrade vers notre propre utilisateur (pas root) une fois le
        // jail cree : les fichiers produits (snapshot, mem) restent
        // lisibles/supprimables par nous sans sudo.
        uid: nix::unistd::Uid::current().as_raw(),
        gid: nix::unistd::Gid::current().as_raw(),
        vcpu_count: 1,
        mem_mib: 256,
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
        vsock: None,
    };

    let mut vm = Vm::boot(&base_config, &fixtures.kernel_path, &fixtures.rootfs_path)
        .await
        .expect("le boot jaile de la microVM doit reussir");
    assert!(
        vm.is_running().await.expect("lecture de l'etat de la VM"),
        "la microVM doit tourner (non paused) apres le boot"
    );

    let snapshot = vm
        .snapshot()
        .await
        .expect("le snapshot de la microVM doit reussir");
    assert!(
        snapshot.snapshot_path.exists(),
        "le fichier d'etat du snapshot doit exister: {:?}",
        snapshot.snapshot_path
    );
    assert!(
        snapshot.mem_file_path.exists(),
        "le fichier memoire du snapshot doit exister: {:?}",
        snapshot.mem_file_path
    );

    let restore_config = VmConfig {
        jail_id: format!("atelier-test-restore-{}", std::process::id()),
        ..base_config
    };
    let mut restored = vm
        .restore(snapshot, &restore_config)
        .await
        .expect("la restauration depuis le snapshot doit reussir");
    assert!(
        restored
            .is_running()
            .await
            .expect("lecture de l'etat de la VM restauree"),
        "la microVM restauree doit tourner"
    );

    // La VM source (jail d'origine) n'est plus necessaire une fois la
    // restauration demarree. Best-effort : ce rootfs de test CI minimal
    // peut s'eteindre de lui-meme (script de demarrage qui termine puis
    // halt) avant qu'on appelle shutdown() explicitement — deja verifie
    // vivant/fonctionnel par les assertions is_running() ci-dessus, donc ce
    // n'est pas ce qui est teste ici.
    if let Err(err) = vm.shutdown().await {
        eprintln!("arret de la microVM source (non bloquant): {err}");
    }
    if let Err(err) = restored.shutdown().await {
        eprintln!("arret de la microVM restauree (non bloquant): {err}");
    }

    tokio::fs::remove_dir_all(&work_dir).await.ok();
}

/// Contrairement au test precedent, ici la VM source (et son
/// `ResourceSystem`) est **eteinte et abandonnee avant** la restauration :
/// seuls les fichiers `snapshot.state`/`snapshot.mem`, copies au prealable
/// hors du jail d'origine (simulateur d'une publication vers un cache
/// partage), et les memes parametres de boot (kernel/rootfs/vcpu/mem/
/// boot_args) servent a `Vm::restore_persisted`. C'est le scenario reel
/// de mise en veille : `suspend` (snapshot + liberation du pod) et `resume`
/// (nouveau pod, tout autre process) sont separes dans le temps, sans objet
/// `Vm` source survivant entre les deux.
#[tokio::test]
async fn snapshot_persist_and_restore_without_source_vm() {
    let Some(fixtures) = fixtures() else {
        eprintln!(
            "ATELIER_TEST_FIRECRACKER_BIN/JAILER_BIN/VM_KERNEL_PATH/VM_ROOTFS_PATH non definis, test ignore (voir deploy/dev/firecracker/README.md)"
        );
        return;
    };

    // Noms courts deliberement (voir docs/PROGRESS.md, "Lecons retenues") :
    // sockaddr_un.sun_path est limite a 108 octets, et fctools 0.7.0-alpha.2
    // avale silencieusement un ENAMETOOLONG dans une boucle qui ne cede
    // jamais la main a l'executeur (100% CPU, jamais de timeout).
    let work_dir = PathBuf::from("/var/tmp").join(format!("atelier-vmp-{}", std::process::id()));
    tokio::fs::create_dir_all(&work_dir).await.unwrap();
    let chroot_base_dir = work_dir.join("jails");

    let base_config = VmConfig {
        firecracker_bin: fixtures.firecracker_bin.clone(),
        jailer_bin: fixtures.jailer_bin.clone(),
        snapshot_editor_bin: fixtures.snapshot_editor_bin.clone(),
        chroot_base_dir,
        jail_id: format!("vmp-boot-{}", std::process::id()),
        uid: nix::unistd::Uid::current().as_raw(),
        gid: nix::unistd::Gid::current().as_raw(),
        vcpu_count: 1,
        mem_mib: 256,
        boot_args: "console=ttyS0 reboot=k panic=1 pci=off".to_string(),
        vsock: None,
    };

    let mut vm = Vm::boot(&base_config, &fixtures.kernel_path, &fixtures.rootfs_path)
        .await
        .expect("le boot jaile de la microVM doit reussir");

    let snapshot = vm
        .snapshot()
        .await
        .expect("le snapshot de la microVM doit reussir");

    // Publication vers un "cache" simule : copie hors du jail d'origine,
    // qui va etre detruit juste apres.
    let published_snapshot_path = work_dir.join("published-snapshot.state");
    let published_mem_file_path = work_dir.join("published-snapshot.mem");
    tokio::fs::copy(&snapshot.snapshot_path, &published_snapshot_path)
        .await
        .expect("copie du fichier d'etat du snapshot vers le cache simule");
    tokio::fs::copy(&snapshot.mem_file_path, &published_mem_file_path)
        .await
        .expect("copie du fichier memoire du snapshot vers le cache simule");

    // La VM source est completement eteinte : plus aucun etat en memoire du
    // process ne la concerne, seuls les fichiers publies et les parametres
    // de boot d'origine (connus independamment, memes valeurs que
    // `base_config`) survivent — exactement ce qu'un nouveau process
    // `vm-supervisor`, dans un tout autre pod, aurait a sa disposition.
    vm.shutdown()
        .await
        .expect("l'arret de la VM source doit reussir");

    let restore_config = VmConfig {
        jail_id: format!("vmp-restore-{}", std::process::id()),
        ..base_config
    };
    let mut restored = Vm::restore_persisted(
        &restore_config,
        &fixtures.kernel_path,
        &fixtures.rootfs_path,
        None,
        &published_snapshot_path,
        &published_mem_file_path,
    )
    .await
    .expect("la restauration depuis un snapshot persiste (sans VM source vivante) doit reussir");
    assert!(
        restored
            .is_running()
            .await
            .expect("lecture de l'etat de la VM restauree"),
        "la microVM restauree depuis un snapshot persiste doit tourner"
    );

    if let Err(err) = restored.shutdown().await {
        eprintln!("arret de la microVM restauree (non bloquant): {err}");
    }

    tokio::fs::remove_dir_all(&work_dir).await.ok();
}
