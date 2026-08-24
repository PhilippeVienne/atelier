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

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::Router;

use crate::session_auth::SessionAuthCache;
use crate::ssh_authorized_key::SshAuthorizedKeyCache;

#[derive(Clone)]
pub struct MetadataState {
    pub session_auth: SessionAuthCache,
    /// Jalon M4, tache 4.2.3 (`exec_in_workshop`) : voir
    /// `crate::ssh_authorized_key`.
    pub ssh_authorized_key: SshAuthorizedKeyCache,
}

pub fn router(state: MetadataState) -> Router {
    Router::new()
        .route("/session-auth", get(session_auth))
        .route("/ssh-authorized-key", get(ssh_authorized_key))
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

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use tokio::sync::RwLock;
    use tower::ServiceExt;

    #[tokio::test]
    async fn returns_503_before_the_first_successful_refresh() {
        let app = router(MetadataState {
            session_auth: Arc::new(RwLock::new(None)),
            ssh_authorized_key: Arc::new(RwLock::new(None)),
        });

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
        let app = router(MetadataState {
            session_auth: Arc::new(RwLock::new(Some("s3cr3t".to_string()))),
            ssh_authorized_key: Arc::new(RwLock::new(None)),
        });

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
        let app = router(MetadataState {
            session_auth: Arc::new(RwLock::new(None)),
            ssh_authorized_key: Arc::new(RwLock::new(None)),
        });

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
        let app = router(MetadataState {
            session_auth: Arc::new(RwLock::new(None)),
            ssh_authorized_key: Arc::new(RwLock::new(Some(
                "ssh-ed25519 AAAAtest workshop-demo".to_string(),
            ))),
        });

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
}
