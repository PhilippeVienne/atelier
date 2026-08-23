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
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use jsonwebtoken::jwk::{Jwk, JwkSet};
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use kube::api::{Api, DeleteParams, Patch, PatchParams};
use kube::Client;
use serde_json::{json, Value};
use tower::ServiceExt;

const ISSUER: &str = "https://kanidm.test/oauth2/openid/atelier";
const AUDIENCE: &str = "atelier";

/// `portforward_relays_through_api_server_to_net_proxy` et
/// `vscode_proxy_relays_http_through_api_server_to_test_server` mutent
/// toutes les deux des variables d'environnement globales au process
/// (`ATELIER_NET_PROXY_CONTROL_PORT`/`ATELIER_VSCODE_PORT`, lues par
/// `atelier-api-server` au moment de traiter une requete) — `cargo test`
/// execute les tests d'un meme binaire en parallele par defaut, d'ou ce
/// verrou pour serialiser ces deux-la entre eux (les autres tests de ce
/// fichier ne touchent pas ces variables).
static ENV_LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();

fn env_lock() -> &'static tokio::sync::Mutex<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    ENV_LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

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
    // `aud` inclus deliberement (contrairement a une version anterieure de
    // ce test) : un vrai token Kanidm en porte toujours un, et
    // `jsonwebtoken` valide `aud` des qu'elle est presente — sans ce champ
    // ici, ce test ne peut pas detecter une regression sur cette
    // validation (constate en pratique, voir docs/PROGRESS.md).
    let claims = json!({ "sub": sub, "iss": ISSUER, "aud": AUDIENCE, "exp": now + 3600 });
    jsonwebtoken::encode(&header, &claims, &key.encoding_key).expect("signature JWT")
}

async fn try_client() -> Option<Client> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    Client::try_default().await.ok()
}

