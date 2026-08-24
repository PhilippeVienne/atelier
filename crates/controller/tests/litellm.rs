//! Test d'integration : necessite une vraie instance LiteLLM accessible
//! (voir `deploy/dev/llm-proxy/README.md`) :
//!
//!   kubectl create configmap atelier-llm-proxy-config \
//!     --from-file=config.yaml=deploy/dev/llm-proxy/config.yaml
//!   kubectl create secret generic atelier-llm-proxy-dev \
//!     --from-literal=DEEPSEEK_API_KEY=unused \
//!     --from-literal=ANTHROPIC_API_KEY=unused \
//!     --from-literal=LITELLM_MASTER_KEY=<jeton arbitraire>
//!   kubectl apply -f deploy/dev/llm-proxy/dev-deployment.yaml
//!   kubectl port-forward svc/atelier-llm-proxy 4000:4000 &
//!
//!   export ATELIER_LLM_PROXY_ADDR=127.0.0.1:4000
//!   export ATELIER_LLM_PROXY_AUTH_TOKEN=<le meme jeton arbitraire>
//!   cargo test -p atelier-controller --test litellm
//!
//! `dev-deployment.yaml` deploie egalement une instance Postgres DEDIEE
//! (`atelier-llm-proxy-db`, distincte de `atelier-postgres-dev` partagee par
//! `api-server`/un Workshop reel) : LiteLLM exige une base pour ses
//! endpoints `/key/generate`/`/key/delete` (verifie en pratique : sans elle,
//! ces routes renvoient 500 "DB not connected").
//!
//! Le modele `atelier-budget-test` (voir `deploy/dev/llm-proxy/config.yaml`)
//! ne contacte jamais de vrai fournisseur (`mock_response`, fonctionnalite
//! native de LiteLLM) mais porte un cout par appel explicite
//! (`model_info.input_cost_per_token`/`output_cost_per_token`) : suffisant
//! pour verifier reellement l'enforcement d'un budget de Virtual Key
//! (`max_budget`) SANS jamais depenser un centime aupres de DeepSeek ou
//! Anthropic — conformement a l'adaptation demandee pour ce jalon en
//! l'absence de cles de provider reelles dans cet environnement.

use atelier_controller::litellm::{LiteLlmClient, LiteLlmConfig};
use std::time::{SystemTime, UNIX_EPOCH};

fn env_config() -> Option<LiteLlmConfig> {
    atelier_controller::litellm::config_from_env(
        std::env::var("ATELIER_LLM_PROXY_ADDR").ok(),
        std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN").ok(),
    )
}

fn unique_alias(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("horloge systeme valide")
        .as_nanos();
    format!("{prefix}-{nanos}")
}

/// `POST /chat/completions` sur le modele de test mock, avec la Virtual Key
/// donnee. Renvoie le statut HTTP brut (pas de `error_for_status`) : ce test
/// a justement besoin de distinguer un succes d'un `429`/`403` de budget
/// depasse.
async fn call_mock_model(base_url: &str, virtual_key: &str) -> reqwest::StatusCode {
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base_url}/chat/completions"))
        .bearer_auth(virtual_key)
        .json(&serde_json::json!({
            "model": "atelier-budget-test",
            "messages": [{"role": "user", "content": "0123456789012345678901234567890123456789"}],
        }))
        .send()
        .await
        .expect("appel /chat/completions vers LiteLLM");
    response.status()
}

async fn key_spend(base_url: &str, master_key: &str, virtual_key: &str) -> f64 {
    let client = reqwest::Client::new();
    let body: serde_json::Value = client
        .get(format!("{base_url}/key/info"))
        .bearer_auth(master_key)
        .query(&[("key", virtual_key)])
        .send()
        .await
        .expect("appel /key/info vers LiteLLM")
        .json()
        .await
        .expect("reponse /key/info illisible");
    body["info"]["spend"].as_f64().unwrap_or(0.0)
}

