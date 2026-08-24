//! Pont HTTP+WebSocket vers `code-server` (port 8080 dans la microVM
//! agent), au-dessus du protocole `portforward` existant
//! (`crate::portforward`, `crates/net-proxy/src/portforward.rs`) — ce
//! dernier ne transporte que des octets bruts TCP/UDP multiplexes, pas
//! HTTP/WebSocket, donc pas directement navigable par un navigateur.
//!
//! `code-server` supporte nativement d'etre servi sous un sous-chemin
//! arbitraire (documente officiellement par `coder/code-server` : URLs
//! relatives des lors que le prefixe est retire avant de l'atteindre, meme
//! convention que leurs exemples Caddy/nginx) — ce module retire donc
//! `/v1/workshops/{name}/vscode` avant de relayer, sans avoir besoin de
//! sous-domaine ni de reecriture de HTML/JS.
//!
//! Le dashboard (Next.js) ne fait qu'un reverse-proxy same-origin fin
//! au-dessus de cet endpoint (ajoute le `Authorization: Bearer` cote
//! serveur) : toute la traduction de protocole vit ici.

use crate::auth::AuthenticatedUser;
use crate::routes::{ensure_owner, resolve_running_pod_ip, workshops_api, ApiError, AppState};
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{self, Request, StatusCode};
use axum::response::Response;
use base64::Engine;
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message as TsMessage;

