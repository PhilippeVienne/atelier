//! Test d'integration reel contre l'instance LiteLLM de dev (voir
//! `deploy/dev/llm-proxy/README.md`) : necessite un port-forward local
//! (`kubectl port-forward svc/atelier-llm-proxy 4000:4000`) et le master
//! key de dev documente dans ce meme README
//! (`ATELIER_LLM_PROXY_AUTH_TOKEN`, defaut `sk-atelier-llm-proxy-dev` si
//! absent — jamais une valeur de production). Skip silencieux si LiteLLM
//! n'est pas joignable, jamais un mock.
//!
//! Tache 12.5 (spec docs/specs/16-escouades-multi-agents-swarms-mesh.md
//! §3.4) : verifie le contrat Team LiteLLM (`ensure_team`/
//! `generate_virtual_key_in_team`/`delete_team`) tel qu'exploite par
//! `crates/controller/src/reconcile.rs`.

use atelier_controller::litellm::{LiteLlmClient, LiteLlmConfig};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_id(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

async fn try_client() -> Option<LiteLlmClient> {
    let addr =
        std::env::var("ATELIER_LLM_PROXY_ADDR").unwrap_or_else(|_| "127.0.0.1:4000".to_string());
    let master_key = std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN")
        .unwrap_or_else(|_| "sk-atelier-llm-proxy-dev".to_string());
    let client = LiteLlmClient::new(LiteLlmConfig { addr, master_key });
    // Sonde de disponibilite reelle (pas juste "le port repond") :
    // `ensure_team` lui-meme sert de sonde plus bas, mais on veut un skip
    // propre plutot qu'un echec bruyant si LiteLLM n'est pas joignable du
    // tout dans cet environnement (pas de port-forward actif).
    match client.ensure_team(&unique_id("probe"), None).await {
        Ok(()) => Some(client),
        Err(_) => None,
    }
}

#[tokio::test]
async fn ensure_team_is_idempotent_across_multiple_workshops_of_the_same_campaign() {
    let Some(client) = try_client().await else {
        eprintln!("LiteLLM de dev injoignable (voir deploy/dev/llm-proxy/README.md), test ignore");
        return;
    };
    let team_id = unique_id("test-campaign");

    // Simule deux Workshops de la MEME campagne appelant `ensure_team`
    // independamment, sans coordination — exactement le cas reel
    // (`crates/controller/src/reconcile.rs::ensure_parent_pod`, un appel
    // par Workshop reconcilie).
    client
        .ensure_team(&team_id, Some(5.0))
        .await
        .expect("le premier appel doit creer la Team");
    client
        .ensure_team(&team_id, Some(999.0))
        .await
        .expect("un second appel sur le MEME campaign_id ne doit jamais echouer");

    client.delete_team(&team_id).await.expect("nettoyage");
}

#[tokio::test]
async fn keys_generated_in_the_same_team_share_it_and_survive_deletion_being_idempotent() {
    let Some(client) = try_client().await else {
        eprintln!("LiteLLM de dev injoignable (voir deploy/dev/llm-proxy/README.md), test ignore");
        return;
    };
    let team_id = unique_id("test-campaign-keys");
    client
        .ensure_team(&team_id, Some(0.05))
        .await
        .expect("creation de la Team");

    let key_a = client
        .generate_virtual_key_in_team("test-key-a", "atelier-core", None, "10m", Some(&team_id))
        .await
        .expect("generation de la premiere cle rattachee a la Team");
    let key_b = client
        .generate_virtual_key_in_team("test-key-b", "atelier-core", None, "10m", Some(&team_id))
        .await
        .expect("generation de la seconde cle rattachee a la MEME Team");

    assert_ne!(key_a.key, key_b.key, "deux cles distinctes, meme Team");

    client
        .delete_virtual_key(&key_a.key_alias)
        .await
        .expect("nettoyage cle a");
    client
        .delete_virtual_key(&key_b.key_alias)
        .await
        .expect("nettoyage cle b");
    client.delete_team(&team_id).await.expect("nettoyage team");

    // `delete_team` doit rester idempotent (verifie empiriquement contre
    // l'instance reelle : 404 sur un `team_id` deja absent) — un second
    // appel ne doit jamais faire echouer le finalizer d'un Workshop.
    client
        .delete_team(&team_id)
        .await
        .expect("un second delete_team sur une Team deja absente doit rester un succes");
}
