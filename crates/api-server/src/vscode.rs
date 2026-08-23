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

pub async fn vscode_proxy(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path((name, path)): Path<(String, String)>,
    mut req: Request<Body>,
) -> Result<Response, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;
    let pod_ip = resolve_running_pod_ip(&state, &workshop).await?;

    let stream = open_forwarded_tcp_stream(&pod_ip, code_server_port())
        .await
        .map_err(|err| {
            ApiError::bad_gateway(format!("connexion a code-server impossible: {err}"))
        })?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|err| {
            ApiError::bad_gateway(format!("handshake HTTP avec code-server echoue: {err}"))
        })?;
    tokio::spawn(conn.with_upgrades());

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
    parts.headers.remove(http::header::HOST);
    if !is_upgrade {
        parts.headers.remove(http::header::CONNECTION);
    }
    let outbound = Request::from_parts(parts, body);

    let mut upstream_response = sender
        .send_request(outbound)
        .await
        .map_err(|err| ApiError::bad_gateway(format!("requete vers code-server echouee: {err}")))?;

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

        // Relai brut, sans reinterpreter les frames WebSocket (meme
        // philosophie que `net-proxy::proxy::tunnel` pour `CONNECT`) : une
        // fois les deux cotes upgrades, ce n'est plus que des octets.
        tokio::spawn(async move {
            match (server_upgrade.await, client_upgrade.await) {
                (Ok(server_io), Ok(client_io)) => {
                    let mut server_io = TokioIo::new(server_io);
                    let mut client_io = TokioIo::new(client_io);
                    if let Err(err) =
                        tokio::io::copy_bidirectional(&mut server_io, &mut client_io).await
                    {
                        tracing::debug!(%err, "tunnel websocket code-server ferme");
                    }
                }
                _ => tracing::warn!("upgrade websocket vers code-server echoue"),
            }
        });
        return Ok(reply);
    }

    let (parts, incoming_body) = upstream_response.into_parts();
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if name == http::header::CONNECTION || name == http::header::TRANSFER_ENCODING {
            continue;
        }
        builder = builder.header(name, value);
    }
    builder
        .body(Body::new(incoming_body))
        .map_err(|err| ApiError::bad_gateway(format!("reponse de code-server invalide: {err}")))
}

/// Ouvre une connexion TCP "virtuelle" vers `(pod_ip, remote_port)` a
/// travers le protocole `portforward` de `net-proxy` (voir commentaire de
/// module de `crates/net-proxy/src/portforward.rs` pour le detail du
/// multiplexage par canal) et l'expose comme un flux d'octets ordinaire —
/// pas besoin de reimplementer `Sink`/`Stream` a la main : une tache de
/// fond pompe entre le websocket et une moitie de `tokio::io::duplex`,
/// l'autre moitie est rendue a l'appelant.
async fn open_forwarded_tcp_stream(
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
