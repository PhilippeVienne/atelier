//! Injection du Basic Auth de session (secret OpenBao `session_auth`) dans
//! les tunnels VS Code/Terminal (`crate::vscode`, `crate::terminal`) — voir
//! la tache 1.2.6 de `docs/specs/PLAN-ACTION-GLOBAL.md`.
//!
//! `api-server` est un composant cluster-wide (une seule instance pour tous
//! les Workshops, pas un pod par Workshop) : il ne peut donc pas utiliser le
//! role OpenBao `workshop-<name>` scope a un seul Workshop, comme le font
//! `identity-proxy`/`mcp-gateway`/`net-proxy`, qui tournent chacun DANS le
//! pod du Workshop concerne (voir `atelier_common::OpenBaoClient`). Il
//! s'authentifie donc avec son propre role/ServiceAccount cluster-wide
//! (`atelier-api-server`, provisionne une seule fois au demarrage du
//! controller, voir
//! `crates/controller/src/openbao.rs::ensure_api_server_role`), dont la
//! policy n'autorise que la LECTURE de `secret/{data,metadata}/workshops/+/
//! {session_auth,ssh_key}` (le `+` est un wildcard OpenBao pour un seul
//! segment de chemin, donc un seul Workshop a la fois) — rien d'autre.
//! `ssh_key` (cle privee, tache 4.2.3) sert a `crate::exec`.

use atelier_common::OpenBaoClient;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Meme valeur que `atelier_controller::openbao::API_SERVER_ROLE` : crates
/// distinctes (`api-server` ne depend pas de `controller`), donc dupliquee
/// ici plutot que partagee — c'est un identifiant de role OpenBao, pas de la
/// logique.
const API_SERVER_OPENBAO_ROLE: &str = "atelier-api-server";

/// Le role Kubernetes-auth OpenBao provisionne par le controller a un TTL de
/// 15 minutes (`ensure_api_server_role`) : on renouvelle le token client un
/// peu avant, pour ne jamais presenter a OpenBao un token deja expire.
const TOKEN_TTL_MARGIN: Duration = Duration::from_secs(12 * 60);

/// Client partage (un seul par process `api-server`, voir `AppState`) qui
/// lit le mot de passe Basic Auth de session d'un Workshop donne, avec mise
/// en cache du token client OpenBao (le login Kubernetes-auth n'a pas besoin
/// d'etre refait a chaque requete).
#[derive(Clone)]
pub struct SessionAuthClient {
    client: Arc<OpenBaoClient>,
    cached_token: Arc<Mutex<Option<(String, Instant)>>>,
    /// Cookie de session `code-server` par Workshop (voir
    /// [`Self::code_server_cookie`]). Partage entre toutes les requetes du
    /// process : `code-server` refuse le Basic Auth, il faut lui presenter
    /// le cookie qu'il delivre lui-meme apres un POST sur `/login`, et
    /// rejouer ce login a chaque requete d'un IDE qui en emet des centaines
    /// serait absurde.
    code_server_cookies: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

impl SessionAuthClient {
    pub fn from_env(openbao_addr: String) -> Self {
        let client = OpenBaoClient::from_env_with_role(
            openbao_addr,
            String::new(),
            API_SERVER_OPENBAO_ROLE.to_string(),
        );
        Self {
            client: Arc::new(client),
            cached_token: Arc::new(Mutex::new(None)),
            code_server_cookies: Arc::new(Mutex::new(std::collections::HashMap::new())),
        }
    }

    /// Cookie de session `code-server` connu pour ce Workshop, s'il y en a
    /// un de valide en cache.
    pub async fn code_server_cookie(&self, workshop_name: &str) -> Option<String> {
        self.code_server_cookies
            .lock()
            .await
            .get(workshop_name)
            .cloned()
    }

    pub async fn store_code_server_cookie(&self, workshop_name: &str, cookie: String) {
        self.code_server_cookies
            .lock()
            .await
            .insert(workshop_name.to_string(), cookie);
    }

    /// Oublie le cookie d'un Workshop : appele quand `code-server` renvoie
    /// malgre tout une redirection vers `/login`, c'est-a-dire quand le
    /// cookie en cache ne vaut plus rien (microVM redemarree avec un
    /// nouveau mot de passe, secret tourne...). La requete suivante en
    /// obtiendra un neuf.
    pub async fn forget_code_server_cookie(&self, workshop_name: &str) {
        self.code_server_cookies.lock().await.remove(workshop_name);
    }

