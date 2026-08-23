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
//!   export S3_FORCE_PATH_STYLE="true"
//!   cargo test -p atelier-api-server --test storage

use atelier_api_server::storage::{S3StorageBackend, StorageBackend};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;

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
