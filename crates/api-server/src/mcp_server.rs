//! Serveur MCP externe embarque dans `api-server` (Jalon M4, `/v1/mcp`) :
//! permet a un client MCP generique (Claude Desktop, Cursor...) de piloter
//! le cycle de vie des `Workshop` de son proprietaire authentifie, avec les
//! memes regles de visibilite/action que la route REST equivalente (voir
//! `crate::routes::ensure_owner`) — ce module reutilise directement les
//! memes primitives (`workshops_api`, `ensure_owner`, `validate_name`,
//! `patch_desired_state`) plutot que d'appeler les handlers Axum de
//! `crate::routes`, pour ne pas avoir a reconstruire des extracteurs Axum a
//! la main.
//!
//! ## Transport (tache 4.1.3)
//! La spec d'origine (`docs/specs/04-external-mcp-server.md`) decrit le
//! transport legacy MCP 2024-11-05 (`GET /sse` + `POST /messages` separes).
//! Le SDK Rust officiel (`rmcp` 3.1.4, deja utilise par
//! `crates/mcp-gateway`) n'implemente plus ce transport : seul le
//! transport **Streamable HTTP** (spec MCP courante, un seul endpoint qui
//! sert a la fois le flux SSE en `GET` et la reception des appels en
//! `POST`) est disponible cote serveur. C'est ce que parlent les clients
//! MCP actuels (dont Claude Desktop/Cursor) — adaptation deliberee,
//! documentee ici plutot que de reimplementer un protocole obsolete a la
//! main.
//!
//! ## Authentification (tache 4.1.4)
//! Pas de logique d'auth propre a ce module : les routes `/v1/mcp*` sont
//! montees derriere le meme middleware [`crate::auth::require_auth`] que le
//! reste de l'API (voir `crate::routes::router`), qui injecte
//! `AuthenticatedUser` dans les extensions de la requete HTTP. `rmcp`
//! propage ces `http::request::Parts` jusqu'aux handlers d'outils (voir
//! `StreamableHttpService`, section "Accessing HTTP request data from tool
//! handlers") : [`authenticated_user`] les relit a chaque appel d'outil.

use crate::auth::AuthenticatedUser;
use crate::routes::{
    ensure_owner, patch_desired_state, resolve_running_pod_ip, validate_name, workshops_api,
    AppState,
};
use atelier_common::{
    DevcontainerSource, Workshop, WorkshopDesiredState, WorkshopResources, WorkshopSpec,
};
use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use http::request::Parts;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::tool::Extension as McpExtension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{ServerCapabilities, ServerInfo};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};

#[derive(Clone)]
pub struct WorkshopMcpServer {
    tool_router: ToolRouter<Self>,
    state: AppState,
    /// Identite pre-fixee pour le transport WebSocket (`async_rw`). Nul pour
    /// le transport Streamable HTTP, ou l'identite est lue depuis les
    /// `http::request::Parts` propagees par `StreamableHttpService` a chaque
    /// appel d'outil.
    pinned_user: Option<AuthenticatedUser>,
}

impl WorkshopMcpServer {
    pub fn new(state: AppState) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
            pinned_user: None,
        }
    }

    /// Constructeur pour le transport WebSocket : pre-fixe l'identite
    /// authentifiee (extraite avant l'upgrade WebSocket) plutot que de la
    /// relire depuis des `Parts` HTTP inexistantes dans un flux `async_rw`.
    pub fn with_user(state: AppState, user: AuthenticatedUser) -> Self {
        Self {
            tool_router: Self::tool_router(),
            state,
            pinned_user: Some(user),
        }
    }
}

/// Relit le sujet JWT authentifie depuis les `http::request::Parts`
/// propagees par `StreamableHttpService`, ou depuis l'identite pre-fixee
/// dans le serveur pour le transport WebSocket. Reste un `Result` pour ne
/// jamais paniquer si ce module etait un jour monte hors de ce middleware.
fn authenticated_user(
    pinned: Option<&AuthenticatedUser>,
    parts: &Parts,
) -> Result<AuthenticatedUser, ErrorData> {
    if let Some(user) = pinned {
        return Ok(user.clone());
    }
    parts
        .extensions
        .get::<AuthenticatedUser>()
        .cloned()
        .ok_or_else(|| {
            ErrorData::internal_error(
                "requete MCP recue sans identite authentifiee (middleware require_auth absent ?)",
                None,
            )
        })
}

