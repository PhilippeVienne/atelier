//! `exec_in_workshop` (Jalon M4, tache 4.2.3) : execution asynchrone et
//! bufferisee d'une commande dans le guest, decouplee de la connexion du
//! client MCP qui l'a demandee.
//!
//! Canal : SSH (cle Ed25519 par Workshop, voir
//! `crates/controller/src/openbao.rs::ensure_ssh_key` et le depot
//! `atelier-workspace`), atteint via le meme tunnel `portforward` que
//! `crate::vscode`/`crate::terminal`
//! (`crate::vscode::open_forwarded_tcp_stream`) — pas de connexion TCP
//! directe au pod, tout passe deja par `net-proxy`.
//!
//! La commande est enregistree dans `exec_commands` (PostgreSQL, RLS par
//! `owner_subject`) AVANT de retourner : l'appelant recoit un
//! `execution_id` immediatement, l'execution continue en arriere-plan
//! (`tokio::spawn`) meme si le client se deconnecte. `stdout`/`stderr` sont
//! appendes chunk par chunk (jamais bufferises entierement en memoire cote
//! serveur) — voir `GET /v1/workshops/{name}/exec/{id}/stream` pour la
//! reconnexion (`crate::routes`).
//!
//! Pas de verification de la cle hote SSH du guest (`check_server_key`
//! renvoie toujours `Ok(true)`) : ce canal transite deja exclusivement par
//! le tunnel `portforward` interne au pod (`net-proxy`), adresse par IP de
//! pod Kubernetes — un attaquant capable de le detourner controlerait deja
//! le netns du pod, ce qui rend une verification de cle hote SSH
//! supplementaire sans valeur de securite reelle ici (meme raisonnement
//! implicite que le reste des tunnels `crate::vscode`/`crate::terminal`,
//! qui ne font pas non plus de verification d'identite du guest au-dela de
//! l'adressage par IP de pod).

use crate::auth::AuthenticatedUser;
use crate::routes::{ensure_owner, workshops_api, ApiError, AppState};
use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use axum::response::IntoResponse;
use axum::Extension;
use futures_util::stream;
use russh::client;
use russh::keys::{decode_secret_key, PrivateKeyWithHashAlg};
use russh::ChannelMsg;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

/// Port sur lequel `sshd` ecoute dans la microVM agent (voir
/// github.com/PhilippeVienne/atelier-workspace `.devcontainer/atelier-sshd.conf`)
/// — canal separe de `ttyd`/`code-server`, dedie a `exec_in_workshop`.
fn ssh_port() -> u16 {
    std::env::var("ATELIER_SSH_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(22)
}

/// Utilisateur systeme du devcontainer (voir
/// `atelier-fetch-ssh-authorized-key.sh` : `authorized_keys` de
/// `~vscode/.ssh/`) — meme convention fixe que `code_server_port()`/
/// `terminal_port()`, pas encore configurable dans le CRD.
const SSH_USER: &str = "vscode";

struct ExecClientHandler;

impl client::Handler for ExecClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &russh::keys::PublicKeyOrCertificate,
    ) -> Result<bool, Self::Error> {
        Ok(true)
    }
}

/// Enregistre la commande (statut `Running`) et lance son execution en
/// arriere-plan ; renvoie l'`execution_id` immediatement (avant meme que la
/// connexion SSH ne soit etablie).
pub async fn spawn(
    state: AppState,
    owner_subject: String,
    workshop_name: String,
    pod_ip: String,
    private_key_pem: String,
    command: String,
    devcontainer_repo: String,
) -> Result<Uuid, sqlx::Error> {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exec_commands (owner_subject, workshop_name, command) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(&owner_subject)
    .bind(&workshop_name)
    .bind(&command)
    .fetch_one(&state.db_pool)
    .await?;

    tokio::spawn(run_and_persist(
        state,
        id,
        owner_subject,
        pod_ip,
        private_key_pem,
        command,
        workspace_dir(&devcontainer_repo),
    ));

    Ok(id)
}

