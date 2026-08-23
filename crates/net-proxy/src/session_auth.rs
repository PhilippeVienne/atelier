//! Cache en memoire du mot de passe de session (Basic Auth `code-server`/
//! `ttyd`) de ce Workshop, rafraichi periodiquement depuis OpenBao — meme
//! schema que `crates/identity-proxy/src/secrets.rs`, mais une seule valeur
//! (pas une carte de regles) : `net-proxy` sert ce secret au guest via un
//! endpoint HTTP dedie (`crate::metadata`), a l'adresse link-local
//! `169.254.0.1` que seule la microVM peut atteindre.
//!
//! Le secret lui-meme est ecrit une fois par le controller
//! (`crates/controller/src/openbao.rs::ensure_session_auth`, token
//! d'administration) ; `net-proxy` ne fait que le relire avec son propre
//! login Kubernetes-auth (role `workshop-<name>`, meme role que le reste du
//! pod). Valeur jamais journalisee.

use std::sync::Arc;
use std::time::Duration;

use atelier_common::OpenBaoClient;
use tokio::sync::RwLock;

/// Meme raisonnement que `identity-proxy` : le token client OpenBao a un TTL
/// de 15 minutes cote serveur (role Kubernetes-auth), on se re-authentifie
/// et on relit bien avant expiration.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub type SessionAuthCache = Arc<RwLock<Option<String>>>;

async fn refresh_once(client: &OpenBaoClient) -> anyhow::Result<String> {
    let client_token = client.login().await?;
    client
        .read_field(&client_token, "session_auth", "password")
        .await
}

/// Boucle de rafraichissement en tache de fond : login + relecture du
/// secret `session_auth` toutes les [`REFRESH_INTERVAL`]. Un echec (secret
/// pas encore provisionne par le controller, OpenBao temporairement
/// injoignable) est journalise mais ne vide pas le cache : le mot de passe
/// deja connu reste servi jusqu'au prochain cycle reussi.
pub async fn refresh_loop(client: OpenBaoClient, cache: SessionAuthCache) {
    loop {
        match refresh_once(&client).await {
            Ok(password) => {
                tracing::info!("mot de passe de session rafraichi");
                *cache.write().await = Some(password);
            }
            Err(err) => {
                tracing::warn!(%err, "echec du rafraichissement du mot de passe de session");
            }
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}