fn api_error_to_mcp(err: crate::routes::ApiError) -> ErrorData {
    ErrorData::invalid_request(err.message().to_string(), None)
}

/// Tache 4.1.2 (Fast-Fail) : refuse immediatement les appels de `create_workshop`
/// si LiteLLM ou OpenBao est injoignable, pour ne jamais provisionner un
/// Workshop sans politique de budget (LiteLLM) ou de secrets (OpenBao)
/// active. Comme le reste du projet, une dependance non configuree
/// (`Option::None`) est traitee comme "fonctionnalite desactivee", pas
/// comme "injoignable" — seule une dependance CONFIGUREE mais injoignable
/// bloque la creation. Adaptation au transport JSON-RPC : la spec d'origine
/// demandait un HTTP 503, structurellement impossible a renvoyer une fois
/// a l'interieur d'un appel d'outil MCP reussi au niveau transport (la
/// reponse HTTP Streamable est toujours 200, le JSON-RPC porte sa propre
/// erreur) — cette fonction renvoie donc une erreur JSON-RPC explicite a la
/// place, que le client MCP restitue a l'utilisateur/l'agent appelant.
async fn ensure_state_creating_dependencies_reachable(state: &AppState) -> Result<(), ErrorData> {
    if let Some(litellm_addr) = &state.litellm_addr {
        let reachable = reqwest::Client::new()
            .get(format!("http://{litellm_addr}/health/liveliness"))
            .send()
            .await
            .map(|resp| resp.status().is_success())
            .unwrap_or(false);
        if !reachable {
            return Err(fast_fail_error("LiteLLM"));
        }
    }
    if let Some(openbao_addr) = &state.openbao_addr {
        let reachable = reqwest::Client::new()
            .get(format!("{openbao_addr}/v1/sys/health"))
            .send()
            .await
            .is_ok();
        if !reachable {
            return Err(fast_fail_error("OpenBao"));
        }
    }
    Ok(())
}

fn fast_fail_error(dependency: &str) -> ErrorData {
    ErrorData::internal_error(
        format!(
            "Security dependencies unreachable: {dependency} injoignable, creation refusee \
             (Fast-Fail — voir docs/specs/04-external-mcp-server.md)"
        ),
        None,
    )
}