/// Port sur lequel `code-server` ecoute dans la microVM agent (voir
/// github.com/PhilippeVienne/atelier-workspace `.devcontainer/atelier-code-server.service`) —
/// convention fixe par Workshop pour ce lot (pas encore configurable dans
/// le CRD). `ATELIER_VSCODE_PORT` reste overridable pour les tests
/// (eviter un conflit avec un vrai port 8080 deja occupe sur la machine).
fn code_server_port() -> u16 {
    std::env::var("ATELIER_VSCODE_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(8080)
}

pub async fn vscode_proxy_root(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    req: Request<Body>,
) -> Result<Response, ApiError> {
    proxy_to_guest_port(
        state,
        user,
        GuestProxyTarget {
            name,
            path: String::new(),
            port: code_server_port(),
            url_prefix: "vscode",
            record_session: false,
        },
        req,
    )
    .await
}

pub async fn vscode_proxy(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path((name, path)): Path<(String, String)>,
    req: Request<Body>,
) -> Result<Response, ApiError> {
    proxy_to_guest_port(
        state,
        user,
        GuestProxyTarget {
            name,
            path,
            port: code_server_port(),
            url_prefix: "vscode",
            record_session: false,
        },
        req,
    )
    .await
}

/// Parametres d'un pont HTTP+WebSocket vers un port de la microVM agent
/// (voir [`proxy_to_guest_port`]) : regroupes en struct plutot qu'en
/// arguments positionnels separes (au-dela de 7, `clippy::too_many_arguments`
/// se declenche, et la confusion entre plusieurs `String`/`u16`/`bool`
/// positionnels devient reelle).
pub(crate) struct GuestProxyTarget {
    pub name: String,
    pub path: String,
    pub port: u16,
    pub url_prefix: &'static str,
    /// Si vrai, la sortie du tunnel (direction serveur->client) est
    /// dupliquee vers `crate::session_recorder` et archivee sur S3 (voir ce
    /// module) — seul `crate::terminal` l'active, jamais `code-server`.
    pub record_session: bool,
}

/// Pont HTTP+WebSocket generique vers un port de la microVM agent, reutilise
/// pour tous les services embarques dans le devcontainer (`code-server` sur
/// `code_server_port()`, terminal `ttyd` sur `crate::terminal::terminal_port()`,
/// voir `crate::terminal`) : meme mecanisme de bout en bout (portforward ->
/// duplex -> hyper client avec upgrades), seul le port cible et le prefixe
/// d'URL a retirer/reecrire (voir `Location` plus bas) changent.
pub(crate) async fn proxy_to_guest_port(
    state: AppState,
    user: AuthenticatedUser,
    target: GuestProxyTarget,
    mut req: Request<Body>,
) -> Result<Response, ApiError> {
    let GuestProxyTarget {
        name,
        path,
        port,
        url_prefix,
        record_session,
    } = target;
    tracing::debug!(name = %name, path = %path, port, user = %user.0, "proxy_to_guest_port appele");
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    let pod_ip = resolve_running_pod_ip(&state, &workshop).await?;

    let stream = open_forwarded_tcp_stream(&pod_ip, port)
        .await
        .map_err(|err| {
            tracing::warn!(%err, pod_ip, port, "connexion au guest impossible (portforward)");
            ApiError::bad_gateway(format!("connexion au guest impossible: {err}"))
        })?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|err| {
            tracing::warn!(%err, "handshake HTTP avec le guest echoue");
            ApiError::bad_gateway(format!("handshake HTTP avec le guest echoue: {err}"))
        })?;
    tokio::spawn(async move {
        if let Err(err) = conn.with_upgrades().await {
            tracing::warn!(%err, "connexion hyper vers le guest terminee en erreur");
        }
    });

    let is_upgrade = req.headers().get(http::header::UPGRADE).is_some();
    let server_upgrade = is_upgrade.then(|| hyper::upgrade::on(&mut req));

    let new_path_and_query = match req.uri().query() {
        Some(q) => format!("/{path}?{q}"),
        None => format!("/{path}"),
    };
    let (mut parts, body) = req.into_parts();
    parts.uri = new_path_and_query
        .parse()
        .map_err(|_| ApiError::bad_request("chemin invalide"))?;
    // Codee en dur sur le port de code-server (127.0.0.1:8080) avant cette
    // correction : fonctionnait par coincidence pour `code-server`, mais
    // cassait silencieusement le pont vers `ttyd` (port 7681) — l'en-tete
    // doit refleter le port reellement cible, pas un seul des deux
    // services embarques.
    parts.headers.insert(
        http::header::HOST,
        http::HeaderValue::from_str(&format!("127.0.0.1:{port}"))
            .map_err(|_| ApiError::bad_request("port invalide"))?,
    );
    if !is_upgrade {
        parts.headers.remove(http::header::CONNECTION);
    }
    // Mot de passe de session provisionne par le controller
    // (`crates/controller/src/openbao.rs::ensure_session_auth`), injecte
    // ici plutot que laisse au client : `code-server`/`ttyd` exigent tous
    // les deux ce Basic Auth (voir `crates/net-proxy/src/metadata.rs`), et
    // un client externe ne doit jamais avoir besoin de le connaitre — seul
    // `api-server` (via son role OpenBao cluster-wide `atelier-api-server`,
    // voir `crate::session_auth`) le lit. Remplace un eventuel `Authorization`
    // du client (son JWT `Bearer`, deja verifie par `require_auth` pour
    // atteindre ce handler) : ce n'est de toute facon pas ce que le guest
    // attend. Si le secret est absent/OpenBao non configure, on relaie tel
    // quel (comportement degrade, pas d'erreur bloquante).
    if let Some(session_auth) = &state.session_auth {
        if let Some(password) = session_auth.session_password(&name).await {
            let credentials =
                base64::engine::general_purpose::STANDARD.encode(format!("atelier:{password}"));
            match http::HeaderValue::from_str(&format!("Basic {credentials}")) {
                Ok(value) => {
                    parts.headers.insert(http::header::AUTHORIZATION, value);
                }
                Err(err) => {
                    tracing::warn!(%err, "en-tete Authorization Basic invalide, requete relayee sans injection");
                }
            }
        }
    }
    let outbound = Request::from_parts(parts, body);

    let mut upstream_response = sender.send_request(outbound).await.map_err(|err| {
        tracing::warn!(%err, "requete vers le guest echouee (send_request)");
        ApiError::bad_gateway(format!("requete vers le guest echouee: {err}"))
    })?;

    if is_upgrade && upstream_response.status() == StatusCode::SWITCHING_PROTOCOLS {
        let client_upgrade = hyper::upgrade::on(&mut upstream_response);
        let server_upgrade = server_upgrade.expect("capture avant consommation de req");

        let mut reply = Response::builder().status(StatusCode::SWITCHING_PROTOCOLS);
        for (name, value) in upstream_response.headers() {
            reply = reply.header(name, value);
        }
        let reply = reply
            .body(Body::empty())
            .map_err(|err| ApiError::bad_gateway(format!("reponse d'upgrade invalide: {err}")))?;

        let recording = if record_session {
            state
                .storage
                .clone()
                .map(|storage| crate::session_recorder::SessionRecording::start(storage, name))
        } else {
            None
        };
        tokio::spawn(async move {
            match (server_upgrade.await, client_upgrade.await) {
                (Ok(server_io), Ok(client_io)) => {
                    let mut server_io = TokioIo::new(server_io);
                    let mut client_io = TokioIo::new(client_io);
                    if let Err(err) =
                        copy_bidirectional_with_recording(&mut server_io, &mut client_io, recording)
                            .await
                    {
                        tracing::debug!(%err, "tunnel websocket ferme");
                    }
                }
                _ => tracing::warn!("upgrade websocket vers le guest echoue"),
            }
        });
        return Ok(reply);
    }

    let (parts, incoming_body) = upstream_response.into_parts();
    let mut builder = Response::builder().status(parts.status);
    let prefix = format!("/v1/workshops/{name}/{url_prefix}");
    for (hdr_name, value) in parts.headers.iter() {
        if hdr_name == http::header::CONNECTION || hdr_name == http::header::TRANSFER_ENCODING {
            continue;
        }
        if hdr_name == http::header::LOCATION {
            if let Ok(loc) = value.to_str() {
                if loc.starts_with('/') && !loc.starts_with(&prefix) {
                    let rewritten = format!("{prefix}{loc}");
                    builder = builder.header(hdr_name, rewritten);
                    continue;
                }
            }
        }
        builder = builder.header(hdr_name, value);
    }
    builder
        .body(Body::new(incoming_body))
        .map_err(|err| ApiError::bad_gateway(format!("reponse de code-server invalide: {err}")))
}

