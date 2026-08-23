//! Test d'integration reel de bout en bout pour la tache 2.2.3 (Jalon M2,
//! section 5.2) : verifie que le mecanisme production (alias interne
//! `net-proxy` bypassant l'allowlist + injection de PAT par `identity-proxy`)
//! permet reellement a un `git clone` de reussir contre l'instance Forgejo
//! de dev via le nom `git.atelier.internal`, sans que l'hote cible
//! n'apparaisse dans `Workshop.spec.egress_allowlist`.
//!
//! Reproduit fidelement la production, `identity-proxy` y compris : en pod
//! Kubernetes reel, c'est `Pod.spec.hostAliases` (pose par le controller,
//! voir `crates/controller/src/git_identity.rs`) qui rend
//! `git.atelier.internal` resolvable par `identity-proxy`
//! (`crates/identity-proxy/src/proxy.rs` se connecte directement au meme nom
//! que celui recu dans la requete, sans jamais le reecrire). Hors pod, ce
//! test obtient le meme effet avec l'equivalent Docker de `hostAliases`
//! (`docker run --add-host`) : `identity-proxy` tourne dans un conteneur
//! Docker (image `atelier-identity-proxy:dev`, deja construite pour les
//! Workshops reels) avec `git.atelier.internal` mappe vers la passerelle du
//! bridge Docker par defaut, qui route elle-meme vers Forgejo/OpenBao
//! exposes sur l'hote (`kubectl port-forward --address 0.0.0.0`). `net-proxy`,
//! lui, n'a besoin d'aucune resolution DNS pour cet alias (simple
//! correspondance de chaine, voir `crates/net-proxy/src/internal.rs`) et
//! tourne donc comme un process natif ordinaire, comme le reste des tests de
//! ce crate.
//!
//! Necessite (voir `deploy/dev/forgejo/README.md` et
//! `deploy/dev/openbao/README.md`) :
//!   - Forgejo de dev expose sur toutes les interfaces sur un port choisi
//!     (defaut 3300) : `kubectl port-forward svc/atelier-forgejo-dev
//!     --address 0.0.0.0 3300:3000 &` (`--address 0.0.0.0`, pas seulement
//!     `127.0.0.1`, indispensable pour etre joignable depuis un conteneur
//!     Docker via la passerelle du bridge) ;
//!   - OpenBao de dev expose de la meme facon (defaut 8300) : `kubectl
//!     port-forward svc/atelier-openbao-dev --address 0.0.0.0 8300:8200 &` ;
//!   - `docker` disponible localement, avec l'image `atelier-identity-proxy:dev`
//!     deja construite (`docker build -t atelier-identity-proxy:dev
//!     crates/identity-proxy`, ou reutilisation de l'image utilisee par le
//!     controller reel) ;
//!   - un cluster Kubernetes reel (kubeconfig par defaut) pour provisionner
//!     un ServiceAccount et son role Kubernetes-auth OpenBao, exactement
//!     comme le fait `crates/net-proxy/tests/session_auth.rs` ;
//!   - `git` installe.
//!
//!   cargo build -p atelier-net-proxy
//!   export OPENBAO_ADDR=http://127.0.0.1:8200 OPENBAO_TOKEN=root
//!   cargo test -p atelier-net-proxy --test git_identity -- --nocapture

use std::time::{SystemTime, UNIX_EPOCH};

/// Adresses jointes depuis un conteneur Docker (bridge par defaut) pour
/// atteindre les ports exposes sur toutes les interfaces de l'hote, voir le
/// commentaire de tete de fichier. Overridable si le bridge Docker par
/// defaut n'utilise pas cette adresse (`docker network inspect bridge
/// --format '{{(index .IPAM.Config 0).Gateway}}'`).
fn docker_bridge_gateway() -> String {
    std::env::var("ATELIER_TEST_DOCKER_BRIDGE_GATEWAY").unwrap_or_else(|_| "172.17.0.1".to_string())
}

fn forgejo_host_port() -> u16 {
    std::env::var("ATELIER_TEST_FORGEJO_HOST_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3300)
}

fn openbao_host_port() -> u16 {
    std::env::var("ATELIER_TEST_OPENBAO_HOST_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8300)
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

async fn pick_free_port() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

async fn wait_for_port(port: u16) {
    for _ in 0..80 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("le service n'a jamais ouvert son port {port}");
}

struct DockerContainer(String);

impl Drop for DockerContainer {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.0])
            .output();
    }
}

