//! Operateur Kubernetes : reconcilie les CR `Workshop` en pods parents
//! (tooling sidecar + vm-supervisor) et met a jour leur statut.

mod reconcile;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    reconcile::run().await
}
