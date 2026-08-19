use crate::{kanidm, openbao, storage};
use atelier_common::{Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopStatus};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, EmptyDirVolumeSource, EnvVar, PersistentVolumeClaimVolumeSource, Pod,
    PodSpec, PodTemplateSpec, ResourceRequirements, SecurityContext, ServiceAccount, Volume,
    VolumeMount,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use kanidm_client::KanidmClient;
use kube::api::{Api, DeleteParams, ObjectMeta, Patch, PatchParams};
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
const VM_SUPERVISOR_IMAGE: &str = "atelier-vm-supervisor:dev";
const NET_PROXY_IMAGE: &str = "atelier-net-proxy:dev";
const IDENTITY_PROXY_IMAGE: &str = "atelier-identity-proxy:dev";
/// Bloque la suppression effective d'un Workshop tant que
/// `cleanup()` (entite Kanidm, role OpenBao) n'a pas reussi. Les ressources
/// Kubernetes du Workshop (Job, ServiceAccount, Pod) n'en ont pas besoin :
/// elles sont recuperees par le garbage collector standard via leurs owner
/// references.
const CLEANUP_FINALIZER: &str = "atelier.dev/cleanup";
/// Ressource allouable annoncee par `atelier-kvm-device-plugin`
/// (`crates/kvm-device-plugin`, DaemonSet `kube-system`) : demander 1 unite
/// donne acces a `/dev/kvm` ET `/dev/net/tun` (alloues ensemble par ce
/// plugin) sans `securityContext.privileged: true` — voir
/// `kvm_device_resources()` ci-dessous et `docs/PROGRESS.md`.
const KVM_DEVICE_PLUGIN_RESOURCE: &str = "atelier.dev/kvm";

/// `resources.limits` a poser sur tout conteneur ayant besoin de
/// `/dev/kvm`/`/dev/net/tun` : remplace l'ancien `privileged: true` +
/// volumes `hostPath`, desormais inutiles (le kubelet configure lui-meme le
/// device cgroup du conteneur d'apres les `DeviceSpec` renvoyes par
/// `Allocate()` du device plugin).
fn kvm_device_resources() -> ResourceRequirements {
    ResourceRequirements {
        limits: Some(BTreeMap::from([(
            KVM_DEVICE_PLUGIN_RESOURCE.to_string(),
            Quantity("1".to_string()),
        )])),
        ..Default::default()
    }
}

