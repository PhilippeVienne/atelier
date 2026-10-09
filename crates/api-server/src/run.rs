//! `POST /v1/workshops/{name}/exec` (Jalon M14, tache 14.6, spec
//! `docs/specs/19-sessions-pour-apprenants.md` §3.4) : executer UNE commande
//! dans le guest et rendre son resultat dans la reponse.
//!
//! `crate::exec` (l'outil MCP `exec_in_workshop`) est fait pour un agent :
//! execution asynchrone, sortie accumulee en base, a relire par un flux. Un
//! serveur qui VERIFIE quelque chose dans un Workshop (une plateforme de
//! formation qui controle une etape de labo, une CI) a besoin de l'inverse :
//! une commande courte, une reponse, et un resultat sur lequel il peut
//! decider. D'ou ce second chemin, qui garantit ce que le premier ne
//! garantit pas :
//!
//! - **code de sortie OU signal**, jamais ni l'un ni l'autre ;
//! - **sortie standard et sortie d'erreur separees et intactes**, octets
//!   nuls compris (elles voyagent en base64, jamais converties en texte) ;
//! - **un delai et une taille de sortie par appel**, avec un indicateur
//!   quand l'un ou l'autre a coupe le resultat ;
//! - **entree standard fermee** : une commande qui lit son entree voit une
//!   fin de fichier au lieu d'attendre ;
//! - **la reponse arrive quand la commande se termine**, meme si elle a
//!   laisse derriere elle un processus d'arriere-plan qui garde ses
//!   descripteurs ouverts.
//!
//! Meme canal que `crate::exec` : SSH vers le `sshd` injecte, a travers le
//! tunnel `portforward` de `net-proxy`, avec la cle du Workshop.

use crate::auth::AuthenticatedUser;
use crate::routes::{
    ensure_owner, resolve_running_pod_ip, workshop_tenant, workshops_api, ApiError, AppState,
};
use axum::extract::{Path, State};
use axum::{Extension, Json};
use base64::Engine;
use russh::client;
use russh::keys::{decode_secret_key, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Delai d'une commande quand l'appel n'en donne pas.
const DEFAULT_TIMEOUT_SECS: u64 = 60;
/// Taille de sortie gardee, par flux, quand l'appel n'en donne pas.
const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
/// Plafond de `maxOutputBytes` : les deux flux tiennent en memoire dans ce
/// serveur le temps de l'appel.
const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Entre le delai de la commande et celui du serveur : le temps laisse au
/// guest pour tuer la commande lui-meme (`timeout -k`) et rendre son statut.
const KILL_GRACE: Duration = Duration::from_secs(5);
const SERVER_MARGIN: Duration = Duration::from_secs(10);
/// Une fois le statut de sortie recu, temps laisse aux derniers octets de
/// sortie pour arriver. Sans processus d'arriere-plan le canal se ferme
/// avant ; avec, il ne se fermerait jamais, et on n'attend pas plus.
const DRAIN_AFTER_EXIT: Duration = Duration::from_millis(300);

/// Plafond du delai par appel : celui de `exec_in_workshop`
/// (`ATELIER_EXEC_CEILING_SECS`, 20 minutes par defaut).
fn timeout_ceiling_secs() -> u64 {
    std::env::var("ATELIER_EXEC_CEILING_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|secs| *secs > 0)
        .unwrap_or(1200)
}

/// Corps de `POST /v1/workshops/{name}/exec`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunBody {
    /// Commande, interpretee par `sh -c` dans le guest.
    command: String,
    /// Dossier ou lancer la commande. Par defaut le workspace du Workshop.
    /// S'il est donne et n'existe pas, la commande n'est PAS lancee
    /// ailleurs : l'appel rend le code de sortie 126.
    #[serde(default)]
    working_dir: Option<String>,
    /// Variables d'environnement ajoutees a celles de la session.
    #[serde(default)]
    env: BTreeMap<String, String>,
    /// Delai de la commande, en secondes. Par defaut 60, plafonne.
    #[serde(default)]
    timeout_seconds: Option<u64>,
    /// Octets gardes par flux de sortie. Par defaut 1 Mio, plafonne a 8.
    #[serde(default)]
    max_output_bytes: Option<usize>,
}

/// Une commande prete a partir : ce qui a ete demande, valide et borne.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub command: String,
    pub working_dir: Option<String>,
    pub env: BTreeMap<String, String>,
    pub timeout: Duration,
    pub max_output: usize,
}

