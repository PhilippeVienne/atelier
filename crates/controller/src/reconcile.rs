use atelier_common::{Workshop, WorkshopPhase, WorkshopStatus};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{Container, EnvVar, Pod, PodSpec, PodTemplateSpec};
use kube::api::{Api, ObjectMeta, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const FIELD_MANAGER: &str = "atelier-controller";
// TODO: rendre configurable (registre interne) une fois l'image publiee.
const IMAGE_BUILDER_IMAGE: &str = "atelier-image-builder:dev";

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

/// Fait converger un `Workshop` d'un pas de reconciliation et renvoie le
/// statut resultant.
///
/// Deux etapes successives, chacune gardee par l'etape precedente :
/// 1. Tant que `status.imageDigest` est absent, s'assurer qu'un `Job`
///    `image-builder` tourne (phase `BuildingImage`) ; `image-builder` se
///    charge lui-meme de patcher `status.imageDigest` a la fin du build,
///    voir `crates/image-builder`.
/// 2. Une fois l'image disponible, creer/mettre a jour le pod parent
///    (phase `Provisioning` -> `Running`). Iteration 2 : ce pod parent est
///    encore un placeholder (`registry.k8s.io/pause`), pas encore les vrais
///    conteneurs vm-supervisor/net-proxy/identity-proxy/mcp-gateway.
pub async fn apply(client: &Client, workshop: &Workshop) -> anyhow::Result<WorkshopStatus> {
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());
    let name = workshop.name_any();

    let image_digest = workshop
        .status
        .as_ref()
        .and_then(|s| s.image_digest.clone());

    let Some(image_digest) = image_digest else {
        return ensure_image_build_job(client, workshop, &ns, &name).await;
    };

    ensure_parent_pod(client, workshop, &ns, &name, image_digest).await
}

async fn ensure_image_build_job(
    client: &Client,
    workshop: &Workshop,
    ns: &str,
    name: &str,
) -> anyhow::Result<WorkshopStatus> {
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let job_name = format!("{name}-image-build");

    let owner_ref = workshop
        .controller_owner_ref(&())
        .expect("Workshop a un namespace, owner_ref toujours disponible");

    let env = vec![
        env_var("ATELIER_DEVCONTAINER_REPO", &workshop.spec.devcontainer.repo),
        env_var(
            "ATELIER_DEVCONTAINER_REVISION",
            &workshop.spec.devcontainer.revision,
        ),
        env_var(
            "ATELIER_DEVCONTAINER_CONFIG_PATH",
            &workshop.spec.devcontainer.config_path,
        ),
        env_var("ATELIER_WORKSHOP_NAME", name),
        env_var("ATELIER_WORKSHOP_NAMESPACE", ns),
    ];

    let job = Job {
        metadata: ObjectMeta {
            name: Some(job_name.clone()),
            namespace: Some(ns.to_string()),
            owner_references: Some(vec![owner_ref]),
            labels: Some(BTreeMap::from([(
                "atelier.dev/workshop".to_string(),
                name.to_string(),
            )])),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(2),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    restart_policy: Some("Never".into()),
                    containers: vec![Container {
                        name: "image-builder".into(),
                        image: Some(IMAGE_BUILDER_IMAGE.into()),
                        env: Some(env),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        }),
        ..Default::default()
    };

    jobs.patch(
        &job_name,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&job),
    )
    .await?;

    let current = jobs.get_opt(&job_name).await?;
    let job_failed = current
        .as_ref()
        .and_then(|j| j.status.as_ref())
        .and_then(|s| s.failed)
        .unwrap_or(0)
        > 0;

    let phase = if job_failed {
        WorkshopPhase::Failed
    } else {
        WorkshopPhase::BuildingImage
    };

    Ok(carry_forward_status(workshop, phase, None))
}

async fn ensure_parent_pod(
    client: &Client,
    workshop: &Workshop,
    ns: &str,
    name: &str,
    image_digest: String,
) -> anyhow::Result<WorkshopStatus> {
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let pod_name = format!("{name}-parent");

    let owner_ref = workshop
        .controller_owner_ref(&())
        .expect("Workshop a un namespace, owner_ref toujours disponible");

    let pod = Pod {
        metadata: ObjectMeta {
            name: Some(pod_name.clone()),
            namespace: Some(ns.to_string()),
            owner_references: Some(vec![owner_ref]),
            labels: Some(BTreeMap::from([(
                "atelier.dev/workshop".to_string(),
                name.to_string(),
            )])),
            ..Default::default()
        },
        spec: Some(PodSpec {
            containers: vec![Container {
                // TODO: remplacer par vm-supervisor + net-proxy + identity-proxy
                // + mcp-gateway une fois ces images publiees, avec image_digest
                // transmis a vm-supervisor pour recuperer le bon rootfs.
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

    let mut status = carry_forward_status(workshop, phase, Some(image_digest));
    status.pod_name = Some(pod_name);
    Ok(status)
}

fn carry_forward_status(
    workshop: &Workshop,
    phase: WorkshopPhase,
    image_digest: Option<String>,
) -> WorkshopStatus {
    WorkshopStatus {
        phase,
        pod_name: None,
        image_digest,
        snapshot_digest: workshop
            .status
            .as_ref()
            .and_then(|s| s.snapshot_digest.clone()),
        conditions: BTreeMap::new(),
    }
}

fn env_var(name: &str, value: &str) -> EnvVar {
    EnvVar {
        name: name.to_string(),
        value: Some(value.to_string()),
        ..Default::default()
    }
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
