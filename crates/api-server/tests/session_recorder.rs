//! Test d'integration : necessite un vrai serveur compatible S3 accessible
//! (RustFS de dev, voir `deploy/dev/s3/README.md`), meme configuration que
//! `tests/storage.rs` :
//!
//!   kubectl port-forward svc/atelier-s3-dev 9000:9000 &
//!   export S3_ENDPOINT="http://127.0.0.1:9000"
//!   export S3_REGION="us-east-1"
//!   export AWS_ACCESS_KEY_ID="atelier-rustfs-access-key"
//!   export AWS_SECRET_ACCESS_KEY="atelier-rustfs-secret-key"
//!   export S3_BUCKET_SESSIONS="atelier-sessions"
//!   export S3_BUCKET_SNAPSHOTS="atelier-snapshots"
//!   export S3_FORCE_PATH_STYLE="true"
//!   cargo test -p atelier-api-server --test session_recorder
//!
//! Verifie le chemin complet reellement branche dans
//! `crate::vscode::proxy_to_guest_port` (via `crate::session_recorder`) :
//! des blocs ecrits au fil de l'eau sur un `SessionRecording`, une fois
//! `finish()` appele (fin de la session WebSocket), se retrouvent
//! integralement et fidelement sur S3, rejouables via
//! `S3StorageBackend::get_session_stream`.

use atelier_api_server::session_recorder::SessionRecording;
use atelier_api_server::storage::{S3StorageBackend, StorageBackend};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn recorded_chunks_are_archived_and_replayable_on_s3() {
    let Ok(backend) = S3StorageBackend::from_env() else {
        eprintln!("configuration S3 partielle/invalide, test ignore");
        return;
    };
    let Some(backend) = backend else {
        eprintln!("S3_ENDPOINT absent, test ignore (voir l'en-tete de ce fichier)");
        return;
    };
    let backend = Arc::new(backend);

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme")
        .as_nanos();
    let workshop_name = format!("session-recorder-test-{nanos}");

    let chunks: Vec<Vec<u8>> = vec![
        b"$ ls -la\n".to_vec(),
        b"total 12\ndrwxr-xr-x ...\n".to_vec(),
        b"$ echo hello\nhello\n".to_vec(),
    ];
    let expected: Vec<u8> = chunks.concat();

    let mut recording = SessionRecording::start(backend.clone(), workshop_name.clone());
    let session_id = recording.session_id().to_string();
    for chunk in &chunks {
        recording.write_chunk(chunk).await;
    }
    recording.finish().await;

    let key = S3StorageBackend::session_archive_key(&workshop_name, &session_id);
    let mut replay = backend
        .get_session_stream(&key)
        .await
        .expect("recuperation du flux de rejeu de la session enregistree");
    let mut replayed = Vec::new();
    replay
        .read_to_end(&mut replayed)
        .await
        .expect("lecture complete du flux de rejeu");

    assert_eq!(replayed, expected);

    let bucket_sessions =
        std::env::var("S3_BUCKET_SESSIONS").expect("S3_BUCKET_SESSIONS (verifie plus haut)");
    if let Err(err) = backend.delete_object(&bucket_sessions, &key).await {
        eprintln!("nettoyage de l'objet de test echoue (sans impact sur le test) : {err:#}");
    }
}
