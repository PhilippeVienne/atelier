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
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::Client;
use serde::Deserialize;
use std::sync::Arc;

const FIELD_MANAGER: &str = "atelier-api-server";

#[derive(Clone)]
pub struct AppState {
    pub client: Client,
    pub namespace: String,
}

pub fn router(state: AppState, auth: AuthState) -> Router {
    let protected = Router::new()
        .route("/v1/workshops", post(create_workshop).get(list_workshops))
        .route(
            "/v1/workshops/{name}",
            get(get_workshop).delete(delete_workshop),
        )
        .route("/v1/workshops/{name}/suspend", post(suspend_workshop))
        .route("/v1/workshops/{name}/resume", post(resume_workshop))
        .route(
            "/v1/workshops/{name}/portforward",
            get(crate::portforward::portforward),
        )
        .route(
            "/v1/workshops/{name}/vscode/{*path}",
            any(crate::vscode::vscode_proxy),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::new(auth),
            require_auth,
        ))
        .with_state(state);

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .merge(protected)
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
