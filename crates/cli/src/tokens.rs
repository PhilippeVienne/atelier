//! Stockage des jetons OIDC dans le trousseau de cles natif de l'OS
//! (`keyring`), jamais sur disque a cote de `crate::config::Config` (spec
//! §3.3). Une entree par contexte : `atelier-cli` / `<nom-de-contexte>`.

use anyhow::{Context, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "atelier-cli";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenSet {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    pub expires_at: DateTime<Utc>,
    /// Sujet OIDC (claim `sub`), extrait localement du JWT (payload
    /// base64url, PAS revalide ici — seul `api-server` fait foi cote
    /// serveur) uniquement pour affichage dans `atelier auth status`.
    #[serde(default)]
    pub subject: Option<String>,
}

impl TokenSet {
    pub fn is_expired(&self) -> bool {
        Utc::now() >= self.expires_at
    }
}

fn entry(context: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, context).context("ouverture du trousseau de cles de l'OS")
}

pub fn load(context: &str) -> Result<Option<TokenSet>> {
    match entry(context)?.get_password() {
        Ok(raw) => Ok(Some(
            serde_json::from_str(&raw).context("jeton stocke illisible dans le trousseau")?,
        )),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => Err(err).context("lecture du trousseau de cles de l'OS"),
    }
}

pub fn save(context: &str, tokens: &TokenSet) -> Result<()> {
    let raw = serde_json::to_string(tokens).context("serialisation du jeton")?;
    entry(context)?
        .set_password(&raw)
        .context("ecriture dans le trousseau de cles de l'OS")
}

pub fn delete(context: &str) -> Result<()> {
    match entry(context)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => Err(err).context("suppression du trousseau de cles de l'OS"),
    }
}

/// Extrait la claim `sub` d'un JWT sans verifier sa signature : usage
/// strictement local/affichage (`atelier auth status`), jamais pour
/// autoriser quoi que ce soit — c'est `api-server` qui revalide chaque
/// jeton a chaque requete (`crate::api_server::auth::require_auth`).
pub fn extract_subject(jwt: &str) -> Option<String> {
    let payload = jwt.split('.').nth(1)?;
    let decoded = URL_SAFE_NO_PAD.decode(payload).ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    claims.get("sub")?.as_str().map(str::to_string)
}
