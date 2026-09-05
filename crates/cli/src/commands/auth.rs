use crate::config::Config;
use crate::{oidc, tokens};
use anyhow::Result;

pub async fn login() -> Result<()> {
    let config = Config::load()?;
    let (name, ctx) = config.current_context()?;
    let token_set = oidc::login(ctx).await?;
    tokens::save(name, &token_set)?;
    println!("Authentifie sur le contexte '{name}'.");
    Ok(())
}

pub fn logout() -> Result<()> {
    let config = Config::load()?;
    let (name, _) = config.current_context()?;
    tokens::delete(name)?;
    println!("Session locale revoquee pour le contexte '{name}'.");
    Ok(())
}

pub fn status() -> Result<()> {
    let config = Config::load()?;
    let (name, _) = config.current_context()?;
    match tokens::load(name)? {
        None => println!("Contexte '{name}' : non authentifie (`atelier auth login`)."),
        Some(token_set) => {
            let expiry = if token_set.is_expired() {
                "expire".to_string()
            } else {
                format!("valide jusqu'a {}", token_set.expires_at.to_rfc3339())
            };
            println!(
                "Contexte '{name}' : sujet={} ({expiry})",
                token_set.subject.as_deref().unwrap_or("inconnu")
            );
        }
    }
    Ok(())
}

/// Renvoie un jeton d'acces valide pour le contexte actif : renouvelle
/// automatiquement via `refresh_token` si expire, sans reinteraction
/// utilisateur (voir `crate::oidc::refresh`). Appele par toutes les
/// commandes qui parlent a `api-server` (`crate::commands::workshops`).
pub async fn ensure_access_token() -> Result<String> {
    let config = Config::load()?;
    let (name, ctx) = config.current_context()?;
    let Some(token_set) = tokens::load(name)? else {
        anyhow::bail!("non authentifie sur le contexte '{name}' (`atelier auth login`)");
    };
    if !token_set.is_expired() {
        return Ok(token_set.access_token);
    }
    let Some(refresh_token) = &token_set.refresh_token else {
        anyhow::bail!(
            "jeton expire et aucun refresh_token disponible, relancez `atelier auth login`"
        );
    };
    let refreshed = oidc::refresh(ctx, refresh_token).await?;
    tokens::save(name, &refreshed)?;
    Ok(refreshed.access_token)
}
