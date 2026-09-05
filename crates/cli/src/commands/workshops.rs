use crate::api::ApiClient;
use crate::commands::auth::ensure_access_token;
use crate::config::Config;
use anyhow::Result;
use atelier_common::{DevcontainerSource, WorkshopResources};

async fn client() -> Result<ApiClient> {
    let config = Config::load()?;
    let (_, ctx) = config.current_context()?;
    let token = ensure_access_token().await?;
    Ok(ApiClient::new(&ctx.api_url, &token))
}

pub async fn list() -> Result<()> {
    let workshops = client().await?.list_workshops().await?;
    if workshops.is_empty() {
        println!("Aucun Workshop.");
        return Ok(());
    }
    println!("NAME\tPHASE");
    for w in workshops {
        let phase = w
            .status
            .as_ref()
            .map(|s| format!("{:?}", s.phase))
            .unwrap_or_else(|| "Inconnu".to_string());
        println!("{}\t{phase}", w.metadata.name.unwrap_or_default());
    }
    Ok(())
}

pub async fn status(name: String) -> Result<()> {
    let workshop = client().await?.get_workshop(&name).await?;
    println!("{}", serde_json::to_string_pretty(&workshop)?);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn create(
    name: String,
    repo: String,
    revision: Option<String>,
    config_path: Option<String>,
    cpu: String,
    memory: String,
    disk: Option<String>,
    egress_allowlist: Vec<String>,
    tools: Vec<String>,
    owner_group: Option<String>,
) -> Result<()> {
    let devcontainer = DevcontainerSource {
        repo,
        revision: revision.unwrap_or_else(|| "HEAD".to_string()),
        config_path: config_path.unwrap_or_else(|| ".devcontainer/devcontainer.json".to_string()),
    };
    let resources = WorkshopResources {
        cpu,
        memory,
        disk,
        max_llm_budget_usd: None,
    };
    let workshop = client()
        .await?
        .create_workshop(
            &name,
            devcontainer,
            resources,
            egress_allowlist,
            tools,
            owner_group,
        )
        .await?;
    println!(
        "Workshop '{}' cree.",
        workshop.metadata.name.unwrap_or(name)
    );
    Ok(())
}

pub async fn delete(name: String) -> Result<()> {
    client().await?.delete_workshop(&name).await?;
    println!("Suppression de '{name}' demandee.");
    Ok(())
}

pub async fn stop(name: String) -> Result<()> {
    client().await?.suspend_workshop(&name).await?;
    println!("Suspension de '{name}' demandee.");
    Ok(())
}

pub async fn resume(name: String) -> Result<()> {
    client().await?.resume_workshop(&name).await?;
    println!("Reprise de '{name}' demandee.");
    Ok(())
}
