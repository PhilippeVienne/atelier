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
            auth: GuestAuth::CodeServerCookie,
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
            auth: GuestAuth::CodeServerCookie,
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
    /// Comment prouver au service du guest qu'on a le droit d'entrer. Les
    /// deux services embarques n'acceptent PAS la meme chose (voir
    /// [`GuestAuth`]).
    pub auth: GuestAuth,
}

/// Forme d'authentification attendue par le service vise dans le guest.
///
/// Le mot de passe est le meme des deux cotes (secret OpenBao `session_auth`
/// du Workshop, provisionne par le controller) ; c'est sa PRESENTATION qui
/// differe, et croire l'inverse a coute cher :
///
/// - `ttyd` implemente un vrai Basic Auth (`--credential`) : un en-tete
///   `Authorization: Basic` suffit.
/// - `code-server --auth password` **ignore le Basic Auth**. Il repond `302`
///   vers `/login` a toute requete sans cookie, y compris a une requete
///   parfaitement authentifiee en Basic — verifie contre une vraie microVM
///   le 2026-09-01. Il faut donc POSTer le mot de passe sur `/login`, en
///   recuperer le cookie `code-server-session`, et le presenter ensuite.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum GuestAuth {
    /// En-tete `Authorization: Basic atelier:<mot de passe>`.
    Basic,
    /// Login formulaire puis cookie `code-server-session`.
    CodeServerCookie,
}

/// Nom du cookie de session pose par `code-server` apres un login reussi.
const CODE_SERVER_COOKIE: &str = "code-server-session";