    async fn client_token(&self, force_refresh: bool) -> anyhow::Result<String> {
        let mut guard = self.cached_token.lock().await;
        if !force_refresh {
            if let Some((token, obtained_at)) = guard.as_ref() {
                if obtained_at.elapsed() < TOKEN_TTL_MARGIN {
                    return Ok(token.clone());
                }
            }
        }
        let token = self.client.login().await?;
        *guard = Some((token.clone(), Instant::now()));
        Ok(token)
    }

    /// Lit le mot de passe de session d'un Workshop donne
    /// (`secret/data/workshops/<name>/session_auth`, champ `password`).
    /// Renvoie toujours `None` (jamais d'erreur) si le secret est absent ou
    /// que n'importe quelle etape echoue : comportement degrade, coherent
    /// avec le reste du projet ("fonctionnalite optionnelle si non
    /// configuree") — le relai HTTP/WebSocket vers le guest continue
    /// simplement sans injecter de Basic Auth plutot que de bloquer la
    /// requete.
    pub async fn session_password(&self, workshop_name: &str) -> Option<String> {
        self.read_secret_field(workshop_name, "session_auth", "password")
            .await
    }

    /// Lit la cle privee SSH d'un Workshop donne
    /// (`secret/data/workshops/<name>/ssh_key`, champ `privateKey`) —
    /// utilisee par `crate::exec` (`exec_in_workshop`, Jalon M4, tache
    /// 4.2.3) pour s'authentifier aupres du guest. Meme convention
    /// degradee que [`Self::session_password`] : `None` en cas d'echec,
    /// jamais d'erreur.
    pub async fn ssh_private_key(&self, workshop_name: &str) -> Option<String> {
        self.read_secret_field(workshop_name, "ssh_key", "privateKey")
            .await
    }

    /// Depose une valeur dans un secret du Workshop.
    ///
    /// Contrairement aux lectures de ce module, une ecriture qui echoue est
    /// remontee comme une ERREUR et non silencieusement ignoree : lire un
    /// secret absent est un cas degrade acceptable, croire avoir enregistre
    /// un credential qui n'existe pas ne l'est pas.
    pub async fn write_secret_field(
        &self,
        workshop_name: &str,
        secret_path: &str,
        field: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        let token = self.client_token(false).await?;
        self.client
            .write_field_for(&token, workshop_name, secret_path, field, value)
            .await
    }

    /// Supprime definitivement un secret du Workshop.
    pub async fn delete_secret(
        &self,
        workshop_name: &str,
        secret_path: &str,
    ) -> anyhow::Result<()> {
        let token = self.client_token(false).await?;
        self.client
            .delete_secret_for(&token, workshop_name, secret_path)
            .await
    }

    async fn read_secret_field(
        &self,
        workshop_name: &str,
        secret_path: &str,
        field: &str,
    ) -> Option<String> {
        let token = match self.client_token(false).await {
            Ok(token) => token,
            Err(err) => {
                tracing::warn!(%err, "login OpenBao (role api-server) echoue");
                return None;
            }
        };

        match self
            .client
            .read_field_for(&token, workshop_name, secret_path, field)
            .await
        {
            Ok(value) => Some(value),
            Err(first_err) => {
                // Le token cache a pu etre invalide plus tot que prevu
                // (role recree, horloge decalee...) : un seul essai de
                // retry avec un login frais avant d'abandonner.
                let token = match self.client_token(true).await {
                    Ok(token) => token,
                    Err(err) => {
                        tracing::warn!(%err, "re-login OpenBao (role api-server) echoue");
                        return None;
                    }
                };
                match self
                    .client
                    .read_field_for(&token, workshop_name, secret_path, field)
                    .await
                {
                    Ok(value) => Some(value),
                    Err(err) => {
                        tracing::warn!(
                            %first_err,
                            %err,
                            workshop = %workshop_name,
                            secret_path,
                            field,
                            "lecture d'un secret (api-server) echouee"
                        );
                        None
                    }
                }
            }
        }
    }
}