#[tokio::test]
async fn crud_and_ownership_isolation_against_real_cluster() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };

    let key = generate_test_key();
    let mut jwk =
        Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).expect("derivation JWK");
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::Configured {
        issuer: ISSUER.to_string(),
        audience: AUDIENCE.to_string(),
        jwks: JwkSet { keys: vec![jwk] },
    };

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);

    let app = routes::router(
        AppState {
            client: client.clone(),
            namespace,
        },
        auth,
    );

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
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "creation doit reussir"
    );
    let created: Value = body_json(response).await;
    assert_eq!(
        created["spec"]["ownerSubject"], "owner@test.atelier",
        "owner_subject vient du JWT, pas du corps"
    );

    // Le proprietaire voit son Workshop.
    let response = app
        .clone()
        .oneshot(get_request(&format!("/v1/workshops/{name}"), &owner_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    // Un autre sujet JWT ne le voit pas (404, pas 403 : cf. routes.rs).
    let response = app
        .clone()
        .oneshot(get_request(&format!("/v1/workshops/{name}"), &other_token))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "isolation par proprietaire"
    );

    // La liste du proprietaire contient le Workshop cree.
    let response = app
        .clone()
        .oneshot(get_request("/v1/workshops", &owner_token))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let list: Value = body_json(response).await;
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|w| w["metadata"]["name"] == name),
        "la liste du proprietaire doit contenir le Workshop cree"
    );

    // La liste d'un autre sujet ne le contient pas.
    let response = app
        .clone()
        .oneshot(get_request("/v1/workshops", &other_token))
        .await
        .unwrap();
    let list: Value = body_json(response).await;
    assert!(
        !list
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["metadata"]["name"] == name),
        "la liste d'un autre sujet ne doit pas contenir le Workshop d'un autre"
    );

    // suspend -> desiredState=Suspended, verifie directement via kube::Api.
    let response = app
        .clone()
        .oneshot(post_request(
            &format!("/v1/workshops/{name}/suspend"),
            &owner_token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = workshops.get(&name).await.unwrap();
    assert_eq!(
        fetched.spec.desired_state,
        atelier_common::WorkshopDesiredState::Suspended
    );

    // resume -> desiredState=Running.
    let response = app
        .clone()
        .oneshot(post_request(
            &format!("/v1/workshops/{name}/resume"),
            &owner_token,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let fetched = workshops.get(&name).await.unwrap();
    assert_eq!(
        fetched.spec.desired_state,
        atelier_common::WorkshopDesiredState::Running
    );

    // Un autre sujet ne peut pas suspendre le Workshop de quelqu'un d'autre.
    let response = app
        .clone()
        .oneshot(post_request(
            &format!("/v1/workshops/{name}/suspend"),
            &other_token,
        ))
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

/// Port-forward de bout en bout, en conditions reelles : un vrai processus
/// `net-proxy` (binaire compile, pas reimplemente/mocke — meme ethos "pas
/// de mocks" que le reste du projet), un vrai `Pod`/`Workshop` sur le
/// cluster kind, un vrai client websocket qui traverse `api-server` (role
/// de coordinateur : authentification + verification de propriete) jusqu'a
/// `net-proxy` (role de "kubelet") puis jusqu'a un serveur TCP d'echo.
#[tokio::test]
async fn portforward_relays_through_api_server_to_net_proxy() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };
    let net_proxy_bin = std::env::var("ATELIER_TEST_NET_PROXY_BIN")
        .unwrap_or_else(|_| "../../target/debug/atelier-net-proxy".to_string());
    if !std::path::Path::new(&net_proxy_bin).exists() {
        eprintln!("binaire net-proxy introuvable ({net_proxy_bin}), test ignore (cargo build -p atelier-net-proxy)");
        return;
    }

    let _env_guard = env_lock().lock().await;
    let echo_port = spawn_echo_server().await;

    let control_port = pick_free_port().await;
    let mut net_proxy = tokio::process::Command::new(&net_proxy_bin)
        .env("ATELIER_NET_PROXY_LISTEN_ADDR", "127.0.0.1:0")
        .env(
            "ATELIER_NET_PROXY_CONTROL_ADDR",
            format!("127.0.0.1:{control_port}"),
        )
        .env("ATELIER_DNS_LISTEN_ADDR", "127.0.0.1:0")
        .env("ATELIER_VM_ADDR", "127.0.0.1")
        .env("ATELIER_EGRESS_ALLOWLIST", "*")
        .kill_on_drop(true)
        .spawn()
        .expect("lancement du binaire net-proxy");
    wait_for_port(control_port).await;

    let key = generate_test_key();
    let mut jwk =
        Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).expect("derivation JWK");
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::Configured {
        issuer: ISSUER.to_string(),
        audience: AUDIENCE.to_string(),
        jwks: JwkSet { keys: vec![jwk] },
    };
    let owner_token = sign_jwt(&key, "portforward-owner@test.atelier");

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);
    let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), &namespace);
    let name = format!("api-pf-test-{}", std::process::id());
    let pod_name = format!("{name}-parent");

    let _ = workshops.delete(&name, &DeleteParams::default()).await;
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;

    // Un vrai Workshop, avec un sujet proprietaire connu, et son
    // status.podName pointant vers un vrai Pod dont on controle
    // status.podIp (peu importe qu'aucun conteneur n'y tourne reellement :
    // seule l'IP compte pour ce test, net-proxy tourne en local sur cette
    // meme adresse de boucle).
    let mut workshop = Workshop::new(
        &name,
        atelier_common::WorkshopSpec {
            devcontainer: atelier_common::DevcontainerSource {
                repo: "https://example.invalid/repo.git".to_string(),
                revision: "HEAD".to_string(),
                config_path: ".devcontainer/devcontainer.json".to_string(),
            },
            resources: atelier_common::WorkshopResources {
                cpu: "1".into(),
                memory: "512Mi".into(),
                disk: None,
            },
            egress_allowlist: vec![],
            tools: vec![],
            identity_injection_rules: vec![],
            owner_subject: "portforward-owner@test.atelier".to_string(),
            desired_state: atelier_common::WorkshopDesiredState::Running,
        },
    );
    workshop = workshops
        .create(&Default::default(), &workshop)
        .await
        .expect("creation du Workshop");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "status": { "phase": "Running", "podName": pod_name } })),
        )
        .await
        .expect("ecriture de status.podName");

    // node_name vise un noeud inexistant : aucun kubelet reel ne prend donc
    // jamais ce Pod en charge (il reste `Pending` a jamais), ce qui laisse
    // notre patch_status manuel sur `podIP` ci-dessous intact — sans ca, un
    // vrai kubelet planifierait le conteneur et ecraserait `status.podIP`
    // avec sa propre adresse CNI reelle, avant que le test ne puisse
    // controler ce qu'expose net-proxy.
    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: kube::api::ObjectMeta {
            name: Some(pod_name.clone()),
            namespace: Some(namespace.clone()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some("atelier-test-fake-node".into()),
            containers: vec![k8s_openapi::api::core::v1::Container {
                name: "placeholder".into(),
                image: Some("registry.k8s.io/pause:3.9".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    pods.create(&Default::default(), &pod)
        .await
        .expect("creation du Pod");
    pods.patch_status(
        &pod_name,
        &PatchParams::default(),
        &Patch::Merge(&json!({ "status": { "podIP": "127.0.0.1" } })),
    )
    .await
    .expect("ecriture de status.podIP");

    // SAFETY (test) : un seul test de ce binaire touche cette variable, pas
    // de concurrence avec `crud_and_ownership_isolation_against_real_cluster`.
    unsafe { std::env::set_var("ATELIER_NET_PROXY_CONTROL_PORT", control_port.to_string()) };

    let app = routes::router(
        AppState {
            client: client.clone(),
            namespace: namespace.clone(),
        },
        auth,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let url =
        format!("ws://127.0.0.1:{api_port}/v1/workshops/{name}/portforward?ports=tcp:{echo_port}");
    let request = {
        use tokio_tungstenite::tungstenite::client::IntoClientRequest;
        let mut req = url.into_client_request().unwrap();
        req.headers_mut().insert(
            "Authorization",
            format!("Bearer {owner_token}").parse().unwrap(),
        );
        req
    };
    let (mut ws, _response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("connexion websocket au coordinateur port-forward");

    let mut frame = vec![0u8]; // canal 0 = donnees du premier (et seul) port demande
    frame.extend_from_slice(b"hello through api-server");
    ws.send(tokio_tungstenite::tungstenite::Message::Binary(
        frame.into(),
    ))
    .await
    .unwrap();

    let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
        .await
        .expect("reponse recue avant timeout")
        .expect("flux websocket toujours ouvert")
        .expect("message valide");
    let reply_bytes = reply.into_data();
    assert_eq!(reply_bytes[0], 0, "canal de donnees attendu");
    assert_eq!(
        &reply_bytes[1..],
        b"hello through api-server",
        "l'echo doit traverser api-server -> net-proxy -> le port cible"
    );

    server.abort();
    net_proxy.start_kill().ok();
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
    let _ = workshops
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "metadata": { "finalizers": [] } })),
        )
        .await;
    let _ = workshops.delete(&name, &DeleteParams::default()).await;
    let _ = workshop; // conserve pour eviter un warning "unused" selon la version du compilateur
}

async fn spawn_echo_server() -> u16 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (mut socket, _) = match listener.accept().await {
                Ok(pair) => pair,
                Err(_) => break,
            };
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                loop {
                    match socket.read(&mut buf).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if socket.write_all(&buf[..n]).await.is_err() {
                                break;
                            }
                        }
                    }
                }
            });
        }
    });
    port
}

