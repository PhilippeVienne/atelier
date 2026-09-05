//! Integrations ChatOps (tache 9.7, spec
//! `docs/specs/14-devex-cli-simulateurs-hitl.md` §5.4) : notifie Slack de
//! chaque nouvelle demande HITL (`crate::approvals::create_approval`) avec
//! des boutons Approuver/Rejeter (Block Kit), et recoit les clics via
//! `POST /v1/webhooks/slack/interactions`, valide par signature HMAC-SHA256
//! (meme algorithme que documente par Slack : `v0:{timestamp}:{body}`
//! signe avec le secret d'application, prefixe `v0=`).
//!
//! **Limite assumee** (non resolue dans cette tache, voir
//! `crate::approvals::apply_decision`) : une signature HMAC valide prouve
//! que la requete vient bien de l'app Slack configuree, jamais de QUEL
//! groupe l'utilisateur Slack qui a clique est membre — ce module traite
//! donc toute decision recue par ce webhook comme un bypass admin
//! (`is_admin = true`), exactement comme un administrateur de l'instance
//! pourrait le faire via l'API REST. Une cartographie utilisateur
//! Slack -> groupe Atelier resterait a batir avant un usage en production
//! avec plusieurs equipes cloisonnees.
//!
//! **Non verifie dans cette session** (pas de workspace Slack reel ni acces
//! reseau pour confirmer un vecteur de test officiel) : la conformite
//! exacte a l'algorithme documente par Slack, la reception effective d'une
//! notification par un vrai client Slack, et le clic reel sur un bouton
//! Slack. Verifie a la place : round-trip HMAC auto-coherent
//! (`tests::round_trip_sign_then_verify`, corps/secret modifies bien
//! rejetes), et l'envoi HTTP reel d'un payload Block Kit vers un serveur de
//! test local (`crates/api-server/tests/chatops.rs`).

use crate::approvals::HitlRequest;
use crate::routes::AppState;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use hmac::{Hmac, Mac};
use serde_json::json;
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

fn category_label(category: &str) -> &'static str {
    match category {
        "ALLOWLIST_EXPANSION" => "Extension d'allowlist",
        "SECRET_REQUEST" => "Demande de secret",
        "PR_GATEWAY" => "Validation de Pull Request",
        "SHELL_COMMAND" => "Commande shell",
        _ => "Demande d'approbation",
    }
}

/// Construit le payload Slack (Block Kit) pour une nouvelle demande HITL —
/// un bloc de texte descriptif suivi d'un bloc `actions` a deux boutons,
/// dont le `value` porte l'UUID de la demande (relu tel quel par
/// `slack_interactions` a la reception du clic).
pub(crate) fn build_slack_payload(request: &HitlRequest) -> serde_json::Value {
    let text = format!(
        "*{}* sur le Workshop `{}`\nDemande par : `{}`\n```{}```",
        category_label(&request.category),
        request.workshop_name,
        request.requested_by,
        serde_json::to_string_pretty(&request.payload).unwrap_or_default()
    );
    json!({
        "blocks": [
            {
                "type": "section",
                "text": { "type": "mrkdwn", "text": text }
            },
            {
                "type": "actions",
                "block_id": request.id.to_string(),
                "elements": [
                    {
                        "type": "button",
                        "text": { "type": "plain_text", "text": "Approuver" },
                        "style": "primary",
                        "action_id": "approve",
                        "value": request.id.to_string()
                    },
                    {
                        "type": "button",
                        "text": { "type": "plain_text", "text": "Rejeter" },
                        "style": "danger",
                        "action_id": "reject",
                        "value": request.id.to_string()
                    }
                ]
            }
        ]
    })
}

/// Poste la notification vers le webhook Slack configure — best-effort,
/// jamais bloquant pour l'appelant (`create_approval`) : un webhook absent
/// ou momentanement injoignable ne doit jamais empecher l'enregistrement de
/// la demande HITL elle-meme.
pub async fn notify_slack(webhook_url: &str, request: &HitlRequest) {
    let payload = build_slack_payload(request);
    let result = reqwest::Client::new()
        .post(webhook_url)
        .json(&payload)
        .send()
        .await;
    match result {
        Ok(resp) if !resp.status().is_success() => {
            tracing::warn!(status = %resp.status(), "notification Slack refusee");
        }
        Err(err) => tracing::warn!(%err, "webhook Slack injoignable"),
        Ok(_) => {}
    }
}

