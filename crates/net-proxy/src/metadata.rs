//! Endpoint metadata guest : sert le mot de passe de session (Basic Auth
//! `code-server`/`ttyd`) a la microVM elle-meme, via l'adresse link-local du
//! TAP (`169.254.0.1`, voir `crates/firecracker/src/network.rs`) — jamais
//! une variable d'environnement du pod (lisible par quiconque peut lire la
//! spec du pod, pas seulement le guest).
//!
//! Lie a `0.0.0.0` comme le reste des ports "cote guest" de net-proxy (proxy
//! egress, DNS, ports transparents) : la VM les atteint via son unique route
//! par defaut vers `169.254.0.1`, quel que soit le port. Contrairement au
//! port d'administration (`crate::admin`, `127.0.0.1` uniquement, reserve a
//! `mcp-gateway`), ce port est concu pour etre joint par la VM.
//!
//! Le devcontainer (repo separe, ex: `atelier-workspace`) est responsable
//! d'appeler `GET /session-auth` au demarrage de ses services `ttyd`/
//! `code-server` et de configurer leur Basic Auth avec la valeur recue
//! (`--credential atelier:<password>` / `--auth password` + fichier de mot
//! de passe) — ce n'est pas ce module qui pousse la valeur dans ces
//! services, il se contente de la rendre disponible.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::session_auth::SessionAuthCache;
use crate::ssh_authorized_key::SshAuthorizedKeyCache;

/// Un process que `atelier-guest-init` (PID 1 du guest) a trouve vivant et
/// hors de son propre giron (ni lui-meme, ni un des services qu'il supervise
/// — `ttyd`/`code-server`/`sshd`) au moment du heartbeat, avec son age.
///
/// Sert a reperer, depuis l'exterieur de la microVM, un process reste
/// coince alors que rien d'autre ne le signale (voir `HeartbeatReport`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrphanProcess {
    pub pid: i32,
    pub command: String,
    pub age_secs: u64,
}

/// Corps du `POST /heartbeat` envoye periodiquement par `guest-init`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatReport {
    pub uptime_secs: u64,
    /// Total depuis le demarrage du guest, pas seulement depuis le dernier
    /// heartbeat : une valeur qui grimpe normalement au fil de la vie du
    /// Workshop (un agent qui lance `git`/`npm`/... en cree en continu), une
    /// valeur qui n'AUGMENTE PLUS alors que le guest est cense travailler
    /// est le signal utile.
    pub zombies_reaped_total: u64,
    /// Non vide seulement quand `guest-init` a trouve, ET tue, un process
    /// reste vivant plus longtemps que son propre seuil (voir
    /// `crates/guest-init`) : la preuve concrete qu'un exec est reste
    /// coince, independamment du plafond `timeout` cote `crate::exec`
    /// (`atelier-api-server`), qui peut echouer a couvrir un cas non prevu.
    pub killed_stale_orphans: Vec<OrphanProcess>,
}

pub type LastHeartbeat = Arc<RwLock<Option<(u64, HeartbeatReport)>>>;

#[derive(Clone)]
pub struct MetadataState {
    pub session_auth: SessionAuthCache,
    /// Jalon M4, tache 4.2.3 (`exec_in_workshop`) : voir
    /// `crate::ssh_authorized_key`.
    pub ssh_authorized_key: SshAuthorizedKeyCache,
    /// Dernier heartbeat recu, horodate a la RECEPTION (pas a l'emission :
    /// l'horloge du guest n'a aucune raison d'etre synchronisee avec celle
    /// du pod). `None` tant qu'aucun n'est encore arrive — normal les
    /// premieres secondes du boot, ou sur une image de base qui garde son
    /// propre init (systemd) et ne lance donc jamais `atelier-guest-init`.
    pub last_heartbeat: LastHeartbeat,
}

pub fn router(state: MetadataState) -> Router {
    Router::new()
        .route("/session-auth", get(session_auth))
        .route("/ssh-authorized-key", get(ssh_authorized_key))
        .route("/heartbeat", post(heartbeat))
        .with_state(state)
}