impl RunRequest {
    /// Valide et borne un corps de requete. L'erreur est le message a
    /// rendre a l'appelant.
    pub fn new(
        command: String,
        working_dir: Option<String>,
        env: BTreeMap<String, String>,
        timeout_seconds: Option<u64>,
        max_output_bytes: Option<usize>,
    ) -> Result<Self, String> {
        if command.trim().is_empty() {
            return Err("commande vide".to_string());
        }
        // Un octet nul ne peut figurer ni dans un argument ni dans une
        // variable d'environnement : le shell couperait la.
        if command.contains('\0') {
            return Err("la commande contient un octet nul".to_string());
        }
        if let Some(dir) = &working_dir {
            if dir.is_empty() || dir.contains('\0') {
                return Err("workingDir invalide".to_string());
            }
        }
        for (name, value) in &env {
            if !is_env_name(name) {
                return Err(format!(
                    "nom de variable d'environnement invalide : {name:?}"
                ));
            }
            if value.contains('\0') {
                return Err(format!("la variable {name} contient un octet nul"));
            }
        }
        let timeout = timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECS);
        if timeout == 0 {
            return Err("timeoutSeconds doit etre superieur a 0".to_string());
        }
        Ok(Self {
            command,
            working_dir,
            env,
            timeout: Duration::from_secs(timeout.min(timeout_ceiling_secs())),
            max_output: max_output_bytes
                .unwrap_or(DEFAULT_MAX_OUTPUT_BYTES)
                .min(MAX_OUTPUT_BYTES),
        })
    }

    /// Le script remis au shell de session du guest.
    ///
    /// Chaque valeur venue de l'appelant y entre comme UN mot entre
    /// apostrophes ([`quote`]) : ni le dossier, ni une variable, ni la
    /// commande ne peuvent en sortir pour devenir une autre commande du
    /// script.
    ///
    /// `timeout -k` est la forme commune a GNU coreutils et a busybox. Il
    /// place la commande dans son propre groupe de processus et, au delai,
    /// signale le groupe entier : un serveur lance en arriere-plan par la
    /// commande part avec elle. `exec` remplace le shell de session par
    /// `timeout` : le statut que voit `sshd`, signal compris, est celui de
    /// la commande, pas celui d'un shell intermediaire.
    pub fn script(&self, default_dir: &str) -> String {
        let mut script = String::new();
        match &self.working_dir {
            // Dossier demande : y aller ou ne rien lancer. 126 est le code
            // POSIX de « commande trouvee mais non executable ».
            Some(dir) => script.push_str(&format!("cd -- {} || exit 126\n", quote(dir))),
            // Dossier par defaut : meme tolerance que `exec_in_workshop`.
            None => script.push_str(&format!(
                "cd -- {} 2>/dev/null || true\n",
                quote(default_dir)
            )),
        }
        for (name, value) in &self.env {
            script.push_str(&format!("export {name}={}\n", quote(value)));
        }
        script.push_str(&format!(
            "exec timeout -k {} {} sh -c {} </dev/null\n",
            KILL_GRACE.as_secs(),
            self.timeout.as_secs(),
            quote(&self.command)
        ));
        script
    }
}

/// `[A-Za-z_][A-Za-z0-9_]*` : ce que POSIX accepte comme nom de variable,
/// et rien qui puisse etre autre chose dans un `export`.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Ecrit `text` comme un seul mot d'un shell POSIX, quoi qu'il contienne.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// Ce qu'une commande a laisse.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RunOutcome {
    /// Code de sortie, si la commande s'est terminee d'elle-meme.
    pub exit_code: Option<i32>,
    /// Nom du signal (`KILL`, `TERM`, `SEGV`...), si un signal l'a terminee.
    pub signal: Option<String>,
    /// La commande tournait encore au delai et a ete arretee. `exit_code`
    /// et `signal` sont alors vides : son statut n'est pas le sien.
    pub timed_out: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub duration: Duration,
}

/// Garde les `limit` premiers octets d'un flux et note s'il en a perdu.
fn keep(buffer: &mut Vec<u8>, truncated: &mut bool, data: &[u8], limit: usize) {
    let room = limit.saturating_sub(buffer.len());
    buffer.extend_from_slice(&data[..data.len().min(room)]);
    if data.len() > room {
        *truncated = true;
    }
}