// Champs a plat (pas d'imbrication de `atelier_common::{DevcontainerSource,
// WorkshopResources}`) : ces types derivent `JsonSchema` de `schemars 0.8`
// (via `kube-derive`), incompatible avec `schemars 1.x` qu'exige la macro
// `#[tool]` de `rmcp` 3.1.4 (deux versions majeures distinctes de la meme
// crate dans le graphe de dependances, constate a la compilation). Reconstruits
// manuellement en `atelier_common::{DevcontainerSource, WorkshopResources}`
// dans `create_workshop` ci-dessous.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct CreateWorkshopParams {
    /// Nom de la ressource Kubernetes sous-jacente (lettres minuscules,
    /// chiffres, tirets — RFC 1123 DNS label).
    name: String,
    /// URL du depot git contenant la definition devcontainer.
    devcontainer_repo: String,
    /// Branche, tag ou commit a utiliser. Par defaut "HEAD".
    #[serde(default)]
    devcontainer_revision: Option<String>,
    /// Chemin vers devcontainer.json dans le depot. Par defaut
    /// ".devcontainer/devcontainer.json".
    #[serde(default)]
    devcontainer_config_path: Option<String>,
    /// Quantite de CPU allouee au pod parent (ex: "2", "500m").
    cpu: String,
    /// Quantite de memoire allouee au pod parent (ex: "4Gi").
    memory: String,
    /// Taille du disque de la microVM (ex: "20Gi"). Optionnel.
    #[serde(default)]
    disk: Option<String>,
    /// Budget maximal en dollars US alloue a la Virtual Key LiteLLM de ce
    /// Workshop. Optionnel (pas de plafond specifique si absent).
    #[serde(default)]
    max_llm_budget_usd: Option<f64>,
    /// Domaines autorises en sortie par net-proxy (correspondance exacte ou
    /// wildcard `*.domaine`).
    #[serde(default)]
    egress_allowlist: Vec<String>,
    /// Outils/simulateurs a exposer via mcp-gateway (ex: "identity", "egress").
    #[serde(default)]
    tools: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct WorkshopNameParams {
    /// Nom du Workshop cible.
    name: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
struct ExecInWorkshopParams {
    /// Nom du Workshop cible.
    name: String,
    /// Commande shell a executer dans le guest (via SSH, utilisateur
    /// "vscode").
    command: String,
}

#[tool_router]
impl WorkshopMcpServer {
    #[tool(
        description = "Cree un nouveau Workshop (environnement de developpement isole, microVM Firecracker) pour l'utilisateur authentifie. Refuse (Fast-Fail) si LiteLLM ou OpenBao, configures, sont injoignables."
    )]
    async fn create_workshop(
        &self,
        McpExtension(parts): McpExtension<Parts>,
        Parameters(params): Parameters<CreateWorkshopParams>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        ensure_state_creating_dependencies_reachable(&self.state).await?;
        validate_name(&params.name).map_err(api_error_to_mcp)?;

        let workshop = Workshop::new(
            &params.name,
            WorkshopSpec {
                devcontainer: DevcontainerSource {
                    repo: params.devcontainer_repo,
                    revision: params
                        .devcontainer_revision
                        .unwrap_or_else(|| "HEAD".to_string()),
                    config_path: params
                        .devcontainer_config_path
                        .unwrap_or_else(|| ".devcontainer/devcontainer.json".to_string()),
                },
                resources: WorkshopResources {
                    cpu: params.cpu,
                    memory: params.memory,
                    disk: params.disk,
                    max_llm_budget_usd: params.max_llm_budget_usd,
                },
                egress_allowlist: params.egress_allowlist,
                tools: params.tools,
                identity_injection_rules: Vec::new(),
                owner_subject: user.0,
                desired_state: WorkshopDesiredState::Running,
            },
        );

        let created = workshops_api(&self.state)
            .create(&Default::default(), &workshop)
            .await
            .map_err(|err| api_error_to_mcp(err.into()))?;
        serde_json::to_string_pretty(&created)
            .map_err(|err| ErrorData::internal_error(format!("serialisation: {err}"), None))
    }

    #[tool(description = "Liste les Workshops appartenant a l'utilisateur authentifie.")]
    async fn list_workshops(
        &self,
        McpExtension(parts): McpExtension<Parts>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        let all = workshops_api(&self.state)
            .list(&Default::default())
            .await
            .map_err(|err| api_error_to_mcp(err.into()))?;
        let mine: Vec<Workshop> = all
            .items
            .into_iter()
            .filter(|w| w.spec.owner_subject == user.0)
            .collect();
        serde_json::to_string_pretty(&mine)
            .map_err(|err| ErrorData::internal_error(format!("serialisation: {err}"), None))
    }

    #[tool(
        description = "Recupere le statut courant d'un Workshop (phase, pod parent, digest d'image...)."
    )]
    async fn get_workshop_status(
        &self,
        McpExtension(parts): McpExtension<Parts>,
        Parameters(WorkshopNameParams { name }): Parameters<WorkshopNameParams>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        let workshop = workshops_api(&self.state)
            .get(&name)
            .await
            .map_err(|err| api_error_to_mcp(err.into()))?;
        ensure_owner(&workshop, &user).map_err(api_error_to_mcp)?;
        serde_json::to_string_pretty(&workshop.status)
            .map_err(|err| ErrorData::internal_error(format!("serialisation: {err}"), None))
    }

    #[tool(
        description = "Met un Workshop en veille (snapshot Firecracker + liberation du pod parent)."
    )]
    async fn suspend_workshop(
        &self,
        McpExtension(parts): McpExtension<Parts>,
        Parameters(WorkshopNameParams { name }): Parameters<WorkshopNameParams>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        let updated =
            patch_desired_state(&self.state, &user, &name, WorkshopDesiredState::Suspended)
                .await
                .map_err(api_error_to_mcp)?;
        serde_json::to_string_pretty(&updated.0)
            .map_err(|err| ErrorData::internal_error(format!("serialisation: {err}"), None))
    }

    #[tool(
        description = "Reprend un Workshop suspendu (recree le pod parent, restaure depuis le dernier snapshot)."
    )]
    async fn resume_workshop(
        &self,
        McpExtension(parts): McpExtension<Parts>,
        Parameters(WorkshopNameParams { name }): Parameters<WorkshopNameParams>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        let updated = patch_desired_state(&self.state, &user, &name, WorkshopDesiredState::Running)
            .await
            .map_err(api_error_to_mcp)?;
        serde_json::to_string_pretty(&updated.0)
            .map_err(|err| ErrorData::internal_error(format!("serialisation: {err}"), None))
    }

    #[tool(
        description = "Supprime definitivement un Workshop (nettoyage asynchrone via le finalizer atelier.dev/cleanup)."
    )]
    async fn delete_workshop(
        &self,
        McpExtension(parts): McpExtension<Parts>,
        Parameters(WorkshopNameParams { name }): Parameters<WorkshopNameParams>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        let workshop = workshops_api(&self.state)
            .get(&name)
            .await
            .map_err(|err| api_error_to_mcp(err.into()))?;
        ensure_owner(&workshop, &user).map_err(api_error_to_mcp)?;
        workshops_api(&self.state)
            .delete(&name, &Default::default())
            .await
            .map_err(|err| api_error_to_mcp(err.into()))?;
        Ok(format!("suppression de \"{name}\" demandee"))
    }

    #[tool(
        description = "Execute une commande dans le Workshop, de facon asynchrone et bufferisee : renvoie immediatement un execution_id (l'execution continue meme apres deconnexion), reconnexion possible via GET /v1/workshops/{name}/exec/{id}/stream. Refuse (Fast-Fail) si LiteLLM ou OpenBao, configures, sont injoignables, ou si le Workshop n'est pas Running."
    )]
    async fn exec_in_workshop(
        &self,
        McpExtension(parts): McpExtension<Parts>,
        Parameters(ExecInWorkshopParams { name, command }): Parameters<ExecInWorkshopParams>,
    ) -> Result<String, ErrorData> {
        let user = authenticated_user(self.pinned_user.as_ref(), &parts)?;
        // Meme garantie que create_workshop (tache 4.1.2) : exec_in_workshop
        // est elle aussi listee comme "creatrice d'etat" par
        // docs/specs/04-external-mcp-server.md (elle demarre un processus
        // dans le guest).
        ensure_state_creating_dependencies_reachable(&self.state).await?;

        let workshop = workshops_api(&self.state)
            .get(&name)
            .await
            .map_err(|err| api_error_to_mcp(err.into()))?;
        ensure_owner(&workshop, &user).map_err(api_error_to_mcp)?;
        let pod_ip = resolve_running_pod_ip(&self.state, &workshop)
            .await
            .map_err(api_error_to_mcp)?;

        let session_auth = self.state.session_auth.as_ref().ok_or_else(|| {
            ErrorData::internal_error(
                "OPENBAO_ADDR non configure : exec_in_workshop indisponible (aucune cle SSH accessible)",
                None,
            )
        })?;
        let private_key = session_auth.ssh_private_key(&name).await.ok_or_else(|| {
            ErrorData::internal_error(
                "cle SSH indisponible pour ce Workshop (pas encore provisionnee, ou OpenBao injoignable)",
                None,
            )
        })?;

        let execution_id = crate::exec::spawn(
            self.state.clone(),
            user.0,
            name.clone(),
            pod_ip,
            private_key,
            command,
        )
        .await
        .map_err(|err| {
            ErrorData::internal_error(format!("enregistrement de l'execution: {err}"), None)
        })?;

        serde_json::to_string_pretty(&serde_json::json!({
            "executionId": execution_id,
            "streamUrl": format!("/v1/workshops/{name}/exec/{execution_id}/stream"),
        }))
        .map_err(|err| ErrorData::internal_error(format!("serialisation: {err}"), None))
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WorkshopMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Pilote le cycle de vie de Workshops Atelier (environnements de developpement \
             isoles) : create_workshop, list_workshops, get_workshop_status, suspend_workshop, \
             resume_workshop, delete_workshop, exec_in_workshop (asynchrone et bufferise, voir \
             GET /v1/workshops/{name}/exec/{id}/stream pour la reconnexion).",
        )
    }
}

