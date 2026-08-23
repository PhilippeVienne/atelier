//! Serveur d'administration de l'allowlist egress, reserve a `mcp-gateway`
//! (outil MCP `request_egress`) : ajoute un hote a l'allowlist en memoire
//! sans redemarrer net-proxy.
//!
//! Lie explicitement a `127.0.0.1` (`ATELIER_NET_PROXY_ADMIN_ADDR`), jamais
//! `0.0.0.0` : contrairement au port de controle du port-forward
//! (`ATELIER_NET_PROXY_CONTROL_ADDR`), protege seulement par les regles
//! iptables du TAP de la VM, ce bind loopback est structurellement
//! injoignable par la microVM (elle a sa propre netns, pas celle du pod) —
//! seul un autre conteneur du meme pod (`mcp-gateway`) peut l'atteindre.
//!
//! Pas de persistance : un ajout ici ne survit pas a un redemarrage de
//! net-proxy ni ne modifie `Workshop.spec.egress_allowlist` — l'elargissement
//! est scope a la session du pod, pas une modification durable du CR.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::post;
use axum::{Json, Router};
use serde::Deserialize;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct AdminState {
    pub allowlist: Arc<RwLock<Vec<String>>>,
    /// Adresse reelle du simulateur (sidecar `simulator` du pod, ex:
    /// `127.0.0.1:4566`), connue au demarrage (presente si
    /// `Workshop.spec.tools` contient `enable_simulator`, absente sinon) —
    /// distincte de `EgressConfig::simulator` (le meme `Arc`, partage) : ce
    /// champ ne change jamais, seul son contenu (via `enable_simulator_route`)
    /// bascule de `None` a `Some` lors de l'appel MCP.
    pub simulator_target: Option<(String, u16)>,
    pub simulator: Arc<RwLock<Option<(String, u16)>>>,
}

#[derive(Deserialize)]
struct AddHostRequest {
    host: String,
}

pub fn router(state: AdminState) -> Router {
    Router::new()
        .route("/internal/allowlist/add", post(add_host))
        .route("/internal/simulator/enable", post(enable_simulator))
        .with_state(state)
}

async fn add_host(
    State(state): State<AdminState>,
    Json(request): Json<AddHostRequest>,
) -> &'static str {
    let host = request.host.trim().to_string();
    if host.is_empty() {
        return "host vide, ignore";
    }
    let mut list = state.allowlist.write().await;
    if list.iter().any(|entry| entry.eq_ignore_ascii_case(&host)) {
        tracing::info!(
            host,
            "allowlist deja a jour (elargissement demande par mcp-gateway)"
        );
        return "deja present";
    }
    list.push(host.clone());
    tracing::info!(
        host,
        count = list.len(),
        "allowlist elargie a chaud (request_egress)"
    );
    "ajoute"
}

/// Rend l'alias `simulator` joignable (voir `crate::proxy::handle_connection`) :
/// pas d'effet si aucun sidecar `simulator` n'a ete provisionne pour ce pod
/// (`simulator_target` absent, `Workshop.spec.tools` sans `enable_simulator`).
async fn enable_simulator(State(state): State<AdminState>) -> &'static str {
    let Some(target) = state.simulator_target.clone() else {
        tracing::warn!(
            "enable_simulator demande mais aucun sidecar simulateur configure pour ce pod"
        );
        return "aucun simulateur configure pour ce Workshop";
    };
    *state.simulator.write().await = Some(target);
    tracing::info!("alias simulator active (enable_simulator)");
    "simulateur active"
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn state(initial: &[&str]) -> AdminState {
        AdminState {
            allowlist: Arc::new(RwLock::new(initial.iter().map(|s| s.to_string()).collect())),
            simulator_target: None,
            simulator: Arc::new(RwLock::new(None)),
        }
    }

    #[tokio::test]
    async fn enable_simulator_without_target_configured_is_a_noop() {
        let state = state(&[]);
        let simulator = Arc::clone(&state.simulator);
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/simulator/enable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(simulator.read().await.is_none());
    }

    #[tokio::test]
    async fn enable_simulator_activates_the_configured_target() {
        let mut state = state(&[]);
        state.simulator_target = Some(("127.0.0.1".to_string(), 4566));
        let simulator = Arc::clone(&state.simulator);
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/simulator/enable")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            *simulator.read().await,
            Some(("127.0.0.1".to_string(), 4566))
        );
    }

    #[tokio::test]
    async fn adds_a_new_host() {
        let state = state(&["github.com"]);
        let allowlist = Arc::clone(&state.allowlist);
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/allowlist/add")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"host":"crates.io"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let list = allowlist.read().await;
        assert!(list.iter().any(|h| h == "crates.io"));
    }

    #[tokio::test]
    async fn adding_an_existing_host_is_idempotent() {
        let state = state(&["github.com"]);
        let allowlist = Arc::clone(&state.allowlist);
        let app = router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/internal/allowlist/add")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"host":"github.com"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let list = allowlist.read().await;
        assert_eq!(
            list.iter().filter(|h| h.as_str() == "github.com").count(),
            1
        );
    }
}
