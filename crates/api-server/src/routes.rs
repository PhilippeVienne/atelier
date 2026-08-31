//! Endpoints CRUD + suspend/resume sur `Workshop`, proteges par
//! [`crate::auth::require_auth`]. Toutes les operations sont scopees au
//! sujet JWT authentifie (`WorkshopSpec.owner_subject`) : un client ne voit
//! et ne peut agir que sur ses propres Workshops.

use crate::auth::{require_auth, AuthState, AuthenticatedUser, Claims};
use atelier_common::{
    DevcontainerSource, Workshop, WorkshopDesiredState, WorkshopResources, WorkshopSpec,
};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use axum::{Json, Router};
use k8s_openapi::api::core::v1::{Event as K8sEvent, Pod};
use kube::api::{Api, DeleteParams, ListParams, Patch, PatchParams};
use kube::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const FIELD_MANAGER: &str = "atelier-api-server";

#[derive(Clone)]
pub struct AppState {
    pub client: Client,
    pub namespace: String,
    pub db_pool: sqlx::PgPool,
    /// Adresse d'OpenBao (`OPENBAO_ADDR`), utilisee pour la sonde de
    /// disponibilite `/health/readiness` (voir `health_readiness`) et pour
    /// la verification Fast-Fail du serveur MCP (`crate::mcp_server`,
    /// tache 4.1.2).
    pub openbao_addr: Option<String>,
    /// Adresse de la passerelle LiteLLM (`ATELIER_LLM_PROXY_ADDR`), utilisee
    /// uniquement par la verification Fast-Fail du serveur MCP
    /// (`crate::mcp_server`, tache 4.1.2) — `api-server` ne parle pas
    /// directement a LiteLLM en dehors de cette sonde de disponibilite (le
    /// provisioning des Virtual Keys reste gere par `controller`, voir
    /// `crates/controller/src/litellm.rs`).
    pub litellm_addr: Option<String>,
    /// Client de lecture du secret `session_auth` d'un Workshop (mot de
    /// passe Basic Auth injecte dans les tunnels VS Code/Terminal, voir
    /// `crate::session_auth` et `crate::vscode::proxy_to_guest_port`).
    /// `None` si `OPENBAO_ADDR` est absent : les tunnels relaient alors sans
    /// injecter de Basic Auth (fonctionnalite optionnelle si non
    /// configuree, meme convention que le reste du projet).
    pub session_auth: Option<crate::session_auth::SessionAuthClient>,
    /// Lecture de la consommation LLM d'un Workshop (`crate::llm_budget`).
    /// `None` si `ATELIER_LLM_PROXY_ADDR`/`ATELIER_LLM_PROXY_AUTH_TOKEN` sont
    /// absents : l'endpoint repond alors 503, plutot que d'afficher un
    /// « 0,00 $ » qui se ferait passer pour une mesure.
    pub llm_budget: Option<std::sync::Arc<crate::llm_budget::LlmBudgetClient>>,
    /// Backend S3 pour l'archivage des sessions terminal (Jalon M2, voir
    /// `crate::session_recorder`). `None` si `S3_ENDPOINT` est absent :
    /// l'archivage est alors simplement desactive, aucune session n'est
    /// enregistree (fonctionnalite optionnelle si non configuree, meme
    /// convention que `session_auth`).
    pub storage: Option<std::sync::Arc<crate::storage::S3StorageBackend>>,
}

/// Role de realm requis pour les routes d'administration. Correspond au
/// role `admin` du realm Keycloak de dev (`deploy/dev/keycloak/`), a cote de
/// `developer` qui, lui, ne donne aucun privilege particulier.
const ADMIN_ROLE: &str = "admin";
/// Role minimal pour PROVISIONNER un Workshop. Correspond au role
/// `developer` du realm de dev, dont `atelier-pm-bot` est titulaire — le PM
/// provisionne des Workshops comme un utilisateur.
///
/// Seule la CREATION est soumise a un role : c'est elle qui alloue du calcul
/// (une microVM Firecracker). Les autres operations portent sur une
/// ressource qu'on possede deja, et la propriete suffit — retirer un role ne
/// doit pas empecher quelqu'un de suspendre ou supprimer ses propres
/// Workshops, sous peine de laisser tourner des microVM que plus personne ne
/// peut arreter.
const DEVELOPER_ROLE: &str = "developer";

