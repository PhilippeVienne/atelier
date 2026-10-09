//! `crate::run` contre un VRAI `sshd` (OpenSSH), lance sans privilege sur un
//! port libre de la machine de test, avec un vrai shell derriere : rien de
//! simule entre le client SSH de l'api-server et la commande.
//!
//! C'est le comportement d'OpenSSH qui est en jeu ici (quand il envoie le
//! statut de sortie, ce qu'il fait d'un processus d'arriere-plan qui garde
//! ses descripteurs, comment il rapporte un signal) : un serveur SSH ecrit
//! pour le test ne prouverait que ce qu'on y aurait mis.
//!
//! Ignore, avec un message, si `/usr/sbin/sshd` ou `ssh-keygen` manquent.
//! Ce que ce test ne couvre pas : le tunnel `portforward` de `net-proxy` et
//! le `sshd` injecte dans une microVM (compte `vscode`, `PermitUserEnvironment`).

use atelier_api_server::run::{run_over, run_over_within, RunOutcome, RunRequest};
use russh::keys::{Algorithm, PrivateKey};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

const SSHD: &str = "/usr/sbin/sshd";

struct Sshd {
    port: u16,
    user: String,
    /// Cle privee du client, autorisee par ce serveur.
    key: String,
    dir: tempfile::TempDir,
    _process: tokio::process::Child,
}

fn new_key() -> PrivateKey {
    PrivateKey::random(
        &mut rand_core::UnwrapErr(getrandom::SysRng),
        Algorithm::Ed25519,
    )
    .unwrap()
}

fn pem(key: &PrivateKey) -> String {
    key.to_openssh(russh::keys::ssh_key::LineEnding::LF)
        .unwrap()
        .to_string()
}