async fn pick_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

async fn wait_for_port(port: u16) {
    for _ in 0..50 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("net-proxy n'a jamais ouvert son port de controle {port}");
}

/// Pont HTTP+WS vers "code-server" (`crate::vscode`), en conditions
/// reelles : meme mise en scene que le test port-forward ci-dessus (vrai
/// `net-proxy`, vrai Workshop/Pod sur kind), mais la cible finale est ici
/// un vrai petit serveur HTTP (a la place de `code-server`) — verifie que
/// le prefixe `/v1/workshops/{name}/vscode` est bien retire avant
/// d'atteindre cette cible (`code-server` a besoin de recevoir des chemins
/// qui se comportent comme s'il etait a la racine, voir commentaire de
/// module de `crates/api-server/src/vscode.rs`).
#[tokio::test]
async fn vscode_proxy_relays_http_through_api_server_to_test_server() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };
    let net_proxy_bin = std::env::var("ATELIER_TEST_NET_PROXY_BIN")
        .unwrap_or_else(|_| "../../target/debug/atelier-net-proxy".to_string());
    if !std::path::Path::new(&net_proxy_bin).exists() {
        eprintln!("binaire net-proxy introuvable ({net_proxy_bin}), test ignore (cargo build -p atelier-net-proxy)");
        return;
    }

    let _env_guard = env_lock().lock().await;
    let (stub_port, observed_path) = spawn_stub_http_server("bonjour depuis code-server").await;

    let control_port = pick_free_port().await;
    let mut net_proxy = tokio::process::Command::new(&net_proxy_bin)
        .env("ATELIER_NET_PROXY_LISTEN_ADDR", "127.0.0.1:0")
        .env(
            "ATELIER_NET_PROXY_CONTROL_ADDR",
            format!("127.0.0.1:{control_port}"),
        )
        .env("ATELIER_DNS_LISTEN_ADDR", "127.0.0.1:0")
        .env("ATELIER_VM_ADDR", "127.0.0.1")
        .env("ATELIER_EGRESS_ALLOWLIST", "*")
        .kill_on_drop(true)
        .spawn()
        .expect("lancement du binaire net-proxy");
    wait_for_port(control_port).await;

    let key = generate_test_key();
    let mut jwk =
        Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).expect("derivation JWK");
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::Configured {
        issuer: ISSUER.to_string(),
        audience: AUDIENCE.to_string(),
        jwks: JwkSet { keys: vec![jwk] },
    };
    let owner_token = sign_jwt(&key, "vscode-owner@test.atelier");

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);
    let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), &namespace);
    let name = format!("api-vscode-test-{}", std::process::id());
    let pod_name = format!("{name}-parent");

    let _ = workshops.delete(&name, &DeleteParams::default()).await;
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;

    let workshop = Workshop::new(
        &name,
        atelier_common::WorkshopSpec {
            devcontainer: atelier_common::DevcontainerSource {
                repo: "https://example.invalid/repo.git".to_string(),
                revision: "HEAD".to_string(),
                config_path: ".devcontainer/devcontainer.json".to_string(),
            },
            resources: atelier_common::WorkshopResources {
                cpu: "1".into(),
                memory: "512Mi".into(),
                disk: None,
            },
            egress_allowlist: vec![],
            tools: vec![],
            identity_injection_rules: vec![],
            owner_subject: "vscode-owner@test.atelier".to_string(),
            desired_state: atelier_common::WorkshopDesiredState::Running,
        },
    );
    workshops
        .create(&Default::default(), &workshop)
        .await
        .expect("creation du Workshop");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "status": { "phase": "Running", "podName": pod_name } })),
        )
        .await
        .expect("ecriture de status.podName");

    // Voir le commentaire equivalent du test port-forward : un node_name
    // inexistant garde le Pod `Pending` a jamais, pour que notre
    // patch_status manuel sur `podIP` ne soit jamais ecrase par un vrai
    // kubelet.
    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: kube::api::ObjectMeta {
            name: Some(pod_name.clone()),
            namespace: Some(namespace.clone()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some("atelier-test-fake-node".into()),
            containers: vec![k8s_openapi::api::core::v1::Container {
                name: "placeholder".into(),
                image: Some("registry.k8s.io/pause:3.9".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    pods.create(&Default::default(), &pod)
        .await
        .expect("creation du Pod");
    pods.patch_status(
        &pod_name,
        &PatchParams::default(),
        &Patch::Merge(&json!({ "status": { "podIP": "127.0.0.1" } })),
    )
    .await
    .expect("ecriture de status.podIP");

    // SAFETY (test) : proteges par `env_lock()` (voir sa doc) contre la
    // concurrence avec le test port-forward, seul autre test de ce binaire
    // a muter des variables d'environnement globales.
    unsafe {
        std::env::set_var("ATELIER_NET_PROXY_CONTROL_PORT", control_port.to_string());
        std::env::set_var("ATELIER_VSCODE_PORT", stub_port.to_string());
    }

    let app = routes::router(
        AppState {
            client: client.clone(),
            namespace: namespace.clone(),
        },
        auth,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    let http_client = reqwest::Client::new();
    let response = http_client
        .get(format!(
            "http://127.0.0.1:{api_port}/v1/workshops/{name}/vscode/static/foo.js"
        ))
        .header("Authorization", format!("Bearer {owner_token}"))
        .send()
        .await
        .expect("requete vers le pont vscode");
    assert_eq!(response.status(), StatusCode::OK);
    let body = response.text().await.unwrap();
    assert_eq!(body, "bonjour depuis code-server");

    assert_eq!(
        observed_path.lock().await.as_deref(),
        Some("GET /static/foo.js HTTP/1.1"),
        "le prefixe /v1/workshops/{{name}}/vscode doit avoir ete retire avant d'atteindre la cible"
    );

    server.abort();
    net_proxy.start_kill().ok();
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
    let _ = workshops
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "metadata": { "finalizers": [] } })),
        )
        .await;
    let _ = workshops.delete(&name, &DeleteParams::default()).await;
}

/// Meme pont que ci-dessus, mais pour le chemin d'upgrade WebSocket (celui
/// qu'emprunte le canal "live" propre de `code-server`, pas seulement ses
/// assets statiques) : verifie que `hyper::upgrade::on` cote requete
/// entrante ET cote reponse amont sont bien relies par
/// `tokio::io::copy_bidirectional` (`crates/api-server/src/vscode.rs`).
/// Client volontairement en TCP brut (pas un vrai client WebSocket) : on ne
/// teste ici que le relai d'octets bidirectionnel une fois upgrade, pas le
/// framing WebSocket lui-meme (deja hors de portee de ce pont, qui ne
/// reinterprete jamais les frames — meme philosophie que `net-proxy` pour
/// `CONNECT`).
#[tokio::test]
async fn vscode_proxy_relays_websocket_upgrade_through_api_server() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };
    let net_proxy_bin = std::env::var("ATELIER_TEST_NET_PROXY_BIN")
        .unwrap_or_else(|_| "../../target/debug/atelier-net-proxy".to_string());
    if !std::path::Path::new(&net_proxy_bin).exists() {
        eprintln!("binaire net-proxy introuvable ({net_proxy_bin}), test ignore (cargo build -p atelier-net-proxy)");
        return;
    }

    let _env_guard = env_lock().lock().await;
    let stub_port = spawn_stub_upgrade_echo_server().await;

    let control_port = pick_free_port().await;
    let mut net_proxy = tokio::process::Command::new(&net_proxy_bin)
        .env("ATELIER_NET_PROXY_LISTEN_ADDR", "127.0.0.1:0")
        .env(
            "ATELIER_NET_PROXY_CONTROL_ADDR",
            format!("127.0.0.1:{control_port}"),
        )
        .env("ATELIER_DNS_LISTEN_ADDR", "127.0.0.1:0")
        .env("ATELIER_VM_ADDR", "127.0.0.1")
        .env("ATELIER_EGRESS_ALLOWLIST", "*")
        .kill_on_drop(true)
        .spawn()
        .expect("lancement du binaire net-proxy");
    wait_for_port(control_port).await;

    let key = generate_test_key();
    let mut jwk =
        Jwk::from_encoding_key(&key.encoding_key, Algorithm::RS256).expect("derivation JWK");
    jwk.common.key_id = Some(key.kid.clone());
    let auth = AuthState::Configured {
        issuer: ISSUER.to_string(),
        audience: AUDIENCE.to_string(),
        jwks: JwkSet { keys: vec![jwk] },
    };
    let owner_token = sign_jwt(&key, "vscode-ws-owner@test.atelier");

    let namespace = "default".to_string();
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), &namespace);
    let pods: Api<k8s_openapi::api::core::v1::Pod> = Api::namespaced(client.clone(), &namespace);
    let name = format!("api-vscode-ws-test-{}", std::process::id());
    let pod_name = format!("{name}-parent");

    let _ = workshops.delete(&name, &DeleteParams::default()).await;
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;

    let workshop = Workshop::new(
        &name,
        atelier_common::WorkshopSpec {
            devcontainer: atelier_common::DevcontainerSource {
                repo: "https://example.invalid/repo.git".to_string(),
                revision: "HEAD".to_string(),
                config_path: ".devcontainer/devcontainer.json".to_string(),
            },
            resources: atelier_common::WorkshopResources {
                cpu: "1".into(),
                memory: "512Mi".into(),
                disk: None,
            },
            egress_allowlist: vec![],
            tools: vec![],
            identity_injection_rules: vec![],
            owner_subject: "vscode-ws-owner@test.atelier".to_string(),
            desired_state: atelier_common::WorkshopDesiredState::Running,
        },
    );
    workshops
        .create(&Default::default(), &workshop)
        .await
        .expect("creation du Workshop");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "status": { "phase": "Running", "podName": pod_name } })),
        )
        .await
        .expect("ecriture de status.podName");

    let pod = k8s_openapi::api::core::v1::Pod {
        metadata: kube::api::ObjectMeta {
            name: Some(pod_name.clone()),
            namespace: Some(namespace.clone()),
            ..Default::default()
        },
        spec: Some(k8s_openapi::api::core::v1::PodSpec {
            node_name: Some("atelier-test-fake-node".into()),
            containers: vec![k8s_openapi::api::core::v1::Container {
                name: "placeholder".into(),
                image: Some("registry.k8s.io/pause:3.9".into()),
                ..Default::default()
            }],
            ..Default::default()
        }),
        ..Default::default()
    };
    pods.create(&Default::default(), &pod)
        .await
        .expect("creation du Pod");
    pods.patch_status(
        &pod_name,
        &PatchParams::default(),
        &Patch::Merge(&json!({ "status": { "podIP": "127.0.0.1" } })),
    )
    .await
    .expect("ecriture de status.podIP");

    unsafe {
        std::env::set_var("ATELIER_NET_PROXY_CONTROL_PORT", control_port.to_string());
        std::env::set_var("ATELIER_VSCODE_PORT", stub_port.to_string());
    }

    let app = routes::router(
        AppState {
            client: client.clone(),
            namespace: namespace.clone(),
        },
        auth,
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut socket = tokio::net::TcpStream::connect(("127.0.0.1", api_port))
        .await
        .unwrap();
    let request = format!(
        "GET /v1/workshops/{name}/vscode/socket HTTP/1.1\r\n\
         Host: 127.0.0.1\r\n\
         Authorization: Bearer {owner_token}\r\n\
         Connection: Upgrade\r\n\
         Upgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    );
    socket.write_all(request.as_bytes()).await.unwrap();

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut buf))
        .await
        .expect("reponse d'upgrade avant timeout")
        .unwrap();
    let response = String::from_utf8_lossy(&buf[..n]);
    assert!(
        response.starts_with("HTTP/1.1 101"),
        "reponse d'upgrade attendue, obtenu: {response:?}"
    );

    socket.write_all(b"ping-through-tunnel").await.unwrap();
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), socket.read(&mut buf))
        .await
        .expect("echo avant timeout")
        .unwrap();
    assert_eq!(
        &buf[..n],
        b"ping-through-tunnel",
        "les octets envoyes apres upgrade doivent etre echoes par le stub a travers tout le tunnel"
    );

    server.abort();
    net_proxy.start_kill().ok();
    let _ = pods.delete(&pod_name, &DeleteParams::default()).await;
    let _ = workshops
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&json!({ "metadata": { "finalizers": [] } })),
        )
        .await;
    let _ = workshops.delete(&name, &DeleteParams::default()).await;
}

