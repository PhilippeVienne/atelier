//! API externe : cree/liste/detruit des `Workshop` pour un client authentifie
//! par JWT (issuer signe par un provider externe pre-enregistre).

use atelier_api_server::auth::AuthState;
use atelier_api_server::routes::{self, AppState};
use kube::Client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-api-server");

    let client = Client::try_default().await?;
    let namespace = std::env::var("ATELIER_NAMESPACE").unwrap_or_else(|_| "default".to_string());
    let auth = AuthState::from_env().await?;

    let app = routes::router(AppState { client, namespace }, auth);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("atelier-api-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
