use crate::{git_identity, litellm, openbao, storage};
use atelier_common::{
    IdentityInjectionRule, Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopStatus,
    WorkshopUpgradeState,
};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, Container, EmptyDirVolumeSource, EnvVar, HostAlias,
    PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec, ResourceRequirements,
    SecurityContext, ServiceAccount, Volume, VolumeMount,
};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
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
const MCP_GATEWAY_IMAGE: &str = "atelier-mcp-gateway:dev";
// Image officielle (pas de fork/rebuild maison) : premier simulateur du
// projet, voir `docs/PROGRESS.md` ("mcp-gateway", tool `enable_simulator`).
const SIMULATOR_IMAGE: &str = "localstack/localstack:3";
/// Bloque la suppression effective d'un Workshop tant que
/// `cleanup()` (entite Kanidm, role OpenBao) n'a pas reussi. Les ressources
/// Kubernetes du Workshop (Job, ServiceAccount, Pod) n'en ont pas besoin :
/// elles sont recuperees par le garbage collector standard via leurs owner
/// references.
const CLEANUP_FINALIZER: &str = "atelier.dev/cleanup";
/// Annotation posee sur le pod parent au moment de sa creation, contenant le
/// hash du `PodSpec` genere par CETTE version du controller (voir
/// [`pod_spec_template_hash`]). Sert uniquement a detecter, aux reconciles
/// suivants, qu'un `helm upgrade` a change la facon dont le controller
/// construirait ce pod aujourd'hui (nouvelle image `net-proxy`, nouvelle
/// variable d'environnement, etc) sans jamais recreer de force un pod parent
/// existant (contrainte deja documentee au-dessus de `ensure_parent_pod` :
/// une microVM active ne doit jamais etre perturbee par un upgrade). Voir
/// `WorkshopStatus.upgrade_state` / `WorkshopUpgradeState::NeedsRestartForUpgrade`
/// (Jalon M6, tache 6.4.2).
const TEMPLATE_HASH_ANNOTATION: &str = "atelier.dev/template-hash";
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
/// Convertit `Workshop.spec.resources.memory` (format quantite Kubernetes,
/// ex. `"512Mi"`/`"2Gi"`/`"1G"`) en Mio pour `ATELIER_VM_MEM_MIB` — sans
/// cette conversion, la microVM bootait toujours avec le defaut de
/// `vm-supervisor` (256 Mi) quelle que soit la valeur declaree dans le
/// `Workshop`, jamais assez pour un devcontainer reel (systemd +
/// docker-in-docker + code-server). `None` si le format n'est pas reconnu :
/// laisse alors `vm-supervisor` appliquer son propre defaut plutot que de
/// faire echouer toute la reconciliation pour une valeur mal formee.
fn memory_to_mib(memory: &str) -> Option<u32> {
    let memory = memory.trim();
    let (digits, mebibytes_per_unit) = if let Some(n) = memory.strip_suffix("Gi") {
        (n, 1024.0)
    } else if let Some(n) = memory.strip_suffix("Mi") {
        (n, 1.0)
    } else if let Some(n) = memory.strip_suffix("Ki") {
        (n, 1.0 / 1024.0)
    } else if let Some(n) = memory.strip_suffix('G') {
        (n, 1_000_000_000.0 / (1024.0 * 1024.0))
    } else if let Some(n) = memory.strip_suffix('M') {
        (n, 1_000_000.0 / (1024.0 * 1024.0))
    } else if let Some(n) = memory.strip_suffix('K') {
        (n, 1_000.0 / (1024.0 * 1024.0))
    } else {
        (memory, 1.0 / (1024.0 * 1024.0))
    };
    let value: f64 = digits.parse().ok()?;
    Some((value * mebibytes_per_unit).round() as u32)
}

/// Convertit `Workshop.spec.resources.cpu` (format quantite Kubernetes,
/// ex. `"2"`/`"500m"`) en nombre de vCPU Firecracker (`ATELIER_VM_VCPU_COUNT`) —
/// arrondi au superieur, au moins 1.
fn cpu_to_vcpu_count(cpu: &str) -> Option<u8> {
    let cpu = cpu.trim();
    let cores: f64 = if let Some(n) = cpu.strip_suffix('m') {
        n.parse::<f64>().ok()? / 1000.0
    } else {
        cpu.parse().ok()?
    };
    Some(cores.ceil().max(1.0) as u8)
}

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

/// Dependances partagees par toutes les reconciliations. `openbao` est
/// optionnel : sans configuration, le controller fonctionne normalement
/// mais ne provisionne pas de secrets par Workshop. `registry_addr` n'est en
/// revanche pas optionnel : sans registre, la phase `BuildingImage` ne peut
/// pas aboutir (le Job image-builder echoue), mais on garde une valeur par
/// defaut de dev plutot que de faire echouer `run()` au demarrage.
pub struct ReconcileCtx {
    pub client: Client,
    pub openbao: Option<openbao::OpenBaoConfig>,
    pub registry_addr: String,
    pub registry_insecure: bool,
    /// Adresse du service global LiteLLM (`deploy/dev/llm-proxy/`, meme
    /// niveau qu'OpenBao : une seule instance partagee par tous les
    /// Workshops, pas un sidecar par pod). `None` : fonctionnalite
    /// desactivee, aucun alias `llm-proxy` ni injection `ANTHROPIC_*` —
    /// meme convention que `openbao`.
    pub llm_proxy_addr: Option<String>,
    /// Jeton envoye par Claude Code (`ANTHROPIC_AUTH_TOKEN`) et attendu par
    /// LiteLLM (`LITELLM_MASTER_KEY`) — partage par tous les Workshops dans
    /// ce lot, voir "Limites assumees" de `docs/PROGRESS.md`.
    pub llm_proxy_auth_token: Option<String>,
    /// Injection automatique d'un credential Git pour l'agent (Jalon M2,
    /// section 5.2) — voir `crate::git_identity`. `None` : fonctionnalite
    /// desactivee, meme convention que `openbao`/`llm_proxy_addr`.
    pub git_identity: Option<git_identity::GitIdentityConfig>,
    /// Client d'administration LiteLLM (Jalon M3, Virtual Keys par
    /// Workshop) — construit a partir des DEUX memes variables que
    /// `llm_proxy_addr`/`llm_proxy_auth_token` ci-dessus (voir
    /// `crate::litellm::config_from_env`). `None` : fonctionnalite
    /// desactivee (aucune Virtual Key generee, le jeton statique partage
    /// ci-dessus reste le seul chemin, comportement inchange par rapport a
    /// avant ce jalon), meme convention que `openbao`.
    pub litellm: Option<litellm::LiteLlmConfig>,
}

