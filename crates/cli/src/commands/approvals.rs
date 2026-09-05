use crate::api::ApiClient;
use crate::commands::auth::ensure_access_token;
use crate::config::Config;
use anyhow::Result;

async fn client() -> Result<ApiClient> {
    let config = Config::load()?;
    let (_, ctx) = config.current_context()?;
    let token = ensure_access_token().await?;
    Ok(ApiClient::new(&ctx.api_url, &token))
}

/// `atelier approvals list <workshop>` : demandes HITL de ce Workshop.
pub async fn list(workshop_name: String) -> Result<()> {
    let approvals = client().await?.list_approvals(&workshop_name).await?;
    if approvals.is_empty() {
        println!("Aucune demande d'approbation.");
        return Ok(());
    }
    println!("ID\tCATEGORIE\tSTATUT\tDEMANDEUR\tEXPIRE");
    for a in approvals {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            a.id,
            a.category,
            a.status,
            a.requested_by,
            a.expires_at.to_rfc3339()
        );
    }
    Ok(())
}

/// `atelier approvals approve <id>` : approuve une demande en attente.
pub async fn approve(id: String, reason: Option<String>) -> Result<()> {
    let decided = client()
        .await?
        .decide_approval(&id, "APPROVED", reason.as_deref())
        .await?;
    println!("Demande '{id}' approuvee (statut: {}).", decided.status);
    Ok(())
}

/// `atelier approvals reject <id>` : rejette une demande en attente.
pub async fn reject(id: String, reason: Option<String>) -> Result<()> {
    let decided = client()
        .await?
        .decide_approval(&id, "REJECTED", reason.as_deref())
        .await?;
    println!("Demande '{id}' rejetee (statut: {}).", decided.status);
    Ok(())
}