/// Equivalent de `tokio::io::copy_bidirectional`, mais capable de dupliquer
/// (« tee ») le flux serveur->client vers un [`crate::session_recorder::SessionRecording`]
/// au fur et a mesure, sans jamais bufferiser la session entiere. `recording`
/// vaut `None` pour tous les tunnels qui ne sont pas enregistres
/// (`code-server`, ou `ttyd` sans backend S3 configure) : dans ce cas le
/// comportement est strictement identique a `copy_bidirectional`. Seule la
/// direction serveur->client (la sortie affichee du terminal) est
/// enregistree, jamais la saisie utilisateur (voir `crate::session_recorder`).
async fn copy_bidirectional_with_recording(
    server_io: &mut TokioIo<hyper::upgrade::Upgraded>,
    client_io: &mut TokioIo<hyper::upgrade::Upgraded>,
    mut recording: Option<crate::session_recorder::SessionRecording>,
) -> std::io::Result<()> {
    let mut server_to_client = [0u8; 16 * 1024];
    let mut client_to_server = [0u8; 16 * 1024];
    loop {
        tokio::select! {
            n = server_io.read(&mut server_to_client) => {
                let n = n?;
                if n == 0 {
                    break;
                }
                if let Some(recording) = recording.as_mut() {
                    recording.write_chunk(&server_to_client[..n]).await;
                }
                client_io.write_all(&server_to_client[..n]).await?;
            }
            n = client_io.read(&mut client_to_server) => {
                let n = n?;
                if n == 0 {
                    break;
                }
                server_io.write_all(&client_to_server[..n]).await?;
            }
        }
    }
    if let Some(recording) = recording {
        recording.finish().await;
    }
    Ok(())
}

/// Ouvre une connexion TCP "virtuelle" vers `(pod_ip, remote_port)` a
/// travers le protocole `portforward` de `net-proxy` (voir commentaire de
/// module de `crates/net-proxy/src/portforward.rs` pour le detail du
/// multiplexage par canal) et l'expose comme un flux d'octets ordinaire —
/// pas besoin de reimplementer `Sink`/`Stream` a la main : une tache de
/// fond pompe entre le websocket et une moitie de `tokio::io::duplex`,
/// l'autre moitie est rendue a l'appelant.
pub(crate) async fn open_forwarded_tcp_stream(
    pod_ip: &str,
    remote_port: u16,
) -> anyhow::Result<impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static> {
    let target_url = format!(
        "ws://{pod_ip}:{}/portforward?ports=tcp:{remote_port}",
        crate::portforward::net_proxy_control_port()
    );
    let (ws, _response) = tokio_tungstenite::connect_async(&target_url).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (local, mut remote) = tokio::io::duplex(64 * 1024);

    tokio::spawn(async move {
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            tokio::select! {
                read = remote.read(&mut buf) => {
                    match read {
                        Ok(0) => {
                            let _ = ws_tx.close().await;
                            break;
                        }
                        Ok(n) => {
                            let mut msg = Vec::with_capacity(n + 1);
                            msg.push(0u8); // canal 0 = donnees, seul canal demande ici
                            msg.extend_from_slice(&buf[..n]);
                            if ws_tx.send(TsMessage::Binary(msg.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(err) => {
                            tracing::debug!(%err, "lecture locale du pont port-forward terminee");
                            break;
                        }
                    }
                }
                msg = ws_rx.next() => {
                    match msg {
                        Some(Ok(TsMessage::Binary(data))) if !data.is_empty() => {
                            match data[0] {
                                0 => {
                                    if remote.write_all(&data[1..]).await.is_err() {
                                        break;
                                    }
                                }
                                1 => {
                                    tracing::warn!(
                                        error = %String::from_utf8_lossy(&data[1..]),
                                        "erreur distante rapportee par net-proxy (port-forward)"
                                    );
                                    break;
                                }
                                _ => {}
                            }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => {
                            tracing::debug!(%err, "websocket port-forward (net-proxy) ferme en erreur");
                            break;
                        }
                        None => break,
                    }
                }
            }
        }
    });

    Ok(local)
}
