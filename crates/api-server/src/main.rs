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
    // Meme variable que `crates/controller` (`ATELIER_LLM_PROXY_ADDR`, voir
    // `crates/controller/src/reconcile.rs`) : adresse bare `host:port`
    // (sans schema), utilisee uniquement par la verification Fast-Fail du
    // serveur MCP (`crate::mcp_server`, tache 4.1.2).
    let litellm_addr = std::env::var("ATELIER_LLM_PROXY_ADDR").ok();
    let session_auth = openbao_addr
        .clone()
        .map(atelier_api_server::session_auth::SessionAuthClient::from_env);
    let storage =
        atelier_api_server::storage::S3StorageBackend::from_env()?.map(std::sync::Arc::new);

    let app = routes::router(
        AppState {
            client,
            namespace,
            db_pool,
            openbao_addr,
            litellm_addr,
            session_auth,
            storage,
        },
        auth,
    );

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("atelier-api-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
