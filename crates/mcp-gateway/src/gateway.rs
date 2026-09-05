//! Tools MCP exposes a l'agent : `request_credential` (lecture d'un secret
//! OpenBao scope au Workshop), `request_egress` (elargissement a chaud de
//! l'allowlist egress de `net-proxy` pour cette session de pod),
//! `enable_simulator` (rend joignable le sidecar `simulator`/LocalStack du
//! pod, voir `crates/controller/src/reconcile.rs`) et `request_simulator`
//! (tache 9.4 : ajoute un simulateur non pre-declare a
//! `Workshop.spec.simulators`, disponible au prochain cycle stop/resume —
//! un pod Kubernetes ne peut pas recevoir de nouveau conteneur a chaud).
//! Chaque tool n'est actif que si son nom figure dans `Workshop.spec.tools`
//! (`ATELIER_TOOLS`).

use std::collections::HashSet;
use std::sync::Arc;

use atelier_common::{OpenBaoClient, SimulatorSpec, SimulatorType, Workshop};
use kube::api::{Api, Patch, PatchParams};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler};

/// Secret KV v2 fixe sous lequel `request_credential` cherche ses champs :
/// `secret/data/workshops/<name>/mcp`, distinct des secrets d'injection
/// consommes par `identity-proxy` (`secret_path` par regle).
const CREDENTIAL_SECRET_PATH: &str = "mcp";

pub struct GatewayConfig {
    pub enabled_tools: HashSet<String>,
    pub openbao: Option<OpenBaoClient>,
    pub net_proxy_admin_addr: Option<String>,
    pub http: reqwest::Client,
    /// Nom du Workshop courant (`ATELIER_WORKSHOP_NAME`) : cle utilisee par
    /// `request_simulator` pour patcher SA propre ressource, jamais une
    /// autre — voir `workshops` ci-dessous.
    pub workshop_name: String,
    /// Client Kubernetes scope a ce seul Workshop (RBAC via `resourceNames`,
    /// voir `crates/controller/src/reconcile.rs::ensure_parent_pod_workshop_rbac`),
    /// absent si la config "in-cluster" de `kube-rs` n'a pas pu etre
    /// construite (ex: execution hors d'un pod, dev local) — `request_simulator`
    /// echoue alors proprement plutot que de paniquer.
    pub workshops: Option<Api<Workshop>>,
}

#[derive(Clone)]
pub struct Gateway {
    tool_router: ToolRouter<Self>,
    config: Arc<GatewayConfig>,
}

impl Gateway {
    pub fn new(config: Arc<GatewayConfig>) -> Self {
        Self {
            tool_router: Self::tool_router(),
            config,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RequestCredentialParams {
    /// Nom du champ a lire dans le secret OpenBao `workshops/<name>/mcp`.
    field: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RequestSimulatorParams {
    /// Nom logique du simulateur, devient le sous-domaine de l'alias
    /// `<name>.atelier.internal` — doit etre unique parmi les simulateurs
    /// deja declares pour ce Workshop.
    name: String,
    /// "postgres", "localstack" ou "wiremock" — String plutot que
    /// `SimulatorType` directement : `rmcp` genere son schema JSON via
    /// `schemars` 1.x, `atelier_common::SimulatorType` derive `JsonSchema`
    /// de `schemars` 0.8.x (impose par `kube::CustomResource`), deux
    /// versions incompatibles du meme trait. Parse manuellement dans le
    /// corps du tool, voir `parse_simulator_type`.
    #[serde(rename = "type")]
    type_: String,
    /// Variables d'environnement du conteneur sidecar (voir
    /// `atelier_common::SimulatorSpec::env`).
    #[serde(default)]
    env: std::collections::BTreeMap<String, String>,
}

fn parse_simulator_type(raw: &str) -> Result<SimulatorType, ErrorData> {
    match raw {
        "postgres" => Ok(SimulatorType::Postgres),
        "localstack" => Ok(SimulatorType::Localstack),
        "wiremock" => Ok(SimulatorType::Wiremock),
        other => Err(ErrorData::invalid_params(
            format!("type de simulateur inconnu: '{other}' (attendu postgres|localstack|wiremock)"),
            None,
        )),
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct RequestEgressParams {
    /// Hote (domaine ou `*.domaine`) a ajouter a l'allowlist egress de
    /// net-proxy pour la duree de vie de ce pod.
    host: String,
}

#[tool_router]
impl Gateway {
    #[tool(
        description = "Lit un champ du secret OpenBao scope a ce Workshop (secret/data/workshops/<name>/mcp). Necessite que \"identity\" figure dans Workshop.spec.tools."
    )]
    async fn request_credential(
        &self,
        Parameters(RequestCredentialParams { field }): Parameters<RequestCredentialParams>,
    ) -> Result<String, ErrorData> {
        if !self.config.enabled_tools.contains("identity") {
            return Err(ErrorData::invalid_request(
                "tool \"identity\" non active pour ce Workshop (Workshop.spec.tools)",
                None,
            ));
        }
        let client = self.config.openbao.as_ref().ok_or_else(|| {
            ErrorData::internal_error("OpenBao non configure (OPENBAO_ADDR absent)", None)
        })?;
        let token = client
            .login()
            .await
            .map_err(|err| ErrorData::internal_error(format!("login OpenBao: {err}"), None))?;
        client
            .read_field(&token, CREDENTIAL_SECRET_PATH, &field)
            .await
            .map_err(|err| ErrorData::internal_error(format!("lecture OpenBao: {err}"), None))
    }

    #[tool(
        description = "Elargit l'allowlist egress de net-proxy avec un hote supplementaire, pour la duree de vie de ce pod (pas de persistance dans le Workshop). Necessite que \"egress\" figure dans Workshop.spec.tools."
    )]
    async fn request_egress(
        &self,
        Parameters(RequestEgressParams { host }): Parameters<RequestEgressParams>,
    ) -> Result<String, ErrorData> {
        if !self.config.enabled_tools.contains("egress") {
            return Err(ErrorData::invalid_request(
                "tool \"egress\" non active pour ce Workshop (Workshop.spec.tools)",
                None,
            ));
        }
        let addr = self.config.net_proxy_admin_addr.as_ref().ok_or_else(|| {
            ErrorData::internal_error("ATELIER_NET_PROXY_ADMIN_ADDR non configure", None)
        })?;
        let response = self
            .config
            .http
            .post(format!("http://{addr}/internal/allowlist/add"))
            .json(&serde_json::json!({ "host": host }))
            .send()
            .await
            .map_err(|err| {
                ErrorData::internal_error(format!("net-proxy injoignable: {err}"), None)
            })?;
        response.text().await.map_err(|err| {
            ErrorData::internal_error(format!("reponse net-proxy invalide: {err}"), None)
        })
    }

    #[tool(
        description = "Rend joignable le simulateur AWS local (LocalStack) provisionne pour ce Workshop, via l'hote \"simulator\" (meme HTTP_PROXY que le reste de l'egress). Necessite que \"enable_simulator\" figure dans Workshop.spec.tools et qu'un simulateur ait ete demande a la creation du Workshop."
    )]
    async fn enable_simulator(&self) -> Result<String, ErrorData> {
        if !self.config.enabled_tools.contains("enable_simulator") {
            return Err(ErrorData::invalid_request(
                "tool \"enable_simulator\" non active pour ce Workshop (Workshop.spec.tools)",
                None,
            ));
        }
        let addr = self.config.net_proxy_admin_addr.as_ref().ok_or_else(|| {
            ErrorData::internal_error("ATELIER_NET_PROXY_ADMIN_ADDR non configure", None)
        })?;
        let response = self
            .config
            .http
            .post(format!("http://{addr}/internal/simulator/enable"))
            .send()
            .await
            .map_err(|err| {
                ErrorData::internal_error(format!("net-proxy injoignable: {err}"), None)
            })?;
        response.text().await.map_err(|err| {
            ErrorData::internal_error(format!("reponse net-proxy invalide: {err}"), None)
        })
    }

