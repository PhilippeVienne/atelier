//! Operateur Kubernetes : reconcilie les CR `Workshop` en pods parents
//! (tooling sidecar + vm-supervisor) et met a jour leur statut.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-controller");
    atelier_controller::reconcile::run().await
}