/// Bout-en-bout contre une VRAIE instance LiteLLM (voir le commentaire de
/// tete de ce fichier) : genere une Virtual Key a budget plafonne
/// ($1.00), consomme ce budget via des appels reels au modele de test mock
/// jusqu'a depassement, verifie le blocage HTTP 429 emis par LiteLLM
/// lui-meme (pas simule cote test), puis supprime la cle et verifie son
/// invalidation (401 sur un appel ulterieur), enfin verifie l'idempotence de
/// la suppression (un deuxieme appel ne doit pas faire echouer
/// `delete_virtual_key`, condition necessaire au finalizer
/// `atelier.dev/cleanup`, voir tache 3.2.1).
#[tokio::test]
async fn generates_enforces_budget_and_revokes_a_real_virtual_key() {
    let Some(config) = env_config() else {
        eprintln!(
            "ATELIER_LLM_PROXY_ADDR/ATELIER_LLM_PROXY_AUTH_TOKEN non definis, test ignore (voir deploy/dev/llm-proxy/README.md)"
        );
        return;
    };
    let base_url = format!("http://{}", config.addr);
    let master_key = config.master_key.clone();
    let client = LiteLlmClient::new(config);

    let key_alias = unique_alias("atelier-test-budget");

    // 1. Generation avec un budget plafond de 1.00$ et un TTL court, comme
    //    le fera `crate::reconcile::ensure_parent_pod` en production.
    let virtual_key = client
        .generate_virtual_key(&key_alias, "test-user", Some(1.0), "2h")
        .await
        .expect("generation de la Virtual Key");
    assert!(virtual_key.key.starts_with("sk-"), "{}", virtual_key.key);
    assert_eq!(virtual_key.key_alias, key_alias);

    // 2. Consomme le budget via de vrais appels HTTP a LiteLLM (modele mock,
    //    cout explicite dans `deploy/dev/llm-proxy/config.yaml`) jusqu'a ce
    //    que LiteLLM ait effectivement enregistre un depassement — le calcul
    //    de cout est asynchrone cote LiteLLM (constate en pratique), d'ou la
    //    boucle avec relecture de `/key/info` plutot qu'un nombre fixe
    //    d'appels.
    let first_status = call_mock_model(&base_url, &virtual_key.key).await;
    assert_eq!(
        first_status,
        reqwest::StatusCode::OK,
        "le premier appel, sous le budget, doit reussir"
    );

    let mut spend = 0.0;
    for _ in 0..20 {
        spend = key_spend(&base_url, &master_key, &virtual_key.key).await;
        if spend > 1.0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    assert!(
        spend > 1.0,
        "le cout enregistre par LiteLLM ({spend}) devrait depasser le budget (1.0) apres un appel couteux"
    );

    // 3. Verifie le blocage reel emis par LiteLLM (pas une assertion sur une
    //    valeur simulee cote test) : 429 "Budget has been exceeded".
    let blocked_status = call_mock_model(&base_url, &virtual_key.key).await;
    assert_eq!(
        blocked_status,
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "LiteLLM doit bloquer les appels une fois le budget depasse"
    );

    // 4. Suppression puis verification de l'invalidation reelle (401, pas
    //    juste "la fonction n'a pas renvoye d'erreur").
    client
        .delete_virtual_key(&key_alias)
        .await
        .expect("suppression de la Virtual Key");
    let status_after_delete = call_mock_model(&base_url, &virtual_key.key).await;
    assert_eq!(
        status_after_delete,
        reqwest::StatusCode::UNAUTHORIZED,
        "la Virtual Key supprimee ne doit plus etre acceptee par LiteLLM"
    );

    // 5. Idempotence (tache 3.2.1, "404 ignore") : un second appel sur un
    //    alias deja supprime ne doit jamais faire echouer le finalizer.
    client
        .delete_virtual_key(&key_alias)
        .await
        .expect("la suppression d'une cle deja absente doit rester Ok (idempotent)");
}
