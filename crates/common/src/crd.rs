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
    /// Groupe proprietaire : c'est LUI qui donne acces (voir
    /// `docs/specs/07-groupes.md`). Tout membre du groupe peut piloter ce
    /// Workshop, ce qui permet notamment de reprendre l'environnement d'un
    /// collegue absent.
    ///
    /// Obligatoire : le repli sur `owner_subject` qui accompagnait la
    /// transition a ete retire (voir `docs/specs/07-groupes.md`). Un
    /// Workshop sans groupe n'aurait aucun perimetre d'acces defini, et
    /// laisser ce cas possible revenait a garder deux regles d'autorisation
    /// en parallele — celle qui se serait tue en premier.
    pub owner_group: String,
    /// Sujet JWT qui a CREE ce Workshop.
    ///
    /// Ne donne plus l'acces des lors qu'`owner_group` est renseigne : il
    /// repond a « qui l'a provisionne » (audit, tracabilite), pas a « qui y a
    /// droit ». Conserve pour cette raison, y compris apres le depart de la
    /// personne du groupe.
    pub owner_subject: String,
    /// Etat souhaite : `Running` (microVM active) ou `Suspended` (mise en
    /// veille via snapshot Firecracker, pod parent libere). Le controller
    /// fait converger `status.phase` vers cet etat.
    #[serde(default)]
    pub desired_state: WorkshopDesiredState,
    /// Simulateurs ephemeres deployes en sidecars dans le pod parent (spec
    /// `docs/specs/14-devex-cli-simulateurs-hitl.md` §4, tache 9.3) :
    /// dependances d'appoint (base de donnees, mock d'API) pour les tests de
    /// l'agent, sans acces Internet et sans alourdir la microVM elle-meme.
    /// Chaque entree devient joignable in-VM via `<name>.atelier.internal`
    /// (voir `crates/net-proxy/src/internal.rs`).
    #[serde(default)]
    pub simulators: Vec<SimulatorSpec>,
    /// Ports applicatifs exposes aux AUTRES Workshops d'une meme campagne
    /// (spec `docs/specs/16-escouades-multi-agents-swarms-mesh.md` §3.2,
    /// tache 12.1) — jamais a Internet ni au reste du cluster. Chaque entree
    /// devient un Service Kubernetes `<workshop>-<name>` (voir
    /// `crates/controller/src/reconcile.rs::ensure_exported_service`) et un
    /// relais TCP dans le `net-proxy` de CE Workshop (`crates/net-proxy/src/
    /// ingress.rs`) : sans ce relais, rien n'ecoute sur l'IP du pod pour un
    /// port applicatif (celui-ci n'existe que dans le netns de la microVM,
    /// voir la doc de tete de `crate::guest_probe`).
    #[serde(default)]
    pub exported_services: Vec<ExportedService>,
    /// Cibles internes explicitement autorisees vers D'AUTRES Workshops,
    /// format `<service>.<workshop-cible>.atelier.internal:<port>` — jamais
    /// de wildcard (spec 16 §3.2, "Validation Nominative des Cibles, Zero
    /// Wildcard") : une cible absente de cette liste reste inaccessible,
    /// quand bien meme elle appartiendrait a la meme campagne. Resolu par le
    /// `controller` en adresses reelles (ClusterIP du Service correspondant)
    /// et transmis au `net-proxy` de CE Workshop
    /// (`crates/net-proxy/src/internal.rs`, table `squad`).
    #[serde(default)]
    pub allowed_internal_targets: Vec<String>,
    /// Identifiant de campagne multi-Workshops (spec 16 §3.2/§4) : les
    /// Workshops d'une meme campagne (meme valeur ici) ET du meme
    /// `owner_group` peuvent s'echanger des paquets au niveau reseau K8s
    /// (`NetworkPolicy` generee par le controller, voir
    /// `crates/controller/src/reconcile.rs::campaign_network_policy`) — tout
    /// le reste du trafic inter-pod est detruit au niveau noyau. `None` :
    /// Workshop solitaire, comportement inchange (aucune `NetworkPolicy`
    /// generee par ce mecanisme).
    #[serde(default)]
    pub campaign_id: Option<String>,
}

/// Un port applicatif expose aux autres Workshops de la meme campagne — voir
/// la doc du champ `WorkshopSpec::exported_services`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExportedService {
    /// Nom logique, unique parmi les services exportes de ce Workshop :
    /// determine a la fois le nom du Service Kubernetes cree
    /// (`<workshop>-<name>`) et le sous-domaine de l'alias resolu par les
    /// AUTRES Workshops (`<name>.<workshop>.atelier.internal`).
    pub name: String,
    /// Port TCP, a la fois cote Service Kubernetes et cote microVM (le
    /// `net-proxy` de ce Workshop relaie tel quel, sans traduction de port).
    pub port: u16,
}

