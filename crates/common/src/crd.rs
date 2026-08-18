use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Un `Workshop` decrit un environnement isole fourni a un agent de code :
/// source devcontainer, ressources allouees, politique reseau et outillage active.
#[derive(CustomResource, Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "atelier.dev",
    version = "v1alpha1",
    kind = "Workshop",
    plural = "workshops",
    namespaced,
    status = "WorkshopStatus",
    shortname = "wks"
)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopSpec {
    /// Definition de l'environnement au format devcontainer.json (spec VS Code
    /// Dev Containers). C'est cette source qui est construite en rootfs
    /// bootable par le composant `image-builder`.
    pub devcontainer: DevcontainerSource,
    /// Ressources allouees au pod parent (qui heberge la microVM + le tooling).
    pub resources: WorkshopResources,
    /// Politique reseau de sortie appliquee par le net-proxy (domaines autorises).
    #[serde(default)]
    pub egress_allowlist: Vec<String>,
    /// Outils/simulateurs a exposer via le mcp-gateway (ex: "aws-sim", "identity").
    #[serde(default)]
    pub tools: Vec<String>,
    /// Identite du sujet JWT autorise a piloter ce Workshop.
    pub owner_subject: String,
    /// Etat souhaite : `Running` (microVM active) ou `Suspended` (mise en
    /// veille via snapshot Firecracker, pod parent libere). Le controller
    /// fait converger `status.phase` vers cet etat.
    #[serde(default)]
    pub desired_state: WorkshopDesiredState,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, PartialEq, Eq)]
pub enum WorkshopDesiredState {
    #[default]
    Running,
    Suspended,
}

/// Reference vers un projet portant un `.devcontainer/devcontainer.json`
/// (ou equivalent), au sens de la specification VS Code Dev Containers.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DevcontainerSource {
    /// URL du depot git contenant la definition devcontainer.
    pub repo: String,
    /// Branche, tag ou commit a utiliser.
    #[serde(default = "default_revision")]
    pub revision: String,
    /// Chemin vers le fichier devcontainer.json dans le depot.
    #[serde(default = "default_devcontainer_path")]
    pub config_path: String,
}

fn default_revision() -> String {
    "HEAD".to_string()
}

fn default_devcontainer_path() -> String {
    ".devcontainer/devcontainer.json".to_string()
}

/// Quantites au format Kubernetes (ex: "500m", "2Gi").
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
pub struct WorkshopResources {
    pub cpu: String,
    pub memory: String,
    #[serde(default)]
    pub disk: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default)]
#[serde(rename_all = "camelCase")]
pub struct WorkshopStatus {
    pub phase: WorkshopPhase,
    #[serde(default)]
    pub pod_name: Option<String>,
    /// Digest de l'image rootfs construite par `image-builder` a partir du
    /// devcontainer, une fois le build termine (cache content-addressed).
    #[serde(default)]
    pub image_digest: Option<String>,
    /// Reference du dernier snapshot Firecracker (etat VM + memoire) pris
    /// par `vm-supervisor` lors d'une mise en veille, dans le meme cache
    /// content-addressed que `image_digest`. Absent si le Workshop n'a
    /// jamais ete suspendu.
    #[serde(default)]
    pub snapshot_digest: Option<String>,
    #[serde(default)]
    pub conditions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, Default, PartialEq, Eq)]
pub enum WorkshopPhase {
    #[default]
    Pending,
    /// Construction du rootfs en cours via `image-builder`.
    BuildingImage,
    Provisioning,
    Running,
    /// Snapshot Firecracker en cours, pod parent sur le point d'etre libere.
    Suspending,
    /// MicroVM arretee, snapshot disponible dans le cache ; aucun pod parent
    /// n'est alloue tant que le Workshop reste dans cette phase.
    Suspended,
    /// Pod parent recree, restauration de la microVM depuis
    /// `status.snapshot_digest` en cours.
    Resuming,
    Terminating,
    Failed,
}
