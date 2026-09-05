//! Client du pont port-forward (spec `docs/specs/14-devex-cli-simulateurs-hitl.md`
//! §3.7, tache 9.2) : parle le meme sous-protocole websocket que
//! `crates/api-server/src/portforward.rs` (calque sur `portforward.k8s.io`)
//! — un seul port par session, canal `0` pour les donnees, `1` pour les
//! erreurs, chaque message binaire prefixe d'un octet de canal. Une session
//! ne forwarde qu'une seule connexion : `atelier port-forward` en ouvre une
//! nouvelle a chaque connexion TCP locale acceptee (mode ecoute) ou une
//! seule fois (mode `--stdio`, utilisable comme `ProxyCommand` SSH).

use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

const DATA_CHANNEL: u8 = 0;
const ERROR_CHANNEL: u8 = 1;

fn ws_url(api_url: &str, name: &str, remote_port: u16) -> String {
    let scheme_swapped = if let Some(rest) = api_url.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = api_url.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        format!("ws://{api_url}")
    };
    format!(
        "{}/v1/workshops/{name}/portforward?ports={remote_port}",
        scheme_swapped.trim_end_matches('/')
    )
}

async fn connect(
    api_url: &str,
    access_token: &str,
    name: &str,
    remote_port: u16,
) -> Result<
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
> {
    let url = ws_url(api_url, name, remote_port);
    let mut req = url
        .clone()
        .into_client_request()
        .with_context(|| format!("URL de port-forward invalide ({url})"))?;
    req.headers_mut().insert(
        http::header::AUTHORIZATION,
        format!("Bearer {access_token}")
            .parse()
            .context("jeton d'acces invalide pour l'en-tete Authorization")?,
    );
    let (ws, _response) = tokio_tungstenite::connect_async(req)
        .await
        .with_context(|| format!("connexion au port-forward de '{name}' echouee ({url})"))?;
    Ok(ws)
}

/// Relaie une seule connexion : `local` (deja etabli, stdio ou socket TCP
/// accepte) <-> le port distant `remote_port` du Workshop `name`, via
/// `api-server`. Bloque jusqu'a fermeture de l'un des deux cotes.
pub async fn relay_once<S>(
    api_url: &str,
    access_token: &str,
    name: &str,
    remote_port: u16,
    local: S,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let ws = connect(api_url, access_token, name, remote_port).await?;
    let (mut ws_tx, mut ws_rx) = ws.split();
    let (mut local_rx, mut local_tx) = tokio::io::split(local);

    let local_to_ws = async {
        let mut buf = [0u8; 16 * 1024];
        loop {
            let n = match local_rx.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            let mut payload = Vec::with_capacity(n + 1);
            payload.push(DATA_CHANNEL);
            payload.extend_from_slice(&buf[..n]);
            if ws_tx.send(Message::Binary(payload.into())).await.is_err() {
                break;
            }
        }
        let _ = ws_tx.send(Message::Close(None)).await;
    };

    let ws_to_local = async {
        while let Some(Ok(msg)) = ws_rx.next().await {
            match msg {
                Message::Binary(data) if !data.is_empty() && data[0] == DATA_CHANNEL => {
                    if local_tx.write_all(&data[1..]).await.is_err() {
                        break;
                    }
                }
                Message::Binary(data) if !data.is_empty() && data[0] == ERROR_CHANNEL => {
                    let text = String::from_utf8_lossy(&data[1..]);
                    eprintln!("atelier port-forward: erreur distante: {text}");
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    };

    tokio::select! {
        _ = local_to_ws => {}
        _ = ws_to_local => {}
    }
    Ok(())
}

/// Mode `--stdio` : relaie directement stdin/stdout du process — utilisable
/// comme `ProxyCommand` SSH (spec §3.7) ou comme tuyau brut pour tout autre
/// protocole (ex: `code-server`).
pub async fn relay_stdio(
    api_url: &str,
    access_token: &str,
    name: &str,
    remote_port: u16,
) -> Result<()> {
    let stdio = StdioDuplex::new();
    relay_once(api_url, access_token, name, remote_port, stdio).await
}

/// Ecoute sur `local_addr` (ex: `127.0.0.1:8443`) et ouvre une nouvelle
/// session de port-forward a chaque connexion TCP acceptee (une session par
/// connexion : le protocole ne multiplexe pas plusieurs connexions sur un
/// meme port, voir la doc de module).
pub async fn listen_and_forward(
    api_url: &str,
    access_token: &str,
    name: &str,
    local_addr: &str,
    remote_port: u16,
) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(local_addr)
        .await
        .with_context(|| format!("ecoute locale sur {local_addr}"))?;
    println!("Ecoute sur {local_addr} -> Workshop '{name}':{remote_port}");
    loop {
        let (stream, peer) = listener.accept().await.context("acceptation TCP locale")?;
        tracing::info!(%peer, "nouvelle connexion locale, ouverture d'une session de port-forward");
        let api_url = api_url.to_string();
        let access_token = access_token.to_string();
        let name = name.to_string();
        tokio::spawn(async move {
            if let Err(err) = relay_once(&api_url, &access_token, &name, remote_port, stream).await
            {
                tracing::warn!(?err, "session de port-forward terminee en erreur");
            }
        });
    }
}

/// Adapte stdin/stdout du process courant en un seul flux `AsyncRead +
/// AsyncWrite`, pour reutiliser `relay_once` tel quel en mode `--stdio`.
struct StdioDuplex {
    stdin: tokio::io::Stdin,
    stdout: tokio::io::Stdout,
}

impl StdioDuplex {
    fn new() -> Self {
        Self {
            stdin: tokio::io::stdin(),
            stdout: tokio::io::stdout(),
        }
    }
}

impl AsyncRead for StdioDuplex {
    fn poll_read(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stdin).poll_read(cx, buf)
    }
}

impl AsyncWrite for StdioDuplex {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::pin::Pin::new(&mut self.stdout).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stdout).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.stdout).poll_shutdown(cx)
    }
}