/// Le sujet peut-il provisionner ? `admin` vaut `developer` : un role
/// d'administration qui ne permettrait pas ce que permet le role ordinaire
/// serait un piege.
fn may_provision(claims: &Claims) -> bool {
    claims.has_role(DEVELOPER_ROLE) || claims.has_role(ADMIN_ROLE)
}

pub fn router(state: AppState, auth: AuthState) -> Router {
    let health_state = state.clone();
    let mcp_service = crate::mcp_server::streamable_http_service(state.clone());
    let protected = Router::new()
        .route("/v1/workshops", post(create_workshop).get(list_workshops))
        .route(
            "/v1/workshops/{name}",
            get(get_workshop).delete(delete_workshop),
        )
        .route("/v1/workshops/{name}/suspend", post(suspend_workshop))
        .route("/v1/workshops/{name}/resume", post(resume_workshop))
        .route("/v1/workshops/{name}/events", get(list_workshop_events))
        .route("/v1/workshops/{name}/llm-budget", get(workshop_llm_budget))
        .route("/v1/admin/llm", get(admin_llm_overview))
        .route(
            "/v1/workshops/{name}/exec/{id}/stream",
            get(crate::exec::stream_handler),
        )
        .route(
            "/v1/workshops/{name}/portforward",
            get(crate::portforward::portforward),
        )
        .route(
            "/v1/workshops/{name}/vscode",
            any(crate::vscode::vscode_proxy_root),
        )
        .route(
            "/v1/workshops/{name}/vscode/",
            any(crate::vscode::vscode_proxy_root),
        )
        .route(
            "/v1/workshops/{name}/vscode/{*path}",
            any(crate::vscode::vscode_proxy),
        )
        .route(
            "/v1/workshops/{name}/terminal",
            any(crate::terminal::terminal_proxy_root),
        )
        .route(
            "/v1/workshops/{name}/terminal/",
            any(crate::terminal::terminal_proxy_root),
        )
        .route(
            "/v1/workshops/{name}/terminal/{*path}",
            any(crate::terminal::terminal_proxy),
        )
        // Serveur MCP externe (Jalon M4, taches 4.1.1-4.1.4) : transport
        // Streamable HTTP (GET pour le flux SSE, POST pour les appels
        // JSON-RPC, un seul endpoint — voir le commentaire de tete de
        // `crate::mcp_server` pour la justification de cette adaptation du
        // transport legacy 2024-11-05 decrit par
        // `docs/specs/04-external-mcp-server.md`). Protege par le meme
        // middleware OIDC que le reste de cette table de routes, via le
        // `.layer()` ci-dessous (s'applique a toutes les routes/services
        // ajoutes avant lui).
        .nest_service("/v1/mcp", mcp_service)
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(auth),
            require_auth,
        ))
        .with_state(state);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/health/liveness", get(health_liveness))
        .route("/health/readiness", get(health_readiness))
        .with_state(health_state)
        .merge(protected)
}

/// Toujours 200 tant que le process web tourne : ne verifie aucune
/// dependance externe (c'est le role de `/health/readiness`), pour que
/// Kubernetes ne redemarre jamais le pod a cause d'une dependance
/// temporairement indisponible.
async fn health_liveness() -> &'static str {
    "ok"
}