pub async fn run() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let openbao = openbao::config_from_env()?;
    let registry_addr =
        std::env::var("ATELIER_REGISTRY_ADDR").unwrap_or_else(|_| "localhost:5000".to_string());
    let registry_insecure = std::env::var("ATELIER_REGISTRY_INSECURE")
        .map(|v| v == "true")
        .unwrap_or(false);
    let llm_proxy_addr = std::env::var("ATELIER_LLM_PROXY_ADDR").ok();
    let llm_proxy_auth_token = std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN").ok();
    let git_identity = git_identity::config_from_env();
    let litellm_config =
        litellm::config_from_env(llm_proxy_addr.clone(), llm_proxy_auth_token.clone());
    let workshops: Api<Workshop> = Api::all(client.clone());

    Controller::new(workshops, watcher::Config::default())
        .run(
            reconcile,
            error_policy,
            Arc::new(ReconcileCtx {
                client,
                openbao,
                registry_addr,
                registry_insecure,
                llm_proxy_addr,
                llm_proxy_auth_token,
                git_identity,
                litellm: litellm_config,
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
/// d'autoriser sa suppression effective : role OpenBao. Les ressources
/// Kubernetes owned (Job, ServiceAccount, Pod) sont laissees au garbage
/// collector standard.
///
/// `pub` (comme `apply` ci-dessous) pour etre exercee directement par les
/// tests d'integration (`crates/controller/tests/reconcile.rs`) sans
/// demarrer un `Controller` complet ni attendre le cycle finalizer reel.
pub async fn cleanup(ctx: &ReconcileCtx, workshop: &Workshop) -> anyhow::Result<()> {
    let name = workshop.name_any();

    if let Some(openbao_config) = &ctx.openbao {
        openbao::delete_workshop_role(openbao_config, &name).await?;
    }

    // Tache 3.2.1 (Jalon M3) : revoque la Virtual Key LiteLLM de ce
    // Workshop avant de liberer le finalizer, sans quoi elle resterait
    // valide (et consommable) au-dela de la duree de vie du Workshop qui
    // l'a fait naitre, jusqu'a expiration de son TTL court. Idempotent
    // (`LiteLlmClient::delete_virtual_key` traite un 404 comme un succes) :
    // sur du retry apres un echec precedent, ne fait jamais echouer le
    // finalizer pour une cle deja absente.
    if let Some(litellm_config) = &ctx.litellm {
        let client = litellm::LiteLlmClient::new(litellm_config.clone());
        client
            .delete_virtual_key(&litellm::workshop_key_alias(&name))
            .await?;
    }

    Ok(())
}

/// Fait converger un `Workshop` d'un pas de reconciliation et renvoie le
/// statut resultant.
///
/// 1. Si `spec.desiredState == Suspended`, s'assurer qu'aucun pod parent ne
///    tourne (phase `Suspending` -> `Suspended`) et s'arreter la : pas de
///    build d'image ni de pod tant qu'on est suspendu.
/// 2. Sinon (`Running`), tant que `status.imageDigest` est absent,
///    s'assurer qu'un `Job` `image-builder` tourne (phase `BuildingImage`) ;
///    `image-builder` se charge lui-meme de patcher `status.imageDigest` a
///    la fin du build, voir `crates/image-builder`.
/// 3. Une fois l'image disponible, creer/mettre a jour le ServiceAccount et
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

    if workshop.spec.desired_state == WorkshopDesiredState::Suspended {
        return ensure_suspended(ctx, workshop, &ns, &name).await;
    }

    let was_suspended = matches!(
        workshop
            .status
            .as_ref()
            .map(|s| s.phase.clone())
            .unwrap_or_default(),
        WorkshopPhase::Suspended | WorkshopPhase::Suspending
    );

    let image_digest = workshop
        .status
        .as_ref()
        .and_then(|s| s.image_digest.clone());

    let Some(image_digest) = image_digest else {
        return ensure_image_build_job(ctx, workshop, &ns, &name).await;
    };

    ensure_parent_pod(ctx, workshop, &ns, &name, image_digest, was_suspended).await
}

/// Port d'ecoute par defaut du canal de controle HTTP de `vm-supervisor`
/// (`ATELIER_VM_CONTROL_ADDR`, cf. `crates/vm-supervisor/src/main.rs`) — pas
/// `AF_VSOCK` (reserve au canal guest<->hote a l'interieur d'un meme pod,
/// voir `docs/architecture/network-security.md`) : `controller` et
/// `vm-supervisor` sont deux process dans deux pods distincts, joignables
/// via le reseau normal du cluster (IP de pod).
const VM_CONTROL_PORT: u16 = 8081;

/// Fait converger vers l'etat suspendu : libere le pod parent (compute) mais
/// laisse intacts le ServiceAccount et le role OpenBao, pour une reprise
/// rapide sans reprovisionner les secrets du Workshop.
#[tracing::instrument(skip_all)]
async fn ensure_suspended(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
) -> anyhow::Result<WorkshopStatus> {
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let pod_name = format!("{name}-parent");

    let existing_pod = pods.get_opt(&pod_name).await?;
    let mut snapshot_digest = workshop
        .status
        .as_ref()
        .and_then(|s| s.snapshot_digest.clone());

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

    let image_digest = workshop
        .status
        .as_ref()
        .and_then(|s| s.image_digest.clone());
    let mut status = carry_forward_status(workshop, phase, image_digest);
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
            let digest = body
                .get("snapshotDigest")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            if digest.is_none() {
                tracing::warn!(
                    ?body,
                    "reponse de vm-supervisor sans snapshotDigest exploitable"
                );
            }
            digest
        }
        Err(err) => {
            tracing::warn!(%err, "reponse de vm-supervisor illisible, suspension sans snapshot");
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
        .patch(
            job_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&service_account),
        )
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
        .patch(
            job_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&role),
        )
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
        .patch(
            job_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&role_binding),
        )
        .await?;

    Ok(())
}

