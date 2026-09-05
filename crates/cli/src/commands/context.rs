use crate::config::{Config, ContextConfig};
use anyhow::{Context, Result};

pub fn add(
    name: String,
    api_url: String,
    issuer: String,
    client_id: String,
    scope: Option<String>,
) -> Result<()> {
    let mut config = Config::load()?;
    let ctx = ContextConfig {
        api_url,
        issuer,
        client_id,
        scope: scope.unwrap_or_else(|| "openid profile email".to_string()),
    };
    let make_current = config.current.is_none();
    config.contexts.insert(name.clone(), ctx);
    if make_current {
        config.current = Some(name.clone());
    }
    config.save()?;
    println!("Contexte '{name}' enregistre.");
    Ok(())
}

pub fn use_context(name: String) -> Result<()> {
    let mut config = Config::load()?;
    if !config.contexts.contains_key(&name) {
        anyhow::bail!("contexte '{name}' inconnu (`atelier context list`)");
    }
    config.current = Some(name.clone());
    config.save()?;
    println!("Contexte actif : {name}");
    Ok(())
}

pub fn list() -> Result<()> {
    let config = Config::load()?;
    if config.contexts.is_empty() {
        println!("Aucun contexte configure (`atelier context add`).");
        return Ok(());
    }
    for (name, ctx) in &config.contexts {
        let marker = if config.current.as_deref() == Some(name.as_str()) {
            "*"
        } else {
            " "
        };
        println!("{marker} {name}\t{}", ctx.api_url);
    }
    Ok(())
}

pub fn current() -> Result<()> {
    let config = Config::load()?;
    let (name, ctx) = config.current_context().context("aucun contexte actif")?;
    println!("{name}\t{}\t{}", ctx.api_url, ctx.issuer);
    Ok(())
}
