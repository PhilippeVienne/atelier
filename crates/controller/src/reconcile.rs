use atelier_common::{Workshop, WorkshopPhase, WorkshopStatus};
use futures::StreamExt;
use k8s_openapi::api::core::v1::{Container, Pod, PodSpec};
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const FIELD_MANAGER: &str = "atelier-controller";

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

async fn reconcile(workshop: Arc<Workshop>, client: Arc<Client>) -> Result<Action, kube::Error> {
    let name = workshop.name_any();
    tracing::info!(name = %name, "reconciling workshop");

    match apply(&client, &workshop).await {
        Ok(status) => update_status(&client, &workshop, status).await?,
        Err(err) => tracing::error!(name = %name, %err, "reconcile apply failed"),
    }

    Ok(Action::requeue(Duration::from_secs(15)))
}

fn error_policy(_workshop: Arc<Workshop>, _err: &kube::Error, _client: Arc<Client>) -> Action {
    Action::requeue(Duration::from_secs(5))
}

/// Cree/met a jour le pod parent du Workshop et calcule le statut resultant.
///
/// Iteration 1 : le pod parent est un placeholder (`registry.k8s.io/pause`),
/// pas encore les vrais conteneurs vm-supervisor/net-proxy/identity-proxy/
/// mcp-gateway ni le declenchement d'`image-builder`. Le but de cette
/// iteration est de valider la boucle de reconciliation elle-meme (creation
/// idempotente via server-side apply, ownership, suivi de phase) avant d'y
/// brancher le vrai tooling.
pub async fn apply(client: &Client, workshop: &Workshop) -> anyhow::Result<WorkshopStatus> {
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());
    let pods: Api<Pod> = Api::namespaced(client.clone(), &ns);
    let pod_name = format!("{}-parent", workshop.name_any());

    let owner_ref = workshop
        .controller_owner_ref(&())
        .expect("Workshop a un namespace, owner_ref toujours disponible");

    let pod = Pod {
        metadata: kube::api::ObjectMeta {
            name: Some(pod_name.clone()),
            namespace: Some(ns.clone()),
            owner_references: Some(vec![owner_ref]),
            labels: Some(BTreeMap::from([(
                "atelier.dev/workshop".to_string(),
                workshop.name_any(),
            )])),
            ..Default::default()
        },
        spec: Some(PodSpec {
            containers: vec![Container {
                // TODO: remplacer par vm-supervisor + net-proxy + identity-proxy
                // + mcp-gateway une fois ces images publiees
                name: "placeholder".into(),
                image: Some("registry.k8s.io/pause:3.9".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };

    pods.patch(
        &pod_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&pod),
    )
    .await?;

    let current = pods.get_opt(&pod_name).await?;
    let phase = match current
        .as_ref()
        .and_then(|p| p.status.as_ref())
        .and_then(|s| s.phase.as_deref())
    {
        Some("Running") => WorkshopPhase::Running,
        _ => WorkshopPhase::Provisioning,
    };

    Ok(WorkshopStatus {
        phase,
        pod_name: Some(pod_name),
        image_digest: workshop.status.as_ref().and_then(|s| s.image_digest.clone()),
        snapshot_digest: workshop
            .status
            .as_ref()
            .and_then(|s| s.snapshot_digest.clone()),
        conditions: BTreeMap::new(),
    })
}

async fn update_status(
    client: &Client,
    workshop: &Workshop,
    status: WorkshopStatus,
) -> Result<(), kube::Error> {
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());
    let api: Api<Workshop> = Api::namespaced(client.clone(), &ns);
    let patch = serde_json::json!({ "status": status });
    api.patch_status(
        &workshop.name_any(),
        &PatchParams::default(),
        &Patch::Merge(&patch),
    )
    .await?;
    Ok(())
}