/// Stub "code-server" pour le test d'upgrade : repond `101` a la premiere
/// requete (sans valider `Sec-WebSocket-Key`, inutile ici — voir
/// commentaire du test), puis echo brut de tout ce qui suit.
async fn spawn_stub_upgrade_echo_server() -> u16 {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let mut reader = BufReader::new(socket);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => return,
                Ok(_) if line == "\r\n" => break,
                Ok(_) => continue,
                Err(_) => return,
            }
        }
        let mut socket = reader.into_inner();
        let response =
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\r\n";
        if socket.write_all(response.as_bytes()).await.is_err() {
            return;
        }
        let mut buf = vec![0u8; 4096];
        loop {
            match socket.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if socket.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            }
        }
    });
    port
}

/// Serveur HTTP minimal (une seule requete acceptee) qui joue le role de
/// `code-server` pour ce test : enregistre la ligne de requete recue (pour
/// verifier le retrait du prefixe) et repond toujours le meme corps.
async fn spawn_stub_http_server(
    response_body: &'static str,
) -> (u16, std::sync::Arc<tokio::sync::Mutex<Option<String>>>) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    let observed = std::sync::Arc::new(tokio::sync::Mutex::new(None));
    let observed_clone = observed.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        let mut reader = BufReader::new(socket);
        let mut request_line = String::new();
        if reader.read_line(&mut request_line).await.unwrap_or(0) == 0 {
            return;
        }
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) if line == "\r\n" => break,
                Ok(_) => continue,
                Err(_) => break,
            }
        }
        *observed_clone.lock().await = Some(request_line.trim_end().to_string());
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n{}",
            response_body.len(),
            response_body
        );
        let mut socket = reader.into_inner();
        let _ = socket.write_all(response.as_bytes()).await;
    });
    (port, observed)
}