async fn run_and_persist(
    state: AppState,
    id: Uuid,
    owner_subject: String,
    pod_ip: String,
    private_key_pem: String,
    command: String,
    workspace_dir: String,
) {
    let outcome = run_over_ssh(
        &state,
        id,
        &owner_subject,
        &pod_ip,
        &private_key_pem,
        &command,
        &workspace_dir,
    )
    .await;
    match outcome {
        Ok(exit_code) => {
            finalize(&state.db_pool, id, &owner_subject, "Completed", exit_code).await;
        }
        Err(err) => {
            tracing::warn!(%err, %id, workshop = %pod_ip, "exec_in_workshop echoue");
            append_chunk(
                &state.db_pool,
                id,
                &owner_subject,
                OutputStream::Stderr,
                format!("\n[atelier] execution echouee: {err}\n").as_bytes(),
            )
            .await;
            finalize(&state.db_pool, id, &owner_subject, "Failed", None).await;
        }
    }
}

/// Prefixe la commande d'un `cd` vers le workspace du Workshop.
///
/// Une commande SSH non interactive demarre dans le repertoire personnel de
/// l'utilisateur (`/home/vscode`), pas dans les sources — alors que
/// « executer dans le Workshop » veut evidemment dire « executer sur le
/// projet ». Sans ce prefixe, tout appelant devait le savoir et prefixer
/// lui-meme, ce que ni `RunDevcontainerTests` (qui lance
/// `bash .devcontainer/test.sh`) ni `DelegateToClaudeCode` ne faisaient :
/// les tests echouaient sur "No such file or directory", et Claude Code
/// tournait hors du depot, sans fichier a modifier ni rien a commiter — le
/// PM ouvrait donc des PR vides. Bug reel, trouve le 2026-08-30 apres
/// plusieurs pistes erronees (le message d'erreur de Claude Code parlait du
/// modele, jamais du repertoire).
///
/// Le chemin est DERIVE du depot (meme regle que
/// `image-builder::ensure_workspace_clone`), jamais devine en listant
/// `/workspaces` : ce repertoire peut contenir plusieurs entrees, dont
/// celles creees par l'image de base du devcontainer — constate en pratique
/// (`atelier-workspace` present a cote de `todo-app`), ou une heuristique
/// "premier repertoire trouve" tombait sur le mauvais. Le `|| true`
/// preserve le comportement anterieur si le workspace est absent : mieux
/// vaut executer depuis le repertoire personnel que refuser la commande.
pub(crate) fn workspace_dir(devcontainer_repo: &str) -> String {
    let name = devcontainer_repo
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("workspace")
        .trim_end_matches(".git");
    format!("/workspaces/{name}")
}

fn in_workspace(workspace_dir: &str, command: &str) -> String {
    format!("cd {workspace_dir} 2>/dev/null || true; {command}")
}

async fn run_over_ssh(
    state: &AppState,
    id: Uuid,
    owner_subject: &str,
    pod_ip: &str,
    private_key_pem: &str,
    command: &str,
    workspace_dir: &str,
) -> anyhow::Result<Option<i32>> {
    let stream = crate::vscode::open_forwarded_tcp_stream(pod_ip, ssh_port()).await?;
    let key = decode_secret_key(private_key_pem, None)
        .map_err(|err| anyhow::anyhow!("cle SSH privee invalide: {err}"))?;

    let config = Arc::new(client::Config::default());
    let mut session = client::connect_stream(config, stream, ExecClientHandler)
        .await
        .map_err(|err| anyhow::anyhow!("connexion SSH echouee: {err}"))?;

    let hash_alg = session
        .best_supported_rsa_hash()
        .await
        .map_err(|err| anyhow::anyhow!("negociation SSH echouee: {err}"))?
        .flatten();
    let auth = session
        .authenticate_publickey(
            SSH_USER,
            PrivateKeyWithHashAlg::new(Arc::new(key), hash_alg),
        )
        .await
        .map_err(|err| anyhow::anyhow!("authentification SSH echouee: {err}"))?;
    if !auth.success() {
        anyhow::bail!("authentification SSH refusee (cle non autorisee cote guest ?)");
    }

    let mut channel = session
        .channel_open_session()
        .await
        .map_err(|err| anyhow::anyhow!("ouverture du canal SSH echouee: {err}"))?;
    channel
        .exec(true, in_workspace(workspace_dir, command))
        .await
        .map_err(|err| anyhow::anyhow!("exec SSH echoue: {err}"))?;

    let mut exit_code = None;
    while let Some(msg) = channel.wait().await {
        match msg {
            ChannelMsg::Data { data } => {
                append_chunk(
                    &state.db_pool,
                    id,
                    owner_subject,
                    OutputStream::Stdout,
                    &data,
                )
                .await;
            }
            ChannelMsg::ExtendedData { data, ext: 1 } => {
                append_chunk(
                    &state.db_pool,
                    id,
                    owner_subject,
                    OutputStream::Stderr,
                    &data,
                )
                .await;
            }
            ChannelMsg::ExitStatus { exit_status } => {
                exit_code = Some(exit_status as i32);
            }
            _ => {}
        }
    }

    let _ = session
        .disconnect(russh::Disconnect::ByApplication, "", "en")
        .await;
    Ok(exit_code)
}