struct AcceptAnyHostKey;

impl client::Handler for AcceptAnyHostKey {
    type Error = russh::Error;

    /// Pas de verification de la cle d'hote : meme raisonnement que
    /// `crate::exec`, le canal ne quitte pas le tunnel interne au pod.
    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Lance `request` par SSH sur `stream` et attend son resultat.
///
/// `stream` est la connexion au `sshd` du guest (le tunnel `portforward` en
/// production, une socket TCP dans les tests). Une erreur est un echec du
/// CANAL (connexion, authentification) : la commande, elle, ne « rate »
/// jamais cette fonction, son echec est un [`RunOutcome`].
pub async fn run_over<S>(
    stream: S,
    user: &str,
    private_key_pem: &str,
    request: &RunRequest,
    default_dir: &str,
) -> anyhow::Result<RunOutcome>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let key = decode_secret_key(private_key_pem, None)
        .map_err(|err| anyhow::anyhow!("cle SSH privee invalide: {err}"))?;
    let config = Arc::new(client::Config::default());
    let mut session = client::connect_stream(config, stream, AcceptAnyHostKey)
        .await
        .map_err(|err| anyhow::anyhow!("connexion SSH echouee: {err}"))?;
    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .map_err(|err| anyhow::anyhow!("negociation SSH echouee: {err}"))?
        .flatten();
    let auth = session
        .authenticate_publickey(user, PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg))
        .await
        .map_err(|err| anyhow::anyhow!("authentification SSH echouee: {err}"))?;
    if !auth.success() {
        anyhow::bail!("authentification SSH refusee (cle non autorisee cote guest ?)");
    }

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|err| anyhow::anyhow!("ouverture du canal SSH echouee: {err}"))?;
    let started = Instant::now();
    channel
        .exec(true, request.script(default_dir))
        .await
        .map_err(|err| anyhow::anyhow!("exec SSH echoue: {err}"))?;
    // Entree standard fermee des le depart (le script la redirige aussi
    // vers /dev/null : ceinture et bretelles, `sshd` et le shell de session
    // ne reagissent pas tous de la meme facon a un EOF de canal).
    let _ = channel.eof().await;

    let mut outcome = RunOutcome::default();
    // Le guest arrete la commande lui-meme au delai ; ce delai-ci ne sert
    // que si le guest ne repond plus du tout.
    let mut deadline = started + request.timeout + KILL_GRACE + SERVER_MARGIN;
    let mut exited = false;
    let mut abandoned = false;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, channel.wait()).await {
            // Delai du serveur, ou fin de l'attente des derniers octets.
            Err(_) => {
                abandoned = !exited;
                break;
            }
            // Canal ferme : tout a ete recu.
            Ok(None) | Ok(Some(ChannelMsg::Close)) => break,
            Ok(Some(ChannelMsg::Data { data })) => {
                keep(
                    &mut outcome.stdout,
                    &mut outcome.stdout_truncated,
                    &data,
                    request.max_output,
                );
            }
            Ok(Some(ChannelMsg::ExtendedData { data, ext: 1 })) => {
                keep(
                    &mut outcome.stderr,
                    &mut outcome.stderr_truncated,
                    &data,
                    request.max_output,
                );
            }
            Ok(Some(ChannelMsg::ExitStatus { exit_status })) => {
                outcome.exit_code = Some(exit_status as i32);
                exited = true;
                deadline = Instant::now() + DRAIN_AFTER_EXIT;
            }
            Ok(Some(ChannelMsg::ExitSignal { signal_name, .. })) => {
                outcome.signal = Some(match signal_name {
                    russh::Sig::Custom(name) => name,
                    known => format!("{known:?}"),
                });
                exited = true;
                deadline = Instant::now() + DRAIN_AFTER_EXIT;
            }
            Ok(Some(_)) => {}
        }
    }
    outcome.duration = started.elapsed();
    let _ = channel.close().await;
    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;

    // `timeout` rend 124 quand il a du arreter la commande, et 137 quand
    // `-k` a du la tuer (constate contre un vrai `sshd` : il ne meurt pas
    // lui-meme du signal). Une commande peut rendre ces codes d'elle-meme :
    // ce n'est un depassement que si le delai est ecoule, et passe ce delai
    // elle ne tournait de toute facon plus.
    let stopped_by_timeout = outcome.duration >= request.timeout
        && (matches!(outcome.exit_code, Some(124 | 137))
            || outcome.signal.as_deref() == Some("KILL"));
    if abandoned || stopped_by_timeout {
        outcome.timed_out = true;
        outcome.exit_code = None;
        outcome.signal = None;
    }
    Ok(outcome)
}

