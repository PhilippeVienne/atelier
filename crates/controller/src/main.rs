//! Operateur Kubernetes : reconcilie les CR `Workshop` en pods parents
//! (tooling sidecar + vm-supervisor) et met a jour leur statut.

use anyhow::Context;

const DEFAULT_HEALTH_ADDR: &str = "0.0.0.0:8081";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-controller");

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

    // Client Kubernetes distinct de celui de `reconcile::run()` (qui
    // construit le sien) : juste pour la sonde `/health/ready`, `kube::Client`
    // est bon marche a construire (pas de connexion persistante ouverte).
    let k8s_client = kube::Client::try_default()
        .await
        .context("construction du client Kubernetes")?;
    let health_addr = std::env::var("ATELIER_CONTROLLER_HEALTH_ADDR")
        .unwrap_or_else(|_| DEFAULT_HEALTH_ADDR.to_string());
    let health_router =
        atelier_controller::health::router(atelier_controller::health::HealthState {
            k8s_client,
            db_pool,
            openbao_addr: std::env::var("OPENBAO_ADDR").ok(),
        });
    let health_listener = tokio::net::TcpListener::bind(&health_addr).await?;
    tracing::info!(%health_addr, "serveur de sondes de sante en ecoute");
    tokio::spawn(async move {
        if let Err(err) = axum::serve(health_listener, health_router).await {
            tracing::error!(%err, "serveur de sondes de sante arrete en erreur");
        }
    });

    // Provisioning cluster-wide, une seule fois au demarrage (pas par
    // reconciliation) : le role OpenBao dedie a `api-server`, qui n'est pas
    // scope a un seul Workshop (voir doc de
    // `atelier_controller::openbao::ensure_api_server_role`). Meme
    // convention que le reste des fonctionnalites optionnelles : silencieux
    // si `OPENBAO_ADDR` est absent.
    if let Some(openbao_config) = atelier_controller::openbao::config_from_env()? {
        let api_server_namespace = std::env::var("ATELIER_API_SERVER_NAMESPACE")
            .unwrap_or_else(|_| "atelier-system".to_string());
        let api_server_service_account = std::env::var("ATELIER_API_SERVER_SERVICE_ACCOUNT")
            .unwrap_or_else(|_| "atelier-api-server".to_string());
        atelier_controller::openbao::ensure_api_server_role(
            &openbao_config,
            &api_server_namespace,
            &api_server_service_account,
        )
        .await
        .context("provisioning du role OpenBao cluster-wide pour api-server")?;
        tracing::info!(
            namespace = %api_server_namespace,
            service_account = %api_server_service_account,
            "role OpenBao cluster-wide 'atelier-api-server' provisionne"
        );
    }

    atelier_controller::reconcile::run().await
}
