//! Injecte des identites/tokens (ex: credentials cloud, tokens d'API) dans
//! les appels sortants de l'agent, sans jamais exposer le secret brut a la VM.
//!
//! Les secrets destines aux environnements (pas ceux du cluster Kubernetes
//! sous-jacent, qui restent geres par les mecanismes k8s standards) sont
//! stockes dans [OpenBao](https://openbao.org/), sous
//! `secret/workshops/<name>/*`. Pont d'identite : la methode d'auth
//! **Kubernetes** d'OpenBao — identity-proxy s'authentifie avec le
//! ServiceAccount dedie du pod parent (token projete standard, verifie par
//! OpenBao via l'API Kubernetes), provisionne cote controller
//! (`crates/controller/src/openbao.rs`). Aucun secret a distribuer pour
//! amorcer cette confiance.
//!
//! Le secret ainsi recupere est souvent lui-meme l'identite de sortie de
//! l'environnement (ex: une cle d'API que l'environnement presente aux
//! services externes) : identity-proxy est le seul composant a y avoir
//! acces, l'agent dans la microVM ne le voit jamais en clair.

use anyhow::Context;

const DEFAULT_SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-identity-proxy");
    tracing::info!("atelier-identity-proxy starting");

    let Ok(openbao_addr) = std::env::var("OPENBAO_ADDR") else {
        tracing::warn!("OPENBAO_ADDR absent, identity-proxy demarre sans acces aux secrets");
        return Ok(());
    };
    let workshop_name =
        std::env::var("ATELIER_WORKSHOP_NAME").context("ATELIER_WORKSHOP_NAME manquant")?;
    let sa_token_path = std::env::var("ATELIER_K8S_SA_TOKEN_PATH")
        .unwrap_or_else(|_| DEFAULT_SA_TOKEN_PATH.to_string());

    let client_token = openbao_login(&openbao_addr, &workshop_name, &sa_token_path).await?;
    let keys = list_workshop_secret_keys(&openbao_addr, &client_token, &workshop_name).await?;
    tracing::info!(count = keys.len(), ?keys, "secrets disponibles pour ce workshop");

    // TODO: serveur proxy HTTP(S) qui intercepte les appels sortants de
    // l'agent et y injecte les credentials a la volee (via net-proxy ?)
    // TODO: rafraichir client_token avant expiration (ttl=15m cote OpenBao)
    Ok(())
}

/// Authentification aupres d'OpenBao via la methode Kubernetes : envoie le
/// token du ServiceAccount projete dans ce pod, recoit un client token
/// OpenBao scope par le role `workshop-<name>` (policies provisionnees par
/// le controller).
async fn openbao_login(
    openbao_addr: &str,
    workshop_name: &str,
    sa_token_path: &str,
) -> anyhow::Result<String> {
    let jwt = tokio::fs::read_to_string(sa_token_path)
        .await
        .with_context(|| format!("lecture du token ServiceAccount ({sa_token_path})"))?;

    let http = reqwest::Client::new();
    let response: serde_json::Value = http
        .post(format!("{openbao_addr}/v1/auth/kubernetes/login"))
        .json(&serde_json::json!({
            "jwt": jwt.trim(),
            "role": format!("workshop-{workshop_name}"),
        }))
        .send()
        .await
        .context("requete de login OpenBao")?
        .error_for_status()
        .context("login OpenBao refuse")?
        .json()
        .await
        .context("reponse de login OpenBao invalide")?;

    response["auth"]["client_token"]
        .as_str()
        .map(str::to_string)
        .context("client_token absent de la reponse de login OpenBao")
}

/// Liste les cles de secrets disponibles pour ce Workshop (sans jamais
/// journaliser les valeurs).
async fn list_workshop_secret_keys(
    openbao_addr: &str,
    client_token: &str,
    workshop_name: &str,
) -> anyhow::Result<Vec<String>> {
    let http = reqwest::Client::new();
    let response = http
        .get(format!(
            "{openbao_addr}/v1/secret/metadata/workshops/{workshop_name}?list=true"
        ))
        .header("X-Vault-Token", client_token)
        .send()
        .await
        .context("requete de listing des secrets OpenBao")?;

    // Aucun secret encore cree pour ce Workshop : ce n'est pas une erreur.
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(Vec::new());
    }

    let body: serde_json::Value = response
        .error_for_status()
        .context("listing des secrets OpenBao refuse")?
        .json()
        .await
        .context("reponse de listing OpenBao invalide")?;

    Ok(body["data"]["keys"]
        .as_array()
        .map(|keys| {
            keys.iter()
                .filter_map(|k| k.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default())
}