#[tracing::instrument(skip_all)]
async fn ensure_image_build_job(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
) -> anyhow::Result<WorkshopStatus> {
    // Cache partage (PVC) ou image-builder publie le rootfs construit ;
    // cree au besoin, idempotent, pas de owner reference (partage entre
    // Workshops, survit a la suppression de n'importe lequel d'entre eux).
    storage::ensure_image_cache_pvc(&ctx.client, ns, "20Gi").await?;

    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    let job_name = image_build_service_account(name);

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

    // Meme role/policy OpenBao que le pod parent (secret/workshops/<name>/*) :
    // permet a image-builder de lire d'eventuels identifiants git prives
    // (workshops/<name>/git) sans qu'aucun champ dedie n'existe dans le CRD
    // — le secret est simplement absent pour un depot public, lu au besoin
    // sinon (voir plus bas, resolve_git_credentials).
    if let Some(openbao_config) = &ctx.openbao {
        if let Err(err) = openbao::ensure_workshop_role(
            openbao_config,
            name,
            ns,
            &[&job_name, &format!("{name}-parent")],
        )
        .await
        {
            tracing::error!(%err, "provisioning du role OpenBao (image-builder) echoue");
        }
    }

    // Tache 3.1.4 (Jalon M3) : Virtual Key ephemere dediee a CE Job de
    // build, distincte de celle du pod parent (`ensure_parent_pod`) —
    // generee seulement si le Job n'existe pas encore (`spec.template` d'un
    // Job est immuable, voir plus bas : la regenerer a chaque reconcile
    // serait sans effet sur un Job deja cree, et gaspillerait des Virtual
    // Keys jamais utilisees a chaque passage). Best-effort : un echec de
    // generation retombe sur le jeton statique partage historique
    // (`ctx.llm_proxy_auth_token`) plutot que de bloquer tout le build.
    let job_already_exists = jobs.get_opt(&job_name).await?.is_some();
    let build_llm_auth_token: Option<String> = if job_already_exists {
        None
    } else if let Some(litellm_config) = &ctx.litellm {
        let client = litellm::LiteLlmClient::new(litellm_config.clone());
        match client
            .generate_virtual_key(
                &litellm::build_key_alias(name),
                &workshop.spec.owner_subject,
                workshop.spec.resources.max_llm_budget_usd,
                litellm::BUILD_VIRTUAL_KEY_TTL,
            )
            .await
        {
            Ok(virtual_key) => Some(virtual_key.key),
            Err(err) => {
                tracing::error!(%err, "generation de la Virtual Key LiteLLM (build) echouee, repli sur le jeton statique partage");
                ctx.llm_proxy_auth_token.clone()
            }
        }
    } else {
        ctx.llm_proxy_auth_token.clone()
    };

    let net_proxy_port: u16 = 3128;
    // Memes valeurs que `ensure_parent_pod` (`NET_PROXY_TRANSPARENT_HTTP_PORT`/
    // `_TLS_PORT`) : la VM builder utilise desormais elle aussi la
    // passerelle transparente (`enable_transparent_gateway`, appele
    // directement par `image-builder`, qui a `CAP_NET_ADMIN` comme
    // `vm-supervisor`), plus seulement `HTTP_PROXY` — voir
    // docs/architecture/network-security.md.
    let net_proxy_transparent_http_port: u16 = 3180;
    let net_proxy_transparent_tls_port: u16 = 3181;
    let registry_port = ctx
        .registry_addr
        .rsplit_once(':')
        .and_then(|(_, p)| p.parse::<u16>().ok())
        .unwrap_or(5000);

    let env = vec![
        env_var(
            "ATELIER_DEVCONTAINER_REPO",
            &workshop.spec.devcontainer.repo,
        ),
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
        env_var(
            "ATELIER_REGISTRY_INSECURE",
            &ctx.registry_insecure.to_string(),
        ),
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
        env_var(
            "ATELIER_BUILDER_REGISTRY_ALIAS",
            &format!("registry:{registry_port}"),
        ),
        env_var(
            "ATELIER_BUILDER_NET_PROXY_PORT",
            &net_proxy_port.to_string(),
        ),
        env_var(
            "ATELIER_BUILDER_NET_PROXY_TRANSPARENT_HTTP_PORT",
            &net_proxy_transparent_http_port.to_string(),
        ),
        env_var(
            "ATELIER_BUILDER_NET_PROXY_TRANSPARENT_TLS_PORT",
            &net_proxy_transparent_tls_port.to_string(),
        ),
    ]
    .into_iter()
    .chain(
        ctx.openbao
            .as_ref()
            .map(|c| env_var("OPENBAO_ADDR", &c.addr)),
    )
    // Necessaire au moment du build pour ecrire ANTHROPIC_AUTH_TOKEN dans
    // `/etc/environment` (`inject_net_proxy_config`, crates/image-builder) —
    // pas `ATELIER_LLM_PROXY_ADDR` : l'alias `llm-proxy` est resolu au
    // runtime par le `net-proxy` du pod parent, pas au moment du build.
    // Valeur = Virtual Key ephemere de ce Job si LiteLLM est configure et
    // que le Job vient d'etre cree (voir `build_llm_auth_token` plus haut),
    // sinon le jeton statique partage historique.
    .chain(
        build_llm_auth_token
            .as_ref()
            .map(|token| env_var("ATELIER_LLM_PROXY_AUTH_TOKEN", token)),
    )
    .collect::<Vec<_>>();

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
                                env_var(
                                    "ATELIER_NET_PROXY_LISTEN_ADDR",
                                    &format!("0.0.0.0:{net_proxy_port}"),
                                ),
                                env_var(
                                    "ATELIER_NET_PROXY_TRANSPARENT_HTTP_ADDR",
                                    &format!("0.0.0.0:{net_proxy_transparent_http_port}"),
                                ),
                                env_var(
                                    "ATELIER_NET_PROXY_TRANSPARENT_TLS_ADDR",
                                    &format!("0.0.0.0:{net_proxy_transparent_tls_port}"),
                                ),
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

    // `spec.template` d'un Job est immuable une fois cree (contrainte
    // Kubernetes, pas seulement une convention `atelier`) : contrairement
    // au pod parent (`ensure_parent_pod`), re-appliquer un Job deja existant
    // echoue toujours avec "field is immutable", meme a contenu strictement
    // identique — constate en pratique des le deuxieme reconcile d'un
    // Workshop reste en `BuildingImage`. On ne cree donc qu'une seule fois,
    // jamais de re-patch.
    let current = match jobs.get_opt(&job_name).await? {
        Some(existing) => Some(existing),
        None => {
            jobs.patch(
                &job_name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&job),
            )
            .await?;
            jobs.get_opt(&job_name).await?
        }
    };
    let job_failed = current
        .as_ref()
        .and_then(|j| j.status.as_ref())
        .and_then(|s| s.failed)
        .unwrap_or(0)
        > 0;
    let job_succeeded = current
        .as_ref()
        .and_then(|j| j.status.as_ref())
        .and_then(|s| s.succeeded)
        .unwrap_or(0)
        > 0;

    // Suite de la tache 3.1.4 : revoque la Virtual Key ephemere de ce Job
    // des qu'il a atteint un etat terminal (succes ou echec), qu'il ait ete
    // cree lors de CE reconcile ou d'un precedent — `apply()` continue
    // d'appeler cette fonction a chaque cycle tant que
    // `status.imageDigest` n'a pas ete patche par `image-builder` lui-meme,
    // donc plusieurs passages peuvent voir le Job deja termine. Best-effort
    // et non bloquant (comme le reste du provisioning ci-dessus) :
    // idempotent cote LiteLLM (404 traite comme un succes), un echec
    // n'empeche jamais la progression du Workshop.
    if let Some(litellm_config) = &ctx.litellm {
        if job_failed || job_succeeded {
            let client = litellm::LiteLlmClient::new(litellm_config.clone());
            if let Err(err) = client
                .delete_virtual_key(&litellm::build_key_alias(name))
                .await
            {
                tracing::warn!(%err, "revocation de la Virtual Key LiteLLM (build) echouee");
            }
        }
    }

    let phase = if job_failed {
        WorkshopPhase::Failed
    } else {
        WorkshopPhase::BuildingImage
    };

    Ok(carry_forward_status(workshop, phase, None))
}

