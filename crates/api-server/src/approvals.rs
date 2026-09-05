//! Socle HITL ("Human-in-the-Loop", tache 9.5, spec
//! `docs/specs/14-devex-cli-simulateurs-hitl.md` §5) : une demande
//! d'approbation par action sensible de l'agent (extension d'allowlist,
//! secret, validation de PR...) est enregistree dans PostgreSQL
//! (`hitl_requests`, migration `20260905000000_hitl_requests.sql`) avec un
//! TTL de 15 minutes, et attend la decision d'un humain membre du groupe
//! proprietaire du Workshop (ou d'un administrateur de l'instance).
//!
//! Isolation multi-tenant par RLS comme le reste de cette base (`tenant` =
//! groupe proprietaire), avec un deuxieme predicat `app.is_admin` : un
//! administrateur peut decider une demande d'un Workshop dont il n'est pas
//! membre du groupe, exactement comme `routes::list_workshops` lui montre
//! tous les Workshops.
//!
//! Ce module ne cable PAS encore d'appelant machine (`mcp-gateway`,
//! `pm-engine`) vers ces endpoints : ils sont pour l'instant utilisables
//! par tout sujet authentifie proprietaire du Workshop concerne, comme le
//! reste de cette API. Le cablage agent -> HITL est laisse a une tache
//! suivante (voir `docs/specs/PLAN-ACTION-GLOBAL.md`, tache 9.5).

use crate::auth::{AuthenticatedUser, Claims};
use crate::routes::{ensure_owner, workshop_tenant, workshops_api, ApiError, AppState, ADMIN_ROLE};
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{Postgres, Transaction};
use uuid::Uuid;

const VALID_CATEGORIES: &[&str] = &[
    "ALLOWLIST_EXPANSION",
    "SECRET_REQUEST",
    "PR_GATEWAY",
    "SHELL_COMMAND",
];

#[derive(Debug, Serialize, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct HitlRequest {
    id: Uuid,
    tenant: String,
    workshop_name: String,
    category: String,
    requested_by: String,
    payload: serde_json::Value,
    status: String,
    decided_by: Option<String>,
    decision_reason: Option<String>,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
}

