//! Test d'integration reel du serveur MCP externe (Jalon M4, `/v1/mcp`) :
//! vrai client MCP (`rmcp`, transport Streamable HTTP reqwest) contre un
//! vrai routeur Axum servi sur un port TCP reel, vrai `kube::Client` contre
//! le cluster kind de dev, vraie crypto JWT (meme convention que
//! `tests/routes.rs`). Necessite un `kubeconfig` valide pointant sur un
//! cluster avec le CRD `Workshop` applique — silencieusement ignore sans
//! `KUBECONFIG`/config par defaut joignable.

use atelier_api_server::auth::AuthState;
use atelier_api_server::routes::{self, AppState};
use atelier_common::Workshop;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use kube::api::{Api, DeleteParams};
use kube::Client;
use rmcp::model::CallToolRequestParams;
use rmcp::transport::streamable_http_client::{
    StreamableHttpClientTransport, StreamableHttpClientTransportConfig,
};
use rmcp::ServiceExt;
use serde_json::{json, Value};

const ISSUER: &str = "https://kanidm.test/oauth2/openid/atelier";
const AUDIENCE: &str = "atelier";

struct TestKey {
    encoding_key: EncodingKey,
    kid: String,
}

fn generate_test_key() -> TestKey {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let output = std::process::Command::new("openssl")
        .args(["genrsa", "2048"])
        .output()
        .expect("openssl doit etre installe pour ce test");
    assert!(
        output.status.success(),
        "openssl genrsa a echoue: {output:?}"
    );
    TestKey {
        encoding_key: EncodingKey::from_rsa_pem(&output.stdout).expect("cle PEM invalide"),
        kid: "test-key-1".to_string(),
    }
}

fn sign_jwt(key: &TestKey, sub: &str) -> String {
    let header = Header {
        kid: Some(key.kid.clone()),
        ..Header::new(Algorithm::RS256)
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({ "sub": sub, "iss": ISSUER, "aud": AUDIENCE, "exp": now + 3600 });
    jsonwebtoken::encode(&header, &claims, &key.encoding_key).expect("signature JWT")
}

async fn try_client() -> Option<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::try_default().await.ok()
}

async fn test_db_pool() -> sqlx::PgPool {
    let database_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
        "postgres://atelier_admin:dev-only-not-for-production@127.0.0.1:5433/atelier_apiserver"
            .to_string()
    });
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(5)
        .connect(&database_url)
        .await
        .expect("connexion a PostgreSQL de dev (voir deploy/dev/postgres/README.md)");
    sqlx::migrate!("./migrations")
        .run(&pool)
        .await
        .expect("execution des migrations PostgreSQL");
    pool
}

fn unique_name(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis()
            % 10_000_000
    )
}

/// Demarre le routeur reel (memes middlewares/handlers que `main.rs`) sur un
/// port TCP local libre, retourne l'URL de base du serveur MCP.
async fn spawn_server(state: AppState, auth: AuthState) -> String {
    let app = routes::router(state, auth);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}/v1/mcp")
}

fn mcp_client_transport(
    base_url: &str,
    jwt: &str,
) -> StreamableHttpClientTransport<reqwest013::Client> {
    // `StreamableHttpClientTransportConfig` est `#[non_exhaustive]` : pas de
    // construction litterale possible hors de `rmcp`, meme avec
    // `..Default::default()`. Les champs restent publics, donc mutables
    // apres coup.
    let mut config = StreamableHttpClientTransportConfig::default();
    config.uri = base_url.to_string().into();
    config.auth_header = Some(jwt.to_string());
    StreamableHttpClientTransport::with_client(reqwest013::Client::default(), config)
}

fn tool_text_result(result: &rmcp::model::CallToolResult) -> String {
    assert_ne!(
        result.is_error,
        Some(true),
        "l'appel d'outil ne doit pas avoir echoue: {result:?}"
    );
    result
        .content
        .first()
        .and_then(|c| c.as_text())
        .map(|t| t.text.clone())
        .expect("le resultat doit porter un bloc de contenu texte")
}