/// `503` tant que le controller n'a pas encore provisionne (ou que
/// `net-proxy` n'a pas encore relu) le secret — le devcontainer est cense
/// retenter plutot que de demarrer sans Basic Auth.
async fn session_auth(State(state): State<MetadataState>) -> Result<String, StatusCode> {
    state
        .session_auth
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

/// Meme convention que `session_auth` ci-dessus : `503` tant que la cle
/// n'est pas encore disponible, le guest (voir
/// `atelier-fetch-ssh-authorized-key.sh` du depot `atelier-workspace`) est
/// cense retenter plutot que de demarrer `sshd` sans `authorized_keys`.
async fn ssh_authorized_key(State(state): State<MetadataState>) -> Result<String, StatusCode> {
    state
        .ssh_authorized_key
        .read()
        .await
        .clone()
        .ok_or(StatusCode::SERVICE_UNAVAILABLE)
}

/// Recoit et journalise le heartbeat de `atelier-guest-init` (PID 1 du
/// guest). Purement observationnel pour l'instant — pas encore remonte dans
/// `status.conditions` du Workshop (`crates/controller`), ce qui demanderait
/// que le controller vienne LIRE cet etat en plus de simplement le stocker
/// ici ; ce cablage reste a faire, voir docs/architecture/pieges.md.
///
/// Un `killed_stale_orphans` non vide part en `WARN`, pas en `INFO` : c'est
/// la preuve qu'un exec est reste coince malgre le plafond `timeout` cote
/// `crate::exec` (`atelier-api-server`) — un signal qui merite d'etre vu
/// dans les logs meme sans lecture active de cet etat.
async fn heartbeat(
    State(state): State<MetadataState>,
    Json(report): Json<HeartbeatReport>,
) -> StatusCode {
    if report.killed_stale_orphans.is_empty() {
        tracing::debug!(
            uptime_secs = report.uptime_secs,
            zombies_reaped_total = report.zombies_reaped_total,
            "heartbeat guest recu"
        );
    } else {
        tracing::warn!(
            uptime_secs = report.uptime_secs,
            zombies_reaped_total = report.zombies_reaped_total,
            orphans = ?report.killed_stale_orphans,
            "heartbeat guest : des process orphelins sont restes coinces et ont ete tues"
        );
    }
    let received_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    *state.last_heartbeat.write().await = Some((received_at, report));
    StatusCode::NO_CONTENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    fn state_with(
        session_auth: Option<String>,
        ssh_authorized_key: Option<String>,
    ) -> MetadataState {
        MetadataState {
            session_auth: Arc::new(RwLock::new(session_auth)),
            ssh_authorized_key: Arc::new(RwLock::new(ssh_authorized_key)),
            last_heartbeat: Arc::new(RwLock::new(None)),
        }
    }

    #[tokio::test]
    async fn returns_503_before_the_first_successful_refresh() {
        let app = router(state_with(None, None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/session-auth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn serves_the_cached_password_once_available() {
        let app = router(state_with(Some("s3cr3t".to_string()), None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/session-auth")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"s3cr3t");
    }

    #[tokio::test]
    async fn ssh_authorized_key_returns_503_before_the_first_successful_refresh() {
        let app = router(state_with(None, None));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ssh-authorized-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test]
    async fn ssh_authorized_key_serves_the_cached_key_once_available() {
        let app = router(state_with(
            None,
            Some("ssh-ed25519 AAAAtest workshop-demo".to_string()),
        ));

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/ssh-authorized-key")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(&body[..], b"ssh-ed25519 AAAAtest workshop-demo");
    }

    #[tokio::test]
    async fn heartbeat_is_stored_for_later_reading() {
        let state = state_with(None, None);
        let last_heartbeat = state.last_heartbeat.clone();
        let app = router(state);

        let body = serde_json::json!({
            "uptime_secs": 42,
            "zombies_reaped_total": 3,
            "killed_stale_orphans": [
                {"pid": 1234, "command": "node src/server.js", "age_secs": 1800}
            ],
        });
        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/heartbeat")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let stored = last_heartbeat.read().await;
        let (_, report) = stored
            .as_ref()
            .expect("le heartbeat doit avoir ete enregistre");
        assert_eq!(report.uptime_secs, 42);
        assert_eq!(report.killed_stale_orphans.len(), 1);
        assert_eq!(report.killed_stale_orphans[0].pid, 1234);
    }
}