/// Positionne `app.current_tenant` (portee transaction, voir
/// `crate::exec::append_chunk` pour le meme motif) avant toute requete sur
/// `hitl_requests` : c'est ce qui fait respecter le RLS de la migration.
async fn set_tenant(tx: &mut Transaction<'_, Postgres>, tenant: &str) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(tenant)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// Positionne `app.is_admin` : voir la doc de la policy RLS dans la
/// migration. Toujours appele explicitement (jamais implicite) pour qu'un
/// oubli fasse echouer fermé (RLS refuse), pas l'inverse.
async fn set_admin(tx: &mut Transaction<'_, Postgres>, is_admin: bool) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('app.is_admin', $1, true)")
        .bind(if is_admin { "true" } else { "false" })
        .execute(&mut **tx)
        .await?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateApprovalRequest {
    category: String,
    #[serde(default)]
    payload: serde_json::Value,
}

/// `POST /v1/workshops/{name}/approvals` : enregistre une nouvelle demande
/// HITL pour ce Workshop. Le sujet authentifie doit etre membre du groupe
/// proprietaire (meme regle que le reste de cette API) — ce module ne
/// distingue pas encore "l'agent" du developpeur humain (voir doc de tete).
pub async fn create_approval(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    Json(req): Json<CreateApprovalRequest>,
) -> Result<(StatusCode, Json<HitlRequest>), ApiError> {
    if !VALID_CATEGORIES.contains(&req.category.as_str()) {
        return Err(ApiError::bad_request(&format!(
            "categorie invalide '{}', attendu l'une de {VALID_CATEGORIES:?}",
            req.category
        )));
    }
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    let tenant = workshop_tenant(&workshop);

    let mut tx = state.db_pool.begin().await?;
    set_tenant(&mut tx, &tenant).await?;
    let created: HitlRequest = sqlx::query_as(
        "INSERT INTO hitl_requests (tenant, workshop_name, category, requested_by, payload) \
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(&tenant)
    .bind(&name)
    .bind(&req.category)
    .bind(&user.subject)
    .bind(&req.payload)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok((StatusCode::CREATED, Json(created)))
}

/// `GET /v1/workshops/{name}/approvals` : liste les demandes HITL de ce
/// Workshop (tous statuts), les plus recentes en premier.
pub async fn list_approvals(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
) -> Result<Json<Vec<HitlRequest>>, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    let tenant = workshop_tenant(&workshop);

    let mut tx = state.db_pool.begin().await?;
    set_tenant(&mut tx, &tenant).await?;
    expire_stale(&mut tx, &name).await?;
    let rows: Vec<HitlRequest> = sqlx::query_as(
        "SELECT * FROM hitl_requests WHERE workshop_name = $1 ORDER BY created_at DESC",
    )
    .bind(&name)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(rows))
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DecisionRequest {
    /// "APPROVED" ou "REJECTED".
    decision: String,
    #[serde(default)]
    reason: Option<String>,
}

/// `POST /v1/approvals/{id}/decision` : approuve ou rejette une demande en
/// attente. Le sujet authentifie doit etre membre du groupe proprietaire du
/// Workshop concerne, ou administrateur de l'instance — voir la doc de tete
/// de module pour la double policy RLS qui l'applique aussi cote base.
///
/// Fail-closed : une demande dont le TTL est expire est basculee `EXPIRED`
/// (jamais decidee) des qu'on la rencontre, ici comme dans `list_approvals`
/// — pas besoin de tache de fond pour ce socle, l'expiration est purement
/// une fonction du temps courant, jamais un etat qui pourrait etre oublie
/// indefiniment tant qu'aucune lecture ne survient.
pub async fn decide_approval(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Extension(claims): Extension<Claims>,
    Path(id): Path<Uuid>,
    Json(req): Json<DecisionRequest>,
) -> Result<Json<HitlRequest>, ApiError> {
    let new_status = match req.decision.as_str() {
        "APPROVED" => "APPROVED",
        "REJECTED" => "REJECTED",
        other => {
            return Err(ApiError::bad_request(&format!(
                "decision invalide '{other}', attendu 'APPROVED' ou 'REJECTED'"
            )))
        }
    };

    let is_admin = claims.has_role(ADMIN_ROLE);
    let mut tx = state.db_pool.begin().await?;
    set_admin(&mut tx, is_admin).await?;

    // Si non-admin, le sujet doit appartenir au groupe (`tenant`) de la
    // demande : on tente chaque groupe du sujet tour a tour (typiquement
    // un seul) jusqu'a en trouver un qui rend la ligne visible au RLS.
    // Admin : `app.is_admin` deja positionne ci-dessus, `current_tenant`
    // n'a pas besoin d'etre juste.
    let candidate_tenants: Vec<String> = if is_admin {
        vec![String::new()]
    } else {
        user.groups.clone()
    };

    let mut found: Option<HitlRequest> = None;
    for tenant in &candidate_tenants {
        if !is_admin {
            set_tenant(&mut tx, tenant).await?;
        }
        if let Some(row) =
            sqlx::query_as::<_, HitlRequest>("SELECT * FROM hitl_requests WHERE id = $1")
                .bind(id)
                .fetch_optional(&mut *tx)
                .await?
        {
            found = Some(row);
            break;
        }
    }
    let Some(current) = found else {
        return Err(ApiError::not_found_generic(
            "demande d'approbation introuvable ou non autorisee",
        ));
    };

    // Fail-closed : expiree, on ne decide plus rien, on se contente de
    // corriger le statut si ce n'etait pas deja fait.
    if current.status == "PENDING" && current.expires_at < Utc::now() {
        sqlx::query("UPDATE hitl_requests SET status = 'EXPIRED' WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        return Err(ApiError::bad_request(
            "demande expiree (TTL depasse), decision refusee",
        ));
    }
    if current.status != "PENDING" {
        return Err(ApiError::bad_request(&format!(
            "demande deja tranchee (statut '{}')",
            current.status
        )));
    }

    let decided: HitlRequest = sqlx::query_as(
        "UPDATE hitl_requests SET status = $1, decided_by = $2, decision_reason = $3, decided_at = now() \
         WHERE id = $4 RETURNING *",
    )
    .bind(new_status)
    .bind(&user.subject)
    .bind(&req.reason)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(decided))
}

/// Bascule silencieusement en `EXPIRED` toute demande `PENDING` de ce
/// Workshop dont le TTL est deja depasse — appele avant chaque lecture de
/// liste (`list_approvals`) pour que le statut renvoye au client soit
/// toujours a jour, sans dependre d'une tache de fond separee.
async fn expire_stale(
    tx: &mut Transaction<'_, Postgres>,
    workshop_name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE hitl_requests SET status = 'EXPIRED' \
         WHERE workshop_name = $1 AND status = 'PENDING' AND expires_at < now()",
    )
    .bind(workshop_name)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