/// Construit le service Streamable HTTP monté sous `/v1/mcp` par
/// `crate::routes::router` (tache 4.1.3). Une instance de
/// [`WorkshopMcpServer`] est créée par nouvelle session MCP (`AppState`
/// est bon marche à cloner : `kube::Client`/`sqlx::PgPool` sont déjà
/// des poignées partagées en interne).
pub fn streamable_http_service(
    state: AppState,
) -> StreamableHttpService<WorkshopMcpServer, LocalSessionManager> {
    // Protection anti-DNS-rebinding basee sur l'en-tete Host desactivee :
    // ce serveur MCP est deja protege par le meme middleware OIDC
    // (`require_auth`, tache 4.1.4) que le reste de l'API, et le domaine
    // public (`domains.apiServer` du chart Helm) varie par installation —
    // pas de liste statique raisonnable a maintenir ici, contrairement a
    // `crates/mcp-gateway` qui ne voit que ses propres alias internes fixes.
    let http_config = StreamableHttpServerConfig::default().disable_allowed_hosts();
    StreamableHttpService::new(
        move || Ok(WorkshopMcpServer::new(state.clone())),
        Default::default(),
        http_config,
    )
}

/// Handler WebSocket pour `/v1/mcp/ws` (tache 4.1.3 — complement du
/// transport Streamable HTTP).
///
/// La difficulte du transport WebSocket avec `rmcp` est que
/// `StreamableHttpService` propage les `http::request::Parts` jusqu'aux
/// handlers d'outils via le mecanisme `Extension<Parts>` de la crate, ce
/// qui n'est pas disponible dans le transport `async_rw` (flux brut sans
/// contexte HTTP). Solution : extraire l'identite authentifiee **avant**
/// l'upgrade WebSocket (le middleware `require_auth` l'a deja positionnee
/// dans les extensions Axum de la requete d'upgrade), et la stocker dans
/// `WorkshopMcpServer::with_user` pour que les outils y accedent
/// directement plutot que via `Parts`.
///
/// Protocole de framing : chaque frame WebSocket texte contient un message
/// JSON-RPC 2.0 complet. `rmcp::ServiceExt::serve` attend du NDJSON
/// (JSON-RPC termine par `\n` sur un flux continu) — la fonction
/// [`bridge_ws_ndjson`] assure la conversion bidirectionnelle.
pub async fn mcp_ws_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| async move {
        let server = WorkshopMcpServer::with_user(state, user);
        // Cree une paire de canaux duplex en memoire :
        //   - server_side : passe a rmcp (AsyncRead + AsyncWrite), qui y ecrit
        //     ses reponses JSON-RPC et y lit les requetes.
        //   - client_side : lie a la WebSocket par le bridge ci-dessous.
        let (server_side, client_side) = tokio::io::duplex(65_536);
        tokio::spawn(bridge_ws_ndjson(socket, client_side));
        match server.serve(server_side).await {
            Ok(running) => {
                if let Err(err) = running.waiting().await {
                    tracing::debug!(%err, "session MCP WebSocket terminee");
                }
            }
            Err(err) => tracing::warn!(%err, "echec d'initialisation d'une session MCP WebSocket"),
        }
    })
}

