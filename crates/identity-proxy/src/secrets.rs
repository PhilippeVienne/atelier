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

/// Regles partagees et MUTABLES : elles peuvent changer en cours de vie
/// du proxy, sans redemarrage (voir `crate::rules::load`).
pub type SharedRules = Arc<RwLock<Vec<InjectionRule>>>;

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
pub async fn refresh_loop(client: OpenBaoClient, rules: SharedRules, cache: SecretCache) {
    loop {
        // Les REGLES d'abord, les secrets ensuite : une regle ajoutee depuis
        // l'interface doit etre connue avant qu'on aille chercher le secret
        // qu'elle designe, sinon il faudrait deux tours pour qu'un nouveau
        // credential devienne actif.
        match crate::rules::load() {
            // `None` = fichier pas encore monte : on garde les regles
            // courantes plutot que de relayer sans injecter.
            Ok(None) => {}
            Ok(fresh) => {
                let fresh = fresh.unwrap_or_default();
                let mut current = rules.write().await;
                if *current != fresh {
                    tracing::info!(count = fresh.len(), "regles d'injection rechargees");
                    *current = fresh;
                }
            }
            Err(err) => {
                tracing::error!(%err, "regles d'injection illisibles, les precedentes restent en vigueur");
            }
        }

        let snapshot = rules.read().await.clone();
        if snapshot.is_empty() {
            tracing::debug!("aucune regle d'injection, cache de secrets inutile");
        } else {
            match refresh_once(&client, &snapshot).await {
                Ok(values) => {
                    tracing::info!(count = values.len(), "secrets injectes rafraichis");
                    *cache.write().await = values;
                }
                Err(err) => {
                    tracing::error!(%err, "echec du rafraichissement des secrets OpenBao");
                }
            }
        }
        tokio::time::sleep(REFRESH_INTERVAL).await;
    }
}
