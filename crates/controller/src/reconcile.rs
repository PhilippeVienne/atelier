use crate::{git_identity, litellm, openbao, storage};
use atelier_common::{
    IdentityInjectionRule, Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopStatus,
    WorkshopUpgradeState,
};
use futures::StreamExt;
use k8s_openapi::api::batch::v1::{Job, JobSpec};
use k8s_openapi::api::core::v1::{
    Capabilities, ConfigMap, ConfigMapVolumeSource, Container, EmptyDirVolumeSource, EnvVar,
    HostAlias, PersistentVolumeClaimVolumeSource, Pod, PodSpec, PodTemplateSpec,
    ResourceRequirements, SecurityContext, ServiceAccount, Volume, VolumeMount,
};
use k8s_openapi::api::rbac::v1::{PolicyRule, Role, RoleBinding, RoleRef, Subject};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{Api, DeleteParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::runtime::controller::{Action, Controller};
use kube::runtime::finalizer::{self, Event};
use kube::runtime::watcher;
use kube::{Client, Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

const FIELD_MANAGER: &str = "atelier-controller";
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
/// Chemin de montage de la CA d'entreprise dans le Job `image-builder`
/// (tache 11.2) — cle `ca.crt` de la `ConfigMap` `ctx.ca_bundle_configmap`
/// (elle-meme creee par le chart, cle definie dans
/// `charts/atelier/templates/infra/ca-bundle-configmap.yaml`, tache 11.1).
const CA_BUNDLE_MOUNT_PATH: &str = "/etc/atelier/ca/ca.crt";

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
    /// La meme chose, mais telle qu'un POD doit la voir (`net-proxy` s'en
    /// sert comme cible de l'alias `llm-proxy`). Distincte de
    /// `llm_proxy_addr`, qui est l'adresse vue par le controller pour ses
    /// propres appels d'administration : les deux different des que le
    /// controller tourne hors cluster (port-forward). Meme convention que
    /// `OpenBaoConfig::pod_addr`.
    pub llm_proxy_pod_addr: Option<String>,
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
    /// Prefixe de registre pour les images des composants embarques dans
    /// les pods Workshop (vm-supervisor/net-proxy/identity-proxy/
    /// mcp-gateway, Job image-builder) - a NE PAS confondre avec
    /// `registry_addr` ci-dessus, qui sert au rootfs/kernel OCI de la
    /// devcontainer construite par image-builder et consommee par
    /// vm-supervisor (chemin completement independant, jamais pull par
    /// kubelet). `None` (defaut dev) : noms bruts "atelier-<composant>:dev",
    /// presents sur le noeud via `kind load docker-image`
    /// (`deploy/dev/local-stack.sh`), aucun pull reseau necessaire. Sur un
    /// cluster reel (EKS...), doit pointer vers un registre ayant recu ces
    /// images au prealable (voir `deploy/terraform/aws/mirror-images.sh`).
    pub component_image_registry: Option<String>,
    /// Configuration S3 (spec `docs/specs/13-image-cache-offload.md`, tache
    /// 8.3) : le controller lui-meme ne s'en sert pas, il ne fait que la
    /// retransmettre en variables d'environnement au Job `image-builder`
    /// (`ensure_image_build_job`), qui televerse son `rootfs.ext4` publie
    /// vers `S3_BUCKET_IMAGE_CACHE`. `None` : fonctionnalite desactivee,
    /// meme convention que `openbao`/`litellm` ci-dessus — l'offload est
    /// alors simplement saute (best effort, jamais bloquant pour le build).
    pub s3: Option<atelier_common::storage::S3Config>,
    /// Endpoint S3 tel qu'un POD doit le voir, distinct de `s3.endpoint` —
    /// meme raison et meme convention que `OpenBaoConfig::pod_addr`/
    /// `llm_proxy_pod_addr` : en developpement le controller tourne HORS
    /// cluster et joint S3/RustFS par un port-forward (`127.0.0.1:9000`),
    /// adresse qui ne veut rien dire depuis l'interieur du Job
    /// `image-builder` — constate empiriquement en verifiant cette meme
    /// tache (8.3) : sans ce dedoublement, le Job recevait
    /// `S3_ENDPOINT=http://127.0.0.1:9000`. Egal a `s3.endpoint` par
    /// defaut : aucun effet en production, ou le controller tourne dans le
    /// cluster (memes adresses des deux cotes).
    pub s3_pod_endpoint: Option<String>,
    /// Nom de la `ConfigMap` (meme namespace que le Job) portant la CA
    /// d'entreprise a faire confiance (spec docs/specs/15-souverainete-
    /// airgap-inference-gpu.md §3.2/§3.3, tache 11.2) — cree par le chart
    /// quand `tls.customCaBundle` est renseignee
    /// (`charts/atelier/templates/infra/ca-bundle-configmap.yaml`, tache
    /// 11.1). `None` : fonctionnalite desactivee, meme convention que
    /// `openbao`/`litellm` — aucun volume/variable supplementaire sur le Job
    /// `image-builder`.
    pub ca_bundle_configmap: Option<String>,
}

impl ReconcileCtx {
    /// `name` sans prefixe "atelier-" ni suffixe ":dev" (ex: "net-proxy").
    fn component_image(&self, name: &str) -> String {
        component_image_ref(self.component_image_registry.as_deref(), name)
    }
}

/// Fonction libre (donc testable sans construire un `ReconcileCtx` complet,
/// qui exige un `kube::Client` reel) derriere `ReconcileCtx::component_image`.
fn component_image_ref(registry: Option<&str>, name: &str) -> String {
    match registry {
        Some(registry) => format!("{registry}/atelier-{name}:dev"),
        None => format!("atelier-{name}:dev"),
    }
}

#[cfg(test)]
mod component_image_tests {
    use super::component_image_ref;

    #[test]
    fn falls_back_to_the_bare_dev_tag_without_a_registry() {
        assert_eq!(
            component_image_ref(None, "net-proxy"),
            "atelier-net-proxy:dev"
        );
    }

    #[test]
    fn prefixes_with_the_configured_registry() {
        assert_eq!(
            component_image_ref(
                Some("123456789012.dkr.ecr.eu-west-3.amazonaws.com/atelier"),
                "net-proxy"
            ),
            "123456789012.dkr.ecr.eu-west-3.amazonaws.com/atelier/atelier-net-proxy:dev"
        );
    }
}

pub async fn run() -> anyhow::Result<()> {
    let client = Client::try_default().await?;
    let openbao = openbao::config_from_env()?;
    let registry_addr =
        std::env::var("ATELIER_REGISTRY_ADDR").unwrap_or_else(|_| "localhost:5000".to_string());
    let registry_insecure = std::env::var("ATELIER_REGISTRY_INSECURE")
        .map(|v| v == "true")
        .unwrap_or(false);
    // `.filter(...)` et pas seulement `.ok()` : une variable DEFINIE mais
    // VIDE (cas reel — `deploy/dev/local-stack/env.sh` genere
    // `ATELIER_LLM_PROXY_AUTH_TOKEN=""` quand le bloc LiteLLM optionnel du
    // script ne s'est pas declenche) passait le test et etait injectee telle
    // quelle dans `/etc/environment` du guest. L'agent du Workshop
    // s'authentifiait alors aupres de LiteLLM avec un jeton vide et
    // n'obtenait aucune reponse, sans la moindre erreur visible. Une valeur
    // vide vaut "non configure", comme l'absence de la variable.
    let llm_proxy_addr = std::env::var("ATELIER_LLM_PROXY_ADDR")
        .ok()
        .filter(|v| !v.trim().is_empty());
    // Adresse injectee dans les pods, distincte de celle qu'utilise le
    // controller pour ses propres appels d'administration — meme raison et
    // meme convention que `OpenBaoConfig::pod_addr` : en developpement le
    // controller tourne HORS cluster et joint LiteLLM par un port-forward
    // (`127.0.0.1:4000`), adresse qui ne veut rien dire dans un pod. Sans ce
    // dedoublement, `net-proxy` recevait `127.0.0.1:4000` comme cible de
    // l'alias `llm-proxy`. Egale a `llm_proxy_addr` par defaut : aucun effet
    // en production, ou le controller tourne dans le cluster.
    let llm_proxy_pod_addr = std::env::var("ATELIER_LLM_PROXY_POD_ADDR")
        .ok()
        .or_else(|| llm_proxy_addr.clone());
    let llm_proxy_auth_token = std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN")
        .ok()
        .filter(|v| !v.trim().is_empty());
    let git_identity = git_identity::config_from_env();
    // `litellm` sert aux appels d'ADMINISTRATION faits par le controller
    // lui-meme (generation de Virtual Keys) : c'est bien `llm_proxy_addr`,
    // l'adresse vue depuis le controller, qu'il lui faut.
    let litellm_config =
        litellm::config_from_env(llm_proxy_addr.clone(), llm_proxy_auth_token.clone());
    let component_image_registry = std::env::var("ATELIER_COMPONENT_IMAGE_REGISTRY").ok();
    let s3 = atelier_common::storage::config_from_env()?;
    let s3_pod_endpoint = std::env::var("ATELIER_S3_POD_ENDPOINT")
        .ok()
        .or_else(|| s3.as_ref().map(|c| c.endpoint.clone()));
    let ca_bundle_configmap = std::env::var("ATELIER_CA_BUNDLE_CONFIGMAP")
        .ok()
        .filter(|v| !v.trim().is_empty());
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
                llm_proxy_pod_addr,
                llm_proxy_auth_token,
                git_identity,
                litellm: litellm_config,
                component_image_registry,
                s3,
                s3_pod_endpoint,
                ca_bundle_configmap,
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
    let ns = workshop.namespace().unwrap_or_else(|| "default".into());

    if let Some(openbao_config) = &ctx.openbao {
        openbao::delete_workshop_role(openbao_config, &name).await?;
    }

    // Tache 8.1 (spec docs/specs/13-image-cache-offload.md) : corrige une
    // fuite reelle constatee empiriquement (103 Go de snapshots orphelins
    // mesures sur l'instance de dev) — sans ceci, le snapshot Firecracker de
    // ce Workshop (`storage::snapshot_cache_subdir`) survit indefiniment sur
    // le PVC de cache partage, meme apres suppression du Workshop. Best
    // effort et non bloquant, meme discipline que le reste de cette
    // fonction : un echec ici ne doit jamais empecher la suppression reelle
    // du Workshop.
    if let Err(err) = cleanup_snapshot_cache(ctx, &ns, &name).await {
        tracing::warn!(%err, "nettoyage du snapshot en cache (cleanup) echoue, finalizer non bloque");
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
        // Ne fait jamais echouer le finalizer sur une erreur RESEAU (LiteLLM
        // injoignable — ex: DNS interne au cluster non resolvable depuis un
        // controller lance hors cluster, cas de dev documente) : seul le 404
        // etait deja tolere par `delete_virtual_key` lui-meme (`?` propage
        // toute autre erreur, y compris une simple indisponibilite
        // reseau) — sans ce garde, un Workshop devient indefiniment
        // impossible a supprimer des que LiteLLM est injoignable au moment
        // du nettoyage, constate en pratique (finalizer bloque en boucle).
        // Meme tolerance, en miroir, que la generation de la cle a la
        // creation (`ensure_image_build_job`, repli sur le jeton statique).
        if let Err(err) = client
            .delete_virtual_key(&litellm::workshop_key_alias(&name))
            .await
        {
            tracing::warn!(%err, "revocation de la Virtual Key LiteLLM (cleanup) echouee, finalizer non bloque");
        }
    }

    Ok(())
}

/// Lance un Job ephemere qui monte le PVC de cache partage et supprime le
/// sous-repertoire de snapshot de CE Workshop (`storage::
/// snapshot_cache_subdir`) — le controller lui-meme ne monte jamais ce PVC
/// (il ne fait que creer des objets Kubernetes qui le referencent, voir
/// `ensure_image_build_job`), un Job est donc necessaire pour toute
/// operation sur son CONTENU. `ttl_seconds_after_finished` : Kubernetes
/// nettoie lui-meme l'objet Job une fois termine, pas besoin de le faire
/// depuis ce code.
///
/// Idempotent via `Patch::Apply` (comme `storage::ensure_image_cache_pvc`) :
/// un retry apres un echec anterieur ne heurte pas un Job deja cree avec le
/// meme nom — contrairement a `Api::create`, qui echouerait sur un conflit
/// 409 et compliquerait inutilement l'appelant (deja tolerant aux erreurs,
/// voir `cleanup`).
async fn cleanup_snapshot_cache(ctx: &ReconcileCtx, ns: &str, name: &str) -> anyhow::Result<()> {
    let subdir = storage::snapshot_cache_subdir(ns, name);
    let mount_path = storage::IMAGE_CACHE_MOUNT_PATH;

    let cache_volume = Volume {
        name: "cache".to_string(),
        persistent_volume_claim: Some(PersistentVolumeClaimVolumeSource {
            claim_name: storage::IMAGE_CACHE_PVC_NAME.to_string(),
            read_only: Some(false),
        }),
        ..Default::default()
    };
    let cache_mount = VolumeMount {
        name: "cache".to_string(),
        mount_path: mount_path.to_string(),
        ..Default::default()
    };

    let job = Job {
        metadata: ObjectMeta {
            name: Some(format!("{name}-snapshot-cleanup")),
            namespace: Some(ns.to_string()),
            ..Default::default()
        },
        spec: Some(JobSpec {
            backoff_limit: Some(2),
            ttl_seconds_after_finished: Some(300),
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    restart_policy: Some("Never".to_string()),
                    volumes: Some(vec![cache_volume]),
                    containers: vec![Container {
                        name: "cleanup".to_string(),
                        // Image deja utilisee ailleurs dans ce projet (voir
                        // `deploy/dev/*`), pas de nouveau fournisseur
                        // d'image a faire confiance pour une simple
                        // suppression de repertoire.
                        image: Some("busybox:1.36".to_string()),
                        command: Some(vec![
                            "rm".to_string(),
                            "-rf".to_string(),
                            format!("{mount_path}/{subdir}"),
                        ]),
                        volume_mounts: Some(vec![cache_mount]),
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

    let jobs: Api<Job> = Api::namespaced(ctx.client.clone(), ns);
    jobs.patch(
        &format!("{name}-snapshot-cleanup"),
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&job),
    )
    .await?;
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

/// Role + RoleBinding pour le ServiceAccount du POD PARENT lui-meme (pas
/// celui, distinct, du Job image-builder ci-dessus) : `get`+`patch` sur
/// `workshops` (le CRD complet, pas seulement `/status`), scope a ce seul
/// Workshop via `resourceNames`. Necessaire a `mcp-gateway` (tache 9.4,
/// tool `request_simulator`) pour ajouter une entree a
/// `Workshop.spec.simulators` depuis l'interieur du pod, avec le jeton de
/// service account monte automatiquement (config Kubernetes "in-cluster" de
/// `kube-rs`) — jamais un acces plus large qu'a sa propre ressource.
async fn ensure_parent_pod_workshop_rbac(
    ctx: &ReconcileCtx,
    ns: &str,
    sa_name: &str,
    workshop_name: &str,
    owner_ref: &k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference,
) -> anyhow::Result<()> {
    let roles: Api<Role> = Api::namespaced(ctx.client.clone(), ns);
    let role_bindings: Api<RoleBinding> = Api::namespaced(ctx.client.clone(), ns);

    let metadata = ObjectMeta {
        name: Some(sa_name.to_string()),
        namespace: Some(ns.to_string()),
        owner_references: Some(vec![owner_ref.clone()]),
        labels: Some(BTreeMap::from([(
            "atelier.dev/workshop".to_string(),
            workshop_name.to_string(),
        )])),
        ..Default::default()
    };

    let role = Role {
        metadata: metadata.clone(),
        rules: Some(vec![PolicyRule {
            api_groups: Some(vec!["atelier.dev".to_string()]),
            resources: Some(vec!["workshops".to_string()]),
            resource_names: Some(vec![workshop_name.to_string()]),
            verbs: vec!["get".to_string(), "patch".to_string()],
            ..Default::default()
        }]),
    };
    roles
        .patch(
            sa_name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&role),
        )
        .await?;

    let role_binding = RoleBinding {
        metadata,
        role_ref: RoleRef {
            api_group: "rbac.authorization.k8s.io".to_string(),
            kind: "Role".to_string(),
            name: sa_name.to_string(),
        },
        subjects: Some(vec![Subject {
            kind: "ServiceAccount".to_string(),
            name: sa_name.to_string(),
            namespace: Some(ns.to_string()),
            ..Default::default()
        }]),
    };
    role_bindings
        .patch(
            sa_name,
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
                &workshop.spec.owner_group,
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
            .map(|c| env_var("OPENBAO_ADDR", &c.pod_addr)),
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
    // Offload S3 du cache d'images (spec docs/specs/13-image-cache-
    // offload.md, tache 8.3) : le controller ne fait ici que retransmettre
    // sa PROPRE configuration S3 (chargee une seule fois dans `run()`) au
    // Job, qui televerse son `rootfs.ext4` publie vers
    // `S3_BUCKET_IMAGE_CACHE` une fois le build termine
    // (`image-builder::main`). `S3_BUCKET_SESSIONS`/`S3_BUCKET_SNAPSHOTS`
    // sont transmises alors qu'`image-builder` ne les utilise jamais : sa
    // propre lecture de la configuration (`atelier_common::storage::
    // config_from_env`) les exige des que `S3_ENDPOINT` est present, meme
    // discipline de validation que partout ailleurs dans ce module.
    .chain(ctx.s3.iter().flat_map(|s3| {
        vec![
            env_var(
                "S3_ENDPOINT",
                ctx.s3_pod_endpoint.as_deref().unwrap_or(&s3.endpoint),
            ),
            env_var("S3_REGION", &s3.region),
            env_var("S3_BUCKET_SESSIONS", &s3.bucket_sessions),
            env_var("S3_BUCKET_SNAPSHOTS", &s3.bucket_snapshots),
            env_var("S3_FORCE_PATH_STYLE", &s3.force_path_style.to_string()),
            env_var("AWS_ACCESS_KEY_ID", &s3.access_key_id),
            env_var("AWS_SECRET_ACCESS_KEY", &s3.secret_access_key),
        ]
        .into_iter()
        .chain(
            s3.bucket_image_cache
                .as_ref()
                .map(|bucket| env_var("S3_BUCKET_IMAGE_CACHE", bucket)),
        )
    }))
    // CA d'entreprise (tache 11.2, spec docs/specs/15-souverainete-airgap-
    // inference-gpu.md §3.2/§3.3) : `image-builder` en a besoin a deux
    // endroits distincts, tous deux couverts par ce seul chemin monte —
    // `GIT_SSL_CAINFO` pour SON PROPRE `git clone` (processus tournant sur
    // ce pod, PAS le meme chemin que le trafic egress relaye par
    // `net-proxy`, qui ne dechiffre jamais rien — voir le piege documente
    // sur la tache 11.1) et l'injection dans le rootfs produit
    // (`inject_enterprise_ca_bundle`, pour `git`/`npm`/`pip`/`cargo`/`curl`
    // IN-VM au runtime du Workshop).
    .chain(
        ctx.ca_bundle_configmap
            .as_ref()
            .map(|_| env_var("ATELIER_CA_BUNDLE_PATH", CA_BUNDLE_MOUNT_PATH)),
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
    let ca_bundle_volume = ctx.ca_bundle_configmap.as_ref().map(|configmap| Volume {
        name: "ca-bundle".to_string(),
        config_map: Some(ConfigMapVolumeSource {
            name: configmap.clone(),
            ..Default::default()
        }),
        ..Default::default()
    });
    let ca_bundle_mount = ctx.ca_bundle_configmap.as_ref().map(|_| VolumeMount {
        name: "ca-bundle".to_string(),
        mount_path: "/etc/atelier/ca".to_string(),
        read_only: Some(true),
        ..Default::default()
    });
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

    // Alias `git.atelier.internal` pour LE BUILD (2026-09-01, correction
    // d'un bug reel constate en validant le chantier planificateur puis en
    // migrant `pm-engine` de Claude Code vers `opencode` : sans cet alias,
    // `envbuilder` (dans la microVM builder) tentait une connexion DIRECTE
    // a `git.atelier.internal`, que rien ne resout depuis ce Job — la
    // microVM builder s'eteignait en quelques secondes sans avoir clone le
    // depot cible, et `crane export` echouait ensuite sur un manifeste
    // absent (`MANIFEST_UNKNOWN`). Ce Job n'a PAS de sidecar
    // `identity-proxy` (contrairement au pod parent, `ensure_parent_pod`) :
    // inutile pour l'authentification Git au moment du build, deja geree en
    // amont, directement depuis OpenBao, par
    // `crates/image-builder::resolve_git_credentials` (voir le commentaire
    // de tete de `crate::git_identity`, qui documente explicitement cette
    // separation). Il suffit donc de router `git.atelier.internal` vers le
    // ClusterIP resolu de la forge, sans passer par identity-proxy — best
    // effort et non bloquant, meme convention que le reste de ce module :
    // un echec de resolution ne desactive que cet alias pour ce cycle.
    let git_alias_addr = match &ctx.git_identity {
        Some(git_config) => match git_identity::resolve_cluster_ip(&ctx.client, git_config).await {
            Ok(ip) => Some(format!("{ip}:{}", git_config.port)),
            Err(err) => {
                tracing::warn!(
                    %err,
                    "resolution du ClusterIP de la forge Git echouee (Job image-builder), alias git desactive pour ce cycle"
                );
                None
            }
        },
        None => None,
    };

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
                    volumes: Some(
                        [Some(cache_volume), Some(tools_volume), ca_bundle_volume]
                            .into_iter()
                            .flatten()
                            .collect(),
                    ),
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
                            image: Some(ctx.component_image("net-proxy")),
                            restart_policy: Some("Always".into()),
                            env: Some(
                                vec![
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
                                ]
                                .into_iter()
                                .chain(
                                    git_alias_addr
                                        .as_ref()
                                        .map(|addr| env_var("ATELIER_GIT_ALIAS_ADDR", addr)),
                                )
                                .collect::<Vec<_>>(),
                            ),
                            ..Default::default()
                        },
                        Container {
                            name: "copy-tools".into(),
                            image: Some(ctx.component_image("image-builder")),
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
                        image: Some(ctx.component_image("image-builder")),
                        env: Some(env),
                        volume_mounts: Some(
                            [Some(cache_mount), Some(tools_mount), ca_bundle_mount]
                                .into_iter()
                                .flatten()
                                .collect(),
                        ),
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
    ensure_parent_pod_workshop_rbac(ctx, ns, &sa_name, name, &owner_ref).await?;

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
    // Pose seulement si la Virtual Key LiteLLM a pu etre provisionnee ET le
    // ClusterIP de la passerelle resolu : dans ce cas l'alias `llm-proxy` du
    // guest est aiguille vers identity-proxy (qui injecte la cle) au lieu de
    // LiteLLM directement, et ce `hostAlias` donne a identity-proxy le moyen
    // de resoudre `llm-proxy` pour joindre la vraie passerelle. Meme montage
    // que `git_host_alias`.
    let mut llm_host_alias: Option<HostAlias> = None;
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
    // La REGLE et le `hostAlias` sont recalcules a CHAQUE reconciliation ; seule
    // la generation de la cle reste conditionnee a la creation du pod
    // (`generate_virtual_key` n'est pas idempotent : un second appel cree une
    // cle SUPPLEMENTAIRE, voir `crate::litellm`).
    //
    // Les separer n'est pas cosmetique : les regles sont desormais publiees a
    // chaque passage dans une ConfigMap relue a chaud. En laissant la regle a
    // l'interieur de ce garde, la ConfigMap tombait a `[]` des la deuxieme
    // reconciliation, et le premier rechargement aurait EFFACE l'injection de
    // la Virtual Key — le guest serait silencieusement retombe sur le jeton
    // statique partage, sans qu'aucune erreur ne le signale.
    if let (Some(_), Some(_)) = (&ctx.litellm, &ctx.openbao) {
        match resolve_llm_cluster_ip(&ctx.client, ctx.llm_proxy_pod_addr.as_deref()).await {
            Ok(ip) => {
                effective_identity_injection_rules.push(IdentityInjectionRule {
                    host: litellm::LLM_PROXY_ALIAS_HOST.to_string(),
                    header: "Authorization".to_string(),
                    prefix: "Bearer ".to_string(),
                    secret_path: litellm::LLM_VIRTUAL_KEY_SECRET_PATH.to_string(),
                    field: litellm::LLM_VIRTUAL_KEY_SECRET_FIELD.to_string(),
                });
                llm_host_alias = Some(HostAlias {
                    ip: ip.to_string(),
                    hostnames: Some(vec![litellm::LLM_PROXY_ALIAS_HOST.to_string()]),
                });
            }
            Err(err) => {
                tracing::warn!(%err, "resolution du ClusterIP de LiteLLM echouee, l'alias llm-proxy reste direct et la Virtual Key ne sera pas injectee");
            }
        }
    }

    if pod_will_be_created {
        if let (Some(litellm_config), Some(openbao_config)) = (&ctx.litellm, &ctx.openbao) {
            let client = litellm::LiteLlmClient::new(litellm_config.clone());
            match client
                .generate_virtual_key(
                    &litellm::workshop_key_alias(name),
                    &workshop.spec.owner_group,
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
                            // Regle et `hostAlias` deja poses plus haut : ici
                            // on ne fait qu'ecrire la cle fraiche dans
                            // OpenBao, a l'emplacement qu'ils designent.
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

    // Publiees a CHAQUE reconciliation, y compris quand le pod existe deja :
    // c'est tout l'interet du fichier sur la variable d'environnement, qui
    // exigeait de recreer le pod — donc d'eteindre la microVM de l'agent —
    // pour qu'un credential ajoute depuis l'interface prenne effet.
    if let Err(err) = ensure_injection_rules_config_map(
        &ctx.client,
        ns,
        workshop,
        name,
        &identity_injection_rules,
    )
    .await
    {
        // Non bloquant : sans ConfigMap, `identity-proxy` retombe sur la
        // variable d'environnement figee. Il perd le rechargement a chaud,
        // pas l'injection elle-meme.
        tracing::warn!(%err, "publication des regles d'injection echouee, rechargement a chaud indisponible");
    }
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
    /// Port SSH du guest, tel qu'ecrit dans le `sshd_config` injecte par
    /// `crates/image-builder` et attendu par `crate::exec` cote api-server
    /// (`ATELIER_SSH_PORT`).
    const GUEST_SSH_PORT: u16 = 2222;
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
            // Les deux alias cohabitent : la forge Git et la passerelle LLM
            // sont deux destinations distinctes qu'identity-proxy doit
            // pouvoir resoudre. `None` si aucune n'est active, pour ne pas
            // poser un champ vide dans la spec du pod.
            host_aliases: {
                let aliases: Vec<HostAlias> = git_host_alias
                    .clone()
                    .into_iter()
                    .chain(llm_host_alias.clone())
                    .collect();
                (!aliases.is_empty()).then_some(aliases)
            },
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
                // Regles d'injection, montees en FICHIER et non passees en
                // variable d'environnement : une variable est figee a la
                // creation du pod, si bien qu'ajouter un credential depuis
                // l'interface n'avait d'effet qu'apres une mise en veille
                // puis une reprise du Workshop — donc apres avoir eteint la
                // microVM de l'agent. kubelet met a jour ce volume sans
                // redemarrage, et `identity-proxy` le relit periodiquement.
                Volume {
                    name: "injection-rules".to_string(),
                    config_map: Some(ConfigMapVolumeSource {
                        name: injection_rules_config_map(name),
                        optional: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Volume {
                    name: "jailer".to_string(),
                    empty_dir: Some(EmptyDirVolumeSource::default()),
                    ..Default::default()
                },
            ]),
            containers: vec![
                Container {
                    name: "vm-supervisor".into(),
                    image: Some(ctx.component_image("vm-supervisor")),
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
                        // Offload S3 des snapshots (spec docs/specs/13-image-
                        // cache-offload.md, tache 8.4) : meme retransmission
                        // de `ctx.s3` que pour le Job image-builder (8.3),
                        // avec le meme garde-fou `s3_pod_endpoint`. Prefixe
                        // de cle S3 = meme convention que le repertoire
                        // local (`storage::snapshot_cache_subdir`), pour que
                        // `vm-supervisor` n'ait jamais besoin de connaitre
                        // `ns`/`name` lui-meme — il ne recoit qu'un chemin
                        // local et, desormais, un prefixe de cle, exactement
                        // comme `ATELIER_VM_SNAPSHOT_DIR` deja transmis tel
                        // quel ci-dessus.
                        .chain(ctx.s3.iter().flat_map(|s3| {
                            vec![
                                env_var(
                                    "S3_ENDPOINT",
                                    ctx.s3_pod_endpoint.as_deref().unwrap_or(&s3.endpoint),
                                ),
                                env_var("S3_REGION", &s3.region),
                                env_var("S3_BUCKET_SESSIONS", &s3.bucket_sessions),
                                env_var("S3_BUCKET_SNAPSHOTS", &s3.bucket_snapshots),
                                env_var("S3_FORCE_PATH_STYLE", &s3.force_path_style.to_string()),
                                env_var("AWS_ACCESS_KEY_ID", &s3.access_key_id),
                                env_var("AWS_SECRET_ACCESS_KEY", &s3.secret_access_key),
                                env_var(
                                    "ATELIER_VM_SNAPSHOT_S3_PREFIX",
                                    &storage::snapshot_cache_subdir(ns, name),
                                ),
                            ]
                        }))
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
                    image: Some(ctx.component_image("net-proxy")),
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
                            // Canal de controle de `vm-supervisor`, dans CE
                            // pod : c'est par lui que net-proxy demande le
                            // confinement quand il detecte une anomalie
                            // reseau (tache 4.2.4). `127.0.0.1` — les
                            // conteneurs d'un pod partagent le netns, et ce
                            // canal n'a aucune raison de sortir.
                            env_var(
                                "ATELIER_VM_CONTROL_ADDR",
                                &format!("127.0.0.1:{VM_CONTROL_PORT}"),
                            ),
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
                        // Alias `<name>.atelier.internal` par simulateur
                        // declare (tache 9.3) : distinct de `simulator_enabled`
                        // ci-dessus (mecanisme historique, un seul LocalStack,
                        // gate par `Workshop.spec.tools`) — ceux-ci sont
                        // toujours actifs des qu'ils sont declares dans
                        // `Workshop.spec.simulators`, voir
                        // `crates/net-proxy/src/internal.rs`.
                        .chain((!workshop.spec.simulators.is_empty()).then(|| {
                            let aliases = workshop
                                .spec
                                .simulators
                                .iter()
                                .map(|s| {
                                    format!("{}=127.0.0.1:{}", s.name, simulator_port(s.type_))
                                })
                                .collect::<Vec<_>>()
                                .join(",");
                            env_var("ATELIER_SIMULATORS", &aliases)
                        }))
                        // Service global du cluster (voir `deploy/dev/llm-proxy/`),
                        // pas un sidecar de ce pod : toujours cable des que
                        // configure, contrairement a `simulator` (gate par
                        // `Workshop.spec.tools`) — voir `ReconcileCtx::llm_proxy_addr`.
                        // Cible de l'alias `llm-proxy` vu par le guest. Deux
                        // cas, et c'est tout l'enjeu de l'isolation par
                        // Workshop :
                        //  - Virtual Key injectable (`llm_host_alias` pose) :
                        //    on aiguille vers identity-proxy, qui remplace
                        //    l'`Authorization` du guest par la cle dediee
                        //    avant de joindre la vraie passerelle. C'est le
                        //    seul montage ou le plafond de depense contraint
                        //    reellement quelque chose.
                        //  - sinon : aiguillage direct vers LiteLLM, comme
                        //    avant. L'agent garde un acces au modele avec le
                        //    jeton statique partage — degradation assumee,
                        //    preferable a un Workshop coupe du LLM.
                        .chain(
                            llm_host_alias
                                .as_ref()
                                .map(|_| {
                                    env_var(
                                        "ATELIER_LLM_PROXY_ADDR",
                                        &format!("127.0.0.1:{IDENTITY_PROXY_PORT}"),
                                    )
                                })
                                .or_else(|| {
                                    ctx.llm_proxy_pod_addr
                                        .as_ref()
                                        .map(|addr| env_var("ATELIER_LLM_PROXY_ADDR", addr))
                                }),
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
                                .map(|c| env_var("OPENBAO_ADDR", &c.pod_addr)),
                        )
                        .collect::<Vec<_>>(),
                    ),
                    ..Default::default()
                },
                Container {
                    name: "identity-proxy".into(),
                    image: Some(ctx.component_image("identity-proxy")),
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
                            // Le fichier prime sur la variable ci-dessus, qui
                            // reste comme valeur de depart si la ConfigMap
                            // n'est pas encore montee.
                            env_var(
                                "ATELIER_IDENTITY_INJECTION_RULES_FILE",
                                INJECTION_RULES_PATH,
                            ),
                        ]
                        .into_iter()
                        .chain(
                            ctx.openbao
                                .as_ref()
                                .map(|c| env_var("OPENBAO_ADDR", &c.pod_addr)),
                        )
                        .collect::<Vec<_>>(),
                    ),
                    volume_mounts: Some(vec![VolumeMount {
                        name: "injection-rules".to_string(),
                        mount_path: INJECTION_RULES_DIR.to_string(),
                        read_only: Some(true),
                        ..Default::default()
                    }]),
                    ..Default::default()
                },
                Container {
                    name: "mcp-gateway".into(),
                    image: Some(ctx.component_image("mcp-gateway")),
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
                                .map(|c| env_var("OPENBAO_ADDR", &c.pod_addr)),
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
            .chain(workshop.spec.simulators.iter().map(simulator_container))
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
                    // `ttyd` NE SUFFIT PAS comme preuve d'utilisabilite : il
                    // ecoute avant `sshd`, et un Workshop annonce `Running`
                    // sur cette seule base faisait echouer tout
                    // `exec_in_workshop` lance dans la foulee
                    // (`connexion SSH echouee: Disconnected`). C'est ce qui
                    // arretait net le graphe du PM des la delegation a
                    // l'agent, en donnant l'apparence d'une panne de SSH
                    // alors qu'il s'agissait d'une simple course. Un
                    // Workshop n'est utilisable que quand SES DEUX portes
                    // d'entree repondent : le terminal et l'exec.
                    crate::guest_probe::guest_tcp_port_open(
                        &pod_ip,
                        NET_PROXY_CONTROL_PORT,
                        GUEST_TERMINAL_PORT,
                    )
                    .await
                        && crate::guest_probe::guest_tcp_port_open(
                            &pod_ip,
                            NET_PROXY_CONTROL_PORT,
                            GUEST_SSH_PORT,
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

    // Confinement de securite (tache 4.2.4) : `vm-supervisor` l'expose, le
    // controller le remonte dans `status.conditions`. Sans cela, un Workshop
    // confine s'affiche `Running` alors que son reseau est coupe et son etat
    // archive — la phase seule ne peut pas le dire, puisque la microVM est
    // deliberement conservee pour rester analysable.
    if let Some(pod_ip) = current
        .as_ref()
        .and_then(|p| p.status.as_ref())
        .and_then(|s| s.pod_ip.clone())
    {
        if is_locked_down(&pod_ip).await {
            status
                .conditions
                .insert("SecurityLockdown".to_string(), "true".to_string());
        }
    }
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

/// Nom et namespace du Service LiteLLM, deduits de l'adresse cluster
/// configuree (`ATELIER_LLM_PROXY_POD_ADDR`, ex.
/// `atelier-llm-proxy.default.svc.cluster.local:4000`).
///
/// Deduit plutot que configure a part : cette adresse EST deja la
/// designation du Service, et ajouter deux variables d'environnement
/// supplementaires qu'il faudrait garder coherentes avec elle serait une
/// source de divergence silencieuse. `None` si l'adresse n'a pas la forme
/// d'un nom DNS de Service Kubernetes — l'appelant retombe alors sur le
/// comportement anterieur plutot que de deviner.
fn llm_service_ref(pod_addr: &str) -> Option<(String, String)> {
    let host = pod_addr.split(':').next()?;
    let mut parts = host.split('.');
    let name = parts.next()?;
    let namespace = parts.next()?;
    if name.is_empty() || namespace.is_empty() || !host.contains(".svc") {
        return None;
    }
    Some((name.to_string(), namespace.to_string()))
}

async fn resolve_llm_cluster_ip(
    client: &Client,
    pod_addr: Option<&str>,
) -> anyhow::Result<std::net::IpAddr> {
    let pod_addr = pod_addr.ok_or_else(|| anyhow::anyhow!("adresse cluster LiteLLM absente"))?;
    let (name, namespace) = llm_service_ref(pod_addr).ok_or_else(|| {
        anyhow::anyhow!("adresse LiteLLM {pod_addr:?} n'est pas un nom de Service Kubernetes")
    })?;
    let services: Api<k8s_openapi::api::core::v1::Service> =
        Api::namespaced(client.clone(), &namespace);
    let service = services
        .get(&name)
        .await
        .map_err(|err| anyhow::anyhow!("lecture du Service {namespace}/{name} echouee: {err}"))?;
    service
        .spec
        .and_then(|spec| spec.cluster_ip)
        .filter(|ip| ip != "None")
        .ok_or_else(|| anyhow::anyhow!("Service {namespace}/{name} sans ClusterIP exploitable"))?
        .parse()
        .map_err(|err| anyhow::anyhow!("ClusterIP de {namespace}/{name} illisible: {err}"))
}

/// Repertoire de montage des regles d'injection dans `identity-proxy`.
const INJECTION_RULES_DIR: &str = "/etc/atelier/injection";
/// Fichier lu par `identity-proxy` (`ATELIER_IDENTITY_INJECTION_RULES_FILE`).
const INJECTION_RULES_PATH: &str = "/etc/atelier/injection/rules.json";
/// Cle de la ConfigMap : c'est elle qui devient le nom du fichier monte.
const INJECTION_RULES_KEY: &str = "rules.json";

fn injection_rules_config_map(workshop_name: &str) -> String {
    format!("{workshop_name}-injection-rules")
}

/// Publie les regles d'injection dans une ConfigMap, que kubelet propage au
/// pod sans le redemarrer.
///
/// `replace` plutot que `patch` : la liste de regles est remplacee en bloc,
/// une regle retiree doit disparaitre. Un `merge patch` sur une chaine ne
/// saurait de toute facon pas faire autrement, mais l'intention merite
/// d'etre explicite.
async fn ensure_injection_rules_config_map(
    client: &Client,
    namespace: &str,
    workshop: &Workshop,
    name: &str,
    rules_json: &str,
) -> Result<(), kube::Error> {
    let api: Api<ConfigMap> = Api::namespaced(client.clone(), namespace);
    let cm_name = injection_rules_config_map(name);
    let config_map = ConfigMap {
        metadata: ObjectMeta {
            name: Some(cm_name.clone()),
            namespace: Some(namespace.to_string()),
            // Rattachee au Workshop : elle disparait avec lui, sans passer
            // par le finalizer.
            owner_references: workshop.controller_owner_ref(&()).map(|r| vec![r]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            INJECTION_RULES_KEY.to_string(),
            rules_json.to_string(),
        )])),
        ..Default::default()
    };
    match api.create(&PostParams::default(), &config_map).await {
        Ok(_) => Ok(()),
        Err(kube::Error::Api(err)) if err.code == 409 => api
            .replace(&cm_name, &PostParams::default(), &config_map)
            .await
            .map(|_| ()),
        Err(err) => Err(err),
    }
}

/// Le `vm-supervisor` de ce pod signale-t-il un confinement de securite ?
///
/// Best-effort : un superviseur injoignable ou une reponse illisible valent
/// « pas de confinement ». Se tromper dans ce sens n'invente pas d'incident ;
/// l'inverse ferait clignoter une alerte de securite sur un simple hoquet
/// reseau, et une alerte qui crie faux finit par n'etre plus lue.
async fn is_locked_down(pod_ip: &str) -> bool {
    let url = format!("http://{pod_ip}:{VM_CONTROL_PORT}/lockdown");
    let Ok(response) = reqwest::Client::new()
        .get(&url)
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await
    else {
        return false;
    };
    response
        .json::<serde_json::Value>()
        .await
        .ok()
        .and_then(|body| body.get("lockdown").and_then(|v| v.as_bool()))
        .unwrap_or(false)
}

fn carry_forward_status(
    workshop: &Workshop,
    phase: WorkshopPhase,
    image_digest: Option<String>,
) -> WorkshopStatus {
    WorkshopStatus {
        phase,
        // Report de la derniere valeur connue, comme les digests juste en
        // dessous. `None` en dur effacait `status.podName` sur TOUT chemin de
        // reconciliation qui ne le renseigne pas — celui du build d'image,
        // notamment. Or l'api-server s'en sert pour retrouver le pod parent
        // (`crate::routes`) : une reconciliation concurrente pouvait donc
        // faire echouer `exec_in_workshop` avec "le Workshop n'a pas de pod
        // parent actif" alors que le pod tournait.
        //
        // Volontairement PAS de `skip_serializing_if` sur ce champ, au
        // contraire de `image_digest`/`snapshot_digest` : la mise en veille
        // (`suspend`) doit pouvoir l'effacer pour de bon, et le fait en
        // ecrasant explicitement ce report par `None` juste apres l'appel.
        pod_name: workshop.status.as_ref().and_then(|s| s.pod_name.clone()),
        // Report de la derniere valeur connue quand l'appelant n'en a pas :
        // le digest appartient a `image-builder`, un chemin de
        // reconciliation qui ne l'a pas lu n'a aucune raison de l'effacer
        // (meme logique que `snapshot_digest`/`upgrade_state` ci-dessous).
        image_digest: image_digest.or_else(|| {
            workshop
                .status
                .as_ref()
                .and_then(|s| s.image_digest.clone())
        }),
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

/// Port interne par defaut d'un simulateur sidecar (tache 9.3, spec
/// `docs/specs/14-devex-cli-simulateurs-hitl.md` §4.3) : toujours lie a
/// `127.0.0.1` du pod (voir `crates/net-proxy/src/internal.rs`), jamais
/// expose a la VM directement — seulement via l'alias
/// `<name>.atelier.internal` de `net-proxy`.
fn simulator_port(type_: atelier_common::SimulatorType) -> u16 {
    match type_ {
        atelier_common::SimulatorType::Postgres => 5432,
        atelier_common::SimulatorType::Localstack => 4566,
        atelier_common::SimulatorType::Wiremock => 8080,
    }
}

/// Construit le conteneur sidecar pour un simulateur declare
/// (`Workshop.spec.simulators`, tache 9.3). Le nom du conteneur est le nom
/// logique declare par l'utilisateur : doit donc etre un nom de conteneur
/// Kubernetes valide (DNS-1123), meme convention que le nom du `Workshop`
/// lui-meme — non revalide ici, la validation cote `api-server` (chemin
/// normal de creation) est la premiere ligne de defense.
fn simulator_container(sim: &atelier_common::SimulatorSpec) -> Container {
    let (image, default_env): (&str, &[(&str, &str)]) = match sim.type_ {
        // `POSTGRES_PASSWORD` est obligatoire cote image officielle (le
        // conteneur refuse de demarrer sans lui, ni `POSTGRES_HOST_AUTH_METHOD`) :
        // valeur par defaut fournie si l'utilisateur ne l'a pas declaree dans
        // `sim.env`, jamais utilisee pour autre chose qu'un test ephemere
        // detruit avec le Workshop.
        atelier_common::SimulatorType::Postgres => {
            ("postgres:16-alpine", &[("POSTGRES_PASSWORD", "postgres")])
        }
        atelier_common::SimulatorType::Localstack => ("localstack/localstack:3", &[]),
        atelier_common::SimulatorType::Wiremock => ("wiremock/wiremock:3", &[]),
    };
    let env = default_env
        .iter()
        .filter(|(k, _)| !sim.env.contains_key(*k))
        .map(|(k, v)| env_var(k, v))
        .chain(sim.env.iter().map(|(k, v)| env_var(k, v)))
        .collect();
    Container {
        name: sim.name.clone(),
        image: Some(image.to_string()),
        env: Some(env),
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
    use super::{carry_forward_status, pod_spec_template_hash};
    use atelier_common::crd::{
        DevcontainerSource, Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopResources,
        WorkshopSpec, WorkshopStatus,
    };
    use k8s_openapi::api::core::v1::{Container, PodSpec};
    use kube::core::ObjectMeta;

    /// Le nom du Service LiteLLM est DEDUIT de l'adresse cluster configuree,
    /// pas configure a part : une adresse qui n'a pas la forme d'un nom DNS
    /// de Service doit donner `None`, pour que l'appelant retombe sur le
    /// comportement anterieur (alias direct, jeton statique) plutot que de
    /// deviner un nom de Service inexistant.
    #[test]
    fn llm_service_is_derived_from_the_cluster_address() {
        use super::llm_service_ref;
        assert_eq!(
            llm_service_ref("atelier-llm-proxy.default.svc.cluster.local:4000"),
            Some(("atelier-llm-proxy".to_string(), "default".to_string()))
        );
        assert_eq!(
            llm_service_ref("atelier-llm-proxy.atelier-system.svc:4000"),
            Some((
                "atelier-llm-proxy".to_string(),
                "atelier-system".to_string()
            ))
        );
        // Port-forward local : pas un Service, on ne doit rien deduire.
        assert_eq!(llm_service_ref("127.0.0.1:4000"), None);
        assert_eq!(llm_service_ref("localhost:4000"), None);
    }

    /// Regression : `carry_forward_status` mettait `pod_name` a `None` en
    /// dur, si bien que tout chemin de reconciliation qui ne le renseigne pas
    /// (celui du build d'image, par exemple) effacait `status.podName`.
    /// L'api-server s'en sert pour retrouver le pod parent : une
    /// reconciliation concurrente faisait donc echouer `exec_in_workshop`
    /// avec "le Workshop n'a pas de pod parent actif" alors que le pod
    /// tournait. Meme classe de bug que l'effacement de `image_digest`.
    #[test]
    fn pod_name_survives_a_reconciliation_that_does_not_set_it() {
        let workshop = Workshop {
            metadata: ObjectMeta {
                name: Some("ws".into()),
                ..Default::default()
            },
            spec: WorkshopSpec {
                devcontainer: DevcontainerSource {
                    repo: "https://example.invalid/repo.git".into(),
                    revision: "HEAD".into(),
                    config_path: ".devcontainer/devcontainer.json".into(),
                },
                resources: WorkshopResources {
                    cpu: "100m".into(),
                    memory: "128Mi".into(),
                    disk: None,
                    max_llm_budget_usd: None,
                },
                egress_allowlist: vec![],
                tools: vec![],
                identity_injection_rules: vec![],
                owner_group: "atelier-core".into(),
                owner_subject: "test-user".into(),
                desired_state: WorkshopDesiredState::Running,
                simulators: vec![],
            },
            status: Some(WorkshopStatus {
                phase: WorkshopPhase::Running,
                pod_name: Some("ws-parent".into()),
                ..Default::default()
            }),
        };

        let carried = carry_forward_status(&workshop, WorkshopPhase::BuildingImage, None);
        assert_eq!(
            carried.pod_name.as_deref(),
            Some("ws-parent"),
            "le nom du pod parent doit etre reporte, pas efface"
        );
    }

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
