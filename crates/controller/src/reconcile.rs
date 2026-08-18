use crate::{kanidm, openbao};
use atelier_common::{Workshop, WorkshopPhase, WorkshopStatus};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{Container, EnvVar, Pod, PodSpec, PodTemplateSpec, ServiceAccount};
use kanidm_client::KanidmClient;
use kube::api::{Api, ObjectMeta, Patch, PatchParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::finalizer::{self, Event};
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const FIELD_MANAGER: &str = "atelier-controller";
// TODO: rendre configurable (registre interne) une fois l'image publiee.
const IMAGE_BUILDER_IMAGE: &str = "atelier-image-builder:dev";
/// Bloque la suppression effective d'un Workshop tant que
/// `cleanup()` (entite Kanidm, role OpenBao) n'a pas reussi. Les ressources
/// Kubernetes du Workshop (Job, ServiceAccount, Pod) n'en ont pas besoin :
/// elles sont recuperees par le garbage collector standard via leurs owner
/// references.
const CLEANUP_FINALIZER: &str = "atelier.dev/cleanup";

#[derive(Debug, thiserror::Error)]
enum ReconcileError {
    #[error(transparent)]
    Apply(#[from] anyhow::Error),
    #[error(transparent)]
    Kube(#[from] kube::Error),
}

/// Dependances partagees par toutes les reconciliations. `kanidm` et
/// `openbao` sont optionnels : sans configuration, le controller fonctionne
/// normalement mais ne provisionne pas d'identite/secrets par Workshop.
pub struct ReconcileCtx {
    pub client: Client,
    pub kanidm: Option<Arc<KanidmClient>>,
    pub openbao: Option<openbao::OpenBaoConfig>,
}

pub async fn run() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let kanidm = kanidm::client_from_env().await?.map(Arc::new);
    let openbao = openbao::config_from_env()?;
    let workshops: Api<Workshop> = Api::all(client.clone());

    Controller::new(workshops, watcher::Config::default())
        .run(
            reconcile,
            error_policy,
            Arc::new(ReconcileCtx {
                client,
                kanidm,
                openbao,
            }),
        )
        .for_each(|res| async move {
            if let Err(e) = res {
                tracing::error!(error = %e, "reconcile failed");
            }
        })
        .await;

    Ok(())
}

#[tracing::instrument(skip_all, fields(workshop = %workshop.name_any()))]
async fn reconcile(
    workshop: Arc<Workshop>,
    ctx: Arc<ReconcileCtx>,
) -> Result<Action, finalizer::Error<ReconcileError>> {
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());
    let api: Api<Workshop> = Api::namespaced(ctx.client.clone(), &ns);

    finalizer::finalizer(&api, CLEANUP_FINALIZER, workshop, |event| async {
        match event {
            Event::Apply(workshop) => {
                tracing::info!("reconciling workshop");
                let status = apply(&ctx, &workshop).await?;
                update_status(&ctx.client, &workshop, status).await?;
                Ok(Action::requeue(Duration::from_secs(15)))
            }
            Event::Cleanup(workshop) => {
                tracing::info!("cleaning up workshop");
                cleanup(&ctx, &workshop).await?;
                Ok(Action::await_change())
            }
        }
    })
    .await
}

fn error_policy(
    _workshop: Arc<Workshop>,
    err: &finalizer::Error<ReconcileError>,
    _ctx: Arc<ReconcileCtx>,
) -> Action {
    tracing::error!(%err, "reconcile failed");
    Action::requeue(Duration::from_secs(5))
}

/// Nettoie les ressources externes (non Kubernetes) d'un Workshop avant
/// d'autoriser sa suppression effective : entite Kanidm et role OpenBao. Les
/// ressources Kubernetes owned (Job, ServiceAccount, Pod) sont laissees au
/// garbage collector standard.
async fn cleanup(ctx: &ReconcileCtx, workshop: &Workshop) -> anyhow::Result<()> {
    let name = workshop.name_any();

    if let Some(kanidm_client) = &ctx.kanidm {
        kanidm::delete_workshop_entity(kanidm_client, &name).await?;
    }
    if let Some(openbao_config) = &ctx.openbao {
        openbao::delete_workshop_role(openbao_config, &name).await?;
    }

    Ok(())
}

/// Fait converger un `Workshop` d'un pas de reconciliation et renvoie le
/// statut resultant.
///
/// Trois etapes, les deux dernieres gardees par la precedente :
/// 1. Si Kanidm est configure et qu'aucune entite n'existe encore pour ce
///    Workshop, la provisionner (`WorkshopStatus.kanidm_entity_id`). Rapide
///    (un appel HTTP synchrone), donc fait directement ici plutot que via un
///    Job asynchrone comme pour l'image.
/// 2. Tant que `status.imageDigest` est absent, s'assurer qu'un `Job`
///    `image-builder` tourne (phase `BuildingImage`) ; `image-builder` se
///    charge lui-meme de patcher `status.imageDigest` a la fin du build,
///    voir `crates/image-builder`.
/// 3. Une fois l'image disponible, creer/mettre a jour le ServiceAccount et
///    le pod parent (phase `Provisioning` -> `Running`), et si OpenBao est
///    configure, le role Kubernetes-auth qui scope l'acces de ce
///    ServiceAccount aux seuls secrets de ce Workshop (voir
///    `crates/controller/src/openbao.rs`). Iteration 2 : ce pod parent est
///    encore un placeholder (`registry.k8s.io/pause`), pas encore les vrais
///    conteneurs vm-supervisor/net-proxy/identity-proxy/mcp-gateway.
#[tracing::instrument(skip_all, fields(workshop = %workshop.name_any()))]
pub async fn apply(ctx: &ReconcileCtx, workshop: &Workshop) -> anyhow::Result<WorkshopStatus> {
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());
    let name = workshop.name_any();

    let kanidm_entity_id = resolve_kanidm_entity(ctx, workshop, &name).await;

    let image_digest = workshop
        .status
        .as_ref()
        .and_then(|s| s.image_digest.clone());

    let Some(image_digest) = image_digest else {
        return ensure_image_build_job(ctx, workshop, &ns, &name, kanidm_entity_id).await;
    };

    ensure_parent_pod(ctx, workshop, &ns, &name, image_digest, kanidm_entity_id).await
}

