//! Port-forward de la microVM vers l'exterieur, dans le style du
//! `kubectl port-forward` de Kubernetes : net-proxy tient ici le role du
//! **kubelet** (il vit a cote de la charge de travail et sait seul y
//! ouvrir une connexion), et c'est `api-server` — pas un client externe
//! direct — qui tient le role de **coordinateur** : il authentifie le
//! demandeur (JWT/Kanidm, proprietaire du `Workshop`) puis ouvre une seule
//! connexion websocket vers ce endpoint pour relayer le flux. net-proxy ne
//! fait donc aucune authentification lui-meme ; il fait confiance a
//! quiconque peut atteindre ce port (qui ne doit jamais etre expose
//! au-dela du reseau interne du pod/cluster — c'est le role d'`api-server`
//! de rester le seul point d'entree autorise pour un client final).
//!
//! Protocole websocket, calque sur le sous-protocole historique
//! `portforward.k8s.io` de Kubernetes (celui utilise quand SPDY n'est pas
//! disponible) : le client demande une liste de ports via
//! `GET /portforward?ports=tcp:8443,udp:53`, puis chaque port `i` de cette
//! liste se voit assigner deux "canaux" multiplexes sur l'unique connexion
//! websocket : `2*i` pour les donnees, `2*i+1` pour les erreurs. Chaque
//! message binaire commence par un octet de canal, suivi de la charge
//! utile. Comme cote Kubernetes, une session ne forwarde qu'une seule
//! connexion par port (la cible est jointe des l'ouverture du websocket) —
//! pas de multiplexage de plusieurs connexions sur un meme port au sein
//! d'une session.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;

use crate::forward::{self, Protocol};

#[derive(Clone)]
pub struct PortForwardState {
    /// Adresse a laquelle joindre la microVM (interface du pod cote tap
    /// device, ou `127.0.0.1` si le reseau de la VM est aujourd'hui relaye
    /// localement — voir `ATELIER_VM_ADDR`).
    pub vm_addr: Arc<str>,
}

pub fn router(state: PortForwardState) -> Router {
    Router::new()
        .route("/portforward", get(handler))
        .with_state(state)
}

#[derive(Deserialize)]
struct PortForwardQuery {
    ports: String,
}

async fn handler(
    ws: WebSocketUpgrade,
    State(state): State<PortForwardState>,
    Query(query): Query<PortForwardQuery>,
) -> Response {
    match forward::parse_ports_query(&query.ports) {
        Ok(specs) => ws.on_upgrade(move |socket| run_session(socket, specs, state.vm_addr)),
        Err(err) => {
            tracing::warn!(%err, ports = %query.ports, "requete de port-forward invalide");
            axum::response::IntoResponse::into_response((
                axum::http::StatusCode::BAD_REQUEST,
                err.to_string(),
            ))
        }
    }
}

/// Un canal de sortie pour reinjecter les octets recus depuis la microVM
/// vers le client (i.e. le port-forward "aller" ou "retour" selon le sens),
/// commun aux deux protocoles.
type OutgoingTx = mpsc::UnboundedSender<Message>;

enum PortSink {
    Tcp(tokio::net::tcp::OwnedWriteHalf),
    Udp(Arc<UdpSocket>),
    /// La connexion/le bind vers la microVM a echoue : les donnees entrantes
    /// pour ce port sont journalisees puis ignorees.
    Failed,
}

async fn run_session(socket: WebSocket, specs: Vec<forward::PortSpec>, vm_addr: Arc<str>) {
    let (mut ws_sink, mut ws_stream) = socket.split();
    let (tx, mut rx): (OutgoingTx, _) = mpsc::unbounded_channel();

    // Une seule tache possede le sink websocket : les taches par-port lui
    // envoient leurs trames au lieu d'ecrire directement dessus.
    let sink_task = tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            if ws_sink.send(msg).await.is_err() {
                break;
            }
        }
    });

    let mut sinks: Vec<PortSink> = Vec::with_capacity(specs.len());
    for (index, spec) in specs.iter().enumerate() {
        let data_channel = channel_byte(index, false);
        let error_channel = channel_byte(index, true);
        sinks.push(open_port(*spec, &vm_addr, data_channel, error_channel, tx.clone()).await);
    }

    while let Some(Ok(msg)) = ws_stream.next().await {
        let Message::Binary(data) = msg else {
            continue;
        };
        let Some((&channel, payload)) = data.split_first() else {
            continue;
        };
        // Seuls les canaux pairs (donnees) acceptent des ecritures client ->
        // microVM ; les canaux impairs (erreurs) ne sont utilises que dans
        // le sens net-proxy -> client, comme cote Kubernetes.
        if channel % 2 != 0 {
            continue;
        }
        let index = (channel / 2) as usize;
        let Some(sink) = sinks.get_mut(index) else {
            continue;
        };
        write_to_sink(sink, payload).await;
    }

    drop(tx);
    let _ = sink_task.await;
}

fn channel_byte(index: usize, error: bool) -> u8 {
    (index * 2 + usize::from(error)) as u8
}