/// Verifie la connectivite active a PostgreSQL (`SELECT 1`), et a OpenBao si
/// `OPENBAO_ADDR` est configure (`GET /v1/sys/health`, sans authentification
/// requise) — reflete l'etat reel des dependances dont ce process a besoin
/// pour servir du trafic, contrairement a `/health/liveness`.
async fn health_readiness(State(state): State<AppState>) -> Response {
    if let Err(err) = sqlx::query("SELECT 1").execute(&state.db_pool).await {
        tracing::warn!(%err, "readiness: PostgreSQL injoignable");
        return (StatusCode::SERVICE_UNAVAILABLE, "postgresql injoignable").into_response();
    }

    if let Some(openbao_addr) = &state.openbao_addr {
        let reachable = reqwest::Client::new()
            .get(format!("{openbao_addr}/v1/sys/health"))
            .send()
            .await
            .is_ok();
        if !reachable {
            tracing::warn!("readiness: OpenBao injoignable");
            return (StatusCode::SERVICE_UNAVAILABLE, "openbao injoignable").into_response();
        }
    }

    (StatusCode::OK, "ok").into_response()
}

pub(crate) fn workshops_api(state: &AppState) -> Api<Workshop> {
    Api::namespaced(state.client.clone(), &state.namespace)
}

/// Une seule regle de visibilite/action partout : le sujet JWT authentifie
/// doit etre le proprietaire enregistre du Workshop. Renvoie 404 (pas 403)
/// pour un Workshop existant mais appartenant a quelqu'un d'autre : evite
/// de confirmer a un client non autorise qu'un nom donne existe deja.
/// Groupe dans lequel provisionner, valide contre ceux du jeton.
///
/// Un client ne choisit pas son perimetre, il choisit PARMI les siens : le
/// groupe demande doit figurer dans le jeton, sinon `403`. Voir
/// `docs/specs/07-groupes.md`, section 3.
///
/// Sans groupe demande : un seul groupe est retenu implicitement, plusieurs
/// exigent d'etre departages (`400`), aucun interdit la creation (`403`) —
/// deviner reviendrait a placer un environnement, et sa depense, dans un
/// groupe au hasard.
pub(crate) fn resolve_owner_group(
    requested: Option<&str>,
    user: &AuthenticatedUser,
) -> Result<String, ApiError> {
    match requested {
        Some(group) => {
            if user.groups.iter().any(|g| g == group) {
                Ok(group.to_string())
            } else {
                Err(ApiError::forbidden(format!(
                    "groupe {group:?} absent de vos groupes"
                )))
            }
        }
        None => match user.groups.as_slice() {
            [only] => Ok(only.clone()),
            [] => Err(ApiError::forbidden(
                "aucun groupe : impossible de rattacher un Workshop",
            )),
            several => Err(ApiError::bad_request(&format!(
                "plusieurs groupes ({}) : precisez `ownerGroup`",
                several.join(", ")
            ))),
        },
    }
}

pub(crate) fn ensure_owner(workshop: &Workshop, user: &AuthenticatedUser) -> Result<(), ApiError> {
    // Le GROUPE donne l'acces des qu'il est renseigne : tout membre pilote le
    // Workshop, y compris pour reprendre l'environnement d'un collegue
    // absent. C'est le sens meme de « un Workshop appartient a un groupe ».
    if let Some(group) = workshop.spec.owner_group.as_deref() {
        if user.groups.iter().any(|g| g == group) {
            return Ok(());
        }
        tracing::warn!(
            workshop_group = %group,
            jwt_user = %user.subject,
            "acces refuse : le sujet n'appartient pas au groupe proprietaire"
        );
        return Err(ApiError::not_found());
    }

    // Repli, le temps de la transition : les Workshops crees avant
    // l'introduction des groupes n'en portent pas. Disparaitra quand
    // `owner_group` deviendra obligatoire (voir docs/specs/07-groupes.md).
    if workshop.spec.owner_subject != user.subject {
        tracing::warn!(
            workshop_owner = %workshop.spec.owner_subject,
            jwt_user = %user.subject,
            "acces refuse : Workshop sans groupe, et le sujet n'en est pas le createur"
        );
        return Err(ApiError::not_found());
    }
    Ok(())
}