enum OutputStream {
    Stdout,
    Stderr,
}

/// `SET LOCAL app.current_tenant` (via `set_config(..., true)`, portee
/// transaction) avant chaque ecriture : meme politique RLS que
/// `session_logs`/`audit_events`/`exec_commands` (voir la migration
/// `20260824000001_mcp_exec_commands.sql`), y compris depuis cette tache de
/// fond qui n'a plus de contexte de requete HTTP. Best-effort : une
/// ecriture qui echoue (connexion Postgres temporairement indisponible)
/// n'interrompt jamais l'execution SSH elle-meme, seul le buffer stocke est
/// incomplet pour ce chunk.
async fn append_chunk(
    pool: &sqlx::PgPool,
    id: Uuid,
    owner_subject: &str,
    stream: OutputStream,
    data: &[u8],
) {
    if data.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(data);
    let Ok(mut tx) = pool.begin().await else {
        return;
    };
    if sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(owner_subject)
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return;
    }
    let query = match stream {
        OutputStream::Stdout => {
            "UPDATE exec_commands SET stdout_buffer = stdout_buffer || $1, updated_at = now() WHERE id = $2"
        }
        OutputStream::Stderr => {
            "UPDATE exec_commands SET stderr_buffer = stderr_buffer || $1, updated_at = now() WHERE id = $2"
        }
    };
    let _ = sqlx::query(query)
        .bind(text.as_ref())
        .bind(id)
        .execute(&mut *tx)
        .await;
    let _ = tx.commit().await;
}

async fn finalize(
    pool: &sqlx::PgPool,
    id: Uuid,
    owner_subject: &str,
    status: &str,
    exit_code: Option<i32>,
) {
    let Ok(mut tx) = pool.begin().await else {
        return;
    };
    if sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(owner_subject)
        .execute(&mut *tx)
        .await
        .is_err()
    {
        return;
    }
    let _ = sqlx::query(
        "UPDATE exec_commands SET status = $1, exit_code = $2, updated_at = now() WHERE id = $3",
    )
    .bind(status)
    .bind(exit_code)
    .bind(id)
    .execute(&mut *tx)
    .await;
    let _ = tx.commit().await;
}

const POLL_INTERVAL: Duration = Duration::from_millis(300);

#[derive(sqlx::FromRow)]
struct ExecRow {
    status: String,
    exit_code: Option<i32>,
    stdout_buffer: String,
    stderr_buffer: String,
}