#[tracing::instrument(skip_all)]
async fn ensure_parent_pod(
    ctx: &ReconcileCtx,
    workshop: &Workshop,
    ns: &str,
    name: &str,
    image_digest: String,
    resuming: bool,
) -> anyhow::Result<WorkshopStatus> {
    let service_accounts: Api<ServiceAccount> = Api::namespaced(ctx.client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(ctx.client.clone(), ns);
    let pod_name = format!("{name}-parent");
    let sa_name = pod_name.clone();
    // Determine en amont si CE pod va etre cree par cet appel (branche
    // `None` du `match` plus bas) : `spec.containers[].env` d'un pod deja
    // existant est immuable (meme contrainte que pour le Job de build), donc
    // regenerer une Virtual Key a chaque reconcile d'un pod deja en place
    // serait sans effet sur lui (il ne la recevrait jamais) tout en
    // consommant inutilement des Virtual Keys cote LiteLLM. Voir tache 3.1.3.
    let pod_will_be_created = pods.get_opt(&pod_name).await?.is_none();

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
        if let Err(err) = openbao::ensure_workshop_role(
            openbao_config,
            name,
            ns,
            &[&sa_name, &image_build_service_account(name)],
        )
        .await
        {
            tracing::error!(%err, "provisioning du role OpenBao echoue");
        }
        // Best-effort et non bloquant, comme le role ci-dessus : `net-proxy`
        // relit lui-meme ce secret (voir `openbao::ensure_session_auth`), un
        // echec ici ne fait que retarder la disponibilite du Basic Auth
        // guest, pas la reconciliation du Workshop.
        if let Err(err) = openbao::ensure_session_auth(openbao_config, name).await {
            tracing::error!(%err, "provisioning du secret session_auth OpenBao echoue");
        }
        // Meme raisonnement : `net-proxy` (cle publique) et `api-server`
        // (cle privee, role cluster-wide) relisent chacun ce secret de leur
        // cote — voir `openbao::ensure_ssh_key` (Jalon M4, tache 4.2.3).
        if let Err(err) = openbao::ensure_ssh_key(openbao_config, name).await {
            tracing::error!(%err, "provisioning du secret ssh_key OpenBao echoue");
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
    // Regle d'injection Git calculee cote controller (2.2.2, Jalon M2) :
    // jamais ecrite dans `workshop.spec` lui-meme (qui reste la source de
    // verite declarative de l'utilisateur), seulement ajoutee ici, a la
    // volee, a la liste serialisee vers `ATELIER_IDENTITY_INJECTION_RULES`.
    // Best-effort et non bloquant (comme le provisioning OpenBao ci-dessus) :
    // un echec de resolution du ClusterIP ne desactive que cette
    // fonctionnalite pour ce cycle de reconciliation, jamais toute la
    // reconciliation du Workshop — voir `crate::git_identity`.
    let mut effective_identity_injection_rules = workshop.spec.identity_injection_rules.clone();
    let mut git_host_alias: Option<HostAlias> = None;
    if let Some(git_config) = &ctx.git_identity {
        match git_identity::resolve_cluster_ip(&ctx.client, git_config).await {
            Ok(ip) => {
                effective_identity_injection_rules.push(git_identity::injection_rule(git_config));
                git_host_alias = Some(HostAlias {
                    ip: ip.to_string(),
                    hostnames: Some(vec![atelier_common::GIT_ALIAS_HOST.to_string()]),
                });
            }
            Err(err) => {
                tracing::warn!(
                    %err,
                    "resolution du ClusterIP de la forge Git echouee, injection Git desactivee pour ce cycle"
                );
            }
        }
    }
    // Taches 3.1.3 (Jalon M3) : a chaque (re)creation du pod parent
    // (provisioning initial OU reprise post-suspension, `resume` supprime
    // puis recree ce meme pod via `ensure_suspended`/ce chemin — voir
    // `pod_will_be_created` ci-dessus), genere une Virtual Key LiteLLM
    // dediee, isolee, a budget plafonne
    // (`Workshop.spec.resources.maxLlmBudgetUsd`) et TTL court
    // ([`litellm::VIRTUAL_KEY_TTL`]). Necessite OpenBao EN PLUS de LiteLLM :
    // c'est le seul canal disponible ici pour livrer cette cle a l'agent
    // sans jamais la faire transiter par la spec du pod (lisible via
    // `kubectl get pod -o yaml`) — voir le commentaire de tete de
    // `crate::litellm` pour la justification complete de ce choix
    // d'injection (regle d'injection `identity-proxy` generique,
    // reutilisee telle quelle, plutot qu'un nouveau canal metadata).
    if pod_will_be_created {
        if let (Some(litellm_config), Some(openbao_config)) = (&ctx.litellm, &ctx.openbao) {
            let client = litellm::LiteLlmClient::new(litellm_config.clone());
            match client
                .generate_virtual_key(
                    &litellm::workshop_key_alias(name),
                    &workshop.spec.owner_subject,
                    workshop.spec.resources.max_llm_budget_usd,
                    litellm::VIRTUAL_KEY_TTL,
                )
                .await
            {
                Ok(virtual_key) => {
                    match openbao::ensure_llm_virtual_key_secret(
                        openbao_config,
                        name,
                        &virtual_key.key,
                    )
                    .await
                    {
                        Ok(()) => {
                            effective_identity_injection_rules.push(IdentityInjectionRule {
                                host: litellm::LLM_PROXY_ALIAS_HOST.to_string(),
                                header: "Authorization".to_string(),
                                prefix: "Bearer ".to_string(),
                                secret_path: litellm::LLM_VIRTUAL_KEY_SECRET_PATH.to_string(),
                                field: litellm::LLM_VIRTUAL_KEY_SECRET_FIELD.to_string(),
                            });
                        }
                        Err(err) => {
                            tracing::error!(%err, "ecriture de la Virtual Key LiteLLM dans OpenBao echouee, isolation par Workshop desactivee pour cette session");
                        }
                    }
                }
                Err(err) => {
                    tracing::error!(%err, "generation de la Virtual Key LiteLLM echouee, isolation par Workshop desactivee pour cette session");
                }
            }
        }
    }
    let identity_injection_rules = serde_json::to_string(&effective_identity_injection_rules)
        .unwrap_or_else(|_| "[]".to_string());
    let tools = workshop.spec.tools.join(",");
    // IP guest fixe et deterministe : `vm-supervisor` cree toujours son TAP
    // avec le sous-reseau link-local d'index 0 (une seule microVM par pod),
    // ce qui fixe host_ip=169.254.0.1 / guest_ip=169.254.0.2 (arithmetique
    // de `fctools::extension::link_local::LinkLocalSubnet`, voir
    // `crates/firecracker/src/network.rs`).
    const VM_GUEST_IP: &str = "169.254.0.2";
    const NET_PROXY_PORT: u16 = 3128;
    // Ports d'ecoute "transparents" de net-proxy (voir
    // `crates/firecracker::network::NetworkSetup::enable_transparent_gateway`),
    // cibles des redirections iptables posees par `vm-supervisor` : la VM
    // n'a besoin de connaitre ni ces ports ni meme l'existence de
    // net-proxy — communs aux deux conteneurs (net-proxy les ecoute,
    // vm-supervisor y redirige), d'ou une seule paire de constantes.
    const NET_PROXY_TRANSPARENT_HTTP_PORT: u16 = 3180;
    const NET_PROXY_TRANSPARENT_TLS_PORT: u16 = 3181;
    // `identity-proxy`, `mcp-gateway` et `net-proxy` partagent le netns du
    // pod (tous les conteneurs d'un meme Pod) : joignables en `127.0.0.1`,
    // sans service Kubernetes ni DNS.
    const IDENTITY_PROXY_PORT: u16 = 3129;
    const MCP_GATEWAY_PORT: u16 = 3130;
    // Lie a `127.0.0.1` uniquement cote net-proxy (voir
    // `crates/net-proxy/src/admin.rs`) : joignable par mcp-gateway (meme
    // pod), structurellement injoignable par la VM (netns distincte).
    const NET_PROXY_ADMIN_PORT: u16 = 9001;
    // Control-plane `portforward` de net-proxy (`crates/net-proxy/src/main.rs`,
    // `DEFAULT_CONTROL_ADDR`) : contrairement a `NET_PROXY_ADMIN_PORT`, lie a
    // `0.0.0.0` — c'est par la que passe `crate::guest_probe`, depuis
    // l'exterieur du pod.
    const NET_PROXY_CONTROL_PORT: u16 = 9000;
    // Port `ttyd` dans le guest (voir `crates/api-server/src/terminal.rs`,
    // meme convention) : canari le plus rapide a demarrer parmi les
    // services embarques par le devcontainer, utilise comme signal de
    // readiness avant de marquer le Workshop `Running`.
    const GUEST_TERMINAL_PORT: u16 = 7681;
    // "Edge port" LocalStack (sert la quasi-totalite des API AWS emulees
    // sur ce seul port) : lie a `127.0.0.1` du pod, jamais expose
    // directement a la VM (seulement via l'alias `simulator` de net-proxy,
    // et seulement une fois `enable_simulator` appele, cf.
    // `crates/net-proxy/src/admin.rs`).
    const SIMULATOR_PORT: u16 = 4566;
    let simulator_enabled = workshop.spec.tools.iter().any(|t| t == "enable_simulator");
    // Base des jails Firecracker, partagee via le volume "jailer" ci-dessus
    // (defaut binaire identique, mais fixe explicitement ici : le controller
    // et `mcp-gateway` doivent s'accorder sur le meme chemin absolu sans
    // dependre d'un defaut cote binaire qui pourrait changer independamment).
    const JAILER_CHROOT_BASE_DIR: &str = "/srv/jailer";
    const VM_JAIL_ID: &str = "atelier-vm";
    const VM_VSOCK_UDS_FILENAME: &str = "vsock.sock";
    let jailer_mount = VolumeMount {
        name: "jailer".to_string(),
        mount_path: JAILER_CHROOT_BASE_DIR.to_string(),
        read_only: Some(false),
        ..Default::default()
    };
    // Le jailer insere le nom de l'executable comme composant de chemin
    // (`--chroot-base-dir/<exec_file_name>/<jail_id>/root/`, constate en
    // pratique par inspection reelle de l'arborescence produite) : pas
    // `<chroot_base_dir>/<jail_id>/root/` comme on pourrait le supposer par
    // analogie avec `--chroot-base-dir` seul.
    let vsock_uds_path =
        format!("{JAILER_CHROOT_BASE_DIR}/firecracker/{VM_JAIL_ID}/root/{VM_VSOCK_UDS_FILENAME}");

    let mut pod = Pod {
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
            // Entree `/etc/hosts` posee par Kubernetes lui-meme sur TOUS les
            // conteneurs du pod (net-proxy, identity-proxy, etc, qui
            // partagent le netns du pod) : c'est ce qui rend
            // `atelier_common::GIT_ALIAS_HOST` reellement resolvable par
            // `identity-proxy` au moment de relayer vers la vraie
            // destination — voir `crate::git_identity` et le commentaire de
            // tete de `crates/net-proxy/src/internal.rs`. Absent si la
            // resolution du ClusterIP a echoue ou si la fonctionnalite n'est
            // pas configuree (`git_host_alias` est alors `None`).
            host_aliases: git_host_alias.clone().map(|alias| vec![alias]),
            volumes: Some(vec![
                Volume {
                    name: "cache".to_string(),
                    persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
                        claim_name: storage::IMAGE_CACHE_PVC_NAME.to_string(),
                        read_only: Some(false),
                    }),
                    ..Default::default()
                },
                // Partage entre `vm-supervisor` et `mcp-gateway` : le socket
                // Unix "principal" du device vsock est cree par Firecracker
                // *a l'interieur du jail* (`chroot_base_dir/jail_id/root/`,
                // voir `crates/firecracker/src/vm.rs`) — `mcp-gateway` doit
                // voir ce meme chemin hote pour y lier a son tour le socket
                // "<uds>_<port>" qui recoit les connexions initiees par le
                // guest (convention Firecracker). `emptyDir` : contenu
                // ephemere, scope au pod, pas de persistance necessaire.
                Volume {
                    name: "jailer".to_string(),
                    empty_dir: Some(EmptyDirVolumeSource::default()),
                    ..Default::default()
                },
            ]),
            containers: vec![
                Container {
                    name: "vm-supervisor".into(),
                    image: Some(VM_SUPERVISOR_IMAGE.into()),
                    env: Some(
                        vec![
                            env_var("ATELIER_VM_ROOTFS_PATH", &rootfs_path),
                            env_var("ATELIER_VM_SNAPSHOT_DIR", &snapshot_dir),
                            env_var("ATELIER_NET_PROXY_PORT", &NET_PROXY_PORT.to_string()),
                            env_var(
                                "ATELIER_NET_PROXY_TRANSPARENT_HTTP_PORT",
                                &NET_PROXY_TRANSPARENT_HTTP_PORT.to_string(),
                            ),
                            env_var(
                                "ATELIER_NET_PROXY_TRANSPARENT_TLS_PORT",
                                &NET_PROXY_TRANSPARENT_TLS_PORT.to_string(),
                            ),
                            env_var("ATELIER_VM_CHROOT_BASE_DIR", JAILER_CHROOT_BASE_DIR),
                            env_var("ATELIER_VM_JAIL_ID", VM_JAIL_ID),
                            env_var("ATELIER_VM_VSOCK_UDS_FILENAME", VM_VSOCK_UDS_FILENAME),
                        ]
                        .into_iter()
                        .chain(
                            memory_to_mib(&workshop.spec.resources.memory)
                                .map(|mib| env_var("ATELIER_VM_MEM_MIB", &mib.to_string())),
                        )
                        .chain(
                            cpu_to_vcpu_count(&workshop.spec.resources.cpu)
                                .map(|vcpus| env_var("ATELIER_VM_VCPU_COUNT", &vcpus.to_string())),
                        )
                        .collect::<Vec<_>>(),
                    ),
                    volume_mounts: Some(vec![cache_mount, jailer_mount.clone()]),
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
                    env: Some(
                        vec![
                            env_var("ATELIER_EGRESS_ALLOWLIST", &egress_allowlist),
                            env_var(
                                "ATELIER_NET_PROXY_LISTEN_ADDR",
                                &format!("0.0.0.0:{NET_PROXY_PORT}"),
                            ),
                            env_var(
                                "ATELIER_NET_PROXY_TRANSPARENT_HTTP_ADDR",
                                &format!("0.0.0.0:{NET_PROXY_TRANSPARENT_HTTP_PORT}"),
                            ),
                            env_var(
                                "ATELIER_NET_PROXY_TRANSPARENT_TLS_ADDR",
                                &format!("0.0.0.0:{NET_PROXY_TRANSPARENT_TLS_PORT}"),
                            ),
                            env_var("ATELIER_VM_ADDR", VM_GUEST_IP),
                            // Tout l'egress autorise par net-proxy est chaine
                            // vers identity-proxy des lors qu'il est configure
                            // (voir docs/architecture/network-security.md,
                            // "identity-proxy : jamais joint directement par la
                            // VM") : c'est lui, pas la VM, qui decide ensuite
                            // d'injecter un credential ou de relayer tel quel.
                            env_var(
                                "ATELIER_IDENTITY_PROXY_ADDR",
                                &format!("127.0.0.1:{IDENTITY_PROXY_PORT}"),
                            ),
                            // Alias interne, jamais dans l'allowlist de
                            // l'utilisateur : voir `crates/net-proxy/src/internal.rs`.
                            env_var(
                                "ATELIER_MCP_GATEWAY_ADDR",
                                &format!("127.0.0.1:{MCP_GATEWAY_PORT}"),
                            ),
                            env_var("ATELIER_WORKSHOP_NAME", name),
                        ]
                        .into_iter()
                        .chain(simulator_enabled.then(|| {
                            env_var(
                                "ATELIER_SIMULATOR_ADDR",
                                &format!("127.0.0.1:{SIMULATOR_PORT}"),
                            )
                        }))
                        // Service global du cluster (voir `deploy/dev/llm-proxy/`),
                        // pas un sidecar de ce pod : toujours cable des que
                        // configure, contrairement a `simulator` (gate par
                        // `Workshop.spec.tools`) — voir `ReconcileCtx::llm_proxy_addr`.
                        .chain(
                            ctx.llm_proxy_addr
                                .as_ref()
                                .map(|addr| env_var("ATELIER_LLM_PROXY_ADDR", addr)),
                        )
                        // Alias `git.atelier.internal` (2.2.3, Jalon M2) :
                        // pointe directement vers identity-proxy (meme
                        // adresse que `ATELIER_IDENTITY_PROXY_ADDR`
                        // ci-dessus), pour que ce nom bypass l'allowlist
                        // comme les autres alias internes — voir
                        // `crates/net-proxy/src/internal.rs` et
                        // `crate::git_identity`. Cable seulement si la
                        // resolution du ClusterIP a reussi (`git_host_alias`),
                        // sinon `identity-proxy` n'aurait de toute facon
                        // aucun moyen de resoudre ce nom (pas de `hostAlias`
                        // pose sur le pod, voir plus bas).
                        .chain(git_host_alias.as_ref().map(|_| {
                            env_var(
                                "ATELIER_GIT_ALIAS_ADDR",
                                &format!("127.0.0.1:{IDENTITY_PROXY_PORT}"),
                            )
                        }))
                        // Permet a net-proxy de lire lui-meme le secret
                        // `session_auth` (mot de passe Basic Auth guest, voir
                        // `openbao::ensure_session_auth`) via son propre login
                        // Kubernetes-auth, plutot que de le faire transiter en
                        // clair par une variable d'environnement du pod — voir
                        // `crates/net-proxy/src/session_auth.rs`.
                        .chain(
                            ctx.openbao
                                .as_ref()
                                .map(|c| env_var("OPENBAO_ADDR", &c.addr)),
                        )
                        .collect::<Vec<_>>(),
                    ),
                    ..Default::default()
                },
                Container {
                    name: "identity-proxy".into(),
                    image: Some(IDENTITY_PROXY_IMAGE.into()),
                    env: Some(
                        vec![
                            env_var(
                                "ATELIER_IDENTITY_PROXY_LISTEN_ADDR",
                                &format!("0.0.0.0:{IDENTITY_PROXY_PORT}"),
                            ),
                            env_var(
                                "ATELIER_IDENTITY_INJECTION_RULES",
                                &identity_injection_rules,
                            ),
                            env_var("ATELIER_WORKSHOP_NAME", name),
                        ]
                        .into_iter()
                        .chain(
                            ctx.openbao
                                .as_ref()
                                .map(|c| env_var("OPENBAO_ADDR", &c.addr)),
                        )
                        .collect::<Vec<_>>(),
                    ),
                    ..Default::default()
                },
                Container {
                    name: "mcp-gateway".into(),
                    image: Some(MCP_GATEWAY_IMAGE.into()),
                    env: Some(
                        vec![
                            env_var(
                                "ATELIER_MCP_GATEWAY_LISTEN_ADDR",
                                &format!("0.0.0.0:{MCP_GATEWAY_PORT}"),
                            ),
                            env_var("ATELIER_WORKSHOP_NAME", name),
                            env_var("ATELIER_TOOLS", &tools),
                            env_var(
                                "ATELIER_NET_PROXY_ADMIN_ADDR",
                                &format!("127.0.0.1:{NET_PROXY_ADMIN_PORT}"),
                            ),
                            env_var("ATELIER_MCP_GATEWAY_VSOCK_UDS_PATH", &vsock_uds_path),
                        ]
                        .into_iter()
                        .chain(
                            ctx.openbao
                                .as_ref()
                                .map(|c| env_var("OPENBAO_ADDR", &c.addr)),
                        )
                        .collect::<Vec<_>>(),
                    ),
                    volume_mounts: Some(vec![jailer_mount]),
                    ..Default::default()
                },
            ]
            .into_iter()
            .chain(simulator_enabled.then(|| Container {
                name: "simulator".into(),
                image: Some(SIMULATOR_IMAGE.into()),
                env: Some(vec![
                    // "Edge port" unique : pas besoin d'activer les
                    // services AWS individuellement, LocalStack les sert
                    // tous derriere ce port des qu'on leur parle.
                    env_var("GATEWAY_LISTEN", &format!("127.0.0.1:{SIMULATOR_PORT}")),
                ]),
                ..Default::default()
            }))
            .collect(),
            ..Default::default()
        }),
        ..Default::default()
    };

    // Hash du `PodSpec` tel que CE reconcile le construirait aujourd'hui —
    // calcule avant toute mutation des metadonnees du pod, pour que la
    // valeur ne depende jamais d'elle-meme. Voir `TEMPLATE_HASH_ANNOTATION`
    // et la tache 6.4.2 (Jalon M6) pour l'usage qui en est fait ci-dessous.
    let desired_template_hash = pod_spec_template_hash(pod.spec.as_ref());
    pod.metadata
        .annotations
        .get_or_insert_with(BTreeMap::new)
        .insert(
            TEMPLATE_HASH_ANNOTATION.to_string(),
            desired_template_hash.clone(),
        );

    // La plupart des champs de `spec.containers` (ex: `env`) sont immuables
    // une fois le pod cree — contrainte Kubernetes, pas contournable par
    // Server-Side Apply meme avec `.force()`. Un pod parent deja existant
    // peut heberger une microVM active (session utilisateur en cours,
    // snapshot restaure) : le RECREER a chaque changement de spec (ex:
    // nouvelle variable d'environnement ajoutee a `net-proxy` par une
    // nouvelle version du controller) romprait a tort cette session — jamais
    // acceptable. Meme strategie que pour le Job image-builder ci-dessus :
    // on ne cree qu'une seule fois, jamais de re-patch d'un pod deja
    // existant. Une mise a jour de spec ne prend effet qu'au prochain cycle
    // suspend/resume (`ensure_suspended` supprime le pod, `ensure_parent_pod`
    // le recree alors avec la spec a jour).
    let current = match pods.get_opt(&pod_name).await? {
        Some(existing) => Some(existing),
        None => {
            pods.patch(
                &pod_name,
                &PatchParams::apply(FIELD_MANAGER).force(),
                &Patch::Apply(&pod),
            )
            .await?;
            pods.get_opt(&pod_name).await?
        }
    };
    // Tache 6.4.2 (Jalon M6) : le pod deja en place (cree lors d'un cycle
    // anterieur, potentiellement par une version anterieure du controller)
    // porte le hash du template avec lequel il a ete provisionne. S'il
    // diverge du hash que CE reconcile construirait aujourd'hui, la microVM
    // active de ce pod n'est PAS redemarree de force (voir commentaire
    // ci-dessus) : on se contente de signaler dans le statut qu'un
    // redemarrage (cycle suspend/resume manuel, ou prochaine liberation du
    // pod) sera necessaire pour converger vers le nouveau template. Un pod
    // fraichement cree porte forcement le hash courant (ecrit juste
    // au-dessus) : jamais `NeedsRestartForUpgrade` a l'issue de sa propre
    // creation.
    let upgrade_state = current.as_ref().and_then(|p| {
        let existing_hash = p
            .metadata
            .annotations
            .as_ref()?
            .get(TEMPLATE_HASH_ANNOTATION)?;
        (existing_hash != &desired_template_hash)
            .then_some(WorkshopUpgradeState::NeedsRestartForUpgrade)
    });
    let pod_running = current
        .as_ref()
        .and_then(|p| p.status.as_ref())
        .and_then(|s| s.phase.as_deref())
        == Some("Running");
    // Le pod Kubernetes passe `Running` des que le kernel de la microVM a
    // booté — bien avant que systemd, a l'interieur du guest, ait fini de
    // demarrer `ttyd`/`code-server` (constate en pratique). Sonder `ttyd`
    // (le plus rapide des deux a demarrer) via `net-proxy` avant d'annoncer
    // `Running` evite ce mensonge : le badge de statut refletera alors
    // reellement "utilisable", pas seulement "le pod a booté". Ne sonde que
    // si le pod est deja `Running` (sinon le control-plane net-proxy
    // lui-meme ne repond pas encore) ET que le Workshop n'etait pas deja
    // confirme `Running` lors du reconcile precedent — sans ce
    // court-circuit, cette sonde tournerait a chaque reconcile (toutes les
    // 5s, indefiniment) pour un Workshop stable, juste pour reconfirmer un
    // etat deja connu.
    let already_confirmed_running =
        workshop.status.as_ref().map(|s| &s.phase) == Some(&WorkshopPhase::Running);
    let guest_ready = pod_running
        && (already_confirmed_running
            || match current
                .as_ref()
                .and_then(|p| p.status.as_ref())
                .and_then(|s| s.pod_ip.clone())
            {
                Some(pod_ip) => {
                    crate::guest_probe::guest_tcp_port_open(
                        &pod_ip,
                        NET_PROXY_CONTROL_PORT,
                        GUEST_TERMINAL_PORT,
                    )
                    .await
                }
                None => false,
            });
    let phase = match (guest_ready, resuming) {
        (true, _) => WorkshopPhase::Running,
        (false, true) => WorkshopPhase::Resuming,
        (false, false) => WorkshopPhase::Provisioning,
    };

    let mut status = carry_forward_status(workshop, phase, Some(image_digest));
    status.pod_name = Some(pod_name);
    status.upgrade_state = upgrade_state;
    Ok(status)
}