/// Verifie la signature HMAC-SHA256 d'une requete de webhook Slack, selon
/// l'algorithme documente par Slack (`v0:{timestamp}:{body}`, hex, prefixe
/// `v0=`). Comparaison en temps constant (pas de retour anticipe des le
/// premier octet different) : une signature est un secret partiel, la
/// comparer comme une chaine ordinaire ouvrirait une attaque par
/// canal auxiliaire (timing).
pub(crate) fn verify_slack_signature(
    signing_secret: &str,
    timestamp: &str,
    body: &str,
    signature: &str,
) -> bool {
    let Ok(mut mac) = HmacSha256::new_from_slice(signing_secret.as_bytes()) else {
        return false;
    };
    mac.update(format!("v0:{timestamp}:{body}").as_bytes());
    let computed = mac.finalize().into_bytes();
    let computed_hex: String = computed.iter().map(|b| format!("{b:02x}")).collect();
    let expected = format!("v0={computed_hex}");
    constant_time_eq(expected.as_bytes(), signature.as_bytes())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// `POST /v1/webhooks/slack/interactions` : recoit le clic sur un bouton
/// Slack. Hors du groupe de routes protegees par JWT OIDC
/// (`crate::auth::require_auth`) — l'authenticite est etablie par la
/// signature HMAC ci-dessus, jamais par un jeton (Slack n'en presente pas).
pub async fn slack_interactions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Result<StatusCode, StatusCode> {
    let Some(signing_secret) = &state.slack_signing_secret else {
        tracing::warn!("ATELIER_SLACK_SIGNING_SECRET absent, webhook Slack refuse");
        return Err(StatusCode::SERVICE_UNAVAILABLE);
    };
    let timestamp = headers
        .get("X-Slack-Request-Timestamp")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let signature = headers
        .get("X-Slack-Signature")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let body_str = std::str::from_utf8(&body).map_err(|_| StatusCode::BAD_REQUEST)?;

    if !verify_slack_signature(signing_secret, timestamp, body_str, signature) {
        tracing::warn!("signature Slack invalide, webhook refuse");
        return Err(StatusCode::UNAUTHORIZED);
    }

    // Corps `application/x-www-form-urlencoded` avec un seul champ
    // `payload` portant le JSON de l'interaction (format Slack standard
    // pour les block actions).
    let form: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(&body).into_owned().collect();
    let Some(payload_json) = form.get("payload") else {
        return Err(StatusCode::BAD_REQUEST);
    };
    let payload: serde_json::Value =
        serde_json::from_str(payload_json).map_err(|_| StatusCode::BAD_REQUEST)?;

    let action = payload["actions"].get(0).ok_or(StatusCode::BAD_REQUEST)?;
    let action_id = action["action_id"]
        .as_str()
        .ok_or(StatusCode::BAD_REQUEST)?;
    let value = action["value"].as_str().ok_or(StatusCode::BAD_REQUEST)?;
    let id = uuid::Uuid::parse_str(value).map_err(|_| StatusCode::BAD_REQUEST)?;
    let decision = match action_id {
        "approve" => "APPROVED",
        "reject" => "REJECTED",
        _ => return Err(StatusCode::BAD_REQUEST),
    };
    let slack_user = payload["user"]["username"]
        .as_str()
        .or_else(|| payload["user"]["id"].as_str())
        .unwrap_or("slack-user-inconnu");

    // Bypass admin assume, voir la doc de tete de module.
    match crate::approvals::apply_decision(
        &state,
        id,
        decision,
        None,
        &format!("slack:{slack_user}"),
        true,
        &[String::new()],
    )
    .await
    {
        Ok(_) => Ok(StatusCode::OK),
        Err(err) => {
            tracing::warn!(reason = %err.message(), "decision via webhook Slack refusee");
            Ok(StatusCode::OK) // Slack n'exploite pas le code d'erreur, seulement le corps.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Round-trip auto-coherent (signe puis verifie avec le meme code) :
    /// PAS une verification contre le vecteur officiel Slack (aucun acces
    /// reseau disponible dans cette session pour le confirmer aupres de la
    /// documentation Slack — a faire avant une mise en production reelle,
    /// voir la doc de tete de module). Verifie neanmoins que la formule
    /// (`v0:{timestamp}:{body}`, HMAC-SHA256, hex, prefixe `v0=`) produit
    /// une signature stable et deterministe.
    fn sign(signing_secret: &str, timestamp: &str, body: &str) -> String {
        let mut mac = HmacSha256::new_from_slice(signing_secret.as_bytes()).unwrap();
        mac.update(format!("v0:{timestamp}:{body}").as_bytes());
        let hex: String = mac
            .finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        format!("v0={hex}")
    }

    #[test]
    fn round_trip_sign_then_verify() {
        let secret = "un-secret-de-test";
        let timestamp = "1531420618";
        let body = "payload=%7B%22actions%22%3A%5B%5D%7D";
        let signature = sign(secret, timestamp, body);
        assert!(verify_slack_signature(secret, timestamp, body, &signature));
    }

    #[test]
    fn rejects_a_tampered_body() {
        let secret = "un-secret-de-test";
        let timestamp = "1531420618";
        let signature = sign(secret, timestamp, "corps-original");
        assert!(!verify_slack_signature(
            secret,
            timestamp,
            "corps-modifie",
            &signature
        ));
    }

    #[test]
    fn rejects_a_signature_from_a_different_secret() {
        let timestamp = "1531420618";
        let body = "payload=%7B%7D";
        let signature = sign("secret-a", timestamp, body);
        assert!(!verify_slack_signature(
            "secret-b", timestamp, body, &signature
        ));
    }

    #[test]
    fn block_kit_payload_carries_the_request_id_as_button_value() {
        let request = HitlRequest {
            id: uuid::Uuid::new_v4(),
            tenant: "atelier-demo".into(),
            workshop_name: "demo".into(),
            category: "ALLOWLIST_EXPANSION".into(),
            requested_by: "agent".into(),
            payload: json!({ "host": "api.stripe.com" }),
            status: "PENDING".into(),
            decided_by: None,
            decision_reason: None,
            created_at: chrono::Utc::now(),
            expires_at: chrono::Utc::now(),
            decided_at: None,
        };
        let payload = build_slack_payload(&request);
        let actions = &payload["blocks"][1]["elements"];
        assert_eq!(actions[0]["value"], request.id.to_string());
        assert_eq!(actions[0]["action_id"], "approve");
        assert_eq!(actions[1]["action_id"], "reject");
    }
}
