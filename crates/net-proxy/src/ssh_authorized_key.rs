//! Cache en memoire de la cle publique SSH autorisee (Jalon M4, tache
//! 4.2.3) de ce Workshop, rafraichi periodiquement depuis OpenBao — meme
//! schema exact que `crate::session_auth`, une seule valeur servie au
//! guest via `crate::metadata` a l'adresse link-local `169.254.0.1`.
//!
//! La paire de cles est generee une seule fois par le controller
//! (`crates/controller/src/openbao.rs::ensure_ssh_key`) : seule la cle
//! PUBLIQUE est lue ici (meme role Kubernetes-auth `workshop-<name>` que le
//! reste du pod) — la cle privee n'est jamais accessible a `net-proxy` ni
//! au guest, seul `api-server` (role cluster-wide dedie) la lit pour
//! s'authentifier en SSH.

use std::sync::Arc;
use std::time::Duration;

use atelier_common::OpenBaoClient;
use tokio::sync::RwLock;

const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

pub type SshAuthorizedKeyCache = Arc<RwLock<Option<String>>>;

async fn refresh_once(client: &OpenBaoClient) -> anyhow::Result<String> {
    let client_token = client.login().await?;
    client
        .read_field(&client_token, "ssh_key", "publicKey")
        .await
}

/// Boucle de rafraichissement en tache de fond, meme convention que
/// `crate::session_auth::refresh_loop` : un echec est journalise mais ne
/// vide pas le cache.
pub async fn refresh_loop(client: OpenBaoClient, cache: SshAuthorizedKeyCache) {
    loop {
        match refresh_once(&client).await {
            Ok(public_key) => {
                tracing::info!("cle publique SSH autorisee rafraichie");
                *cache.write().await = Some(public_key);
            }
            Err(err) => {
                tracing::warn!(%err, "echec du rafraichissement de la cle publique SSH autorisee");
            }
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}
