//! Test d'integration reel de `exec_in_workshop` (Jalon M4, tache 4.2.3) :
//! vrai binaire `net-proxy` (meme convention que
//! `tests/routes.rs::portforward_relays_through_api_server_to_net_proxy`),
//! vrai serveur SSH (implementation reelle du protocole cote serveur via
//! `russh::server`, pas un mock/stub — substitue au vrai `sshd` de la
//! microVM Firecracker pour rendre ce test rapide/portable, mais parle le
//! vrai protocole SSH de bout en bout), vrai cluster Kubernetes, vrai
//! PostgreSQL. Verifie que `crate::exec` (le client SSH, `russh::client`)
//! s'authentifie avec la bonne cle, execute la commande, et bufferise
//! stdout/stderr/exit code dans `exec_commands` comme attendu.

use atelier_api_server::routes::AppState;
use kube::Client;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Msg, Server as _, Session};
use russh::{Channel, ChannelId};
use std::sync::Arc;
use std::time::Duration;
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
        .expect("execution des migrations PostgreSQL");
    pool
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
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("le port {port} n'a jamais ouvert");
}

/// Serveur SSH minimal : accepte uniquement `expected_key`, et repond a
/// `exec` en ecrivant `echo <command>` sur stdout, `stderr from <command>`
/// sur stderr, puis un statut de sortie fixe — suffisant pour verifier que
/// `crate::exec` transporte reellement les deux flux et l'exit code, sans
/// avoir besoin d'un vrai shell.
#[derive(Clone)]
struct MockSshServer {
    expected_key: Arc<PublicKey>,
}

impl russh::server::Server for MockSshServer {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        self.clone()
    }
}

impl russh::server::Handler for MockSshServer {
    type Error = russh::Error;