/// `GET /v1/workshops/{name}/exec/{id}/stream` (tache 4.2.3) : reconnexion
/// possible a tout moment (avant, pendant, ou apres la fin de l'execution)
/// — relit le buffer complet accumule dans PostgreSQL depuis le debut a
/// chaque (re)connexion (pas seulement les octets "manques"), puis continue
/// a streamer les nouveaux chunks au fur et a mesure (sondage de la ligne
/// toutes les [`POLL_INTERVAL`], plus simple et tout aussi correct qu'un
/// canal en memoire pour un usage de reconnexion peu frequent). Se termine
/// (fin du flux SSE) une fois `status != "Running"`, apres un dernier
/// evenement `status` portant `exitCode`.
pub async fn stream_handler(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path((name, id)): Path<(String, Uuid)>,
) -> Result<impl IntoResponse, ApiError> {
    // Verifie la propriete du Workshop (source de verite : le CRD, comme le
    // reste de l'API) avant meme d'ouvrir la connexion PostgreSQL — RLS
    // (`app.current_tenant`) reste la deuxieme barriere, pas la seule.
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;

    let stream = stream::unfold(
        (state, user.0, name, id, 0usize, 0usize, false),
        |(state, owner_subject, name, id, mut stdout_sent, mut stderr_sent, done)| async move {
            if done {
                return None;
            }
            let row = fetch_row(&state.db_pool, &owner_subject, &name, id).await;
            let Some(row) = row else {
                let event: Result<Event, Infallible> = Ok(Event::default()
                    .event("error")
                    .data("execution introuvable"));
                return Some((
                    event,
                    (
                        state,
                        owner_subject,
                        name,
                        id,
                        stdout_sent,
                        stderr_sent,
                        true,
                    ),
                ));
            };

            let stdout_new = row
                .stdout_buffer
                .get(stdout_sent..)
                .unwrap_or_default()
                .to_string();
            let stderr_new = row
                .stderr_buffer
                .get(stderr_sent..)
                .unwrap_or_default()
                .to_string();
            stdout_sent = row.stdout_buffer.len();
            stderr_sent = row.stderr_buffer.len();

            let finished = row.status != "Running";
            let event = if !stdout_new.is_empty() {
                Event::default().event("stdout").data(stdout_new)
            } else if !stderr_new.is_empty() {
                Event::default().event("stderr").data(stderr_new)
            } else if finished {
                Event::default().event("status").data(
                    serde_json::json!({ "status": row.status, "exitCode": row.exit_code })
                        .to_string(),
                )
            } else {
                Event::default().event("ping").data("")
            };

            if !finished {
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Some((
                Ok(event),
                (
                    state,
                    owner_subject,
                    name,
                    id,
                    stdout_sent,
                    stderr_sent,
                    finished,
                ),
            ))
        },
    );

    Ok(Sse::new(stream))
}

async fn fetch_row(
    pool: &sqlx::PgPool,
    owner_subject: &str,
    workshop_name: &str,
    id: Uuid,
) -> Option<ExecRow> {
    let mut tx = pool.begin().await.ok()?;
    sqlx::query("SELECT set_config('app.current_tenant', $1, true)")
        .bind(owner_subject)
        .execute(&mut *tx)
        .await
        .ok()?;
    let row = sqlx::query_as::<_, ExecRow>(
        "SELECT status, exit_code, stdout_buffer, stderr_buffer FROM exec_commands WHERE id = $1 AND workshop_name = $2",
    )
    .bind(id)
    .bind(workshop_name)
    .fetch_optional(&mut *tx)
    .await
    .ok()?;
    row
}

#[cfg(test)]
mod workspace_tests {
    use super::{in_workspace, workspace_dir};

    /// Regression (2026-08-30) : une commande SSH non interactive demarre
    /// dans `/home/vscode`, pas dans les sources. `RunDevcontainerTests`
    /// echouait donc sur `bash .devcontainer/test.sh` ("No such file"), et
    /// Claude Code tournait hors du depot — d'ou des PR vides.
    #[test]
    fn derives_workspace_from_repo_url() {
        assert_eq!(
            workspace_dir("http://forge.internal/acme/todo-app.git"),
            "/workspaces/todo-app"
        );
        // Sans suffixe `.git`, et avec une barre finale.
        assert_eq!(
            workspace_dir("https://github.com/acme/widgets"),
            "/workspaces/widgets"
        );
        assert_eq!(
            workspace_dir("https://github.com/acme/widgets/"),
            "/workspaces/widgets"
        );
    }

    /// Le `cd` ne doit jamais faire echouer la commande : un workspace
    /// absent laisse simplement l'execution dans le repertoire personnel.
    #[test]
    fn prefixes_command_without_making_it_fail() {
        let wrapped = in_workspace("/workspaces/todo-app", "npm test");
        assert!(wrapped.starts_with("cd /workspaces/todo-app"));
        assert!(wrapped.contains("|| true"));
        assert!(wrapped.ends_with("npm test"));
    }
}