/// Bout en bout reel : `tools/list` annonce les 6 outils lifecycle
/// (tache 4.2.1), puis `create_workshop` -> `list_workshops` ->
/// `get_workshop_status` -> `suspend_workshop` -> `resume_workshop` ->
/// `delete_workshop`, chacun verifie contre le vrai cluster Kubernetes.
#[tokio::test]
async fn mcp_lifecycle_tools_drive_a_real_workshop() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };

    let key = generate_test_key();
    let mut jwk = Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).unwrap();
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::from_static_jwks(
        ISSUER.to_string(),
        AUDIENCE.to_string(),
        JwkSet { keys: vec![jwk] },
    );

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);
    let name = unique_name("test-mcp-lifecycle");

    let base_url = spawn_server(
        AppState {
            client: client.clone(),
            namespace,
            db_pool: test_db_pool().await,
            openbao_addr: None,
            litellm_addr: None,
            session_auth: None,
            storage: None,
        },
        auth,
    )
    .await;

    let jwt = sign_jwt(&key, "mcp-owner@test.atelier");
    let transport = mcp_client_transport(&base_url, &jwt);
    let mcp_client = ().serve(transport).await.expect("connexion/handshake MCP");

    let tools = mcp_client
        .peer()
        .list_tools(None)
        .await
        .expect("tools/list");
    let tool_names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();
    for expected in [
        "create_workshop",
        "list_workshops",
        "get_workshop_status",
        "suspend_workshop",
        "resume_workshop",
        "delete_workshop",
    ] {
        assert!(
            tool_names.contains(&expected),
            "tools/list doit annoncer {expected}, recu: {tool_names:?}"
        );
    }

    let create_result = mcp_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("create_workshop").with_arguments(
                json!({
                    "name": name,
                    "devcontainerRepo": "https://example.invalid/repo.git",
                    "cpu": "1",
                    "memory": "1Gi",
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .expect("appel create_workshop");
    let created: Value = serde_json::from_str(&tool_text_result(&create_result))
        .expect("create_workshop doit renvoyer le Workshop en JSON");
    assert_eq!(created["spec"]["ownerSubject"], "mcp-owner@test.atelier");

    // Verifie directement contre l'API Kubernetes (pas seulement la reponse
    // MCP) que la ressource existe reellement.
    workshops
        .get(&name)
        .await
        .expect("le Workshop doit reellement exister dans le cluster");

    let list_result = mcp_client
        .peer()
        .call_tool(CallToolRequestParams::new("list_workshops"))
        .await
        .expect("appel list_workshops");
    let listed: Vec<Value> = serde_json::from_str(&tool_text_result(&list_result)).unwrap();
    assert!(
        listed.iter().any(|w| w["metadata"]["name"] == name),
        "list_workshops doit inclure le Workshop cree"
    );

    let status_result = mcp_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get_workshop_status")
                .with_arguments(json!({ "name": name }).as_object().unwrap().clone()),
        )
        .await
        .expect("appel get_workshop_status");
    // Pas d'assertion forte sur la phase (depend de la reconciliation reelle
    // du controller, hors de la portee de ce test) : seule la reussite de
    // l'appel (statut serialisable, pas d'erreur JSON-RPC) est verifiee.
    let _status: Value = serde_json::from_str(&tool_text_result(&status_result)).unwrap();

    let suspend_result = mcp_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("suspend_workshop")
                .with_arguments(json!({ "name": name }).as_object().unwrap().clone()),
        )
        .await
        .expect("appel suspend_workshop");
    let suspended: Value = serde_json::from_str(&tool_text_result(&suspend_result)).unwrap();
    assert_eq!(suspended["spec"]["desiredState"], "Suspended");

    let resume_result = mcp_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("resume_workshop")
                .with_arguments(json!({ "name": name }).as_object().unwrap().clone()),
        )
        .await
        .expect("appel resume_workshop");
    let resumed: Value = serde_json::from_str(&tool_text_result(&resume_result)).unwrap();
    assert_eq!(resumed["spec"]["desiredState"], "Running");

    mcp_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("delete_workshop")
                .with_arguments(json!({ "name": name }).as_object().unwrap().clone()),
        )
        .await
        .expect("appel delete_workshop");

    // Selon qu'un `atelier-controller` reel tourne ou non contre ce cluster
    // au moment du test (le finalizer `atelier.dev/cleanup` n'est pose que
    // par sa boucle de reconciliation), la suppression est soit differee
    // (deletionTimestamp pose, objet toujours lisible) soit immediate (objet
    // deja absent) — seul un echec de la requete de suppression elle-meme
    // serait une regression de `delete_workshop`.
    match workshops.get(&name).await {
        Ok(after_delete) => assert!(
            after_delete.metadata.deletion_timestamp.is_some(),
            "le Workshop encore lisible apres delete_workshop doit au moins porter un deletionTimestamp"
        ),
        Err(kube::Error::Api(resp)) if resp.code == 404 => {}
        Err(err) => panic!("erreur inattendue en relisant le Workshop apres suppression: {err}"),
    }

    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Meme regle de visibilite que la route REST (`ensure_owner`) : un