/// Hash deterministe du `PodSpec` du pod parent, utilise pour detecter un
/// changement de template entre deux versions du controller (tache 6.4.2,
/// Jalon M6). Se base sur la serialisation JSON du spec plutot que sur
/// `Hash`/`derive` (non implemente par les types `k8s-openapi`) : suffisant
/// ici, ce hash n'a besoin d'aucune propriete cryptographique, seulement
/// d'etre stable pour un meme spec et de changer quand le spec change.
fn pod_spec_template_hash(spec: Option<&PodSpec>) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    // Un `PodSpec` vide (jamais le cas en pratique ici) hash quand meme de
    // facon stable, plutot que de paniquer sur un `unwrap()`.
    let json = spec
        .map(|s| serde_json::to_string(s).unwrap_or_default())
        .unwrap_or_default();
    json.hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
        // Champ ajoute par le Jalon M6 (charts/atelier, voir crates/common/src/crd.rs) :
        // simple report de la valeur precedente par defaut. La reconciliation
        // du pod parent (`ensure_parent_pod`, tache 6.4.2) recalcule et
        // ecrase ensuite explicitement `status.upgrade_state` avec un
        // resultat a jour (voir `pod_spec_template_hash`) ; les autres
        // chemins de reconciliation (build d'image, etc) se contentent de
        // reporter la derniere valeur connue.
        upgrade_state: workshop
            .status
            .as_ref()
            .and_then(|s| s.upgrade_state.clone()),
        conditions: BTreeMap::new(),
    }
}