/// Renvoie l'identite Kanidm existante, ou tente d'en provisionner une
/// nouvelle si Kanidm est configure. Une erreur de provisioning est
/// journalisee mais ne bloque pas le reste de la reconciliation : ce n'est
/// pas une etape bloquante (contrairement au build d'image).
async fn resolve_kanidm_entity(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    name: &str,
) -> Option<String> {
    let existing = workshop
        .status
        .as_ref()
        .and_then(|s| s.kanidm_entity_id.clone());
    if existing.is_some() {
        return existing;
    }

    let kanidm_client = ctx.kanidm.as_ref()?;
    match kanidm::ensure_workshop_entity(kanidm_client, name).await {
        Ok(entity_id) => Some(entity_id),
        Err(err) => {
            tracing::error!(%err, "provisioning de l'identite Kanidm echoue");
            None
        }
    }
}

#[tracing::instrument(skip_all)]
async fn ensure_image_build_job(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
    kanidm_entity_id: Option<String>,
) -> anyhow::Result<WorkshopStatus> {
    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
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

    Ok(carry_forward_status(workshop, phase, None, kanidm_entity_id))
}

#[tracing::instrument(skip_all)]
async fn ensure_parent_pod(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
    image_digest: String,
    kanidm_entity_id: Option<String>,
) -> anyhow::Result<WorkshopStatus> {
    let service_accounts: Api<ServiceAccount> = Api::namespaced(ctx.client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let pod_name = format!("{name}-parent");
    let sa_name = pod_name.clone();

    let owner_ref = workshop
        .controller_owner_ref(&())
        .expect("Workshop a un namespace, owner_ref toujours disponible");

    // ServiceAccount dedie : c'est l'identite que le pod parent (et donc
    // identity-proxy) presente a OpenBao via la methode d'auth Kubernetes.
    let service_account = ServiceAccount {
        metadata: ObjectMeta {
            name: Some(sa_name.clone()),
            namespace: Some(ns.to_string()),
            owner_references: Some(vec![owner_ref.clone()]),
            labels: Some(BTreeMap::from([(
                "atelier.dev/workshop".to_string(),
                name.to_string(),
            )])),
            ..Default::default()
        },
        ..Default::default()
    };
    service_accounts
        .patch(
            &sa_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&service_account),
        )
        .await?;

    if let Some(openbao_config) = &ctx.openbao {
        if let Err(err) =
            openbao::ensure_workshop_role(openbao_config, name, ns, &sa_name).await
        {
            tracing::error!(%err, "provisioning du role OpenBao echoue");
        }
    }

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
            service_account_name: Some(sa_name),
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

    let mut status = carry_forward_status(workshop, phase, Some(image_digest), kanidm_entity_id);
    status.pod_name = Some(pod_name);
    Ok(status)
}

fn carry_forward_status(
    workshop: &Workshop,
    phase: WorkshopPhase,
    image_digest: Option<String>,
    kanidm_entity_id: Option<String>,
) -> WorkshopStatus {
    WorkshopStatus {
        phase,
        pod_name: None,
        image_digest,
        snapshot_digest: workshop
            .status
            .as_ref()
            .and_then(|s| s.snapshot_digest.clone()),
        kanidm_entity_id,
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
