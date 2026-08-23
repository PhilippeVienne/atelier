//! Sondes de sante Kubernetes du controller : un unique `GET /health/ready`
//! verifiant les trois dependances dont `reconcile::run()` a besoin pour
//! fonctionner (API Kubernetes, PostgreSQL, et OpenBao si configure) —
//! contrairement a `atelier-api-server`, le controller n'a pas de notion de
//! "liveness" separee, un seul process de fond tourne, sans requetes HTTP
//! entrantes a servir.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use kube::Client;

#[derive(Clone)]
pub struct HealthState {
    pub k8s_client: Client,
    pub db_pool: sqlx::PgPool,
    pub openbao_addr: Option<String>,
}

pub fn router(state: HealthState) -> Router {
    Router::new()
        .route("/health/ready", get(health_ready))
        .with_state(state)
}

async fn health_ready(State(state): State<HealthState>) -> Response {
    if let Err(err) = state.k8s_client.apiserver_version().await {
        tracing::warn!(%err, "readiness: API Kubernetes injoignable");
        return (StatusCode::SERVICE_UNAVAILABLE, "kubernetes injoignable").into_response();
    }

    if let Err(err) = sqlx::query("SELECT 1").execute(&state.db_pool).await {
        tracing::warn!(%err, "readiness: PostgreSQL injoignable");
        return (StatusCode::SERVICE_UNAVAILABLE, "postgresql injoignable").into_response();
    }

    if let Some(openbao_addr) = &state.openbao_addr {
        let reachable = reqwest::Client::new()
            .get(format!("{openbao_addr}/v1/sys/health"))
            .send()
            .await
            .is_ok();
        if !reachable {
            tracing::warn!("readiness: OpenBao injoignable");
            return (StatusCode::SERVICE_UNAVAILABLE, "openbao injoignable").into_response();
        }
    }

    (StatusCode::OK, "ok").into_response()
}
