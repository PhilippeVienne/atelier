//! Operateur Kubernetes : reconcilie les CR `Workshop` en pods parents
//! (tooling sidecar + vm-supervisor) et met a jour leur statut.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    atelier_controller::reconcile::run().await
}
