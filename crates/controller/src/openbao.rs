//! Provisioning OpenBao par Workshop : une policy + un role d'authentification
//! Kubernetes scopes au ServiceAccount du pod parent de ce Workshop.
//!
//! Pont d'identite retenu : la methode d'auth **Kubernetes** d'OpenBao, pas
//! une federation JWT/OIDC via Kanidm. Le pod parent s'authentifie avec son
//! propre ServiceAccount (token projete, verifie par OpenBao via TokenReview
//! aupres de l'API Kubernetes) : aucun secret a distribuer/stocker pour
//! amorcer la confiance. Kanidm reste la source de verite pour l'identite
//! humaine/utilisateur (`WorkshopSpec.owner_subject`) et pour l'identite
//! d'environnement exposee ailleurs (`WorkshopStatus.kanidm_entity_id`),
//! mais ce n'est pas elle qui porte ce pont-la.
//!
//! Le secret que `identity-proxy` recupere ensuite via ce chemin est
//! lui-meme l'identite de sortie de l'environnement (ex: la cle d'API que
//! l'environnement presente aux services externes) : seul `identity-proxy`
//! peut l'obtenir (il est le seul a detenir le ServiceAccount du pod
//! parent) et donc le seul composant capable d'agir "en tant que"
//! l'environnement auprès de ces services. L'agent dans la microVM n'y a
//! jamais acces directement.

pub struct OpenBaoConfig {
    pub addr: String,
    pub token: String,
}

/// Renvoie `Ok(None)` si `OPENBAO_ADDR` est absent (fonctionnalite
/// desactivee), une erreur si elle est partiellement configuree.
pub fn config_from_env() -> anyhow::Result<Option<OpenBaoConfig>> {
    let Ok(addr) = std::env::var("OPENBAO_ADDR") else {
        tracing::info!("OPENBAO_ADDR absent, provisioning OpenBao desactive");
        return Ok(None);
    };
    let token = std::env::var("OPENBAO_TOKEN")
        .map_err(|_| anyhow::anyhow!("OPENBAO_ADDR est defini mais OPENBAO_TOKEN est absent"))?;
    Ok(Some(OpenBaoConfig { addr, token }))
}

/// Chemins KV (v2) sous lesquels vivent les secrets d'un Workshop. KV v2
/// separe le chemin de lecture (`data/`) de celui de listing (`metadata/`) :
/// une policy pour ce Workshop doit couvrir les deux.
pub fn secrets_data_path(workshop_name: &str) -> String {
    format!("secret/data/workshops/{workshop_name}/*")
}

pub fn secrets_metadata_path(workshop_name: &str) -> String {
    format!("secret/metadata/workshops/{workshop_name}/*")
}

/// Cree (ou met a jour, idempotent) la policy et le role Kubernetes-auth
/// scopant l'acces d'un Workshop a ses seuls secrets. Renvoie le nom du role
/// (deterministe : `workshop-{name}`), a utiliser comme `role` lors du login
/// (`identity-proxy`, `mcp-gateway`, et desormais `image-builder` pour lire
/// d'eventuels identifiants git — voir `crates/image-builder/src/main.rs`).
///
/// Accepte plusieurs ServiceAccounts (pas un seul) : le pod parent et le Job
/// `image-builder` d'un meme Workshop ont chacun le leur, mais partagent le
/// meme role/policy OpenBao (memes secrets, `secret/workshops/<name>/*`) —
/// un seul appel avec la liste complete, plutot que d'ecraser
/// `bound_service_account_names` a chaque appel avec un seul nom (PUT
/// remplace tout le champ, pas un ajout).
pub async fn ensure_workshop_role(
    config: &OpenBaoConfig,
    workshop_name: &str,
    namespace: &str,
    service_accounts: &[&str],
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let role_name = format!("workshop-{workshop_name}");

    let policy_hcl = format!(
        "path \"{}\" {{ capabilities = [\"read\"] }}\npath \"{}\" {{ capabilities = [\"read\", \"list\"] }}",
        secrets_data_path(workshop_name),
        secrets_metadata_path(workshop_name),
    );

    client
        .put(format!("{}/v1/sys/policy/{role_name}", config.addr))
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({ "policy": policy_hcl }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture de la policy OpenBao: {e}"))?;

    client
        .put(format!(
            "{}/v1/auth/kubernetes/role/{role_name}",
            config.addr
        ))
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({
            "bound_service_account_names": service_accounts,
            "bound_service_account_namespaces": [namespace],
            "policies": [role_name],
            "ttl": "15m",
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture du role kubernetes-auth OpenBao: {e}"))?;

    Ok(role_name)
}

/// Supprime le role kubernetes-auth et la policy d'un Workshop. Idempotent :
/// un 404 (deja absent) n'est pas une erreur ; toute autre erreur est
/// remontee pour que le finalizer retente plutot que de laisser un role
/// orphelin (surface d'acces residuelle a des secrets).
pub async fn delete_workshop_role(
    config: &OpenBaoConfig,
    workshop_name: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let role_name = format!("workshop-{workshop_name}");

    delete_if_present(
        &client,
        &format!("{}/v1/auth/kubernetes/role/{role_name}", config.addr),
        &config.token,
    )
    .await?;
    delete_if_present(
        &client,
        &format!("{}/v1/sys/policy/{role_name}", config.addr),
        &config.token,
    )
    .await?;

    Ok(())
}

async fn delete_if_present(client: &reqwest::Client, url: &str, token: &str) -> anyhow::Result<()> {
    let response = client
        .delete(url)
        .header("X-Vault-Token", token)
        .send()
        .await?;

    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "suppression OpenBao ({url}) a echoue: {}",
        response.status()
    ))
}