/// utilisateur authentifie different du proprietaire ne doit jamais pouvoir
/// lire ou agir sur ce Workshop via MCP (erreur JSON-RPC, jamais de fuite
/// d'existence — voir `crate::routes::ensure_owner`).
#[tokio::test]
async fn mcp_tools_enforce_ownership_isolation() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };

    let key = generate_test_key();
    let mut jwk = Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).unwrap();
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::from_static_jwks(
        ISSUER.to_string(),
        AUDIENCE.to_string(),
        JwkSet { keys: vec![jwk] },
    );

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);
    let name = unique_name("test-mcp-isolation");

    let base_url = spawn_server(
        AppState {
            client: client.clone(),
            namespace,
            db_pool: test_db_pool().await,
            openbao_addr: None,
            litellm_addr: None,
            session_auth: None,
            storage: None,
        },
        auth,
    )
    .await;

    let owner_jwt = sign_jwt(&key, "mcp-owner-2@test.atelier");
    let owner_client =
        ().serve(mcp_client_transport(&base_url, &owner_jwt))
            .await
            .expect("connexion MCP (owner)");
    owner_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("create_workshop").with_arguments(
                json!({
                    "name": name,
                    "devcontainerRepo": "https://example.invalid/repo.git",
                    "cpu": "1",
                    "memory": "1Gi",
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .expect("appel create_workshop (owner)");

    let intruder_jwt = sign_jwt(&key, "mcp-intruder@test.atelier");
    let intruder_client =
        ().serve(mcp_client_transport(&base_url, &intruder_jwt))
            .await
            .expect("connexion MCP (intruder)");
    // `api_error_to_mcp` (crate::mcp_server) traduit `ApiError::not_found()`
    // en erreur JSON-RPC de niveau protocole (`ErrorData`), pas en
    // `CallToolResult { is_error: true, .. }` — `call_tool()` cote client
    // renvoie donc directement une erreur ici, jamais un resultat "reussi"
    // portant un contenu d'erreur.
    let err = intruder_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("get_workshop_status")
                .with_arguments(json!({ "name": name }).as_object().unwrap().clone()),
        )
        .await
        .expect_err("un tiers ne doit jamais pouvoir lire le statut du Workshop d'un autre");
    let message = err.to_string();
    assert!(
        message.contains("introuvable"),
        "l'erreur doit rester generique (404, pas 403) pour ne pas confirmer l'existence du Workshop a un tiers non autorise : {message}"
    );

    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Tache 4.1.2 (Fast-Fail) : `create_workshop` doit refuser immediatement
/// (erreur JSON-RPC explicite, jamais de Workshop cree) quand une
/// dependance de securite CONFIGUREE (ici LiteLLM) est injoignable — reel
/// port TCP local ferme, pas un mock.
#[tokio::test]
async fn mcp_create_workshop_fast_fails_when_litellm_unreachable() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };

    let key = generate_test_key();
    let mut jwk = Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).unwrap();
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::from_static_jwks(
        ISSUER.to_string(),
        AUDIENCE.to_string(),
        JwkSet { keys: vec![jwk] },
    );

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);
    let name = unique_name("test-mcp-fastfail");

    // Port TCP local jamais lie (`bind` puis `drop` immediat du listener) :
    // reellement injoignable, pas une adresse inventee qui pourrait par
    // hasard repondre.
    let closed_addr = {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        listener.local_addr().unwrap()
    };

    let base_url = spawn_server(
        AppState {
            client: client.clone(),
            namespace,
            db_pool: test_db_pool().await,
            openbao_addr: None,
            litellm_addr: Some(closed_addr.to_string()),
            session_auth: None,
            storage: None,
        },
        auth,
    )
    .await;

    let jwt = sign_jwt(&key, "mcp-fastfail@test.atelier");
    let mcp_client =
        ().serve(mcp_client_transport(&base_url, &jwt))
            .await
            .expect("connexion/handshake MCP");

    let err = mcp_client
        .peer()
        .call_tool(
            CallToolRequestParams::new("create_workshop").with_arguments(
                json!({
                    "name": name,
                    "devcontainerRepo": "https://example.invalid/repo.git",
                    "cpu": "1",
                    "memory": "1Gi",
                })
                .as_object()
                .unwrap()
                .clone(),
            ),
        )
        .await
        .expect_err("create_workshop doit refuser quand LiteLLM est injoignable");
    assert!(
        err.to_string().contains("injoignable"),
        "l'erreur doit expliquer quelle dependance est injoignable: {err}"
    );

    assert!(
        workshops.get(&name).await.is_err(),
        "aucun Workshop ne doit avoir ete cree quand le Fast-Fail se declenche"
    );
}