/// Reponse de `POST /v1/workshops/{name}/exec`.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResponse {
    /// Code de sortie, ou `null` (signal, delai depasse).
    exit_code: Option<i32>,
    /// Nom du signal qui a termine la commande, ou `null`.
    signal: Option<String>,
    timed_out: bool,
    /// Sortie standard, en base64 : les octets tels que la commande les a
    /// ecrits, y compris ceux qui ne forment pas du texte.
    stdout_base64: String,
    stderr_base64: String,
    stdout_truncated: bool,
    stderr_truncated: bool,
    duration_ms: u64,
}

impl From<RunOutcome> for RunResponse {
    fn from(outcome: RunOutcome) -> Self {
        let encode = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        Self {
            exit_code: outcome.exit_code,
            signal: outcome.signal,
            timed_out: outcome.timed_out,
            stdout_base64: encode(&outcome.stdout),
            stderr_base64: encode(&outcome.stderr),
            stdout_truncated: outcome.stdout_truncated,
            stderr_truncated: outcome.stderr_truncated,
            duration_ms: outcome.duration.as_millis() as u64,
        }
    }
}

/// `POST /v1/workshops/{name}/exec` : lance la commande dans le Workshop et
/// rend son resultat. Voir la doc de tete de ce module.
///
/// Statuts : `400` requete invalide ou Workshop sans pod actif, `404`
/// Workshop introuvable ou d'un autre groupe, `502` guest injoignable par
/// SSH, `503` cle SSH indisponible. Une commande qui echoue, qui depasse
/// son delai ou qu'un signal termine est une reponse `200` : c'est un
/// resultat, pas une erreur de l'API.
pub async fn run_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    Json(body): Json<RunBody>,
) -> Result<Json<RunResponse>, ApiError> {
    let request = RunRequest::new(
        body.command,
        body.working_dir,
        body.env,
        body.timeout_seconds,
        body.max_output_bytes,
    )
    .map_err(|message| ApiError::bad_request(&message))?;

    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    let pod_ip = resolve_running_pod_ip(&state, &workshop).await?;
    let session_auth = state.session_auth.as_ref().ok_or_else(|| {
        ApiError::service_unavailable(
            "OPENBAO_ADDR non configure : exec indisponible (aucune cle SSH accessible)",
        )
    })?;
    let private_key = session_auth.ssh_private_key(&name).await.ok_or_else(|| {
        ApiError::service_unavailable(
            "cle SSH indisponible pour ce Workshop (pas encore provisionnee, ou OpenBao injoignable)",
        )
    })?;

    let ssh_port = crate::exec::ssh_port(&workshop);
    let stream = crate::vscode::open_forwarded_tcp_stream(&pod_ip, ssh_port)
        .await
        .map_err(|err| {
            tracing::warn!(%err, pod_ip, ssh_port, "exec: guest injoignable (portforward)");
            ApiError::bad_gateway(format!("connexion au guest impossible: {err}"))
        })?;
    let default_dir = crate::exec::workspace_dir(&workshop.spec.devcontainer.repo);
    let outcome = run_over(
        stream,
        crate::exec::SSH_USER,
        &private_key,
        &request,
        &default_dir,
    )
    .await
    .map_err(|err| {
        tracing::warn!(%err, workshop = %name, "exec: canal SSH en echec");
        ApiError::bad_gateway(format!("{err}"))
    })?;

    // Trace d'audit, sans la sortie : qui a lance quoi, et comment cela
    // s'est termine. Le locataire est le groupe du Workshop, comme pour
    // `exec_in_workshop`.
    tracing::info!(
        workshop = %name,
        tenant = %workshop_tenant(&workshop),
        subject = %user.subject,
        exit_code = ?outcome.exit_code,
        signal = ?outcome.signal,
        timed_out = outcome.timed_out,
        duration_ms = outcome.duration.as_millis() as u64,
        "exec"
    );
    Ok(Json(outcome.into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(command: &str) -> RunRequest {
        RunRequest::new(command.to_string(), None, BTreeMap::new(), None, None).unwrap()
    }

    #[test]
    fn defaults_and_ceilings_bound_every_call() {
        let plain = request("true");
        assert_eq!(plain.timeout, Duration::from_secs(60));
        assert_eq!(plain.max_output, 1024 * 1024);

        let greedy = RunRequest::new(
            "true".into(),
            None,
            BTreeMap::new(),
            Some(u64::MAX),
            Some(usize::MAX),
        )
        .unwrap();
        assert_eq!(greedy.timeout, Duration::from_secs(timeout_ceiling_secs()));
        assert_eq!(greedy.max_output, MAX_OUTPUT_BYTES);
    }

    #[test]
    fn invalid_requests_are_named() {
        let new = |command: &str, dir: Option<&str>, env: &[(&str, &str)], timeout| {
            RunRequest::new(
                command.to_string(),
                dir.map(str::to_string),
                env.iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect(),
                timeout,
                None,
            )
        };
        assert!(new("  ", None, &[], None).unwrap_err().contains("vide"));
        assert!(new("a\0b", None, &[], None).is_err());
        assert!(new("true", Some(""), &[], None).is_err());
        assert!(new("true", None, &[], Some(0)).is_err());
        for name in ["", "1A", "A-B", "A B", "A=B", "A;id", "É"] {
            let err = new("true", None, &[(name, "x")], None).unwrap_err();
            assert!(err.contains("nom de variable"), "{name:?}: {err}");
        }
        assert!(new("true", None, &[("A", "x\0")], None).is_err());
        assert!(new("true", None, &[("_A1", "x")], None).is_ok());
    }

    #[test]
    fn the_script_quotes_everything_the_caller_gave() {
        let mut env = BTreeMap::new();
        env.insert("NOM".to_string(), "l'été; rm -rf ~".to_string());
        let request = RunRequest::new(
            "echo 'a' \"$NOM\"".to_string(),
            Some("/tmp/x'; touch pirate; '".to_string()),
            env,
            Some(30),
            None,
        )
        .unwrap();
        assert_eq!(
            request.script("/workspaces/repo"),
            "cd -- '/tmp/x'\\''; touch pirate; '\\''' || exit 126\n\
             export NOM='l'\\''été; rm -rf ~'\n\
             exec timeout -k 5 30 sh -c 'echo '\\''a'\\'' \"$NOM\"' </dev/null\n"
        );
    }

    #[test]
    fn without_a_working_dir_the_workspace_is_tried_not_required() {
        assert_eq!(
            request("pwd").script("/workspaces/repo"),
            "cd -- '/workspaces/repo' 2>/dev/null || true\n\
             exec timeout -k 5 60 sh -c 'pwd' </dev/null\n"
        );
    }

    #[test]
    fn keep_stops_at_the_limit_and_says_so() {
        let (mut buffer, mut truncated) = (Vec::new(), false);
        keep(&mut buffer, &mut truncated, b"abc", 5);
        assert_eq!((buffer.as_slice(), truncated), (&b"abc"[..], false));
        keep(&mut buffer, &mut truncated, b"defg", 5);
        assert_eq!((buffer.as_slice(), truncated), (&b"abcde"[..], true));
        keep(&mut buffer, &mut truncated, b"h", 5);
        assert_eq!(buffer, b"abcde");
        // Pile a la limite : rien n'est perdu, rien n'est signale.
        let (mut exact, mut cut) = (Vec::new(), false);
        keep(&mut exact, &mut cut, b"12345", 5);
        assert!(!cut);
    }

    #[test]
    fn the_response_carries_bytes_untouched() {
        let outcome = RunOutcome {
            exit_code: Some(0),
            stdout: vec![0, 255, b'a', 0],
            ..Default::default()
        };
        let json = serde_json::to_value(RunResponse::from(outcome)).unwrap();
        assert_eq!(json["stdoutBase64"], "AP9hAA==");
        assert_eq!(json["stderrBase64"], "");
        assert_eq!(json["exitCode"], 0);
        assert!(json["signal"].is_null());
        assert_eq!(json["timedOut"], false);
    }
}
