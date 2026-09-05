//! Test d'integration reel de l'integration ChatOps Slack (tache 9.7) :
//! vrai routeur Axum sur un port TCP reel, vraie base PostgreSQL (RLS
//! compris), vrai serveur HTTP local recevant la notification sortante.
//! Necessite un `kubeconfig` valide (le routeur complet exige un
//! `kube::Client`, meme si ce test ne touche a aucune ressource
//! Kubernetes) — silencieusement ignore sans lui, comme les autres tests
//! d'integration de ce depot.

use atelier_api_server::auth::AuthState;
use atelier_api_server::routes::{self, AppState};
use kube::Client;
use uuid::Uuid;

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
        .expect("migrations sqlx");
    pool
}

async fn spawn_server(state: AppState) -> String {
    let app = routes::router(state, AuthState::Disabled);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://{addr}")
}

fn base_state(client: Client, db_pool: sqlx::PgPool) -> AppState {
    AppState {
        client,
        namespace: "default".to_string(),
        db_pool,
        openbao_addr: None,
        litellm_addr: None,
        llm_budget: None,
        llm_salt_key_configured: false,
        session_auth: None,
        storage: None,
        slack_webhook_url: None,
        slack_signing_secret: None,
    }
}

/// Insere directement une ligne `hitl_requests` (contourne
/// `approvals::create_approval`, qui exige un vrai Workshop Kubernetes —
/// hors de portee de ce test, centre sur le webhook Slack lui-meme, pas
/// sur la creation de la demande).
async fn insert_pending_request(pool: &sqlx::PgPool, tenant: &str, workshop: &str) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO hitl_requests (tenant, workshop_name, category, requested_by, payload) \
         VALUES ($1, $2, 'ALLOWLIST_EXPANSION', 'agent-test', '{}'::jsonb) RETURNING id",
    )
    .bind(tenant)
    .bind(workshop)
    .fetch_one(pool)
    .await
    .expect("insertion de la demande de test")
}

fn sign(secret: &str, timestamp: &str, body: &str) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(format!("v0:{timestamp}:{body}").as_bytes());
    let hex: String = mac
        .finalize()
        .into_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("v0={hex}")
}

/// Bout en bout reel : un clic "Approuver" signe correctement bascule une
/// vraie ligne `hitl_requests` en `APPROVED`, verifie directement en base
/// (pas seulement via le code HTTP de la reponse).
#[tokio::test]
async fn slack_interaction_with_valid_signature_approves_the_request() {
    let Some(client) = try_client().await else {
        eprintln!("kubeconfig requis, test ignore");
        return;
    };
    let pool = test_db_pool().await;
    let tenant = "atelier-demo";
    let workshop = "chatops-test-workshop";
    let id = insert_pending_request(&pool, tenant, workshop).await;

    let signing_secret = "test-signing-secret";
    let mut state = base_state(client, pool.clone());
    state.slack_signing_secret = Some(signing_secret.to_string());
    let base_url = spawn_server(state).await;

    let payload = serde_json::json!({
        "actions": [{ "action_id": "approve", "value": id.to_string() }],
        "user": { "username": "alice" },
    });
    let body = format!(
        "payload={}",
        url::form_urlencoded::byte_serialize(payload.to_string().as_bytes()).collect::<String>()
    );
    let timestamp = "1700000000";
    let signature = sign(signing_secret, timestamp, &body);

    let resp = reqwest::Client::new()
        .post(format!("{base_url}/v1/webhooks/slack/interactions"))
        .header("X-Slack-Request-Timestamp", timestamp)
        .header("X-Slack-Signature", signature)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .expect("requete webhook");
    assert_eq!(resp.status(), 200);

    let (status, decided_by): (String, Option<String>) =
        sqlx::query_as("SELECT status, decided_by FROM hitl_requests WHERE id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .expect("relecture de la demande");
    assert_eq!(status, "APPROVED");
    assert_eq!(decided_by.as_deref(), Some("slack:alice"));

    sqlx::query("DELETE FROM hitl_requests WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

/// Une signature invalide (secret different) ne doit RIEN changer en base.
#[tokio::test]
async fn slack_interaction_with_invalid_signature_is_rejected() {
    let Some(client) = try_client().await else {
        eprintln!("kubeconfig requis, test ignore");
        return;
    };
    let pool = test_db_pool().await;
    let id = insert_pending_request(&pool, "atelier-demo", "chatops-test-workshop-2").await;

    let mut state = base_state(client, pool.clone());
    state.slack_signing_secret = Some("le-vrai-secret".to_string());
    let base_url = spawn_server(state).await;

    let payload = serde_json::json!({
        "actions": [{ "action_id": "approve", "value": id.to_string() }],
        "user": { "username": "mallory" },
    });
    let body = format!(
        "payload={}",
        url::form_urlencoded::byte_serialize(payload.to_string().as_bytes()).collect::<String>()
    );
    let timestamp = "1700000000";
    // Signe avec un AUTRE secret que celui configure cote serveur.
    let signature = sign("secret-invente-par-lattaquant", timestamp, &body);

    let resp = reqwest::Client::new()
        .post(format!("{base_url}/v1/webhooks/slack/interactions"))
        .header("X-Slack-Request-Timestamp", timestamp)
        .header("X-Slack-Signature", signature)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .expect("requete webhook");
    assert_eq!(resp.status(), 401);

    let status: String = sqlx::query_scalar("SELECT status FROM hitl_requests WHERE id = $1")
        .bind(id)
        .fetch_one(&pool)
        .await
        .expect("relecture de la demande");
    assert_eq!(
        status, "PENDING",
        "une signature invalide ne doit rien changer"
    );

    sqlx::query("DELETE FROM hitl_requests WHERE id = $1")
        .bind(id)
        .execute(&pool)
        .await
        .ok();
}

/// `notify_slack` envoie un vrai POST HTTP (pas seulement construit un
/// payload en memoire) contenant un Block Kit valide (bouton "Approuver"
/// portant l'UUID de la demande), vers un serveur de test local reel.
#[tokio::test]
async fn notify_slack_posts_real_http_request_with_valid_block_kit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut buf = vec![0u8; 8192];
        let n = socket.read(&mut buf).await.unwrap();
        let request = String::from_utf8_lossy(&buf[..n]).to_string();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let _ = tx.send(request);
    });

    let request = atelier_api_server::approvals::HitlRequest {
        id: Uuid::new_v4(),
        tenant: "atelier-demo".to_string(),
        workshop_name: "demo-workshop".to_string(),
        category: "ALLOWLIST_EXPANSION".to_string(),
        requested_by: "agent".to_string(),
        payload: serde_json::json!({ "host": "api.stripe.com" }),
        status: "PENDING".to_string(),
        decided_by: None,
        decision_reason: None,
        created_at: chrono::Utc::now(),
        expires_at: chrono::Utc::now(),
        decided_at: None,
    };

    atelier_api_server::chatops::notify_slack(&format!("http://{addr}"), &request).await;

    let raw = tokio::time::timeout(std::time::Duration::from_secs(5), rx)
        .await
        .expect("timeout en attente de la requete webhook")
        .expect("le canal ne doit pas se fermer sans message");
    assert!(raw.starts_with("POST /"), "requete HTTP recue: {raw}");
    assert!(
        raw.contains(&request.id.to_string()),
        "le corps doit porter l'UUID de la demande en valeur de bouton"
    );
    assert!(
        raw.contains("Approuver") && raw.contains("Rejeter"),
        "le corps doit porter les deux boutons Block Kit"
    );
}
