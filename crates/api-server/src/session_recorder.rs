//! Enregistrement best-effort de la sortie d'une session terminal (`ttyd`)
//! vers S3 (Jalon M2, DoD "sessions terminal ... compressees et archivees
//! sur S3", voir `docs/specs/PLAN-ACTION-GLOBAL.md`).
//!
//! Seul le terminal est enregistre, pas VS Code (`code-server`) : c'est une
//! decision produit deliberee, pas un oubli — le tunnel `code-server` ne
//! transporte que le protocole HTTP/WebSocket interne de l'editeur (assets,
//! LSP...), qui n'a aucune semantique de "rejeu" exploitable une fois
//! archive. La sortie d'un terminal, elle, se rejoue nativement (meme
//! convention qu'`asciinema` : seule la sortie serveur->client est
//! enregistree, pas la saisie utilisateur — rejouer une session ne necessite
//! que ce qui s'est affiche a l'ecran).
//!
//! Le flux est pousse en streaming vers [`crate::storage::S3StorageBackend`]
//! au fur et a mesure de la session (jamais de buffer complet en memoire,
//! y compris pour une session de plusieurs heures) : un `tokio::io::duplex`
//! sert de tuyau interne dont la moitie lecture est immediatement consommee
//! par un televersement S3 en arriere-plan (compression zstd streaming,
//! upload multipart, voir `crate::storage`), pendant que la moitie ecriture
//! est alimentee au fil de l'eau par le pont WebSocket
//! (`crate::vscode::proxy_to_guest_port`).

use crate::storage::S3StorageBackend;
use std::sync::Arc;
use tokio::io::{AsyncWriteExt, DuplexStream};
use tokio::task::JoinHandle;

/// Taille du tuyau interne entre le pont WebSocket et l'upload S3 : une
/// valeur modeste suffit, ce n'est qu'un tampon de decouplage entre la
/// vitesse d'ecriture (sortie terminal) et la vitesse d'upload (reseau vers
/// S3), jamais un stockage de la session entiere.
const RECORDING_PIPE_BUFFER: usize = 64 * 1024;

/// Poignee d'un enregistrement de session en cours. Tant que cette valeur
/// n'est pas droppee, le televersement S3 sous-jacent reste ouvert ; la
/// dropper (fin de la session WebSocket) clot le flux, ce qui termine la
/// compression zstd et finalise l'upload multipart S3.
pub struct SessionRecording {
    session_id: String,
    sink: DuplexStream,
    upload: JoinHandle<()>,
}

impl SessionRecording {
    /// Demarre l'enregistrement d'une nouvelle session terminal pour
    /// `workshop_name`. Genere un `session_id` (UUID v4) qui identifie de
    /// facon unique cette connexion terminal dans la cle S3 (voir
    /// `S3StorageBackend::session_archive_key`) : rejouable plus tard via
    /// `S3StorageBackend::get_session_stream`, mais aucun mecanisme de
    /// listage/consultation cote API n'est encore construit par cette tache
    /// (hors perimetre du DoD M2, qui ne demande que l'archivage).
    pub fn start(storage: Arc<S3StorageBackend>, workshop_name: String) -> Self {
        let session_id = uuid::Uuid::new_v4().to_string();
        let (sink, source) = tokio::io::duplex(RECORDING_PIPE_BUFFER);
        let upload = {
            let session_id = session_id.clone();
            tokio::spawn(async move {
                if let Err(err) = storage
                    .upload_session_archive(&workshop_name, &session_id, source)
                    .await
                {
                    tracing::warn!(
                        %err, workshop = %workshop_name, session_id = %session_id,
                        "archivage S3 de la session terminal echoue"
                    );
                } else {
                    tracing::debug!(
                        workshop = %workshop_name, session_id = %session_id,
                        "session terminal archivee sur S3"
                    );
                }
            })
        };
        Self {
            session_id,
            sink,
            upload,
        }
    }

    /// Identifiant (UUID v4) de cette session, tel qu'utilise dans la cle S3
    /// (voir `S3StorageBackend::session_archive_key`). Expose pour permettre
    /// a l'appelant de journaliser/persister l'association
    /// session<->archive, et pour les tests d'integration qui doivent
    /// reconstruire la cle afin de verifier l'archive apres coup.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Pousse un bloc d'octets vers l'upload en arriere-plan. Best-effort
    /// deliberement : un enregistrement qui echoue (upload S3 en panne,
    /// pipe ferme cote consommateur) ne doit jamais interrompre la session
    /// terminal elle-meme, seulement son archivage.
    pub async fn write_chunk(&mut self, data: &[u8]) {
        if let Err(err) = self.sink.write_all(data).await {
            tracing::debug!(%err, "enregistrement de session interrompu (best-effort)");
        }
    }

    /// Cloture l'enregistrement : ferme la moitie ecriture du tuyau interne
    /// (ce qui termine le flux zstd cote upload et finalise le televersement
    /// multipart S3), puis attend que l'upload en arriere-plan se termine
    /// reellement avant de rendre la main — plutot qu'un simple `drop`, pour
    /// que l'appelant (fin de la session WebSocket) sache que l'archivage
    /// est bien complet (ou en echec, deja journalise) avant de considerer
    /// le tunnel entierement termine.
    pub async fn finish(self) {
        drop(self.sink);
        let _ = self.upload.await;
    }
}
