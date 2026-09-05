//! Flux OAuth2 Device Authorization Grant (RFC 8628), generique a tout
//! fournisseur OIDC conforme (Keycloak, Entra ID, Kanidm...) : uniquement
//! via decouverte standard (RFC 8414
//! `.well-known/openid-configuration`), jamais d'appel a une API propre a un
//! produit (voir `docs/specs/00-architecture-principles-substitutability.md`
//! et `crates/api-server/src/auth.rs` qui suit le meme principe cote
//! verification de jeton).

use crate::config::ContextConfig;
use crate::tokens::{extract_subject, TokenSet};
use anyhow::{bail, Context, Result};
use chrono::{Duration, Utc};
use serde::Deserialize;
use std::time::Duration as StdDuration;

#[derive(Debug, Deserialize)]
struct DiscoveryDocument {
    device_authorization_endpoint: String,
    token_endpoint: String,
}

#[derive(Debug, Deserialize)]
struct DeviceAuthorizationResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[serde(default)]
    verification_uri_complete: Option<String>,
    #[serde(rename = "expires_in")]
    expires_in_secs: u64,
    #[serde(default = "default_interval")]
    interval: u64,
}

fn default_interval() -> u64 {
    5
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct TokenErrorResponse {
    error: String,
}

async fn discover(client: &reqwest::Client, issuer: &str) -> Result<DiscoveryDocument> {
    let url = format!(
        "{}/.well-known/openid-configuration",
        issuer.trim_end_matches('/')
    );
    client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("requete de decouverte OIDC ({url})"))?
        .error_for_status()
        .context("le document de decouverte OIDC n'a pas pu etre recupere")?
        .json()
        .await
        .context("document de decouverte OIDC invalide")
}

/// Conduit le flux complet : affiche le code utilisateur, poll le
/// `token_endpoint` jusqu'a decision de l'utilisateur, retourne le jeton
/// obtenu. Bloquant du point de vue de l'appelant (poll a intervalle fixe
/// impose par le fournisseur), borne par `expires_in` du serveur
/// d'autorisation.
pub async fn login(ctx: &ContextConfig) -> Result<TokenSet> {
    let client = reqwest::Client::new();
    let discovery = discover(&client, &ctx.issuer).await?;

    let device: DeviceAuthorizationResponse = client
        .post(&discovery.device_authorization_endpoint)
        .form(&[("client_id", ctx.client_id.as_str()), ("scope", &ctx.scope)])
        .send()
        .await
        .context("requete d'autorisation d'appareil (device authorization)")?
        .error_for_status()
        .context("le fournisseur OIDC a refuse la demande d'autorisation d'appareil")?
        .json()
        .await
        .context("reponse d'autorisation d'appareil invalide")?;

    println!(
        "Ouvrez {} et entrez le code : {}",
        device.verification_uri, device.user_code
    );
    if let Some(complete) = &device.verification_uri_complete {
        println!("(ou directement : {complete})");
    }

    let deadline = Utc::now() + Duration::seconds(device.expires_in_secs as i64);
    let mut interval = StdDuration::from_secs(device.interval);

    loop {
        tokio::time::sleep(interval).await;
        if Utc::now() >= deadline {
            bail!("delai d'autorisation expire cote fournisseur OIDC (`expires_in`)");
        }

        let resp = client
            .post(&discovery.token_endpoint)
            .form(&[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device.device_code),
                ("client_id", &ctx.client_id),
            ])
            .send()
            .await
            .context("requete de jeton (device code)")?;

        if resp.status().is_success() {
            let token: TokenResponse = resp.json().await.context("reponse de jeton invalide")?;
            let subject = extract_subject(&token.access_token);
            return Ok(TokenSet {
                access_token: token.access_token,
                refresh_token: token.refresh_token,
                expires_at: Utc::now() + Duration::seconds(token.expires_in),
                subject,
            });
        }

        let status = resp.status();
        let err: TokenErrorResponse = resp
            .json()
            .await
            .with_context(|| format!("reponse d'erreur de jeton illisible (HTTP {status})"))?;
        match err.error.as_str() {
            "authorization_pending" => continue,
            "slow_down" => {
                interval += StdDuration::from_secs(5);
                continue;
            }
            "expired_token" => {
                bail!("le code d'autorisation a expire, relancez `atelier auth login`")
            }
            "access_denied" => bail!("autorisation refusee par l'utilisateur"),
            other => bail!("erreur du fournisseur OIDC : {other}"),
        }
    }
}

/// Renouvelle un jeton d'acces expire via son `refresh_token`, sans
/// redemander d'interaction utilisateur.
pub async fn refresh(ctx: &ContextConfig, refresh_token: &str) -> Result<TokenSet> {
    let client = reqwest::Client::new();
    let discovery = discover(&client, &ctx.issuer).await?;

    let resp = client
        .post(&discovery.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &ctx.client_id),
        ])
        .send()
        .await
        .context("requete de renouvellement de jeton")?
        .error_for_status()
        .context("le renouvellement du jeton a ete refuse, relancez `atelier auth login`")?;

    let token: TokenResponse = resp.json().await.context("reponse de jeton invalide")?;
    let subject = extract_subject(&token.access_token);
    Ok(TokenSet {
        access_token: token.access_token,
        refresh_token: token
            .refresh_token
            .or_else(|| Some(refresh_token.to_string())),
        expires_at: Utc::now() + Duration::seconds(token.expires_in),
        subject,
    })
}