#[tokio::test]
async fn git_clone_through_net_proxy_succeeds_with_the_pat_injected_by_identity_proxy() {
    if std::process::Command::new("docker")
        .arg("info")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!("docker indisponible, test ignore");
        return;
    }
    if std::process::Command::new("docker")
        .args(["image", "inspect", "atelier-identity-proxy:dev"])
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        eprintln!(
            "image atelier-identity-proxy:dev introuvable, test ignore (voir crates/identity-proxy/Dockerfile)"
        );
        return;
    }

    let bridge_gateway = docker_bridge_gateway();
    let forgejo_port = forgejo_host_port();
    let openbao_port = openbao_host_port();

    let http = reqwest::Client::new();
    let forgejo_base = format!("http://127.0.0.1:{forgejo_port}");
    if http
        .get(format!("{forgejo_base}/api/v1/version"))
        .send()
        .await
        .is_err()
    {
        eprintln!(
            "Forgejo de dev injoignable sur 127.0.0.1:{forgejo_port}, test ignore (voir le commentaire de tete de ce fichier pour le port-forward requis)"
        );
        return;
    }
    let openbao_addr_from_container = format!("http://{bridge_gateway}:{openbao_port}");
    let openbao_addr_from_host = format!("http://127.0.0.1:{openbao_port}");
    let Ok(openbao_token) = std::env::var("OPENBAO_TOKEN") else {
        eprintln!("OPENBAO_TOKEN non defini, test ignore");
        return;
    };
    if http
        .get(format!("{openbao_addr_from_host}/v1/sys/health"))
        .send()
        .await
        .is_err()
    {
        eprintln!(
            "OpenBao de dev injoignable sur 127.0.0.1:{openbao_port}, test ignore (voir le commentaire de tete de ce fichier)"
        );
        return;
    }

    atelier_common::telemetry::ensure_crypto_provider();
    let Ok(k8s_client) = kube::Client::try_default().await else {
        eprintln!("kubeconfig requis (cluster kind local), test ignore");
        return;
    };

    let net_proxy_bin = std::env::var("ATELIER_TEST_NET_PROXY_BIN")
        .unwrap_or_else(|_| "../../target/debug/atelier-net-proxy".to_string());
    if !std::path::Path::new(&net_proxy_bin).exists() {
        eprintln!("binaire net-proxy introuvable ({net_proxy_bin}), test ignore (cargo build -p atelier-net-proxy)");
        return;
    }
    if std::process::Command::new("git")
        .arg("--version")
        .output()
        .is_err()
    {
        eprintln!("git non installe, test ignore");
        return;
    }

    // --- 1. Provisionne un vrai PAT Forgejo et un depot prive frais -------
    let token_name = unique_name("atelier-net-proxy-git-identity-test");
    let token_output = std::process::Command::new("kubectl")
        .args([
            "exec",
            "atelier-forgejo-dev",
            "--",
            "su-exec",
            "1000:1000",
            "forgejo",
            "admin",
            "user",
            "generate-access-token",
            "--username",
            "atelier_admin",
            "--token-name",
            &token_name,
            "--scopes",
            "all",
        ])
        .output()
        .expect("kubectl doit etre disponible");
    assert!(
        token_output.status.success(),
        "generation du token Forgejo a echoue: {token_output:?}"
    );
    let token_stdout = String::from_utf8_lossy(&token_output.stdout).to_string();
    let pat = token_stdout
        .rsplit_once(": ")
        .map(|(_, tok)| tok.trim().to_string())
        .expect("sortie inattendue de generate-access-token");

    let repo_name = unique_name("net-proxy-git-identity-repo");
    let create_repo = http
        .post(format!("{forgejo_base}/api/v1/user/repos"))
        .header("Authorization", format!("token {pat}"))
        .json(&serde_json::json!({ "name": repo_name, "private": true, "auto_init": true }))
        .send()
        .await
        .expect("requete de creation de depot");
    assert!(
        create_repo.status().is_success(),
        "creation du depot prive de test a echoue: {}",
        create_repo.status()
    );

    // --- 2. Provisionne OpenBao (meme pattern que session_auth.rs) --------
    let workshop_name = unique_name("test-net-proxy-git-identity");
    let sa_name = format!("{workshop_name}-parent");
    let service_accounts: kube::Api<k8s_openapi::api::core::v1::ServiceAccount> =
        kube::Api::namespaced(k8s_client.clone(), "default");
    service_accounts
        .create(
            &kube::api::PostParams::default(),
            &k8s_openapi::api::core::v1::ServiceAccount {
                metadata: kube::api::ObjectMeta {
                    name: Some(sa_name.clone()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .expect("creation du ServiceAccount");

    let role_name = format!("workshop-{workshop_name}");
    let policy_hcl = format!(
        "path \"secret/data/workshops/{workshop_name}/*\" {{ capabilities = [\"read\"] }}\npath \"secret/metadata/workshops/{workshop_name}/*\" {{ capabilities = [\"read\", \"list\"] }}",
    );
    http.put(format!(
        "{openbao_addr_from_host}/v1/sys/policy/{role_name}"
    ))
    .header("X-Vault-Token", &openbao_token)
    .json(&serde_json::json!({ "policy": policy_hcl }))
    .send()
    .await
    .expect("requete d'ecriture de policy")
    .error_for_status()
    .expect("ecriture de la policy OpenBao");
    http.put(format!(
        "{openbao_addr_from_host}/v1/auth/kubernetes/role/{role_name}"
    ))
    .header("X-Vault-Token", &openbao_token)
    .json(&serde_json::json!({
        "bound_service_account_names": [sa_name],
        "bound_service_account_namespaces": ["default"],
        "policies": [role_name],
        "ttl": "15m",
    }))
    .send()
    .await
    .expect("requete d'ecriture du role")
    .error_for_status()
    .expect("ecriture du role kubernetes-auth OpenBao");

    // Meme convention que `crates/image-builder/src/main.rs::resolve_git_credentials`
    // (secret_path="git", champ "password") — voir la decision documentee
    // dans `crates/controller/src/git_identity.rs`.
    http.put(format!(
        "{openbao_addr_from_host}/v1/secret/data/workshops/{workshop_name}/git"
    ))
    .header("X-Vault-Token", &openbao_token)
    .json(&serde_json::json!({ "data": { "password": pat } }))
    .send()
    .await
    .expect("requete d'ecriture du secret git")
    .error_for_status()
    .expect("ecriture du secret git");

    let sa_token_output = std::process::Command::new("kubectl")
        .args(["create", "token", &sa_name, "-n", "default"])
        .output()
        .expect("kubectl doit etre disponible");
    assert!(
        sa_token_output.status.success(),
        "kubectl create token a echoue: {sa_token_output:?}"
    );
    let sa_token_path =
        std::env::temp_dir().join(unique_name("atelier-test-git-identity-sa-token"));
    tokio::fs::write(&sa_token_path, &sa_token_output.stdout)
        .await
        .expect("ecriture du token temporaire");

    // --- 3. Lance identity-proxy dans un conteneur Docker ------------------
    // `--add-host` est l'equivalent Docker de `Pod.spec.hostAliases` (voir
    // le commentaire de tete du fichier) : c'est ce qui rend
    // `git.atelier.internal` reellement resolvable par identity-proxy, en
    // pointant vers la passerelle du bridge Docker, elle-meme routee vers
    // les ports Forgejo/OpenBao exposes sur l'hote.
    let identity_container_name = unique_name("atelier-test-identity-proxy");
    let identity_published_port = pick_free_port().await;
    let rules = serde_json::json!([{
        "host": "git.atelier.internal",
        "header": "Authorization",
        "prefix": "token ",
        "secretPath": "git",
        "field": "password",
    }]);
    let run_output = std::process::Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            &identity_container_name,
            "--add-host",
            &format!("git.atelier.internal:{bridge_gateway}"),
            "-p",
            &format!("{identity_published_port}:3129"),
            "-e",
            "ATELIER_IDENTITY_PROXY_LISTEN_ADDR=0.0.0.0:3129",
            "-e",
            &format!("ATELIER_IDENTITY_INJECTION_RULES={rules}"),
            "-e",
            &format!("ATELIER_WORKSHOP_NAME={workshop_name}"),
            "-e",
            &format!("OPENBAO_ADDR={openbao_addr_from_container}"),
            "-v",
            &format!(
                "{}:/var/run/secrets/kubernetes.io/serviceaccount/token:ro",
                sa_token_path.display()
            ),
            "atelier-identity-proxy:dev",
        ])
        .output()
        .expect("lancement du conteneur identity-proxy");
    assert!(
        run_output.status.success(),
        "docker run (identity-proxy) a echoue: {run_output:?}"
    );
    let _identity_container = DockerContainer(identity_container_name);
    wait_for_port(identity_published_port).await;
    // Laisse le temps a la boucle de rafraichissement des secrets
    // (`crates/identity-proxy/src/secrets.rs`) de faire son premier login
    // OpenBao et de peupler le cache avant la premiere requete.
    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    // --- 4. Lance net-proxy (process natif) avec l'alias Git ---------------
    // `ATELIER_EGRESS_ALLOWLIST` reste absente/vide : la seule raison pour
    // laquelle la requete passe est l'alias interne `git.atelier.internal`,
    // jamais l'allowlist — c'est exactement ce que 2.2.3 doit garantir.
    let net_proxy_listen_port = pick_free_port().await;
    let net_proxy_control_port = pick_free_port().await;
    let mut net_proxy = tokio::process::Command::new(&net_proxy_bin)
        .env(
            "ATELIER_NET_PROXY_LISTEN_ADDR",
            format!("127.0.0.1:{net_proxy_listen_port}"),
        )
        .env(
            "ATELIER_NET_PROXY_CONTROL_ADDR",
            format!("127.0.0.1:{net_proxy_control_port}"),
        )
        .env("ATELIER_DNS_LISTEN_ADDR", "127.0.0.1:0")
        .env("ATELIER_VM_ADDR", "127.0.0.1")
        .env(
            "ATELIER_GIT_ALIAS_ADDR",
            format!("127.0.0.1:{identity_published_port}"),
        )
        .kill_on_drop(true)
        .spawn()
        .expect("lancement du binaire net-proxy");
    wait_for_port(net_proxy_control_port).await;
    wait_for_port(net_proxy_listen_port).await;

    // --- 5. Vrai `git clone` a travers net-proxy, sans PAT dans l'URL ------
    // Le client git n'a jamais besoin de resoudre `git.atelier.internal`
    // lui-meme : la requete part en forme absolue vers le proxy configure
    // (`http_proxy`), qui seul a besoin de savoir ou l'envoyer.
    let clone_dir = std::env::temp_dir().join(unique_name("atelier-git-identity-clone"));
    let clone_status = std::process::Command::new("git")
        .env(
            "http_proxy",
            format!("http://127.0.0.1:{net_proxy_listen_port}"),
        )
        .env(
            "https_proxy",
            format!("http://127.0.0.1:{net_proxy_listen_port}"),
        )
        .arg("clone")
        .arg(format!(
            "http://git.atelier.internal:{forgejo_port}/atelier_admin/{repo_name}.git"
        ))
        .arg(&clone_dir)
        .status()
        .expect("lancement de git clone");

    net_proxy.start_kill().ok();

    assert!(
        clone_status.success(),
        "git clone doit reussir : le depot est prive, donc un succes prouve que \
         identity-proxy a bien injecte le header Authorization (sans allowlist \
         egress configuree cote net-proxy, donc uniquement grace a l'alias interne \
         git.atelier.internal)"
    );
    assert!(
        clone_dir.join("README.md").exists(),
        "le contenu reel du depot (auto_init) doit avoir ete clone"
    );

    // --- Nettoyage ----------------------------------------------------------
    tokio::fs::remove_dir_all(&clone_dir).await.ok();
    tokio::fs::remove_file(&sa_token_path).await.ok();
    service_accounts
        .delete(&sa_name, &kube::api::DeleteParams::default())
        .await
        .ok();
    http.delete(format!(
        "{openbao_addr_from_host}/v1/auth/kubernetes/role/{role_name}"
    ))
    .header("X-Vault-Token", &openbao_token)
    .send()
    .await
    .ok();
    http.delete(format!(
        "{openbao_addr_from_host}/v1/sys/policy/{role_name}"
    ))
    .header("X-Vault-Token", &openbao_token)
    .send()
    .await
    .ok();
    http.delete(format!(
        "{forgejo_base}/api/v1/repos/atelier_admin/{repo_name}"
    ))
    .header("Authorization", format!("token {pat}"))
    .send()
    .await
    .ok();
}