/// Nom du ServiceAccount du Job `image-builder` d'un Workshop — memes regles
/// de nommage que `job_name` (`ensure_image_build_job`), extrait ici pour
/// que `ensure_parent_pod` puisse le referencer sans dupliquer le format.
fn image_build_service_account(workshop_name: &str) -> String {
    format!("{workshop_name}-image-build")
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

#[cfg(test)]
mod template_hash_tests {
    use super::pod_spec_template_hash;
    use k8s_openapi::api::core::v1::{Container, PodSpec};

    #[test]
    fn identical_specs_hash_identically() {
        let spec = PodSpec {
            containers: vec![Container {
                name: "vm-supervisor".into(),
                image: Some("atelier-vm-supervisor:dev".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        assert_eq!(
            pod_spec_template_hash(Some(&spec)),
            pod_spec_template_hash(Some(&spec))
        );
    }

    #[test]
    fn differing_container_image_changes_the_hash() {
        let mut spec = PodSpec {
            containers: vec![Container {
                name: "vm-supervisor".into(),
                image: Some("atelier-vm-supervisor:dev".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let before = pod_spec_template_hash(Some(&spec));
        spec.containers[0].image = Some("atelier-vm-supervisor:v2".into());
        let after = pod_spec_template_hash(Some(&spec));
        assert_ne!(
            before, after,
            "changer l'image d'un conteneur doit changer le hash du template"
        );
    }
}

#[cfg(test)]
mod resource_conversion_tests {
    use super::{cpu_to_vcpu_count, memory_to_mib};

    #[test]
    fn memory_binary_suffixes() {
        assert_eq!(memory_to_mib("512Mi"), Some(512));
        assert_eq!(memory_to_mib("2Gi"), Some(2048));
        assert_eq!(memory_to_mib("1024Ki"), Some(1));
    }

    #[test]
    fn memory_decimal_suffixes() {
        assert_eq!(memory_to_mib("1G"), Some(954));
        assert_eq!(memory_to_mib("500M"), Some(477));
    }

    #[test]
    fn memory_bare_bytes() {
        assert_eq!(memory_to_mib("1048576"), Some(1));
    }

    #[test]
    fn memory_malformed_returns_none() {
        assert_eq!(memory_to_mib("not-a-quantity"), None);
    }

    #[test]
    fn cpu_whole_cores() {
        assert_eq!(cpu_to_vcpu_count("1"), Some(1));
        assert_eq!(cpu_to_vcpu_count("2"), Some(2));
    }

    #[test]
    fn cpu_millicores_round_up_to_at_least_one() {
        assert_eq!(cpu_to_vcpu_count("500m"), Some(1));
        assert_eq!(cpu_to_vcpu_count("1500m"), Some(2));
    }

    #[test]
    fn cpu_malformed_returns_none() {
        assert_eq!(cpu_to_vcpu_count("lots"), None);
    }
}