impl Sshd {
    async fn start() -> Option<Self> {
        if !Path::new(SSHD).exists() {
            eprintln!("{SSHD} absent, test ignore");
            return None;
        }
        let dir = tempfile::tempdir().unwrap();
        let path = |name: &str| dir.path().join(name).to_string_lossy().into_owned();
        let keygen = std::process::Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f", &path("host_key")])
            .status();
        if !keygen.is_ok_and(|status| status.success()) {
            eprintln!("ssh-keygen indisponible, test ignore");
            return None;
        }

        let client_key = new_key();
        std::fs::write(
            path("authorized_keys"),
            format!("{}\n", client_key.public_key().to_openssh().unwrap()),
        )
        .unwrap();

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        // Sans privilege, `sshd` ne peut ouvrir de session que pour le
        // compte qui le lance : c'est celui du test.
        std::fs::write(
            path("sshd_config"),
            format!(
                "Port {port}\nListenAddress 127.0.0.1\nHostKey {}\nAuthorizedKeysFile {}\n\
                 PidFile {}\nStrictModes no\nUsePAM no\nPasswordAuthentication no\n\
                 KbdInteractiveAuthentication no\nPubkeyAuthentication yes\nLogLevel ERROR\n",
                path("host_key"),
                path("authorized_keys"),
                path("sshd.pid"),
            ),
        )
        .unwrap();
        let process = tokio::process::Command::new(SSHD)
            .args(["-D", "-e", "-f", &path("sshd_config")])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        for _ in 0..100 {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_ok()
            {
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
                return Some(Self {
                    port,
                    user,
                    key: pem(&client_key),
                    dir,
                    _process: process,
                });
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("sshd n'a jamais ouvert le port {port}");
    }

    async fn run(&self, request: RunRequest) -> RunOutcome {
        self.run_with_key(&self.key, request)
            .await
            .expect("le canal SSH doit fonctionner")
    }

    async fn run_with_key(&self, key: &str, request: RunRequest) -> anyhow::Result<RunOutcome> {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", self.port)).await?;
        // Dossier par defaut : celui du test, pour ne rien ecrire ailleurs.
        let default_dir = self.dir.path().to_string_lossy().into_owned();
        run_over(stream, &self.user, key, &request, &default_dir).await
    }
}

fn request(command: &str) -> RunRequest {
    RunRequest::new(command.to_string(), None, BTreeMap::new(), Some(20), None).unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

#[tokio::test]
async fn output_streams_stay_separate_and_the_exit_code_comes_back() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let outcome = sshd
        .run(request("printf sortie; printf erreur >&2; exit 3"))
        .await;
    assert_eq!(text(&outcome.stdout), "sortie");
    assert_eq!(text(&outcome.stderr), "erreur");
    assert_eq!(
        (outcome.exit_code, &outcome.signal, outcome.timed_out),
        (Some(3), &None, false)
    );
    assert!(!outcome.stdout_truncated && !outcome.stderr_truncated);

    // `sh -c` : tubes, variables et enchainements fonctionnent.
    let outcome = sshd
        .run(request(
            "echo un deux | tr ' ' '\\n' | wc -l && test -n \"$HOME\"",
        ))
        .await;
    assert_eq!(text(&outcome.stdout).trim(), "2");
    assert_eq!(outcome.exit_code, Some(0));

    // Une commande qui n'existe pas est un resultat (127), pas une erreur.
    let outcome = sshd.run(request("commande-qui-n-existe-pas")).await;
    assert_eq!(outcome.exit_code, Some(127));
    assert!(!outcome.stderr.is_empty());
}

#[tokio::test]
async fn bytes_that_are_not_text_come_back_untouched() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let outcome = sshd
        .run(request("printf 'a\\000b\\377\\n'; printf '\\000' >&2"))
        .await;
    assert_eq!(outcome.stdout, [b'a', 0, b'b', 0xff, b'\n']);
    assert_eq!(outcome.stderr, [0]);
}

#[tokio::test]
async fn a_signal_is_reported_as_a_signal() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    for (signal, name) in [("TERM", "TERM"), ("KILL", "KILL"), ("SEGV", "SEGV")] {
        let outcome = sshd
            .run(request(&format!("echo avant; kill -{signal} $$")))
            .await;
        assert_eq!(outcome.signal.as_deref(), Some(name), "{outcome:?}");
        assert_eq!(outcome.exit_code, None, "{outcome:?}");
        assert!(!outcome.timed_out, "{outcome:?}");
        assert_eq!(text(&outcome.stdout), "avant\n");
    }
}

#[tokio::test]
async fn a_command_past_its_time_limit_is_stopped_and_said_so() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let limited = |command: &str| {
        RunRequest::new(command.to_string(), None, BTreeMap::new(), Some(1), None).unwrap()
    };
    let started = Instant::now();
    let outcome = sshd.run(limited("echo debut; sleep 30; echo fin")).await;
    assert!(outcome.timed_out, "{outcome:?}");
    assert_eq!((outcome.exit_code, &outcome.signal), (None, &None));
    assert_eq!(
        text(&outcome.stdout),
        "debut\n",
        "ce qui a ete ecrit avant reste"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );

    // Une commande qui ignore TERM est tuee apres le delai de grace.
    let started = Instant::now();
    let outcome = sshd.run(limited("trap '' TERM; sleep 30")).await;
    assert!(outcome.timed_out, "{outcome:?}");
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );

    // Rendre 124 de soi-meme, bien avant le delai, n'est pas un depassement.
    let outcome = sshd.run(request("exit 124")).await;
    assert_eq!((outcome.exit_code, outcome.timed_out), (Some(124), false));
}

