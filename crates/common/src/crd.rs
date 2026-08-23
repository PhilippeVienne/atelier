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
#[serde(rename_all = "camelCase")]
pub struct WorkshopResources {
    pub cpu: String,
    pub memory: String,
    #[serde(default)]
    pub disk: Option<String>,
    /// Budget maximal en dollars US alloue a la Virtual Key LiteLLM de ce
    /// Workshop (voir `docs/specs/03-litellm-proxy.md`). Absent = pas de
    /// plafond specifique impose par le Workshop (comportement par defaut
    /// de LiteLLM/politique globale du cluster).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_llm_budget_usd: Option<f64>,
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

#[cfg(test)]
mod tests {
    use super::*;
    use kube::CustomResourceExt;

    /// La generation du manifest CRD (`cargo run -p atelier-controller --bin
    /// crdgen`, publie dans `crds/workshop.yaml`) ne doit jamais paniquer :
    /// c'est le premier signal si un type imbrique (ex: `WorkshopResources`,
    /// `WorkshopStatus`) devient incompatible avec `schemars`.
    #[test]
    fn generate_crd() {
        let crd = Workshop::crd();
        let yaml = serde_yaml::to_string(&crd).expect("le CRD doit se serialiser en YAML");
        assert!(yaml.contains("kind: CustomResourceDefinition"));
        assert!(
            !yaml.contains("kanidmEntityId"),
            "le champ Kanidm retire de WorkshopStatus ne doit plus apparaitre dans le schema"
        );
        assert!(
            yaml.contains("maxLlmBudgetUsd"),
            "le budget LLM par Workshop doit apparaitre dans le schema (camelCase)"
        );
    }

    /// Round-trip JSON et YAML sur un `Workshop` complet (spec + status),
    /// garantissant que ce que le controller ecrit reste lisible par
    /// `kube-rs` (et reciproquement) apres le nettoyage Kanidm.
    #[test]
    fn workshop_roundtrip_json_and_yaml() {
        let workshop = Workshop::new(
            "test-workshop",
            WorkshopSpec {
                devcontainer: DevcontainerSource {
                    repo: "https://example.invalid/repo.git".into(),
                    revision: "HEAD".into(),
                    config_path: ".devcontainer/devcontainer.json".into(),
                },
                resources: WorkshopResources {
                    cpu: "500m".into(),
                    memory: "1Gi".into(),
                    disk: None,
                    max_llm_budget_usd: Some(2.5),
                },
                egress_allowlist: vec!["github.com".into()],
                tools: vec![],
                identity_injection_rules: vec![],
                owner_subject: "user@example.invalid".into(),
                desired_state: WorkshopDesiredState::Running,
            },
        );

        let json = serde_json::to_string(&workshop).expect("serialisation JSON");
        assert!(json.contains("\"maxLlmBudgetUsd\":2.5"));
        assert!(!json.contains("kanidmEntityId"));
        let from_json: Workshop = serde_json::from_str(&json).expect("deserialisation JSON");
        assert_eq!(from_json.spec.resources.max_llm_budget_usd, Some(2.5));

        let yaml = serde_yaml::to_string(&workshop).expect("serialisation YAML");
        let from_yaml: Workshop = serde_yaml::from_str(&yaml).expect("deserialisation YAML");
        assert_eq!(from_yaml.spec.owner_subject, workshop.spec.owner_subject);
    }
}
