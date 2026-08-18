//! API externe : cree/liste/detruit des `Workshop` pour un client authentifie
//! par JWT (issuer signe par un provider externe pre-enregistre).

mod auth;
mod routes;

use axum::Router;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let app = Router::new().merge(routes::router());

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    tracing::info!("atelier-api-server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await?;
    Ok(())
}