async fn open_port(
    spec: forward::PortSpec,
    vm_addr: &str,
    data_channel: u8,
    error_channel: u8,
    tx: OutgoingTx,
) -> PortSink {
    match spec.protocol {
        Protocol::Tcp => match TcpStream::connect((vm_addr, spec.port)).await {
            Ok(stream) => {
                let (mut read_half, write_half) = stream.into_split();
                let tx = tx.clone();
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 16 * 1024];
                    loop {
                        match read_half.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if tx.send(framed(data_channel, &buf[..n])).is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
                PortSink::Tcp(write_half)
            }
            Err(err) => {
                report_error(&tx, error_channel, spec, &err);
                PortSink::Failed
            }
        },
        Protocol::Udp => match UdpSocket::bind(("0.0.0.0", 0)).await {
            Ok(socket) => match socket.connect((vm_addr, spec.port)).await {
                Ok(()) => {
                    let socket = Arc::new(socket);
                    let reader = Arc::clone(&socket);
                    let tx = tx.clone();
                    tokio::spawn(async move {
                        let mut buf = vec![0u8; 64 * 1024];
                        while let Ok(n) = reader.recv(&mut buf).await {
                            if tx.send(framed(data_channel, &buf[..n])).is_err() {
                                break;
                            }
                        }
                    });
                    PortSink::Udp(socket)
                }
                Err(err) => {
                    report_error(&tx, error_channel, spec, &err);
                    PortSink::Failed
                }
            },
            Err(err) => {
                report_error(&tx, error_channel, spec, &err);
                PortSink::Failed
            }
        },
    }
}

fn report_error(tx: &OutgoingTx, error_channel: u8, spec: forward::PortSpec, err: &std::io::Error) {
    tracing::warn!(port = spec.port, protocol = ?spec.protocol, %err, "port-forward: connexion a la microVM echouee");
    let _ = tx.send(framed(error_channel, err.to_string().as_bytes()));
}

fn framed(channel: u8, payload: &[u8]) -> Message {
    let mut buf = Vec::with_capacity(payload.len() + 1);
    buf.push(channel);
    buf.extend_from_slice(payload);
    Message::Binary(buf.into())
}

async fn write_to_sink(sink: &mut PortSink, payload: &[u8]) {
    match sink {
        PortSink::Tcp(write_half) => {
            if write_half.write_all(payload).await.is_err() {
                *sink = PortSink::Failed;
            }
        }
        PortSink::Udp(socket) => {
            let _ = socket.send(payload).await;
        }
        PortSink::Failed => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    async fn spawn_echo_server() -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                tokio::spawn(async move {
                    let mut buf = vec![0u8; 4096];
                    loop {
                        match socket.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                if socket.write_all(&buf[..n]).await.is_err() {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        port
    }

    async fn spawn_control_server() -> u16 {
        let router = router(PortForwardState {
            vm_addr: "127.0.0.1".into(),
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        port
    }

    #[tokio::test]
    async fn relays_data_through_a_single_tcp_channel() {
        let echo_port = spawn_echo_server().await;
        let control_port = spawn_control_server().await;

        let url = format!("ws://127.0.0.1:{control_port}/portforward?ports=tcp:{echo_port}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

        let mut frame = vec![0u8]; // canal 0 = donnees du port d'index 0
        frame.extend_from_slice(b"hello");
        ws.send(WsMessage::Binary(frame.into())).await.unwrap();

        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("reponse recue avant timeout")
            .expect("flux websocket toujours ouvert")
            .unwrap();
        let WsMessage::Binary(data) = reply else {
            panic!("message non binaire recu: {reply:?}");
        };
        assert_eq!(data[0], 0, "doit revenir sur le canal de donnees");
        assert_eq!(&data[1..], b"hello");
    }

    #[tokio::test]
    async fn reports_connection_failure_on_the_error_channel() {
        // Port improbable qu'aucun service local n'ecoute.
        let dead_port = 1;
        let control_port = spawn_control_server().await;

        let url = format!("ws://127.0.0.1:{control_port}/portforward?ports=tcp:{dead_port}");
        let (mut ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();

        let reply = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
            .await
            .expect("reponse recue avant timeout")
            .expect("flux websocket toujours ouvert")
            .unwrap();
        let WsMessage::Binary(data) = reply else {
            panic!("message non binaire recu: {reply:?}");
        };
        assert_eq!(data[0], 1, "doit arriver sur le canal d'erreur du port 0");
    }

    #[tokio::test]
    async fn rejects_malformed_ports_query() {
        let control_port = spawn_control_server().await;
        let url = format!("http://127.0.0.1:{control_port}/portforward?ports=");
        let response = reqwest_status(&url).await;
        assert_eq!(response, 400);
    }

    async fn reqwest_status(url: &str) -> u16 {
        // Pas de dependance reqwest en dev-dependency : une requete HTTP/1.1
        // minimale suffit a verifier le code de statut sur ce endpoint.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpStream;
        let uri: axum::http::Uri = url.parse().unwrap();
        let host = uri.host().unwrap();
        let port = uri.port_u16().unwrap();
        let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/");
        let mut stream = TcpStream::connect((host, port)).await.unwrap();
        stream
            .write_all(
                format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        response.split_whitespace().nth(1).unwrap().parse().unwrap()
    }
}