/// IP du pod parent d'un Workshop en cours d'execution — precondition
/// commune a `portforward` et `vscode` (les deux relaient vers un port
/// Vue d'administration de la passerelle LiteLLM.
///
/// Premiere route du projet reservee a un ROLE, et non a un proprietaire :
/// elle montre les cles et la depense de TOUS les Workshops, ce qui n'a de
/// sens que pour qui administre l'instance. Le controle se fait ici, cote
/// serveur — masquer l'entree de menu cote navigateur n'empeche personne
/// d'appeler la route directement.
async fn admin_llm_overview(
    State(state): State<AppState>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<crate::llm_budget::LlmOverview>, ApiError> {
    if !claims.has_role(ADMIN_ROLE) {
        return Err(ApiError::forbidden(
            "reserve aux administrateurs de l'instance",
        ));
    }
    let client = state.llm_budget.as_ref().ok_or_else(|| {
        ApiError::service_unavailable("passerelle LiteLLM non configuree sur cette instance")
    })?;
    Ok(Json(client.overview().await))
}

/// Consommation LLM d'un Workshop (`crate::llm_budget`).
///
/// Lecture seule et soumise a la meme regle de propriete que le reste de
/// l'API : la depense d'un Workshop en dit long sur ce qui y tourne, elle
/// n'a pas a etre lisible par un autre utilisateur.
async fn workshop_llm_budget(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<crate::llm_budget::LlmBudget>, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;

    let client = state.llm_budget.as_ref().ok_or_else(|| {
        ApiError::service_unavailable("passerelle LiteLLM non configuree sur cette instance")
    })?;
    client
        .workshop_budget(&name)
        .await
        .map(Json)
        .ok_or_else(|| ApiError::service_unavailable("passerelle LiteLLM injoignable"))
}

/// ouvert par une microVM hebergee dans ce pod, via `net-proxy`).
pub(crate) async fn resolve_running_pod_ip(
    state: &AppState,
    workshop: &Workshop,
) -> Result<String, ApiError> {
    let pod_name = workshop
        .status
        .as_ref()
        .and_then(|s| s.pod_name.clone())
        .ok_or_else(|| {
            ApiError::bad_request("le Workshop n'a pas de pod parent actif (suspendu ?)")
        })?;
    let pods: Api<Pod> = Api::namespaced(state.client.clone(), &state.namespace);
    let pod = pods.get(&pod_name).await?;
    pod.status
        .as_ref()
        .and_then(|s| s.pod_ip.clone())
        .ok_or_else(|| ApiError::bad_request("le pod parent n'a pas encore d'adresse IP"))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWorkshopRequest {
    /// Nom de la ressource Kubernetes sous-jacente : au client de le
    /// choisir (comme `kubectl create -f` avec un nom explicite), pas
    /// genere serveur — c'est ce nom qui apparait dans
    /// `secret/workshops/<name>/*` cote OpenBao et dans les URLs de cette
    /// API.
    name: String,
    devcontainer: DevcontainerSource,
    resources: WorkshopResources,
    #[serde(default)]
    egress_allowlist: Vec<String>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    identity_injection_rules: Vec<atelier_common::IdentityInjectionRule>,
    /// Groupe proprietaire. Facultatif si l'appelant n'a qu'un seul groupe ;
    /// obligatoire s'il en a plusieurs (voir `resolve_owner_group`).
    #[serde(default)]
    owner_group: Option<String>,
}

async fn create_workshop(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Extension(claims): Extension<Claims>,
    Json(req): Json<CreateWorkshopRequest>,
) -> Result<(StatusCode, Json<Workshop>), ApiError> {
    // Provisionner alloue une microVM : jusqu'ici, TOUT sujet authentifie
    // pouvait le faire, y compris un compte du realm sans aucun rapport avec
    // Atelier. Le role est donc exige ici, et nulle part ailleurs (voir
    // `DEVELOPER_ROLE`).
    if !may_provision(&claims) {
        return Err(ApiError::forbidden(
            "provisionner un Workshop requiert le role 'developer' ou 'admin'",
        ));
    }
    let owner_group = resolve_owner_group(req.owner_group.as_deref(), &user)?;
    validate_name(&req.name)?;

    let workshop = Workshop::new(
        &req.name,
        WorkshopSpec {
            devcontainer: req.devcontainer,
            resources: req.resources,
            egress_allowlist: req.egress_allowlist,
            tools: req.tools,
            identity_injection_rules: req.identity_injection_rules,
            // Jamais depuis le corps de la requete : c'est l'identite
            // verifiee par le JWT qui devient le proprietaire, pas une
            // valeur que le client pourrait usurper.
            owner_group: Some(owner_group),
            owner_subject: user.subject,
            desired_state: WorkshopDesiredState::Running,
        },
    );

    let created = workshops_api(&state)
        .create(&Default::default(), &workshop)
        .await?;
    Ok((StatusCode::CREATED, Json(created)))
}

async fn list_workshops(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Extension(claims): Extension<Claims>,
) -> Result<Json<Vec<Workshop>>, ApiError> {
    let all = workshops_api(&state).list(&Default::default()).await?;
    // Un administrateur voit tous les Workshops de l'instance : c'est le sens
    // meme de ce role, et sans cela il ne peut ni constater ce qui tourne ni
    // rattacher une depense LLM a son environnement. Les autres ne voient que
    // les leurs, comme avant.
    let visible = all
        .items
        .into_iter()
        .filter(|w| claims.has_role(ADMIN_ROLE) || w.spec.owner_subject == user.subject)
        .collect();
    Ok(Json(visible))
}

async fn get_workshop(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<Workshop>, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    Ok(Json(workshop))
}

#[derive(Debug, Serialize)]
struct WorkshopEvent {
    #[serde(rename = "type")]
    type_: String,
    reason: String,
    message: String,
    #[serde(rename = "involvedObject")]
    involved_object: String,
    /// RFC 3339, le plus recent des deux horodatages k8s disponibles
    /// (`lastTimestamp` pour les Event legacy, `eventTime` pour les
    /// Event "v1" plus recents emis par certains controllers).
    timestamp: Option<String>,
    count: i32,
}

/// Journal de creation/progression d'un Workshop : plutot que d'ajouter un
/// mecanisme de log applicatif dedie, on relaie les Event Kubernetes deja
/// emis nativement par le control plane (scheduler, kubelet, job
/// controller) pour les objets impliques — le pod parent (`{name}-parent`)
/// et le Job de build d'image (`{name}-image-build`, cf.
/// `crates/controller/src/reconcile.rs`), en plus du Workshop lui-meme.
/// Aucun nouvel etat a maintenir : c'est ce que `kubectl describe` montre
/// deja, seulement filtre et mis en forme pour le dashboard.
async fn list_workshop_events(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<Vec<WorkshopEvent>>, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;

    let pod_name = format!("{name}-parent");
    let job_name = format!("{name}-image-build");
    // Le Job cree lui-meme un pod par tentative, nomme `<job_name>-<suffix
    // aleatoire>` (convention standard du controller Job Kubernetes) : les
    // events utiles (Pulling/Pulled/Started/BackOff) sont attaches a ce pod,
    // pas au Job. D'ou le prefixe en plus de l'egalite stricte.
    let job_pod_prefix = format!("{job_name}-");

    let events_api: Api<K8sEvent> = Api::namespaced(state.client.clone(), &state.namespace);
    let all = events_api.list(&ListParams::default()).await?;

    let mut events: Vec<WorkshopEvent> = all
        .items
        .into_iter()
        .filter(|ev| {
            let involved = ev.involved_object.name.as_deref().unwrap_or_default();
            involved == name
                || involved == pod_name
                || involved == job_name
                || involved.starts_with(&job_pod_prefix)
        })
        .map(|ev| WorkshopEvent {
            type_: ev.type_.unwrap_or_else(|| "Normal".to_string()),
            reason: ev.reason.unwrap_or_default(),
            message: ev.message.unwrap_or_default(),
            involved_object: format!(
                "{}/{}",
                ev.involved_object.kind.unwrap_or_default(),
                ev.involved_object.name.unwrap_or_default()
            ),
            timestamp: ev
                .last_timestamp
                .map(|t| t.0.to_rfc3339())
                .or_else(|| ev.event_time.map(|t| t.0.to_rfc3339())),
            count: ev.count.unwrap_or(1),
        })
        .collect();

    events.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(Json(events))
}

async fn delete_workshop(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    // Pose seulement deletionTimestamp : la suppression effective attend
    // que le controller leve le finalizer atelier.dev/cleanup (entite
    // Kanidm / role OpenBao nettoyes), voir crates/controller/src/reconcile.rs.
    workshops_api(&state)
        .delete(&name, &DeleteParams::default())
        .await?;
    Ok(StatusCode::ACCEPTED)
}

async fn suspend_workshop(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<Workshop>, ApiError> {
    patch_desired_state(&state, &user, &name, WorkshopDesiredState::Suspended).await
}

async fn resume_workshop(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<Workshop>, ApiError> {
    patch_desired_state(&state, &user, &name, WorkshopDesiredState::Running).await
}

/// Patche `spec.desiredState` ; le `controller` se charge du reste
/// (snapshot Firecracker + liberation du pod parent pour `Suspended`,
/// recreation du pod + restauration depuis le snapshot pour `Running` — cf.
/// `docs/ARCHITECTURE.md`, section "Mise en veille").
pub(crate) async fn patch_desired_state(
    state: &AppState,
    user: &AuthenticatedUser,
    name: &str,
    desired_state: WorkshopDesiredState,
) -> Result<Json<Workshop>, ApiError> {
    let api = workshops_api(state);
    let workshop = api.get(name).await?;
    ensure_owner(&workshop, user)?;

    let patch = serde_json::json!({ "spec": { "desiredState": desired_state } });
    let updated = api
        .patch(
            name,
            &PatchParams::apply(FIELD_MANAGER),
            &Patch::Merge(&patch),
        )
        .await?;
    Ok(Json(updated))
}

/// Meme convention de nommage que les objets Kubernetes eux-memes (RFC 1123
/// DNS label) : evite un rejet tardif et moins clair cote API server K8s.
pub(crate) fn validate_name(name: &str) -> Result<(), ApiError> {
    let valid = !name.is_empty()
        && name.len() <= 63
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-')
        && !name.ends_with('-');
    if valid {
        Ok(())
    } else {
        Err(ApiError {
            status: StatusCode::BAD_REQUEST,
            message: "nom invalide : lettres minuscules/chiffres/tirets, 1 a 63 caracteres"
                .to_string(),
        })
    }
}

pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found() -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: "workshop introuvable".to_string(),
        }
    }

    pub(crate) fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }

    /// Fonctionnalite optionnelle non configuree, ou dependance externe
    /// momentanement injoignable : ni une erreur du client, ni un defaut de
    /// ce serveur — d'ou `503` plutot que `400` ou `500`.
    /// Le sujet est authentifie mais n'a pas le privilege requis — `403`, a
    /// distinguer d'un `401` (pas d'identite du tout) et d'un `404` (que l'on
    /// reserve a ce qui n'existe pas ou ne lui appartient pas).
    pub(crate) fn forbidden(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            message: message.into(),
        }
    }

    pub(crate) fn service_unavailable(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            message: message.into(),
        }
    }

    pub(crate) fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
    }

    /// Reutilise par `crate::mcp_server` pour formater une erreur JSON-RPC
    /// MCP a partir d'une `ApiError` (memes handlers CRUD que la route REST,
    /// voir tache 4.2.1).
    pub(crate) fn message(&self) -> &str {
        &self.message
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, self.message).into_response()
    }
}

impl From<kube::Error> for ApiError {
    fn from(err: kube::Error) -> Self {
        if let kube::Error::Api(ref resp) = err {
            if resp.code == 404 {
                return Self::not_found();
            }
            if resp.code == 409 {
                return Self {
                    status: StatusCode::CONFLICT,
                    message: "un Workshop porte deja ce nom".to_string(),
                };
            }
        }
        tracing::error!(%err, "erreur kube inattendue");
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "erreur interne".to_string(),
        }
    }
}