#[tokio::test]
async fn standard_input_is_closed() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let started = Instant::now();
    let outcome = sshd.run(request("cat; echo apres")).await;
    assert_eq!(text(&outcome.stdout), "apres\n");
    assert_eq!(outcome.exit_code, Some(0));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_background_process_does_not_hold_the_answer() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    // Le `sleep` herite de la sortie standard de la commande et la garde
    // ouverte : `sshd` ne ferme pas le canal tant qu'il vit. Le resultat
    // doit pourtant revenir des que la commande elle-meme se termine.
    let started = Instant::now();
    let outcome = sshd.run(request("sleep 15 & echo lance")).await;
    assert_eq!(text(&outcome.stdout), "lance\n");
    assert_eq!(
        (outcome.exit_code, outcome.timed_out),
        (Some(0), false),
        "{outcome:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn working_dir_and_environment_are_per_call() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let dir = sshd.dir.path().join("mon dossier d'essai");
    std::fs::create_dir(&dir).unwrap();
    let mut env = BTreeMap::new();
    env.insert("NOM".to_string(), "l'été $HOME `id`".to_string());
    let outcome = sshd
        .run(
            RunRequest::new(
                "pwd; printf '%s' \"$NOM\"".to_string(),
                Some(dir.to_string_lossy().into_owned()),
                env,
                Some(20),
                None,
            )
            .unwrap(),
        )
        .await;
    assert_eq!(
        text(&outcome.stdout),
        format!("{}\nl'été $HOME `id`", dir.display()),
        "le dossier et la variable arrivent tels quels, sans etre interpretes"
    );

    // Sans dossier demande : celui par defaut.
    let outcome = sshd.run(request("pwd")).await;
    assert_eq!(
        text(&outcome.stdout).trim(),
        sshd.dir.path().to_string_lossy()
    );

    // Dossier demande mais absent : la commande n'est pas lancee ailleurs.
    let missing = sshd.dir.path().join("absent");
    let outcome = sshd
        .run(
            RunRequest::new(
                "touch temoin; echo lance".to_string(),
                Some(missing.to_string_lossy().into_owned()),
                BTreeMap::new(),
                Some(20),
                None,
            )
            .unwrap(),
        )
        .await;
    assert_eq!(outcome.exit_code, Some(126), "{outcome:?}");
    assert!(outcome.stdout.is_empty());
    assert!(!sshd.dir.path().join("temoin").exists());
}

#[tokio::test]
async fn output_is_cut_at_the_limit_of_the_call() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let outcome = sshd
        .run(
            RunRequest::new(
                "head -c 300000 /dev/zero | tr '\\0' x; echo court >&2".to_string(),
                None,
                BTreeMap::new(),
                Some(20),
                Some(1000),
            )
            .unwrap(),
        )
        .await;
    assert_eq!(outcome.stdout.len(), 1000);
    assert!(outcome.stdout_truncated);
    assert_eq!(text(&outcome.stderr), "court\n");
    assert!(!outcome.stderr_truncated);
    // La commande est allee au bout malgre la sortie ecartee.
    assert_eq!(outcome.exit_code, Some(0));
}

#[tokio::test]
async fn a_key_the_guest_does_not_know_is_an_error_of_the_channel() {
    let Some(sshd) = Sshd::start().await else {
        return;
    };
    let error = sshd
        .run_with_key(&pem(&new_key()), request("true"))
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("authentification SSH refusee"),
        "{error}"
    );
}

/// Un `sshd` sature ecarte des connexions sans les fermer du point de vue
/// de l'api-server (constate a travers le tunnel `portforward`, a partir
/// d'une dizaine d'appels simultanes sur un meme Workshop) : l'appel doit
/// alors echouer, pas attendre sans fin.
#[tokio::test]
async fn a_guest_that_never_answers_is_an_error_not_a_wait() {
    // Un serveur qui accepte la connexion et ne dit jamais rien.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((socket, _)) = listener.accept().await {
            held.push(socket);
        }
    });
    let stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .unwrap();
    let started = Instant::now();
    let error = run_over_within(
        stream,
        "personne",
        &pem(&new_key()),
        &request("true"),
        "/tmp",
        Duration::from_millis(300),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("n'a pas ouvert de session"),
        "{error}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}