/// Un simulateur sidecar declare pour un `Workshop` — voir la doc du champ
/// `WorkshopSpec::simulators`.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimulatorSpec {
    /// Nom logique, unique parmi les simulateurs de ce Workshop : devient le
    /// sous-domaine de l'alias `<name>.atelier.internal`. Independant de
    /// `type_` pour permettre plusieurs instances d'un meme type (ex: deux
    /// bases Postgres distinctes) sous des noms differents.
    pub name: String,
    #[serde(rename = "type")]
    pub type_: SimulatorType,
    /// Variables d'environnement transmises telles quelles au conteneur
    /// sidecar (ex: `POSTGRES_DB`, `POSTGRES_PASSWORD` pour `Postgres`).
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Type d'un simulateur sidecar : determine l'image de conteneur et le port
/// interne par defaut utilises par `crates/controller/src/reconcile.rs`.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum SimulatorType {
    Postgres,
    Localstack,
    Wiremock,
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
///
/// `PartialEq` : `identity-proxy` compare les regles rechargees aux
/// precedentes pour ne journaliser que les vrais changements.
#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
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

/// Hote interne conventionnel pour la forge Git ciblee par l'agent en cours
/// d'execution dans la microVM (voir `crates/controller/src/git_identity.rs`
/// et `crates/net-proxy/src/internal.rs`) : jamais resolu par DNS classique,
/// toujours par une combinaison `net-proxy` (alias interne, bypass
/// allowlist) + `hostAliases` du pod parent (IP du Service Kubernetes de la
/// forge, injectee dans `/etc/hosts` de tous les conteneurs du pod par
/// Kubernetes lui-meme) + `identity-proxy` (regle d'injection de PAT).
/// Reprend deliberement la meme valeur que `FORGEJO__server__ROOT_URL` de
/// l'instance Forgejo de dev (`deploy/dev/forgejo/dev-pod.yaml`), pas une
/// coincidence : ce nom est concu comme le nom "public" (au sens de l'agent)
/// de la forge, quel que soit son adresse reelle dans le cluster.
pub const GIT_ALIAS_HOST: &str = "git.atelier.internal";

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
    ///
    /// `skip_serializing_if` est ESSENTIEL ici : ce champ est ecrit par
    /// `image-builder` (depuis son Job), pas par le controller, alors que
    /// les deux patchent `status`. Sans lui, un `None` cote controller part
    /// en `"imageDigest": null` dans le JSON merge patch, ce que l'API
    /// Kubernetes interprete comme une SUPPRESSION du champ — le digest tout
    /// juste publie par `image-builder` etait alors efface, et le Workshop
    /// restait bloque en `BuildingImage` indefiniment alors que son image
    /// existait bel et bien dans le cache. Bug reel, observe environ une
    /// fois sur trois builds simultanes (2026-08-30).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_digest: Option<String>,
    /// Reference du dernier snapshot Firecracker (etat VM + memoire) pris
    /// par `vm-supervisor` lors d'une mise en veille, dans le meme cache
    /// content-addressed que `image_digest`. Absent si le Workshop n'a
    /// jamais ete suspendu.
    ///
    /// Meme `skip_serializing_if` que `image_digest`, et pour la meme
    /// raison : ecrit par `vm-supervisor` via le controller, il ne doit
    /// jamais etre efface par un patch qui ne le renseigne simplement pas.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snapshot_digest: Option<String>,
    /// Positionne par le controller (jamais par l'utilisateur) quand le hash
    /// du template de pod parent observe differe de celui utilise pour
    /// provisionner la microVM en cours d'execution — typiquement apres un
    /// `helm upgrade` qui change l'image `atelier-controller`/`atelier-api-server`.
    /// Un `helm upgrade` ne redemarre donc jamais une microVM Firecracker
    /// active de force : ce champ signale seulement qu'un redemarrage (via un
    /// cycle suspend/resume manuel, ou a la prochaine liberation du pod) sera
    /// necessaire pour que ce Workshop beneficie de la nouvelle version. Voir
    /// `docs/specs/02-helm-deployment-admin-doc.md`, section 1.1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_state: Option<WorkshopUpgradeState>,
    #[serde(default)]
    pub conditions: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema, PartialEq, Eq)]
pub enum WorkshopUpgradeState {
    /// Le pod parent de ce Workshop tourne encore avec un template anterieur
    /// a la derniere revision Helm appliquee ; sa microVM active n'a pas ete
    /// perturbee, mais un redemarrage est necessaire pour converger.
    NeedsRestartForUpgrade,
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
                owner_group: "atelier-core".into(),
                owner_subject: "user@example.invalid".into(),
                desired_state: WorkshopDesiredState::Running,
                simulators: vec![],
                exported_services: vec![],
                allowed_internal_targets: vec![],
                campaign_id: None,
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
