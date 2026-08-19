//! Injecte des identites/tokens (ex: credentials cloud, tokens d'API) dans
//! les appels sortants de l'agent, sans jamais exposer le secret brut a la
//! VM : un proxy HTTP explicite (comme `net-proxy`), configure comme
//! `HTTP_PROXY`/`HTTPS_PROXY` cote microVM pour les hotes qui ont besoin
//! d'une identite injectee, qui relaie ensuite vers `net-proxy`
//! (`ATELIER_NET_PROXY_ADDR`) — identity-proxy ne fait jamais lui-meme
//! l'arbitrage allowlist, seulement l'injection.
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

mod http;
mod proxy;
mod rules;
mod secrets;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::RwLock;

use secrets::OpenBaoClient;

const DEFAULT_LISTEN_ADDR: &str = "0.0.0.0:3129";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let _telemetry = atelier_common::telemetry::init("atelier-identity-proxy");

    let listen_addr = std::env::var("ATELIER_IDENTITY_PROXY_LISTEN_ADDR")
        .unwrap_or_else(|_| DEFAULT_LISTEN_ADDR.to_string());
    let next_hop: Option<Arc<str>> = std::env::var("ATELIER_NET_PROXY_ADDR")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .map(Into::into);

    let rules = Arc::new(rules::from_env()?);
    let secret_cache: secrets::SecretCache = Arc::new(RwLock::new(HashMap::new()));

    if rules.is_empty() {
        tracing::warn!(
            "ATELIER_IDENTITY_INJECTION_RULES absente ou vide : identity-proxy relaie sans jamais injecter"
        );
    } else {
        tracing::info!(count = rules.len(), "regles d'injection chargees");
    }
    match &next_hop {
        Some(addr) => tracing::info!(net_proxy = %addr, "atelier-identity-proxy demarre"),
        None => tracing::warn!(
            "ATELIER_NET_PROXY_ADDR absente : connexions directes a la destination (dev/test uniquement)"
        ),
    }

    if let Ok(openbao_addr) = std::env::var("OPENBAO_ADDR") {
        use anyhow::Context;
        let workshop_name =
            std::env::var("ATELIER_WORKSHOP_NAME").context("ATELIER_WORKSHOP_NAME manquant")?;
        let client = OpenBaoClient::from_env(openbao_addr, workshop_name);
        let rules_for_refresh = (*rules).clone();
        let cache_for_refresh = Arc::clone(&secret_cache);
        tokio::spawn(secrets::refresh_loop(
            client,
            rules_for_refresh,
            cache_for_refresh,
        ));
    } else {
        tracing::warn!("OPENBAO_ADDR absent, identity-proxy demarre sans acces aux secrets");
    }

    let config = proxy::ProxyConfig {
        rules,
        secrets: secret_cache,
        next_hop,
    };

    let listener = TcpListener::bind(&listen_addr).await?;
    tracing::info!(%listen_addr, "proxy d'identite en ecoute");

    loop {
        let (socket, peer): (_, SocketAddr) = listener.accept().await?;
        let config = config.clone();
        tokio::spawn(async move {
            if let Err(err) = proxy::handle_connection(socket, peer, config).await {
                tracing::warn!(%peer, %err, "connexion terminee en erreur");
            }
        });
    }
}
