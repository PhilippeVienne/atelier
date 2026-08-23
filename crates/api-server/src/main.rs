//! API externe : cree/liste/detruit des `Workshop` pour un client authentifie
//! par JWT (issuer signe par un provider externe pre-enregistre).

use anyhow::Context;
use atelier_api_server::auth::AuthState;
use atelier_api_server::routes::{self, AppState};
use kube::Client;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let _telemetry = atelier_common::telemetry::init("atelier-api-server");

    let client = Client::try_default().await?;
    let namespace = std::env::var("ATELIER_NAMESPACE").unwrap_or_else(|_| "default".to_string());
    let auth = AuthState::from_env().await?;

    let database_url = std::env::var("DATABASE_URL")
        .context("DATABASE_URL est obligatoire (voir deploy/dev/postgres/README.md)")?;
    let db_pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(&database_url)
        .await
        .context("connexion a PostgreSQL")?;
    sqlx::migrate!("./migrations")
        .run(&db_pool)
        .await
        .context("execution des migrations PostgreSQL")?;
    let openbao_addr = std::env::var("OPENBAO_ADDR").ok();

    let app = routes::router(
        AppState {
            client,
            namespace,
            db_pool,
            openbao_addr,
        },
        auth,
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("atelier-api-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
