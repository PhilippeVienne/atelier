//! Endpoints CRUD + suspend/resume sur `Workshop`, proteges par
//! [`crate::auth::require_auth`]. Toutes les operations sont scopees au
//! sujet JWT authentifie (`WorkshopSpec.owner_subject`) : un client ne voit
//! et ne peut agir que sur ses propres Workshops.

use crate::auth::{require_auth, AuthState, AuthenticatedUser};
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
    /// disponibilite `/health/readiness` (voir `health_readiness`).
    pub openbao_addr: Option<String>,
    /// Client de lecture du secret `session_auth` d'un Workshop (mot de
    /// passe Basic Auth injecte dans les tunnels VS Code/Terminal, voir
    /// `crate::session_auth` et `crate::vscode::proxy_to_guest_port`).
    /// `None` si `OPENBAO_ADDR` est absent : les tunnels relaient alors sans
    /// injecter de Basic Auth (fonctionnalite optionnelle si non
    /// configuree, meme convention que le reste du projet).
    pub session_auth: Option<crate::session_auth::SessionAuthClient>,
}

pub fn router(state: AppState, auth: AuthState) -> Router {
    let health_state = state.clone();
    let protected = Router::new()
        .route("/v1/workshops", post(create_workshop).get(list_workshops))
        .route(
            "/v1/workshops/{name}",
            get(get_workshop).delete(delete_workshop),
        )
        .route("/v1/workshops/{name}/suspend", post(suspend_workshop))
        .route("/v1/workshops/{name}/resume", post(resume_workshop))
        .route("/v1/workshops/{name}/events", get(list_workshop_events))
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
pub(crate) fn ensure_owner(workshop: &Workshop, user: &AuthenticatedUser) -> Result<(), ApiError> {
    if workshop.spec.owner_subject != user.0 {
        tracing::warn!(
            workshop_owner = %workshop.spec.owner_subject,
            jwt_user = %user.0,
            "ensure_owner: sujet JWT ne match pas le proprietaire du Workshop"
        );
        return Err(ApiError::not_found());
    }
    Ok(())
}

/// IP du pod parent d'un Workshop en cours d'execution — precondition
/// commune a `portforward` et `vscode` (les deux relaient vers un port
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
}

async fn create_workshop(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Json(req): Json<CreateWorkshopRequest>,
) -> Result<(StatusCode, Json<Workshop>), ApiError> {
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
            owner_subject: user.0,
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
) -> Result<Json<Vec<Workshop>>, ApiError> {
    let all = workshops_api(&state).list(&Default::default()).await?;
    let mine = all
        .items
        .into_iter()
        .filter(|w| w.spec.owner_subject == user.0)
        .collect();
    Ok(Json(mine))
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
async fn patch_desired_state(
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
fn validate_name(name: &str) -> Result<(), ApiError> {
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

    pub(crate) fn bad_gateway(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
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
