//! Cache de secrets en memoire, rafraichi periodiquement, au-dessus du
//! client OpenBao partage (`atelier_common::OpenBaoClient`) : le token
//! client OpenBao a un TTL de 15 min cote serveur
//! (`crates/controller/src/openbao.rs`), on se re-authentifie et on relit
//! les secrets bien avant expiration.
//!
//! Les valeurs ne sont **jamais** journalisees : seuls les chemins/cles sont
//! traces.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use atelier_common::OpenBaoClient;
use tokio::sync::RwLock;

use crate::rules::{InjectionRule, InjectionRuleExt};

/// Le token client OpenBao pour le role `workshop-<name>` a un TTL de 15
/// minutes (`crates/controller/src/openbao.rs`) ; on se re-authentifie et on
/// relit les secrets bien avant expiration.
const REFRESH_INTERVAL: Duration = Duration::from_secs(5 * 60);

/// Cache partage entre les connexions du proxy : `secret_cache_key() ->
/// valeur`. Vide tant qu'aucun cycle de rafraichissement n'a reussi (pas
/// d'injection possible dans ce cas, les requetes sont relayees telles
/// quelles).
pub type SecretCache = Arc<RwLock<HashMap<String, String>>>;

/// Un cycle : login puis lecture de chaque secret reference par une regle.
/// Une regle dont le secret est illisible (pas encore cree, permission
/// refusee) est ignoree pour ce cycle (loggee), les autres continuent de
/// fonctionner.
async fn refresh_once(
    client: &OpenBaoClient,
    rules: &[InjectionRule],
) -> anyhow::Result<HashMap<String, String>> {
    let client_token = client.login().await?;
    let mut values = HashMap::new();
    for rule in rules {
        match client
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
        match refresh_once(&client, &rules).await {
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
