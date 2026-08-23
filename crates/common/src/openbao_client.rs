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
    /// Role OpenBao utilise lors du login (methode Kubernetes-auth). Par
    /// defaut `workshop-<workshop_name>` (voir [`OpenBaoClient::from_env`]),
    /// mais peut etre un role fixe distinct du `workshop_name` pour un
    /// composant cluster-wide (ex: `atelier-api-server`, voir
    /// [`OpenBaoClient::from_env_with_role`]) qui n'est pas scope a un seul
    /// Workshop et doit donc utiliser son propre role, lie a son propre
    /// ServiceAccount plutot qu'a celui d'un pod de Workshop.
    role: String,
    sa_token_path: String,
    http: reqwest::Client,
}

impl OpenBaoClient {
    /// Role `workshop-<workshop_name>`, comme provisionne par
    /// `crates/controller/src/openbao.rs::ensure_workshop_role` — convention
    /// utilisee par tout composant qui tourne DANS le pod d'un Workshop
    /// precis (`identity-proxy`, `mcp-gateway`, `net-proxy`,
    /// `image-builder`).
    pub fn from_env(openbao_addr: String, workshop_name: String) -> Self {
        let role = format!("workshop-{workshop_name}");
        Self::from_env_with_role(openbao_addr, workshop_name, role)
    }

    /// Variante avec un role explicite, distinct de la convention
    /// `workshop-<name>` : necessaire pour un composant cluster-wide (une
    /// seule instance pour tous les Workshops, pas un pod par Workshop,
    /// ex: `api-server`) qui s'authentifie avec son propre role/ServiceAccount
    /// (voir `crates/controller/src/openbao.rs::ensure_api_server_role`) mais
    /// doit tout de meme lire des secrets scopes a un Workshop donne via
    /// [`OpenBaoClient::read_field_for`].
    pub fn from_env_with_role(openbao_addr: String, workshop_name: String, role: String) -> Self {
        let sa_token_path = std::env::var("ATELIER_K8S_SA_TOKEN_PATH")
            .unwrap_or_else(|_| DEFAULT_SA_TOKEN_PATH.to_string());
        Self {
            addr: openbao_addr,
            workshop_name,
            role,
            sa_token_path,
            http: reqwest::Client::new(),
        }
    }

    /// Authentification aupres d'OpenBao via la methode Kubernetes : envoie
    /// le token du ServiceAccount projete dans ce pod, recoit un client
    /// token OpenBao scope par [`Self::role`].
    pub async fn login(&self) -> anyhow::Result<String> {
        let jwt = tokio::fs::read_to_string(&self.sa_token_path)
            .await
            .with_context(|| format!("lecture du token ServiceAccount ({})", self.sa_token_path))?;

        let response: serde_json::Value = self
            .http
            .post(format!("{}/v1/auth/kubernetes/login", self.addr))
            .json(&serde_json::json!({
                "jwt": jwt.trim(),
                "role": self.role,
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
    /// `secret/workshops/<name>/<secret_path>`, ou `<name>` est le
    /// `workshop_name` fourni a la construction (`from_env`/
    /// `from_env_with_role`).
    pub async fn read_field(
        &self,
        client_token: &str,
        secret_path: &str,
        field: &str,
    ) -> anyhow::Result<String> {
        self.read_field_for(client_token, &self.workshop_name, secret_path, field)
            .await
    }

    /// Variante de [`Self::read_field`] avec un `workshop_name` explicite,
    /// independant de celui fourni a la construction : necessaire pour un
    /// composant cluster-wide (ex: `api-server`, role
    /// `atelier-api-server`) dont chaque appel cible un Workshop different
    /// (extrait du chemin de la requete HTTP), contrairement aux composants
    /// qui tournent DANS le pod d'un Workshop precis et n'en lisent jamais
    /// qu'un seul.
    pub async fn read_field_for(
        &self,
        client_token: &str,
        workshop_name: &str,
        secret_path: &str,
        field: &str,
    ) -> anyhow::Result<String> {
        let response = self
            .http
            .get(format!(
                "{}/v1/secret/data/workshops/{}/{}",
                self.addr, workshop_name, secret_path
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