/// Relaie les messages JSON-RPC entre une WebSocket (framing par message)
/// et un flux NDJSON (framing par ligne `\n`) consomme par rmcp.
///
/// - WS → duplex : chaque frame texte recue → `message + "\n"` ecrit dans
///   la moitie ecriture du canal.
/// - duplex → WS : chaque ligne lue depuis la moitie lecture du canal →
///   frame texte WebSocket (sans le `\n`).
async fn bridge_ws_ndjson(socket: WebSocket, duplex: tokio::io::DuplexStream) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let (ws_sink, mut ws_stream) = socket.split();
    let (duplex_reader, duplex_writer) = tokio::io::split(duplex);
    let ws_sink = std::sync::Arc::new(tokio::sync::Mutex::new(ws_sink));

    // WS → duplex : chaque frame texte → ligne NDJSON
    let ws_to_ndjson = {
        let mut duplex_writer = duplex_writer;
        async move {
            while let Some(msg) = ws_stream.next().await {
                match msg {
                    Ok(WsMessage::Text(text)) => {
                        if duplex_writer.write_all(text.as_bytes()).await.is_err() {
                            break;
                        }
                        if duplex_writer.write_all(b"\n").await.is_err() {
                            break;
                        }
                    }
                    Ok(WsMessage::Close(_)) | Err(_) => break,
                    _ => {} // Ping/Pong/Binary ignores
                }
            }
        }
    };

    // duplex → WS : chaque ligne NDJSON → frame texte
    let ndjson_to_ws = {
        let ws_sink = std::sync::Arc::clone(&ws_sink);
        async move {
            let mut lines = BufReader::new(duplex_reader).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if line.is_empty() {
                    continue;
                }
                let mut sink = ws_sink.lock().await;
                if sink.send(WsMessage::Text(line.into())).await.is_err() {
                    break;
                }
            }
        }
    };

    tokio::select! {
        _ = ws_to_ndjson => {}
        _ = ndjson_to_ws => {}
    }
}
