//! Test d'integration reel : vraie crypto JWT (cle RSA generee via
//! `openssl`, JWKS reel construit depuis cette cle, signature reelle), vrai
//! `kube::Client` contre le cluster kind de dev, vrai routeur axum (appele
//! directement via `tower::ServiceExt::oneshot`, sans TCP — mais le meme
//! code de bout en bout que ce que `main.rs` sert). Seul ce qui *doit*
//! rester hors session (un vrai flux OAuth2 Kanidm) est simule par une cle
//! locale : la logique de validation elle-meme (signature, `iss`, `sub`)
//! est testee pour de vrai, pas mockee.
//!
//! Necessite un `kubeconfig` valide pointant sur un cluster avec le CRD
//! `Workshop` applique (`crds/workshop.yaml`) — silencieusement ignore sans
//! `KUBECONFIG`/config par defaut joignable.

use atelier_api_server::auth::AuthState;
use atelier_api_server::routes::{self, AppState};
use atelier_common::Workshop;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::Client;
use serde_json::{json, Value};
use tower::ServiceExt;

const ISSUER: &str = "https://kanidm.test/oauth2/openid/atelier";

struct TestKey {
    encoding_key: EncodingKey,
    kid: String,
}

fn generate_test_key() -> TestKey {
    let output = std::process::Command::new("openssl")
        .args(["genrsa", "2048"])
        .output()
        .expect("openssl doit etre installe pour ce test");
    assert!(output.status.success(), "openssl genrsa a echoue: {output:?}");

    TestKey {
        encoding_key: EncodingKey::from_rsa_pem(&output.stdout).expect("cle PEM invalide"),
        kid: "test-key-1".to_string(),
    }
}

fn sign_jwt(key: &TestKey, sub: &str) -> String {
    let header = Header { kid: Some(key.kid.clone()), ..Header::new(Algorithm::RS256) };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let claims = json!({ "sub": sub, "iss": ISSUER, "exp": now + 3600 });
    jsonwebtoken::encode(&header, &claims, &key.encoding_key).expect("signature JWT")
}

async fn try_client() -> Option<Client> {
    Client::try_default().await.ok()
}

#[tokio::test]
async fn crud_and_ownership_isolation_against_real_cluster() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };

    let key = generate_test_key();
    let mut jwk = Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).expect("derivation JWK");
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::Configured { issuer: ISSUER.to_string(), jwks: JwkSet { keys: vec![jwk] } };

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);

    let app = routes::router(AppState { client: client.clone(), namespace }, auth);

    let owner_token = sign_jwt(&key, "owner@test.atelier");
    let other_token = sign_jwt(&key, "someone-else@test.atelier");
    let name = format!("api-test-{}", std::process::id());

    let create_body = json!({
        "name": name,
        "devcontainer": { "repo": "https://github.com/microsoft/vscode-remote-try-python" },
        "resources": { "cpu": "1", "memory": "512Mi" },
    });

    // Nettoyage prealable defensif (execution precedente interrompue).
    let _ = workshops.delete(&name, &DeleteParams::default()).await;

    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/workshops")
                .header(header::AUTHORIZATION, format!("Bearer {owner_token}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(create_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED, "creation doit reussir");
    let created: Value = body_json(response).await;
    assert_eq!(created["spec"]["ownerSubject"], "owner@test.atelier", "owner_subject vient du JWT, pas du corps");

    // Le proprietaire voit son Workshop.
    let response = app.clone().oneshot(get_request(&format!("/v1/workshops/{name}"), &owner_token)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Un autre sujet JWT ne le voit pas (404, pas 403 : cf. routes.rs).
    let response = app.clone().oneshot(get_request(&format!("/v1/workshops/{name}"), &other_token)).await.unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND, "isolation par proprietaire");

    // La liste du proprietaire contient le Workshop cree.
    let response = app.clone().oneshot(get_request("/v1/workshops", &owner_token)).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let list: Value = body_json(response).await;
    assert!(
        list.as_array().unwrap().iter().any(|w| w["metadata"]["name"] == name),
        "la liste du proprietaire doit contenir le Workshop cree"
    );

    // La liste d'un autre sujet ne le contient pas.
    let response = app.clone().oneshot(get_request("/v1/workshops", &other_token)).await.unwrap();
    let list: Value = body_json(response).await;
    assert!(
        !list.as_array().unwrap().iter().any(|w| w["metadata"]["name"] == name),
        "la liste d'un autre sujet ne doit pas contenir le Workshop d'un autre"
    );

    // suspend -> desiredState=Suspended, verifie directement via kube::Api.
    let response = app
        .clone()
        .oneshot(post_request(&format!("/v1/workshops/{name}/suspend"), &owner_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = workshops.get(&name).await.unwrap();
    assert_eq!(fetched.spec.desired_state, atelier_common::WorkshopDesiredState::Suspended);

    // resume -> desiredState=Running.
    let response = app
        .clone()
        .oneshot(post_request(&format!("/v1/workshops/{name}/resume"), &owner_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = workshops.get(&name).await.unwrap();
    assert_eq!(fetched.spec.desired_state, atelier_common::WorkshopDesiredState::Running);

    // Un autre sujet ne peut pas suspendre le Workshop de quelqu'un d'autre.
    let response = app
        .clone()
        .oneshot(post_request(&format!("/v1/workshops/{name}/suspend"), &other_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // delete -> 202, deletionTimestamp pose (le finalizer du controller
    // n'est pas exerce ici, aucun controller ne tourne dans ce test).
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/v1/workshops/{name}"))
                .header(header::AUTHORIZATION, format!("Bearer {owner_token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::ACCEPTED);

    // Nettoyage reel : sans controller pour lever le finalizer
    // atelier.dev/cleanup, l'objet resterait bloque en Terminating.
    let _ = workshops
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "metadata": { "finalizers": [] } })),
        )
        .await;
    let _ = workshops.delete(&name, &DeleteParams::default()).await;
}

fn get_request(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn post_request(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

async fn body_json(response: axum::response::Response) -> Value {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("reponse JSON invalide")
}