    #[tool(
        description = "Demande un simulateur sidecar supplementaire (postgres, localstack ou wiremock) non declare a la creation du Workshop, en ajoutant une entree a Workshop.spec.simulators. Necessite que \"request_simulator\" figure dans Workshop.spec.tools. IMPORTANT : contrairement a enable_simulator, ceci ne rend PAS le simulateur immediatement joignable — un pod Kubernetes ne peut pas recevoir de nouveau conteneur a chaud. Le simulateur ne sera reellement disponible (alias <name>.atelier.internal joignable) qu'apres le prochain cycle de suspension/reprise du Workshop (voir Workshop.status.upgradeState)."
    )]
    async fn request_simulator(
        &self,
        Parameters(RequestSimulatorParams { name, type_, env }): Parameters<RequestSimulatorParams>,
    ) -> Result<String, ErrorData> {
        if !self.config.enabled_tools.contains("request_simulator") {
            return Err(ErrorData::invalid_request(
                "tool \"request_simulator\" non active pour ce Workshop (Workshop.spec.tools)",
                None,
            ));
        }
        let workshops = self.config.workshops.as_ref().ok_or_else(|| {
            ErrorData::internal_error(
                "client Kubernetes non configure (execution hors d'un pod ?)",
                None,
            )
        })?;

        let current = workshops
            .get(&self.config.workshop_name)
            .await
            .map_err(|err| {
                ErrorData::internal_error(format!("lecture du Workshop echouee: {err}"), None)
            })?;

        if current.spec.simulators.iter().any(|s| s.name == name) {
            return Ok(format!(
                "Simulateur '{name}' deja declare pour ce Workshop, aucune modification."
            ));
        }
        let type_ = parse_simulator_type(&type_)?;

        let mut simulators = current.spec.simulators.clone();
        simulators.push(SimulatorSpec {
            name: name.clone(),
            type_,
            env,
        });
        let patch = serde_json::json!({ "spec": { "simulators": simulators } });
        workshops
            .patch(
                &self.config.workshop_name,
                &PatchParams::default(),
                &Patch::Merge(&patch),
            )
            .await
            .map_err(|err| {
                ErrorData::internal_error(format!("patch du Workshop echoue: {err}"), None)
            })?;

        Ok(format!(
            "Simulateur '{name}' ajoute a Workshop.spec.simulators. Il sera joignable via \
             '{name}.atelier.internal' apres le prochain cycle stop/resume de ce Workshop."
        ))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Gateway {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Outils pour demander des reglages a l'atelier (credential OpenBao, elargissement \
             d'egress) plutot que d'agir en direct. Voir docs/ARCHITECTURE.md.",
        )
    }
}
