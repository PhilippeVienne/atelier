//! Client OpenBao minimal, partage par tout composant qui doit lire un
//! secret scope a un `Workshop` : authentification via la methode
//! Kubernetes (le pod s'authentifie avec son propre ServiceAccount projete,
//! verifie par OpenBao via l'API Kubernetes, voir
//! `crates/controller/src/openbao.rs`), puis lecture d'un champ KV v2 sous
//! `secret/workshops/<name>/*` (la policy provisionnee par le controller
//! couvre tout ce prefixe, quel que soit le sous-chemin exact).
//!
//! Ne fait aucune mise en cache/rafraichissement : chaque appelant qui a
//! besoin d'un cache periodique (ex: `identity-proxy`, dont les regles
//! d'injection sont relues en continu) le construit au-dessus de ce client.
//!
//! Les valeurs lues ne sont **jamais** journalisees : seuls les chemins/cles
//! le sont.

use anyhow::Context;

const DEFAULT_SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

#[derive(Clone)]
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
    pub async fn login(&self) -> anyhow::Result<String> {
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
    pub async fn read_field(
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
}