    async fn auth_publickey(
        &mut self,
        _user: &str,
        key: &PublicKey,
    ) -> Result<russh::server::Auth, Self::Error> {
        if key.key_data() == self.expected_key.key_data() {
            Ok(russh::server::Auth::Accept)
        } else {
            Ok(russh::server::Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        _channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).to_string();
        session.data(channel, format!("echo {command}").into_bytes())?;
        session.extended_data(channel, 1, format!("stderr from {command}").into_bytes())?;
        session.channel_success(channel)?;
        session.exit_status_request(channel, 7)?;
        session.eof(channel)?;
        session.close(channel)?;
        Ok(())
    }
}

/// Reconstruit `exec_commands` a chaque nouveau test (pas de nettoyage
/// entre tests dans ce fichier — un seul test).
async fn fetch_exec_row(
    pool: &sqlx::PgPool,
    owner_subject: &str,
    id: Uuid,
) -> (String, Option<i32>, Vec<u8>, Vec<u8>) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(owner_subject)
        .execute(&mut *tx)
        .await
        .unwrap();
    let row: (String, Option<i32>, Vec<u8>, Vec<u8>) = sqlx::query_as(
        "SELECT status, exit_code, stdout_buffer, stderr_buffer FROM exec_commands WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    row
}

#[tokio::test]
async fn exec_in_workshop_runs_a_real_command_over_ssh_and_buffers_the_result() {
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

    // Paire de cles Ed25519 (memes primitives que
    // `crates/controller/src/openbao.rs::ensure_ssh_key`, mais generees
    // directement ici : ce test ne dependant pas d'OpenBao).
    let private_key = PrivateKey::random(
        &mut rand_core::UnwrapErr(getrandom::SysRng),
        Algorithm::Ed25519,
    )
    .unwrap();
    let public_key = private_key.public_key().clone();
    let private_key_pem = private_key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .unwrap()
        .to_string();

    let ssh_server_port = pick_free_port().await;
    let mock_server = MockSshServer {
        expected_key: Arc::new(public_key),
    };
    let ssh_listener = tokio::net::TcpListener::bind(("127.0.0.1", ssh_server_port))
        .await
        .unwrap();
    let server_config = Arc::new(russh::server::Config {
        keys: vec![PrivateKey::random(
            &mut rand_core::UnwrapErr(getrandom::SysRng),
            Algorithm::Ed25519,
        )
        .unwrap()],
        ..Default::default()
    });
    let mut mock_server_runner = mock_server.clone();
    tokio::spawn(async move {
        let _ = mock_server_runner
            .run_on_socket(server_config, &ssh_listener)
            .await;
    });

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
    // SAFETY (test) : seul ce test de ce binaire touche cette variable.
    unsafe { std::env::set_var("ATELIER_NET_PROXY_CONTROL_PORT", control_port.to_string()) };

    let namespace = "default".to_string();
    let owner_subject = "exec-owner@test.atelier".to_string();
    let workshop_name = format!("api-exec-test-{}", std::process::id());
    let state = AppState {
        client: client.clone(),
        namespace,
        db_pool: test_db_pool().await,
        openbao_addr: None,
        litellm_addr: None,
        llm_budget: None,
        llm_salt_key_configured: false,
        session_auth: None,
        storage: None,
        slack_webhook_url: None,
        slack_signing_secret: None,
    };

    let id = atelier_api_server::exec::spawn(
        state.clone(),
        owner_subject.clone(),
        workshop_name,
        // Le port SSH cible est celui de notre serveur mock.
        atelier_api_server::exec::GuestAddr {
            pod_ip: "127.0.0.1".to_string(),
            ssh_port: ssh_server_port,
        },
        private_key_pem,
        "echo hello".to_string(),
        "https://example.invalid/repo.git".to_string(),
    )
    .await
    .expect("enregistrement de l'execution");

    let mut status = "Running".to_string();
    let mut exit_code = None;
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    for _ in 0..50 {
        let row = fetch_exec_row(&state.db_pool, &owner_subject, id).await;
        status = row.0;
        exit_code = row.1;
        stdout = row.2;
        stderr = row.3;
        if status != "Running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    assert_eq!(status, "Completed", "l'execution doit reussir");
    assert_eq!(exit_code, Some(7));
    // Le serveur SSH mock renvoie `echo <commande recue>` : `stdout` montre
    // donc la commande TELLE QU'ELLE EST ARRIVEE dans le guest. Elle est
    // enrobee par `exec::in_workspace` (`cd /workspaces/<repo>` derive de
    // l'URL du depot, ici `repo.git`), et c'est precisement ce que ce test
    // doit constater : une commande d'agent qui s'executerait a la racine
    // du guest plutot que dans le workspace ne trouverait pas les sources.
    assert_eq!(
        String::from_utf8_lossy(&stdout),
        "echo exec timeout --kill-after=30s 1200s bash -c 'cd /workspaces/repo 2>/dev/null || true; echo hello' </dev/null"
    );
    assert_eq!(
        String::from_utf8_lossy(&stderr),
        "stderr from exec timeout --kill-after=30s 1200s bash -c 'cd /workspaces/repo 2>/dev/null || true; echo hello' </dev/null"
    );

    net_proxy.start_kill().ok();
}

/// Tache 14.5 : l'outil MCP contre un VRAI `sshd` OpenSSH (sans privilege) et
/// un vrai shell. Le serveur SSH du test precedent ecrit lui-meme ce qu'il
/// renvoie ; ici, c'est OpenSSH qui decide quand il envoie le statut de
/// sortie, comment il rapporte un signal et ce qu'il fait d'un processus
/// d'arriere-plan.
#[tokio::test]
async fn exec_in_workshop_keeps_nul_bytes_signals_and_returns_despite_background_processes() {
    let Some(client) = try_client().await else {
        eprintln!("pas de kubeconfig accessible, test ignore");
        return;
    };
    let net_proxy_bin = std::env::var("ATELIER_TEST_NET_PROXY_BIN")
        .unwrap_or_else(|_| "../../target/debug/atelier-net-proxy".to_string());
    if !std::path::Path::new(&net_proxy_bin).exists()
        || !std::path::Path::new("/usr/sbin/sshd").exists()
    {
        eprintln!("net-proxy ou sshd introuvable, test ignore");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
    if !std::process::Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f", &path("host_key")])
        .status()
        .is_ok_and(|s| s.success())
    {
        eprintln!("ssh-keygen indisponible, test ignore");
        return;
    }
    let client_key = PrivateKey::random(
        &mut rand_core::UnwrapErr(getrandom::SysRng),
        Algorithm::Ed25519,
    )
    .unwrap();
    std::fs::write(
        path("authorized_keys"),
        format!("{}\n", client_key.public_key().to_openssh().unwrap()),
    )
    .unwrap();
    let ssh_port = pick_free_port().await;
    std::fs::write(
        path("sshd_config"),
        format!(
            "Port {ssh_port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\n\
             PidFile {}\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
             PubkeyAuthentication yes\nLogLevel ERROR\n",
            path("host_key"),
            path("authorized_keys"),
            path("sshd.pid"),
        ),
    )
    .unwrap();
    let _sshd = tokio::process::Command::new("/usr/sbin/sshd")
        .args(["-D", "-e", "-f", &path("sshd_config")])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    wait_for_port(ssh_port).await;

    let control_port = pick_free_port().await;
    let _net_proxy = tokio::process::Command::new(&net_proxy_bin)
        .env("ATELIER_NET_PROXY_LISTEN_ADDR", "127.0.0.1:0")
        .env(
            "ATELIER_NET_PROXY_CONTROL_ADDR",
            format!("127.0.0.1:{control_port}"),
        )
        .env("ATELIER_DNS_LISTEN_ADDR", "127.0.0.1:0")
        .env("ATELIER_VM_ADDR", "127.0.0.1")
        .env("ATELIER_EGRESS_ALLOWLIST", "*")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("lancement du binaire net-proxy");
    wait_for_port(control_port).await;
    let user = String::from_utf8(
        std::process::Command::new("id")
            .arg("-un")
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap()
    .trim()
    .to_string();
    // SAFETY (test) : seuls les tests de ce binaire touchent ces variables, et
    // le premier n'en lit aucune de ces deux-la.
    unsafe {
        std::env::set_var("ATELIER_NET_PROXY_CONTROL_PORT", control_port.to_string());
        std::env::set_var("ATELIER_SSH_USER", &user);
    }
    let pem = client_key
        .to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .unwrap()
        .to_string();
    let state = AppState {
        client,
        namespace: "default".to_string(),
        db_pool: test_db_pool().await,
        openbao_addr: None,
        litellm_addr: None,
        llm_budget: None,
        llm_salt_key_configured: false,
        session_auth: None,
        storage: None,
        slack_webhook_url: None,
        slack_signing_secret: None,
    };
    let owner = "exec-real-sshd@test.atelier".to_string();

    // (commande, duree maximale attendue)
    let run = |command: &'static str| {
        let state = state.clone();
        let owner = owner.clone();
        let pem = pem.clone();
        async move {
            let id = atelier_api_server::exec::spawn(
                state.clone(),
                owner.clone(),
                format!("api-exec-real-{}", std::process::id()),
                atelier_api_server::exec::GuestAddr {
                    pod_ip: "127.0.0.1".to_string(),
                    ssh_port,
                },
                pem,
                command.to_string(),
                "https://example.invalid/repo.git".to_string(),
            )
            .await
            .unwrap();
            let started = std::time::Instant::now();
            loop {
                let (status, code, out, err) = fetch_exec_row(&state.db_pool, &owner, id).await;
                if status != "Running" {
                    let signal = fetch_signal(&state.db_pool, &owner, id).await;
                    return (status, code, signal, out, err, started.elapsed());
                }
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "{command} ne rend pas la main"
                );
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };

    let (status, code, _, out, err, _) = run("printf 'a\\0b'; echo erreur >&2").await;
    assert_eq!((status.as_str(), code), ("Completed", Some(0)));
    assert_eq!(out, b"a\0b", "l'octet nul est conserve");
    assert_eq!(err, b"erreur\n");

    let (status, code, signal, ..) = run("kill -SEGV $$").await;
    assert_eq!(status, "Killed");
    assert_eq!(code, None);
    assert_eq!(signal.as_deref(), Some("SEGV"));

    let (status, code, ..) = run("cat").await;
    assert_eq!(
        (status.as_str(), code),
        ("Completed", Some(0)),
        "entree standard fermee"
    );

    let (status, code, _, out, _, took) = run("sleep 30 & echo lance").await;
    assert_eq!((status.as_str(), code), ("Completed", Some(0)));
    assert_eq!(out, b"lance\n");
    assert!(
        took < Duration::from_secs(5),
        "un processus d'arriere-plan retient l'appel: {took:?}"
    );
}

async fn fetch_signal(pool: &sqlx::PgPool, owner_subject: &str, id: Uuid) -> Option<String> {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(owner_subject)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query_scalar("SELECT signal FROM exec_commands WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .unwrap()
}
