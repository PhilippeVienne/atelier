//! Test d'integration : necessite un vrai OpenBao accessible (voir
//! `deploy/dev/openbao/README.md`) et un vrai cluster Kubernetes (kubeconfig
//! par defaut) pour creer un ServiceAccount reel et obtenir son token
//! projete — meme methode que `crates/controller/tests/reconcile.rs`.
//!
//!   export OPENBAO_ADDR=http://127.0.0.1:8200
//!   export OPENBAO_TOKEN=root
//!   cargo test -p atelier-net-proxy --test session_auth
//!
//! Verifie le chemin complet reellement emprunte en production : le
//! controller ecrit le secret `session_auth` avec son token d'administration
//! (simule ici par un appel HTTP direct, `openbao::ensure_session_auth`
//! n'etant pas accessible depuis ce crate), puis `net-proxy` le relit via
//! son propre login Kubernetes-auth scope (`atelier_common::OpenBaoClient`,
//! le meme code que `crate::session_auth::refresh_once`).

use atelier_common::OpenBaoClient;
use k8s_openapi::api::core::v1::ServiceAccount;
use kube::api::{Api, DeleteParams, PostParams};
use kube::Client;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

#[tokio::test]
async fn net_proxy_reads_the_session_auth_password_written_by_the_controller() {
    let (Ok(openbao_addr), Ok(openbao_token)) = (
        std::env::var("OPENBAO_ADDR"),
        std::env::var("OPENBAO_TOKEN"),
    ) else {
        eprintln!(
            "OPENBAO_ADDR/OPENBAO_TOKEN non definis, test ignore (voir deploy/dev/openbao/README.md)"
        );
        return;
    };
    atelier_common::telemetry::ensure_crypto_provider();
    let Ok(client) = Client::try_default().await else {
        eprintln!("kubeconfig requis (cluster kind local), test ignore");
        return;
    };

    let ns = "default";
    let name = unique_name("test-net-proxy-session-auth");
    let sa_name = format!("{name}-parent");
    let service_accounts: Api<ServiceAccount> = Api::namespaced(client.clone(), ns);
    let http = reqwest::Client::new();

    // Meme role/policy que celui provisionne par
    // `crates/controller/src/openbao.rs::ensure_workshop_role`, reconstruit
    // ici a la main pour ne pas dependre de `atelier-controller`.
    service_accounts
        .create(
            &PostParams::default(),
            &ServiceAccount {
                metadata: kube::api::ObjectMeta {
                    name: Some(sa_name.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("creation du ServiceAccount");

    let role_name = format!("workshop-{name}");
    let policy_hcl = format!(
        "path \"secret/data/workshops/{name}/*\" {{ capabilities = [\"read\"] }}\npath \"secret/metadata/workshops/{name}/*\" {{ capabilities = [\"read\", \"list\"] }}",
    );
    http.put(format!("{openbao_addr}/v1/sys/policy/{role_name}"))
        .header("X-Vault-Token", &openbao_token)
        .json(&serde_json::json!({ "policy": policy_hcl }))
        .send()
        .await
        .expect("requete d'ecriture de policy")
        .error_for_status()
        .expect("ecriture de la policy OpenBao");
    http.put(format!(
        "{openbao_addr}/v1/auth/kubernetes/role/{role_name}"
    ))
    .header("X-Vault-Token", &openbao_token)
    .json(&serde_json::json!({
        "bound_service_account_names": [sa_name],
        "bound_service_account_namespaces": [ns],
        "policies": [role_name],
        "ttl": "15m",
    }))
    .send()
    .await
    .expect("requete d'ecriture du role")
    .error_for_status()
    .expect("ecriture du role kubernetes-auth OpenBao");

    // Le controller ecrit le mot de passe avec son token d'administration
    // (voir `openbao::ensure_session_auth`) : simule ici par le meme appel
    // direct, avec le root token de dev.
    let expected_password = "test-password-1234567890123456";
    http.put(format!(
        "{openbao_addr}/v1/secret/data/workshops/{name}/session_auth"
    ))
    .header("X-Vault-Token", &openbao_token)
    .json(&serde_json::json!({ "data": { "password": expected_password } }))
    .send()
    .await
    .expect("requete d'ecriture du secret session_auth")
    .error_for_status()
    .expect("ecriture du secret session_auth");

    // Cote net-proxy : login Kubernetes-auth scope (pas le token
    // d'administration) avec le vrai token projete du ServiceAccount, exact
    // code emprunte par `crate::session_auth::refresh_once` en production.
    let output = std::process::Command::new("kubectl")
        .args(["create", "token", &sa_name, "-n", ns])
        .output()
        .expect("kubectl doit etre disponible");
    assert!(
        output.status.success(),
        "kubectl create token a echoue: {output:?}"
    );
    let sa_token = String::from_utf8(output.stdout).unwrap().trim().to_string();
    let token_path = std::env::temp_dir().join(unique_name("atelier-test-sa-token"));
    std::fs::write(&token_path, &sa_token).expect("ecriture du token temporaire");

    // SAFETY (env non partage entre tests dans le meme process) : ce test
    // est le seul de ce fichier a lire `ATELIER_K8S_SA_TOKEN_PATH`.
    unsafe {
        std::env::set_var("ATELIER_K8S_SA_TOKEN_PATH", &token_path);
    }
    let net_proxy_openbao_client = OpenBaoClient::from_env(openbao_addr.clone(), name.clone());
    let client_token = net_proxy_openbao_client
        .login()
        .await
        .expect("le login Kubernetes-auth doit reussir avec le token du ServiceAccount dedie");
    let password = net_proxy_openbao_client
        .read_field(&client_token, "session_auth", "password")
        .await
        .expect("la lecture du secret session_auth doit reussir");

    assert_eq!(password, expected_password);

    std::fs::remove_file(&token_path).ok();
    service_accounts
        .delete(&sa_name, &DeleteParams::default())
        .await
        .ok();
    http.delete(format!(
        "{openbao_addr}/v1/auth/kubernetes/role/{role_name}"
    ))
    .header("X-Vault-Token", &openbao_token)
    .send()
    .await
    .ok();
    http.delete(format!("{openbao_addr}/v1/sys/policy/{role_name}"))
        .header("X-Vault-Token", &openbao_token)
        .send()
        .await
        .ok();
}