/// Joue le login formulaire de `code-server` sur la connexion deja ouverte
/// vers le guest et renvoie la valeur du cookie `code-server-session`.
///
/// Le login se fait sur la MEME connexion HTTP/1.1 keep-alive que la requete
/// qui suit : pas de second tunnel `portforward` a ouvrir, et l'ordre est
/// garanti.
async fn code_server_login(
    sender: &mut hyper::client::conn::http1::SendRequest<Body>,
    port: u16,
    password: &str,
) -> anyhow::Result<String> {
    let form = format!("password={}", urlencode(password));
    let request = Request::builder()
        .method(http::Method::POST)
        .uri("/login")
        .header(http::header::HOST, format!("127.0.0.1:{port}"))
        .header(
            http::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(http::header::CONTENT_LENGTH, form.len())
        .body(Body::from(form))?;

    let response = sender.send_request(request).await?;
    // `code-server` repond 302 vers `/` ET pose le cookie quand le mot de
    // passe est bon ; 302 vers `/login?error=...` quand il est mauvais. On
    // ne se fie donc pas au statut mais a la PRESENCE du cookie.
    for value in response.headers().get_all(http::header::SET_COOKIE) {
        let value = value.to_str().unwrap_or_default();
        if let Some(rest) = value.strip_prefix(&format!("{CODE_SERVER_COOKIE}=")) {
            let cookie = rest.split(';').next().unwrap_or_default().to_string();
            if !cookie.is_empty() {
                return Ok(cookie);
            }
        }
    }
    anyhow::bail!("code-server n'a pas delivre de cookie de session (mot de passe refuse ?)")
}

/// Encodage `application/x-www-form-urlencoded` du mot de passe. Ecrit ici
/// plutot qu'ajoute en dependance : c'est une seule valeur, et le mot de
/// passe genere par le controller est alphanumerique — mais s'y FIER serait
/// exactement le genre de supposition qui casse le jour ou le generateur
/// change.
fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
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
        auth,
    } = target;
    tracing::debug!(name = %name, path = %path, port, user = %user.subject, "proxy_to_guest_port appele");
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
    // ici plutot que laisse au client : un client externe ne doit jamais
    // avoir besoin de le connaitre — seul `api-server` (via son role
    // OpenBao cluster-wide `atelier-api-server`, voir `crate::session_auth`)
    // le lit. Remplace un eventuel `Authorization` du client (son JWT
    // `Bearer`, deja verifie par `require_auth` pour atteindre ce handler) :
    // ce n'est de toute facon pas ce que le guest attend. Si le secret est
    // absent/OpenBao non configure, on relaie tel quel (comportement
    // degrade, pas d'erreur bloquante).
    //
    // La FORME de la preuve depend du service vise, voir `GuestAuth` : les
    // deux ne se contentent pas du meme en-tete.
    if let Some(session_auth) = &state.session_auth {
        if let Some(password) = session_auth.session_password(&name).await {
            match auth {
                GuestAuth::Basic => {
                    let credentials = base64::engine::general_purpose::STANDARD
                        .encode(format!("atelier:{password}"));
                    match http::HeaderValue::from_str(&format!("Basic {credentials}")) {
                        Ok(value) => {
                            parts.headers.insert(http::header::AUTHORIZATION, value);
                        }
                        Err(err) => {
                            tracing::warn!(%err, "en-tete Authorization Basic invalide, requete relayee sans injection");
                        }
                    }
                }
                GuestAuth::CodeServerCookie => {
                    // Le login n'est rejoue que si aucun cookie n'est connu :
                    // une session VS Code emet des centaines de requetes, et
                    // `code-server` hache le mot de passe en argon2 a chaque
                    // login (volontairement lent).
                    let cookie = match session_auth.code_server_cookie(&name).await {
                        Some(cookie) => Some(cookie),
                        None => match code_server_login(&mut sender, port, &password).await {
                            Ok(cookie) => {
                                session_auth
                                    .store_code_server_cookie(&name, cookie.clone())
                                    .await;
                                Some(cookie)
                            }
                            Err(err) => {
                                tracing::warn!(%err, "login code-server echoue, requete relayee sans cookie");
                                None
                            }
                        },
                    };
                    if let Some(cookie) = cookie {
                        // Le JWT `Bearer` du client n'a rien a faire dans le
                        // guest, et `code-server` ignore `Authorization` de
                        // toute facon.
                        parts.headers.remove(http::header::AUTHORIZATION);
                        match http::HeaderValue::from_str(&format!("{CODE_SERVER_COOKIE}={cookie}"))
                        {
                            Ok(value) => {
                                parts.headers.insert(http::header::COOKIE, value);
                            }
                            Err(err) => {
                                tracing::warn!(%err, "cookie code-server invalide, requete relayee sans injection");
                            }
                        }
                    }
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

    // `code-server` renvoie malgre tout vers `/login` : le cookie en cache
    // ne vaut plus rien (microVM redemarree avec un autre mot de passe,
    // secret tourne). On l'oublie pour que la requete suivante — celle que
    // le navigateur emettra en suivant cette redirection — en obtienne un
    // neuf. Se soigne tout seul au prix d'un aller-retour.
    if auth == GuestAuth::CodeServerCookie && upstream_response.status().is_redirection() {
        let to_login = upstream_response
            .headers()
            .get(http::header::LOCATION)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|location| location.contains("/login"));
        if to_login {
            if let Some(session_auth) = &state.session_auth {
                tracing::info!(workshop = %name, "cookie code-server rejete, oublie pour la prochaine requete");
                session_auth.forget_code_server_cookie(&name).await;
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Ouvre un `SendRequest` branche sur un serveur factice qui repond la
    /// reponse brute fournie — assez pour exercer `code_server_login` sans
    /// microVM ni `code-server`.
    async fn sender_replying(
        raw_response: &'static str,
    ) -> hyper::client::conn::http1::SendRequest<Body> {
        let (client_io, mut server_io) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            // Une seule lecture suffit : la requete de login tient largement
            // dans un segment, et ce stub ne sert qu'a une requete.
            let _ = server_io.read(&mut buf).await;
            let _ = server_io.write_all(raw_response.as_bytes()).await;
            let _ = server_io.flush().await;
        });
        let (sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(client_io))
            .await
            .expect("handshake avec le stub");
        tokio::spawn(async move {
            let _ = conn.await;
        });
        sender
    }

    /// Le cookie est extrait de `Set-Cookie`, attributs (`Path`, `HttpOnly`…)
    /// exclus : c'est la VALEUR seule qui doit repartir dans l'en-tete
    /// `Cookie`.
    #[tokio::test]
    async fn login_extracts_the_session_cookie() {
        let mut sender = sender_replying(
            "HTTP/1.1 302 Found\r\n\
             Location: /\r\n\
             Set-Cookie: code-server-session=%24argon2id%24abc; Path=/; HttpOnly; SameSite=Lax\r\n\
             Content-Length: 0\r\n\r\n",
        )
        .await;
        let cookie = code_server_login(&mut sender, 8080, "secret")
            .await
            .expect("cookie attendu");
        assert_eq!(cookie, "%24argon2id%24abc");
    }

    /// Mot de passe refuse : `code-server` repond AUSSI un 302 (vers
    /// `/login`), mais sans cookie. Se fier au statut ferait passer un echec
    /// pour un succes, d'ou la verification sur la presence du cookie.
    #[tokio::test]
    async fn a_refused_password_is_an_error_not_a_cookie() {
        let mut sender = sender_replying(
            "HTTP/1.1 302 Found\r\n\
             Location: /login?error=1\r\n\
             Content-Length: 0\r\n\r\n",
        )
        .await;
        assert!(code_server_login(&mut sender, 8080, "mauvais")
            .await
            .is_err());
    }

    /// Le mot de passe part dans un formulaire : tout ce qui n'est pas
    /// `unreserved` doit etre encode, sans quoi un `&` ou un `+` dans un mot
    /// de passe futur casserait le login de facon parfaitement obscure.
    #[test]
    fn the_password_is_form_encoded() {
        assert_eq!(super::urlencode("abcXYZ089-_.~"), "abcXYZ089-_.~");
        assert_eq!(super::urlencode("a&b=c"), "a%26b%3Dc");
        assert_eq!(super::urlencode("a+b c"), "a%2Bb%20c");
        assert_eq!(super::urlencode("e\u{0301}"), "e%CC%81");
    }
}
