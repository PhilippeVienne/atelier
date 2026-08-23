//! Tools MCP exposes a l'agent : `request_credential` (lecture d'un secret
//! OpenBao scope au Workshop), `request_egress` (elargissement a chaud de
//! l'allowlist egress de `net-proxy` pour cette session de pod) et
//! `enable_simulator` (rend joignable le sidecar `simulator`/LocalStack du
//! pod, voir `crates/controller/src/reconcile.rs`). Chaque tool n'est actif
//! que si son nom figure dans `Workshop.spec.tools` (`ATELIER_TOOLS`).

use std::collections::HashSet;
use std::sync::Arc;

use atelier_common::OpenBaoClient;
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
