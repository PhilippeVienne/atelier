use atelier_common::Workshop;
use futures::StreamExt;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Api, Client, ResourceExt};
use std::sync::Arc;
use std::time::Duration;

pub async fn run() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let workshops: Api<Workshop> = Api::all(client.clone());

    Controller::new(workshops, watcher::Config::default())
        .run(reconcile, error_policy, Arc::new(client))
        .for_each(|res| async move {
            if let Err(e) = res {
                tracing::error!(error = %e, "reconcile failed");
            }
        })
        .await;

    Ok(())
}

async fn reconcile(workshop: Arc<Workshop>, _ctx: Arc<Client>) -> Result<Action, kube::Error> {
    tracing::info!(name = %workshop.name_any(), "reconciling workshop");
    // TODO: creer/mettre a jour le pod parent (tooling sidecar + vm-supervisor)
    // TODO: appliquer les ResourceQuota / LimitRange associes
    // TODO: mettre a jour WorkshopStatus.phase
    Ok(Action::requeue(Duration::from_secs(30)))
}

fn error_policy(_workshop: Arc<Workshop>, _err: &kube::Error, _ctx: Arc<Client>) -> Action {
    Action::requeue(Duration::from_secs(5))
}