/// `securityContext` pour un conteneur qui lance `jailer`/Firecracker
/// (`crates/firecracker`) et cree un TAP + regles iptables
/// (`crates/firecracker::network`), une fois `/dev/kvm`/`/dev/net/tun`
/// obtenus via le device plugin plutot que `privileged: true`. Determine
/// empiriquement contre kind reel (pas seulement d'apres la doc jailer) :
/// - `NET_ADMIN` : creation du TAP (`ip tuntap add`) — suffisant a lui
///   seul pour cette etape, testee isolement.
/// - `SYS_ADMIN`, `SYS_RESOURCE` : necessaires pour que le jailer (capabilities
///   de fichier posees via `setcap` dans le Dockerfile — voir
///   `crates/vm-supervisor/Dockerfile`) puisse effectivement les elever a
///   l'exec ; sans elles, `Vm::boot_with_network` echoue en "Operation not
///   permitted" au moment de spawner le process jailer, alors meme que la
///   creation du TAP juste avant a reussi (les file capabilities ne
///   peuvent etre elevees que si elles font deja partie du "bounding set"
///   du conteneur). Le reste des capacites listees dans le `setcap` du
///   Dockerfile (`SYS_CHROOT`, `SETUID`, `SETGID`, `MKNOD`,
///   `DAC_OVERRIDE`) fait deja partie de l'ensemble par defaut
///   containerd/Docker, pas besoin de les ajouter explicitement.
fn firecracker_security_context() -> SecurityContext {
    SecurityContext {
        capabilities: Some(Capabilities {
            add: Some(vec![
                "NET_ADMIN".to_string(),
                "SYS_ADMIN".to_string(),
                "SYS_RESOURCE".to_string(),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

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
/// `registry_addr` n'est en revanche pas optionnel : sans registre, la
/// phase `BuildingImage` ne peut pas aboutir (le Job image-builder echoue),
/// mais on garde une valeur par defaut de dev plutot que de faire echouer
/// `run()` au demarrage.
pub struct ReconcileCtx {
    pub client: Client,
    pub kanidm: Option<Arc<KanidmClient>>,
    pub openbao: Option<openbao::OpenBaoConfig>,
    pub registry_addr: String,
    pub registry_insecure: bool,
}

pub async fn run() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let kanidm = kanidm::client_from_env().await?.map(Arc::new);
    let openbao = openbao::config_from_env()?;
    let registry_addr =
        std::env::var("ATELIER_REGISTRY_ADDR").unwrap_or_else(|_| "localhost:5000".to_string());
    let registry_insecure = std::env::var("ATELIER_REGISTRY_INSECURE")
        .map(|v| v == "true")
        .unwrap_or(false);
    let workshops: Api<Workshop> = Api::all(client.clone());

    Controller::new(workshops, watcher::Config::default())
        .run(
            reconcile,
            error_policy,
            Arc::new(ReconcileCtx {
                client,
                kanidm,
                openbao,
                registry_addr,
                registry_insecure,
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
/// 1. Si Kanidm est configure et qu'aucune entite n'existe encore pour ce
///    Workshop, la provisionner (`WorkshopStatus.kanidm_entity_id`). Rapide
///    (un appel HTTP synchrone), donc fait directement ici plutot que via un
///    Job asynchrone comme pour l'image. Fait inconditionnellement, meme
///    Suspended : l'entite Kanidm et le role OpenBao ne sont pas des
///    ressources du pod, ils survivent a un cycle suspend/resume (choix
///    delibere, cf. section « Mise en veille » de l'architecture).
/// 2. Si `spec.desiredState == Suspended`, s'assurer qu'aucun pod parent ne
///    tourne (phase `Suspending` -> `Suspended`) et s'arreter la : pas de
///    build d'image ni de pod tant qu'on est suspendu.
/// 3. Sinon (`Running`), tant que `status.imageDigest` est absent,
///    s'assurer qu'un `Job` `image-builder` tourne (phase `BuildingImage`) ;
///    `image-builder` se charge lui-meme de patcher `status.imageDigest` a
///    la fin du build, voir `crates/image-builder`.
/// 4. Une fois l'image disponible, creer/mettre a jour le ServiceAccount et
///    le pod parent (phase `Provisioning`/`Resuming` -> `Running`), et si
///    OpenBao est configure, le role Kubernetes-auth qui scope l'acces de ce
///    ServiceAccount aux seuls secrets de ce Workshop (voir
///    `crates/controller/src/openbao.rs`). Le pod parent fait tourner un
///    vrai conteneur `vm-supervisor` (boot Firecracker jaile, ou restauration
///    depuis `status.snapshot_digest` si une suspension precedente en a
///    publie un — voir `ensure_suspended`/`request_snapshot`) ; `net-proxy`/
///    `identity-proxy`/`mcp-gateway` restent a y ajouter (item 3 de
///    `docs/PROGRESS.md`, "Prochaines etapes").
#[tracing::instrument(skip_all, fields(workshop = %workshop.name_any()))]
pub async fn apply(ctx: &ReconcileCtx, workshop: &Workshop) -> anyhow::Result<WorkshopStatus> {
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());
    let name = workshop.name_any();

    let kanidm_entity_id = resolve_kanidm_entity(ctx, workshop, &name).await;

    if workshop.spec.desired_state == WorkshopDesiredState::Suspended {
        return ensure_suspended(ctx, workshop, &ns, &name, kanidm_entity_id).await;
    }

    let was_suspended = matches!(
        workshop.status.as_ref().map(|s| s.phase.clone()).unwrap_or_default(),
        WorkshopPhase::Suspended | WorkshopPhase::Suspending
    );

    let image_digest = workshop
        .status
        .as_ref()
        .and_then(|s| s.image_digest.clone());

    let Some(image_digest) = image_digest else {
        return ensure_image_build_job(ctx, workshop, &ns, &name, kanidm_entity_id).await;
    };

    ensure_parent_pod(
        ctx,
        workshop,
        &ns,
        &name,
        image_digest,
        kanidm_entity_id,
        was_suspended,
    )
    .await
}

/// Port d'ecoute par defaut du canal de controle HTTP de `vm-supervisor`
/// (`ATELIER_VM_CONTROL_ADDR`, cf. `crates/vm-supervisor/src/main.rs`) — pas
/// `AF_VSOCK` (reserve au canal guest<->hote a l'interieur d'un meme pod,
/// voir `docs/architecture/network-security.md`) : `controller` et
/// `vm-supervisor` sont deux process dans deux pods distincts, joignables
/// via le reseau normal du cluster (IP de pod).
const VM_CONTROL_PORT: u16 = 8081;

/// Fait converger vers l'etat suspendu : libere le pod parent (compute) mais
/// laisse intacts le ServiceAccount, l'entite Kanidm et le role OpenBao,
/// pour une reprise rapide sans reprovisionner l'identite du Workshop.
#[tracing::instrument(skip_all)]
async fn ensure_suspended(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
    kanidm_entity_id: Option<String>,
) -> anyhow::Result<WorkshopStatus> {
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let pod_name = format!("{name}-parent");

    let existing_pod = pods.get_opt(&pod_name).await?;
    let mut snapshot_digest = workshop.status.as_ref().and_then(|s| s.snapshot_digest.clone());

    let phase = match existing_pod {
        Some(pod) => {
            if let Some(digest) = request_snapshot(&pod).await {
                snapshot_digest = Some(digest);
            }
            pods.delete(&pod_name, &DeleteParams::default()).await?;
            WorkshopPhase::Suspending
        }
        None => WorkshopPhase::Suspended,
    };

    let image_digest = workshop.status.as_ref().and_then(|s| s.image_digest.clone());
    let mut status = carry_forward_status(workshop, phase, image_digest, kanidm_entity_id);
    status.snapshot_digest = snapshot_digest;
    status.pod_name = None;
    Ok(status)
}

/// Demande a `vm-supervisor` (canal de controle HTTP, voir
/// [`VM_CONTROL_PORT`]) de figer la microVM et de publier son snapshot sur
/// le cache partage, avant que ce pod ne soit supprime. Best-effort et non
/// bloquant par conception : si le pod n'a pas encore d'IP (demarrage en
/// cours) ou que l'appel echoue pour n'importe quelle raison (timeout,
/// vm-supervisor pas encore pret, ...), on se contente de journaliser et de
/// liberer le pod sans snapshot — mieux vaut honorer `desired_state:
/// Suspended` sans etat fige que rester bloque indefiniment dessus.
async fn request_snapshot(pod: &Pod) -> Option<String> {
    let pod_ip = pod.status.as_ref().and_then(|s| s.pod_ip.as_deref())?;
    let url = format!("http://{pod_ip}:{VM_CONTROL_PORT}/snapshot");

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .ok()?;
    let response = match client.post(&url).send().await {
        Ok(response) => response,
        Err(err) => {
            tracing::warn!(%err, %url, "echec de l'appel snapshot a vm-supervisor, suspension sans snapshot");
            return None;
        }
    };
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), %url, "vm-supervisor a refuse la demande de snapshot, suspension sans snapshot");
        return None;
    }
    match response.json::<serde_json::Value>().await {
        Ok(body) => {
            let digest = body.get("snapshotDigest").and_then(|v| v.as_str()).map(str::to_string);
            if digest.is_none() {
                tracing::warn!(?body, "reponse de vm-supervisor sans snapshotDigest exploitable");
            }
            digest
        }
        Err(err) => {
            tracing::warn!(%err, "reponse de vm-supervisor illisible, suspension sans snapshot");
            None
        }
    }
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

/// ServiceAccount + Role + RoleBinding dedies au Job `image-builder` d'un
/// Workshop, scopes a ce Workshop precis via `resourceNames` (pas d'acces
/// au statut d'un autre Workshop du meme namespace). Idempotent, owned par
/// le Workshop (nettoye automatiquement par le garbage collector standard).
async fn ensure_image_build_rbac(
    ctx: &ReconcileCtx,
    ns: &str,
    job_name: &str,
    workshop_name: &str,
    owner_ref: &k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> anyhow::Result<()> {
    let service_accounts: Api<ServiceAccount> = Api::namespaced(ctx.client.clone(), ns);
    let roles: Api<Role> = Api::namespaced(ctx.client.clone(), ns);
    let role_bindings: Api<RoleBinding> = Api::namespaced(ctx.client.clone(), ns);

    let metadata = ObjectMeta {
        name: Some(job_name.to_string()),
        namespace: Some(ns.to_string()),
        owner_references: Some(vec![owner_ref.clone()]),
        labels: Some(BTreeMap::from([(
            "atelier.dev/workshop".to_string(),
            workshop_name.to_string(),
        )])),
        ..Default::default()
    };

    let service_account = ServiceAccount {
        metadata: metadata.clone(),
        ..Default::default()
    };
    service_accounts
        .patch(job_name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&service_account))
        .await?;

    let role = Role {
        metadata: metadata.clone(),
        rules: Some(vec![PolicyRule {
            api_groups: Some(vec!["atelier.dev".to_string()]),
            resources: Some(vec!["workshops/status".to_string()]),
            resource_names: Some(vec![workshop_name.to_string()]),
            verbs: vec!["get".to_string(), "patch".to_string()],
            ..Default::default()
        }]),
    };
    roles
        .patch(job_name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&role))
        .await?;

    let role_binding = RoleBinding {
        metadata,
        role_ref: RoleRef {
            api_group: "rbac.authorization.k8s.io".to_string(),
            kind: "Role".to_string(),
            name: job_name.to_string(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: job_name.to_string(),
            namespace: Some(ns.to_string()),
            ..Default::default()
        }]),
    };
    role_bindings
        .patch(job_name, &PatchParams::apply(FIELD_MANAGER).force(), &Patch::Apply(&role_binding))
        .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
async fn ensure_image_build_job(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
    kanidm_entity_id: Option<String>,
) -> anyhow::Result<WorkshopStatus> {
    // Cache partage (PVC) ou image-builder publie le rootfs construit ;
    // cree au besoin, idempotent, pas de owner reference (partage entre
    // Workshops, survit a la suppression de n'importe lequel d'entre eux).
    storage::ensure_image_cache_pvc(&ctx.client, ns, "20Gi").await?;

    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    let job_name = format!("{name}-image-build");

    let owner_ref = workshop
        .controller_owner_ref(&())
        .expect("Workshop a un namespace, owner_ref toujours disponible");

    // ServiceAccount + RBAC dedies (Role/RoleBinding scopes a ce seul
    // Workshop via `resourceNames`, pas au ServiceAccount `default`) :
    // `image-builder` doit patcher `status.imageDigest` sur SON Workshop a
    // la fin du build. Auparavant bloque par un compromis different (le Job
    // demandait aussi `capabilities.add: [SYS_ADMIN]` pour qu'envbuilder
    // tourne dans ce conteneur — refuse pour du code executant le contenu
    // du depot cible, voir docs/PROGRESS.md, "Reseau kind ↔ registre").
    // Cette capacite n'est plus necessaire : `envbuilder` tourne desormais
    // dans la microVM builder, pas dans ce conteneur.
    ensure_image_build_rbac(ctx, ns, &job_name, name, &owner_ref).await?;

    let net_proxy_port: u16 = 3128;
    let registry_port = ctx
        .registry_addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(5000);

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
        env_var("ATELIER_REGISTRY_ADDR", &ctx.registry_addr),
        env_var("ATELIER_REGISTRY_INSECURE", &ctx.registry_insecure.to_string()),
        env_var("ATELIER_IMAGE_CACHE_DIR", storage::IMAGE_CACHE_MOUNT_PATH),
        // `crane` doit vivre sur un point de montage distinct de la racine
        // du conteneur : envbuilder efface le systeme de fichiers du
        // conteneur qui l'execute (sauf /.envbuilder) avant d'y extraire
        // l'image cible, ce qui emporterait un `crane` installe sur `/`
        // (constate en pratique). D'ou l'initContainer qui le copie dans
        // un emptyDir monte a part.
        env_var("ATELIER_CRANE_BIN", "/tools/crane"),
        // Reference d'image donnee au guest via l'alias `registry` du
        // net-proxy sidecar de ce pod (voir plus bas), pas l'adresse reelle
        // du registre : resout vers la meme destination (net-proxy ignore
        // le port du cote client pour un alias interne, seul le host
        // importe) sans que l'utilisateur ait besoin de l'ajouter a
        // `Workshop.spec.egress_allowlist` (voir `crates/net-proxy::internal`
        // et `image_ref_for_guest` dans `crates/image-builder/src/main.rs`).
        env_var("ATELIER_BUILDER_REGISTRY_ALIAS", &format!("registry:{registry_port}")),
        env_var("ATELIER_BUILDER_NET_PROXY_PORT", &net_proxy_port.to_string()),
    ];

    let cache_volume = Volume {
        name: "cache".to_string(),
        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
            claim_name: storage::IMAGE_CACHE_PVC_NAME.to_string(),
            read_only: Some(false),
        }),
        ..Default::default()
    };
    let tools_volume = Volume {
        name: "tools".to_string(),
        empty_dir: Some(EmptyDirVolumeSource::default()),
        ..Default::default()
    };
    let cache_mount = VolumeMount {
        name: "cache".to_string(),
        mount_path: storage::IMAGE_CACHE_MOUNT_PATH.to_string(),
        ..Default::default()
    };
    let tools_mount = VolumeMount {
        name: "tools".to_string(),
        mount_path: "/tools".to_string(),
        ..Default::default()
    };
    // Allowlist d'egress de la microVM builder (net-proxy sidecar) :
    // reutilise telle quelle `Workshop.spec.egress_allowlist`, pensee au
    // depart pour l'usage runtime de l'agent, pas pour les besoins de build
    // (gestionnaires de paquets du devcontainer) — decision explicite :
    // l'utilisateur doit y inclure ses propres registres de paquets si son
    // devcontainer en a besoin. Le registre interne, lui, n'a pas besoin d'y
    // figurer : il est joignable via l'alias `registry` de net-proxy (hors
    // allowlist, voir `ATELIER_REGISTRY_ALIAS_ADDR` plus bas et
    // `crates/net-proxy::internal`).
    let egress_allowlist = workshop.spec.egress_allowlist.join(",");

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
                    service_account_name: Some(job_name.clone()),
                    volumes: Some(vec![cache_volume, tools_volume]),
                    // `net-proxy` est un "sidecar natif" (initContainer avec
                    // `restartPolicy: Always`, K8s >= 1.28/1.29, KEP-753) et
                    // non un simple `containers[]` : il tourne indefiniment
                    // (jamais de code de sortie 0), et un Job n'est marque
                    // termine que quand TOUS ses `containers[]` (pas les
                    // sidecars) ont fini — sans ca, ce Job resterait
                    // "Running" pour toujours meme apres la fin reelle
                    // d'`image-builder`. Demarre avant les init containers
                    // suivants et le conteneur principal (K8s attend qu'il
                    // ait demarre avant de continuer), et termine
                    // automatiquement une fois `image-builder` fini.
                    init_containers: Some(vec![
                        Container {
                            name: "net-proxy".into(),
                            image: Some(NET_PROXY_IMAGE.into()),
                            restart_policy: Some("Always".into()),
                            env: Some(vec![
                                env_var("ATELIER_EGRESS_ALLOWLIST", &egress_allowlist),
                                env_var("ATELIER_NET_PROXY_LISTEN_ADDR", &format!("0.0.0.0:{net_proxy_port}")),
                                // Alias interne hors allowlist : voir
                                // `crates/net-proxy::internal` et le
                                // commentaire sur `egress_allowlist`
                                // ci-dessus.
                                env_var("ATELIER_REGISTRY_ALIAS_ADDR", &ctx.registry_addr),
                            ]),
                            ..Default::default()
                        },
                        Container {
                            name: "copy-tools".into(),
                            image: Some(IMAGE_BUILDER_IMAGE.into()),
                            command: Some(vec![
                                "cp".to_string(),
                                "/usr/local/bin/crane".to_string(),
                                "/tools/crane".to_string(),
                            ]),
                            volume_mounts: Some(vec![tools_mount.clone()]),
                            ..Default::default()
                        },
                    ]),
                    containers: vec![Container {
                        name: "image-builder".into(),
                        image: Some(IMAGE_BUILDER_IMAGE.into()),
                        env: Some(env),
                        volume_mounts: Some(vec![cache_mount, tools_mount]),
                        // /dev/kvm et /dev/net/tun alloues via le device
                        // plugin (`atelier.dev/kvm`, voir
                        // `kvm_device_resources`), CAP_NET_ADMIN pour la
                        // creation du TAP de la microVM builder — plus de
                        // pod privilegie.
                        resources: Some(kvm_device_resources()),
                        security_context: Some(firecracker_security_context()),
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
    resuming: bool,
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

    // Le PVC de cache existe deja (cree lors de la phase BuildingImage),
    // mais s'assurer qu'il existe reste idempotent et bon marche : couvre
    // le cas d'un Workshop dont l'image aurait ete construite autrement.
    storage::ensure_image_cache_pvc(&ctx.client, ns, "20Gi").await?;

    let rootfs_path = format!(
        "{}/{}/rootfs.ext4",
        storage::IMAGE_CACHE_MOUNT_PATH,
        storage::digest_cache_subdir(&image_digest)
    );
    let snapshot_dir = format!(
        "{}/{}",
        storage::IMAGE_CACHE_MOUNT_PATH,
        storage::snapshot_cache_subdir(ns, name)
    );

    // Lecture-ecriture (pas read_only comme avant l'ajout du canal de
    // controle) : `vm-supervisor` doit pouvoir publier les fichiers de
    // snapshot dans ce meme cache au moment de la suspension (voir
    // `ensure_suspended`).
    let cache_mount = VolumeMount {
        name: "cache".to_string(),
        mount_path: storage::IMAGE_CACHE_MOUNT_PATH.to_string(),
        read_only: Some(false),
        ..Default::default()
    };
    // Allowlist d'egress *runtime* de l'agent (contrairement a celle de la
    // microVM builder, ci-dessus dans `ensure_image_build_job`) : c'est ici
    // le sens original de `Workshop.spec.egress_allowlist`.
    let egress_allowlist = workshop.spec.egress_allowlist.join(",");
    let identity_injection_rules = serde_json::to_string(&workshop.spec.identity_injection_rules)
        .unwrap_or_else(|_| "[]".to_string());
    // IP guest fixe et deterministe : `vm-supervisor` cree toujours son TAP
    // avec le sous-reseau link-local d'index 0 (une seule microVM par pod),
    // ce qui fixe host_ip=169.254.0.1 / guest_ip=169.254.0.2 (arithmetique
    // de `fctools::extension::link_local::LinkLocalSubnet`, voir
    // `crates/firecracker/src/network.rs`).
    const VM_GUEST_IP: &str = "169.254.0.2";
    const NET_PROXY_PORT: u16 = 3128;
    // `identity-proxy` et `net-proxy` partagent le netns du pod (tous les
    // conteneurs d'un meme Pod) : joignables en `127.0.0.1`, sans service
    // Kubernetes ni DNS.
    const IDENTITY_PROXY_PORT: u16 = 3129;

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
            // TODO: mcp-gateway reste a ajouter comme conteneur
            // supplementaire de ce pod (expose a l'agent via vsock, voir
            // docs/ARCHITECTURE.md).
            volumes: Some(vec![Volume {
                name: "cache".to_string(),
                persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                    claim_name: storage::IMAGE_CACHE_PVC_NAME.to_string(),
                    read_only: Some(false),
                }),
                ..Default::default()
            }]),
            containers: vec![
                Container {
                    name: "vm-supervisor".into(),
                    image: Some(VM_SUPERVISOR_IMAGE.into()),
                    env: Some(vec![
                        env_var("ATELIER_VM_ROOTFS_PATH", &rootfs_path),
                        env_var("ATELIER_VM_SNAPSHOT_DIR", &snapshot_dir),
                        env_var("ATELIER_NET_PROXY_PORT", &NET_PROXY_PORT.to_string()),
                    ]),
                    volume_mounts: Some(vec![cache_mount]),
                    // /dev/kvm et /dev/net/tun alloues via le device plugin
                    // (`atelier.dev/kvm`, voir `kvm_device_resources`),
                    // CAP_NET_ADMIN pour la creation du TAP + regles
                    // iptables de la microVM de l'agent — plus de pod
                    // privilegie.
                    resources: Some(kvm_device_resources()),
                    security_context: Some(firecracker_security_context()),
                    ..Default::default()
                },
                Container {
                    name: "net-proxy".into(),
                    image: Some(NET_PROXY_IMAGE.into()),
                    env: Some(vec![
                        env_var("ATELIER_EGRESS_ALLOWLIST", &egress_allowlist),
                        env_var("ATELIER_NET_PROXY_LISTEN_ADDR", &format!("0.0.0.0:{NET_PROXY_PORT}")),
                        env_var("ATELIER_VM_ADDR", VM_GUEST_IP),
                        // Tout l'egress autorise par net-proxy est chaine
                        // vers identity-proxy des lors qu'il est configure
                        // (voir docs/architecture/network-security.md,
                        // "identity-proxy : jamais joint directement par la
                        // VM") : c'est lui, pas la VM, qui decide ensuite
                        // d'injecter un credential ou de relayer tel quel.
                        env_var("ATELIER_IDENTITY_PROXY_ADDR", &format!("127.0.0.1:{IDENTITY_PROXY_PORT}")),
                    ]),
                    ..Default::default()
                },
                Container {
                    name: "identity-proxy".into(),
                    image: Some(IDENTITY_PROXY_IMAGE.into()),
                    env: Some(vec![
                        env_var(
                            "ATELIER_IDENTITY_PROXY_LISTEN_ADDR",
                            &format!("0.0.0.0:{IDENTITY_PROXY_PORT}"),
                        ),
                        env_var("ATELIER_IDENTITY_INJECTION_RULES", &identity_injection_rules),
                        env_var("ATELIER_WORKSHOP_NAME", name),
                    ]
                    .into_iter()
                    .chain(ctx.openbao.as_ref().map(|c| env_var("OPENBAO_ADDR", &c.addr)))
                    .collect::<Vec<_>>()),
                    ..Default::default()
                },
            ],
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
    let pod_running = current
        .as_ref()
        .and_then(|p| p.status.as_ref())
        .and_then(|s| s.phase.as_deref())
        == Some("Running");
    let phase = match (pod_running, resuming) {
        (true, _) => WorkshopPhase::Running,
        (false, true) => WorkshopPhase::Resuming,
        (false, false) => WorkshopPhase::Provisioning,
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
