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

#[derive(Clone)]
pub struct MetadataState {
    pub session_auth: SessionAuthCache,
}

pub fn router(state: MetadataState) -> Router {
    Router::new()
        .route("/session-auth", get(session_auth))
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
}
