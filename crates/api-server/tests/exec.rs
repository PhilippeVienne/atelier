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
) -> (String, Option<i32>, String, String) {
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(owner_subject)
        .execute(&mut *tx)
        .await
        .unwrap();
    let row: (String, Option<i32>, String, String) = sqlx::query_as(
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
    // SAFETY (test) : redirige le port SSH cible vers notre serveur mock.
    unsafe { std::env::set_var("ATELIER_SSH_PORT", ssh_server_port.to_string()) };

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
        "127.0.0.1".to_string(),
        private_key_pem,
        "echo hello".to_string(),
        "https://example.invalid/repo.git".to_string(),
    )
    .await
    .expect("enregistrement de l'execution");

    let mut status = "Running".to_string();
    let mut exit_code = None;
    let mut stdout = String::new();
    let mut stderr = String::new();
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
        stdout,
        "echo timeout --kill-after=30s 1200s bash -c 'cd /workspaces/repo 2>/dev/null || true; echo hello'"
    );
    assert_eq!(
        stderr,
        "stderr from timeout --kill-after=30s 1200s bash -c 'cd /workspaces/repo 2>/dev/null || true; echo hello'"
    );

    net_proxy.start_kill().ok();
}
