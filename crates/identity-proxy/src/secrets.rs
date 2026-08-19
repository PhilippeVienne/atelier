//! Client OpenBao : authentification via la methode Kubernetes (voir
//! `docs/ARCHITECTURE.md`, "Pont d'identite vers OpenBao"), puis lecture des
//! secrets referencees par les regles d'injection dans un cache en memoire,
//! rafraichi periodiquement (le token client OpenBao a un TTL de 15 min
//! cote serveur — voir `crates/controller/src/openbao.rs`).
//!
//! Les valeurs ne sont **jamais** journalisees : seuls les chemins/cles sont
//! traces.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::RwLock;

use crate::rules::InjectionRule;

const DEFAULT_SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";
/// Le token client OpenBao pour le role `workshop-<name>` a un TTL de 15
/// minutes (`crates/controller/src/openbao.rs`) ; on se re-authentifie et on
/// relit les secrets bien avant expiration.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Cache partage entre les connexions du proxy : `secret_cache_key() ->
/// valeur`. Vide tant qu'aucun cycle de rafraichissement n'a reussi (pas
/// d'injection possible dans ce cas, les requetes sont relayees telles
/// quelles).
pub type SecretCache = Arc<RwLock<HashMap<String, String>>>;

pub struct OpenBaoClient {
    addr: String,
    workshop_name: String,
    sa_token_path: String,
    http: reqwest::Client,
}

impl OpenBaoClient {
    pub fn from_env(openbao_addr: String, workshop_name: String) -> Self {
        let sa_token_path = std::env::var("ATELIER_K8S_SA_TOKEN_PATH")
            .unwrap_or_else(|_| DEFAULT_SA_TOKEN_PATH.to_string());
        Self {
            addr: openbao_addr,
            workshop_name,
            sa_token_path,
            http: reqwest::Client::new(),
        }
    }

    /// Authentification aupres d'OpenBao via la methode Kubernetes : envoie
    /// le token du ServiceAccount projete dans ce pod, recoit un client
    /// token OpenBao scope par le role `workshop-<name>`.
    async fn login(&self) -> anyhow::Result<String> {
        let jwt = tokio::fs::read_to_string(&self.sa_token_path)
            .await
            .with_context(|| format!("lecture du token ServiceAccount ({})", self.sa_token_path))?;

        let response: serde_json::Value = self
            .http
            .post(format!("{}/v1/auth/kubernetes/login", self.addr))
            .json(&serde_json::json!({
                "jwt": jwt.trim(),
                "role": format!("workshop-{}", self.workshop_name),
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

    /// Lit un champ d'un secret KV v2 sous
    /// `secret/workshops/<name>/<secret_path>`.
    async fn read_field(
        &self,
        client_token: &str,
        secret_path: &str,
        field: &str,
    ) -> anyhow::Result<String> {
        let response = self
            .http
            .get(format!(
                "{}/v1/secret/data/workshops/{}/{}",
                self.addr, self.workshop_name, secret_path
            ))
            .header("X-Vault-Token", client_token)
            .send()
            .await
            .context("requete de lecture de secret OpenBao")?
            .error_for_status()
            .context("lecture de secret OpenBao refusee")?
            .json::<serde_json::Value>()
            .await
            .context("reponse de lecture OpenBao invalide")?;

        response["data"]["data"][field]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("champ '{field}' absent du secret '{secret_path}'"))
    }

    /// Un cycle : login puis lecture de chaque secret reference par une
    /// regle. Une regle dont le secret est illisible (pas encore cree,
    /// permission refusee) est ignoree pour ce cycle (loggee), les autres
    /// continuent de fonctionner.
    async fn refresh_once(
        &self,
        rules: &[InjectionRule],
    ) -> anyhow::Result<HashMap<String, String>> {
        let client_token = self.login().await?;
        let mut values = HashMap::new();
        for rule in rules {
            match self
                .read_field(&client_token, &rule.secret_path, &rule.field)
                .await
            {
                Ok(value) => {
                    values.insert(rule.secret_cache_key(), value);
                }
                Err(err) => {
                    tracing::warn!(
                        secret_path = %rule.secret_path,
                        field = %rule.field,
                        %err,
                        "secret injecte introuvable pour l'instant"
                    );
                }
            }
        }
        Ok(values)
    }
}

/// Boucle de rafraichissement en tache de fond : login + relecture des
/// secrets references par `rules` toutes les [`REFRESH_INTERVAL`], jusqu'a
/// ce que le programme s'arrete. Ne retourne jamais en fonctionnement
/// normal.
pub async fn refresh_loop(client: OpenBaoClient, rules: Vec<InjectionRule>, cache: SecretCache) {
    if rules.is_empty() {
        tracing::info!("aucune regle d'injection configuree, cache de secrets inactif");
        return;
    }
    loop {
        match client.refresh_once(&rules).await {
            Ok(values) => {
                tracing::info!(count = values.len(), "secrets injectes rafraichis");
                *cache.write().await = values;
            }
            Err(err) => {
                tracing::error!(%err, "echec du rafraichissement des secrets OpenBao");
            }
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}
