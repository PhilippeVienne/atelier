//! Coordinateur de port-forward : `api-server` authentifie le client final
//! (JWT/Kanidm), verifie qu'il est bien proprietaire du `Workshop` cible,
//! puis relaie sa connexion websocket vers l'endpoint `/portforward` de
//! `net-proxy` (deja ecrit, `crates/net-proxy/src/portforward.rs`) — sur le
//! modele `kubectl port-forward` : `net-proxy` est le "kubelet" (colocalise
//! avec la charge de travail, seul a savoir y ouvrir une connexion),
//! `api-server` le coordinateur qui authentifie et route. Voir le
//! commentaire de module de `net-proxy::portforward` pour le protocole
//! (sous-protocole `portforward.k8s.io`, canaux multiplexes par port).
//!
//! Aucune authentification cote `net-proxy` lui-meme : le port de controle
//! (`ATELIER_NET_PROXY_CONTROL_ADDR`, port 9000 par defaut) ne doit jamais
//! etre exposable au-dela du reseau interne du cluster — c'est `api-server`
//! qui reste le seul point d'entree autorise pour un client final.

use crate::auth::AuthenticatedUser;
use crate::routes::{ensure_owner, workshops_api, ApiError, AppState};
use axum::extract::ws::{CloseFrame, Message as AxumMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, Path, Query, State};
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::api::Api;
use serde::Deserialize;
use tokio_tungstenite::tungstenite::Message as TsMessage;

const DEFAULT_NET_PROXY_CONTROL_PORT: u16 = 9000;

/// Port de controle de `net-proxy` (`ATELIER_NET_PROXY_CONTROL_ADDR` cote
/// net-proxy, cf. `crates/net-proxy/src/main.rs`) — configurable
/// (`ATELIER_NET_PROXY_CONTROL_PORT`) pour les tests, fixe en production
/// (meme pod, meme port a chaque Workshop).
fn net_proxy_control_port() -> u16 {
    std::env::var("ATELIER_NET_PROXY_CONTROL_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(DEFAULT_NET_PROXY_CONTROL_PORT)
}

#[derive(Deserialize)]
pub struct PortForwardQuery {
    ports: String,
}

/// `GET /v1/workshops/{name}/portforward?ports=tcp:8443,udp:53` — protege
/// par le meme middleware JWT que le reste de l'API (voir `routes::router`).
pub async fn portforward(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    Query(query): Query<PortForwardQuery>,
    ws: WebSocketUpgrade,
) -> Result<Response, ApiError> {
    let workshop = workshops_api(&state).get(&name).await?;
    ensure_owner(&workshop, &user)?;

    let pod_name = workshop
        .status
        .as_ref()
        .and_then(|s| s.pod_name.clone())
        .ok_or_else(|| ApiError::bad_request("le Workshop n'a pas de pod parent actif (suspendu ?)"))?;
    let pods: Api<Pod> = Api::namespaced(state.client.clone(), &state.namespace);
    let pod = pods.get(&pod_name).await?;
    let pod_ip = pod
        .status
        .as_ref()
        .and_then(|s| s.pod_ip.clone())
        .ok_or_else(|| ApiError::bad_request("le pod parent n'a pas encore d'adresse IP"))?;

    let target_url = format!(
        "ws://{pod_ip}:{}/portforward?ports={}",
        net_proxy_control_port(),
        query.ports
    );

    Ok(ws.on_upgrade(move |client_socket| relay(client_socket, target_url)))
}

/// Relaie sans les interpreter les messages entre le websocket du client
/// final et celui de `net-proxy` : `api-server` ne connait pas le sens des
/// octets echanges (multiplexage par canal, gere entierement par
/// `net-proxy`), il se contente d'etre un tuyau bidirectionnel.
async fn relay(client_socket: WebSocket, target_url: String) {
    let net_proxy_socket = match tokio_tungstenite::connect_async(&target_url).await {
        Ok((socket, _response)) => socket,
        Err(err) => {
            tracing::warn!(%err, %target_url, "connexion au port-forward de net-proxy echouee");
            return;
        }
    };

    let (mut client_tx, mut client_rx) = client_socket.split();
    let (mut np_tx, mut np_rx) = net_proxy_socket.split();

    let client_to_np = async {
        while let Some(Ok(msg)) = client_rx.next().await {
            let done = matches!(msg, AxumMessage::Close(_));
            if np_tx.send(to_tungstenite(msg)).await.is_err() || done {
                break;
            }
        }
    };
    let np_to_client = async {
        while let Some(Ok(msg)) = np_rx.next().await {
            let done = matches!(msg, TsMessage::Close(_));
            if client_tx.send(to_axum(msg)).await.is_err() || done {
                break;
            }
        }
    };

    tokio::select! {
        _ = client_to_np => {}
        _ = np_to_client => {}
    }
}

fn to_tungstenite(msg: AxumMessage) -> TsMessage {
    match msg {
        AxumMessage::Text(text) => TsMessage::Text(text.as_str().into()),
        AxumMessage::Binary(data) => TsMessage::Binary(data),
        AxumMessage::Ping(data) => TsMessage::Ping(data),
        AxumMessage::Pong(data) => TsMessage::Pong(data),
        AxumMessage::Close(frame) => TsMessage::Close(frame.map(|f| {
            tokio_tungstenite::tungstenite::protocol::CloseFrame {
                code: f.code.into(),
                reason: f.reason.as_str().into(),
            }
        })),
    }
}

fn to_axum(msg: TsMessage) -> AxumMessage {
    match msg {
        TsMessage::Text(text) => AxumMessage::Text(text.as_str().into()),
        TsMessage::Binary(data) => AxumMessage::Binary(data),
        TsMessage::Ping(data) => AxumMessage::Ping(data),
        TsMessage::Pong(data) => AxumMessage::Pong(data),
        TsMessage::Close(frame) => AxumMessage::Close(frame.map(|f| CloseFrame {
            code: f.code.into(),
            reason: f.reason.as_str().into(),
        })),
        // Un frame brut n'a pas d'equivalent axum direct : ne devrait pas
        // apparaitre en pratique (tungstenite ne le produit qu'en usage bas
        // niveau explicite), traite comme une fermeture propre par defaut.
        TsMessage::Frame(_) => AxumMessage::Close(None),
    }
}
