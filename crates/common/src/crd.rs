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
    /// Regles d'injection de credentials appliquees par `identity-proxy` aux
    /// appels sortants de l'agent (jamais exposees en clair a la VM, voir
    /// `docs/architecture/network-security.md`). Vide : identity-proxy relaie
    /// sans jamais injecter.
    #[serde(default)]
    pub identity_injection_rules: Vec<IdentityInjectionRule>,
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

/// Une regle d'injection `identity-proxy` : les requetes sortantes de
/// l'agent dont l'hote correspond a `host` (correspondance exacte ou
/// wildcard `*.domaine`, meme syntaxe que `egress_allowlist`) recoivent
/// l'en-tete `header` construit comme `prefix` + la valeur du champ `field`
/// du secret OpenBao stocke sous `secret/workshops/<name>/<secret_path>`.
/// Meme forme que `crates/identity-proxy/src/rules.rs::InjectionRule`
/// (serialisee telle quelle vers `ATELIER_IDENTITY_INJECTION_RULES`, JSON).
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct IdentityInjectionRule {
    pub host: String,
    pub header: String,
    #[serde(default)]
    pub prefix: String,
    pub secret_path: String,
    #[serde(default = "default_injection_field")]
    pub field: String,
}

fn default_injection_field() -> String {
    "value".to_string()
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
    /// Identifiant de l'entite machine Kanidm provisionnee pour cet
    /// environnement (distincte du sujet humain `spec.owner_subject`).
    /// C'est cette identite que `identity-proxy` presente a OpenBao pour
    /// recuperer les secrets scopes a ce Workshop.
    #[serde(default)]
    pub kanidm_entity_id: Option<String>,
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
