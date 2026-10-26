//! Test d'integration : necessite un vrai serveur compatible S3 accessible
//! (RustFS de dev, voir `deploy/dev/s3/README.md`) :
//!
//!   kubectl port-forward svc/atelier-s3-dev 9000:9000 &
//!   export S3_ENDPOINT="http://127.0.0.1:9000"
//!   export S3_REGION="us-east-1"
//!   export AWS_ACCESS_KEY_ID="atelier-rustfs-access-key"
//!   export AWS_SECRET_ACCESS_KEY="atelier-rustfs-secret-key"
//!   export S3_BUCKET_SESSIONS="atelier-sessions"
//!   export S3_BUCKET_SNAPSHOTS="atelier-snapshots"
//!   export S3_BUCKET_IMAGE_CACHE="atelier-image-cache" # requis pour le test d'offload d'images (8.3)
//!   export S3_FORCE_PATH_STYLE="true"
//!   cargo test -p atelier-common --test storage

use atelier_common::storage::{S3StorageBackend, StorageBackend};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;

/// Genere un contenu deterministe (pas aleatoire) d'environ 5 Mo : un
/// generateur congruentiel lineaire a graine fixe. Deterministe => le
/// SHA-256 attendu est calculable une seule fois et reste stable d'une
/// execution a l'autre, ce qui permet de verifier l'integrite du
/// rejeu apres compression/decompression zstd.
fn deterministic_session_payload(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state: u64 = 0x1234_5678_9abc_def0;
    while out.len() < len {
        // LCG (constantes de Numerical Recipes) : simple, deterministe, pas
        // besoin d'une dependance externe pour un test.
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

fn sha256_hex(data: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

#[tokio::test]
async fn upload_and_replay_session_archive_preserves_integrity() {
    let Ok(backend) = S3StorageBackend::from_env() else {
        eprintln!("configuration S3 partielle/invalide, test ignore");
        return;
    };
    let Some(backend) = backend else {
        eprintln!("S3_ENDPOINT absent, test ignore (voir l'en-tete de ce fichier)");
        return;
    };

    // ~5 Mo, comme demande par le plan d'action (M2.1).
    let payload = deterministic_session_payload(5 * 1024 * 1024);
    let expected_sha256 = sha256_hex(&payload);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme")
        .as_nanos();
    let workshop_name = format!("storage-test-{nanos}");
    let session_id = "session-1";

    let key = backend
        .upload_session_archive(&workshop_name, session_id, Cursor::new(payload.clone()))
        .await
        .expect("televersement de l'archive de session");
    assert_eq!(
        key,
        format!("workshops/{workshop_name}/sessions/{session_id}.zst")
    );

    let mut replay = backend
        .get_session_stream(&key)
        .await
        .expect("recuperation du flux de rejeu");
    let mut replayed = Vec::new();
    replay
        .read_to_end(&mut replayed)
        .await
        .expect("lecture complete du flux de rejeu");

    assert_eq!(replayed.len(), payload.len());
    assert_eq!(sha256_hex(&replayed), expected_sha256);

    // Nettoyage best-effort : ne fait pas echouer le test si ca ne marche
    // pas (le but du test est deja atteint a ce stade).
    let bucket_sessions =
        std::env::var("S3_BUCKET_SESSIONS").expect("S3_BUCKET_SESSIONS (verifie plus haut)");
    if let Err(err) = backend.delete_object(&bucket_sessions, &key).await {
        eprintln!("nettoyage de l'objet de test echoue (sans impact sur le test) : {err:#}");
    }
}

/// Tache 8.3 (spec `docs/specs/13-image-cache-offload.md`) : `image-builder`
/// televerse le `rootfs.ext4` publie localement vers S3. Verifie le contenu
/// reellement retrouve, pas seulement que l'appel ne renvoie pas d'erreur.
#[tokio::test]
async fn upload_image_cache_file_is_retrievable_with_the_conventional_key() {
    let Ok(backend) = S3StorageBackend::from_env() else {
        eprintln!("configuration S3 partielle/invalide, test ignore");
        return;
    };
    let Some(backend) = backend else {
        eprintln!("S3_ENDPOINT absent, test ignore (voir l'en-tete de ce fichier)");
        return;
    };
    let Ok(bucket_image_cache) = std::env::var("S3_BUCKET_IMAGE_CACHE") else {
        eprintln!("S3_BUCKET_IMAGE_CACHE absent, test ignore (voir l'en-tete de ce fichier)");
        return;
    };

    let payload = deterministic_session_payload(1024 * 1024);
    let expected_sha256 = sha256_hex(&payload);
    let digest = format!("sha256:{expected_sha256}");

    let tmp_path = std::env::temp_dir().join(format!(
        "atelier-storage-test-{}.ext4",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("horloge systeme")
            .as_nanos()
    ));
    tokio::fs::File::create(&tmp_path)
        .await
        .expect("creation du fichier temporaire")
        .write_all(&payload)
        .await
        .expect("ecriture du fichier temporaire");

    backend
        .upload_image_cache_file(&digest, &tmp_path)
        .await
        .expect("televersement du cache d'images");
    tokio::fs::remove_file(&tmp_path).await.ok();

    let key = S3StorageBackend::image_cache_key(&digest);
    assert_eq!(
        key,
        "images/sha256_".to_string() + &expected_sha256 + "/rootfs.ext4"
    );

    let mut downloaded = backend
        .download_stream(&bucket_image_cache, &key)
        .await
        .expect("recuperation de l'objet televerse");
    let mut content = Vec::new();
    downloaded
        .read_to_end(&mut content)
        .await
        .expect("lecture complete du contenu televerse");
    assert_eq!(sha256_hex(&content), expected_sha256);

    if let Err(err) = backend.delete_object(&bucket_image_cache, &key).await {
        eprintln!("nettoyage de l'objet de test echoue (sans impact sur le test) : {err:#}");
    }
}

/// Tache 8.4 (spec `docs/specs/13-image-cache-offload.md`) : `vm-supervisor`
/// televerse ses fichiers de snapshot vers S3 et les retelecharge apres une
/// eviction locale. Verifie le cycle complet ecriture-suppression-locale-
/// retelechargement, pas seulement l'upload isole.
#[tokio::test]
async fn upload_snapshot_file_survives_local_eviction_via_download() {
    let Ok(backend) = S3StorageBackend::from_env() else {
        eprintln!("configuration S3 partielle/invalide, test ignore");
        return;
    };
    let Some(backend) = backend else {
        eprintln!("S3_ENDPOINT absent, test ignore (voir l'en-tete de ce fichier)");
        return;
    };

    let payload = deterministic_session_payload(256 * 1024);
    let expected_sha256 = sha256_hex(&payload);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme")
        .as_nanos();
    let prefix = format!("snapshots/default_storage-test-{nanos}");

    let local_path = std::env::temp_dir().join(format!("atelier-snapshot-test-{nanos}.state"));
    tokio::fs::File::create(&local_path)
        .await
        .expect("creation du fichier local")
        .write_all(&payload)
        .await
        .expect("ecriture du fichier local");

    backend
        .upload_snapshot_file(&prefix, "snapshot.state", &local_path)
        .await
        .expect("televersement du snapshot");

    // Simule une eviction locale (tache 8.5) : le fichier local disparait,
    // seule la copie S3 subsiste.
    tokio::fs::remove_file(&local_path).await.ok();
    assert!(!local_path.exists());

    backend
        .download_snapshot_to_file(&prefix, "snapshot.state", &local_path)
        .await
        .expect("retelechargement du snapshot apres eviction locale");
    let restored = tokio::fs::read(&local_path)
        .await
        .expect("lecture du fichier restaure");
    assert_eq!(sha256_hex(&restored), expected_sha256);

    // Retrait de l'etat avant de publier un nouvel instantane (spec 19,
    // §3.8) : l'objet disparait, et le retirer une seconde fois n'est pas
    // une erreur.
    tokio::fs::remove_file(&local_path).await.ok();
    for _ in 0..2 {
        backend
            .delete_snapshot_file(&prefix, "snapshot.state")
            .await
            .expect("suppression du fichier de snapshot");
    }
    assert!(
        backend
            .download_snapshot_to_file(&prefix, "snapshot.state", &local_path)
            .await
            .is_err(),
        "un fichier de snapshot supprime ne doit plus se telecharger"
    );
    tokio::fs::remove_file(&local_path).await.ok();
}

/// Tache 14.15 : un disque de microVM est un fichier creux. Il part compresse
/// (les trous ne coutent presque rien), revient identique et creux, et les
/// objets d'un Workshop supprime se retirent d'un coup.
#[tokio::test]
async fn sparse_snapshot_file_travels_small_and_comes_back_sparse() {
    use std::os::unix::fs::MetadataExt;
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};

    let Ok(Some(backend)) = S3StorageBackend::from_env() else {
        eprintln!("S3_ENDPOINT absent ou invalide, test ignore (voir l'en-tete de ce fichier)");
        return;
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme")
        .as_nanos();
    let prefix = format!("snapshots/default_sparse-test-{nanos}");
    let source = std::env::temp_dir().join(format!("atelier-sparse-src-{nanos}"));
    let dest = std::env::temp_dir().join(format!("atelier-sparse-dst-{nanos}"));

    // 128 Mio apparents, deux zones ecrites et une queue creuse.
    let data = deterministic_session_payload(300 * 1024);
    let apparent: u64 = 128 * 1024 * 1024;
    let mut file = tokio::fs::File::create(&source).await.unwrap();
    file.write_all(&data).await.unwrap();
    file.seek(std::io::SeekFrom::Start(64 * 1024 * 1024))
        .await
        .unwrap();
    file.write_all(&data).await.unwrap();
    file.set_len(apparent).await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    backend
        .upload_snapshot_file(&prefix, "snapshot.rootfs", &source)
        .await
        .expect("televersement du disque");
    backend
        .download_snapshot_to_file(&prefix, "snapshot.rootfs", &dest)
        .await
        .expect("retelechargement du disque");

    let (src_bytes, dst_bytes) = (
        tokio::fs::read(&source).await.unwrap(),
        tokio::fs::read(&dest).await.unwrap(),
    );
    assert_eq!(
        dst_bytes.len() as u64,
        apparent,
        "taille apparente conservee"
    );
    assert_eq!(
        sha256_hex(&src_bytes),
        sha256_hex(&dst_bytes),
        "contenu identique"
    );
    let used = std::fs::metadata(&dest).unwrap().blocks() * 512;
    assert!(
        used < apparent / 4,
        "le fichier restaure doit rester creux : {used} octets occupes pour {apparent} apparents"
    );

    let removed = backend.delete_snapshot_prefix(&prefix).await.unwrap();
    assert_eq!(removed, 1, "l'objet compresse est le seul sous ce prefixe");
    assert_eq!(backend.delete_snapshot_prefix(&prefix).await.unwrap(), 0);
    tokio::fs::remove_file(&source).await.ok();
    tokio::fs::remove_file(&dest).await.ok();
}

/// Tache 14.15 : un instantane televerse avant la compression (cle sans
/// `.zst`) se retelecharge encore, et se supprime avec les autres.
#[tokio::test]
async fn snapshot_uploaded_before_compression_is_still_restorable() {
    let Ok(Some(backend)) = S3StorageBackend::from_env() else {
        eprintln!("S3_ENDPOINT absent ou invalide, test ignore (voir l'en-tete de ce fichier)");
        return;
    };
    let bucket = std::env::var("S3_BUCKET_SNAPSHOTS").expect("S3_BUCKET_SNAPSHOTS");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme")
        .as_nanos();
    let prefix = format!("snapshots/default_legacy-test-{nanos}");
    let payload = deterministic_session_payload(100 * 1024);
    backend
        .upload_stream(
            &bucket,
            &format!("{prefix}/snapshot.mem"),
            Box::pin(Cursor::new(payload.clone())),
        )
        .await
        .expect("televersement a l'ancien format");

    // Dossier de destination absent, comme apres l'eviction du cache local.
    let dest = std::env::temp_dir()
        .join(format!("atelier-legacy-dir-{nanos}"))
        .join("snapshot.mem");
    backend
        .download_snapshot_to_file(&prefix, "snapshot.mem", &dest)
        .await
        .expect("retelechargement de l'ancien format");
    assert_eq!(tokio::fs::read(&dest).await.unwrap(), payload);

    backend
        .delete_snapshot_file(&prefix, "snapshot.mem")
        .await
        .unwrap();
    assert_eq!(backend.delete_snapshot_prefix(&prefix).await.unwrap(), 0);
    tokio::fs::remove_file(&dest).await.ok();
    if let Some(dir) = dest.parent() {
        tokio::fs::remove_dir(dir).await.ok();
    }
}
