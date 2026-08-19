//! Traitement d'une connexion cliente (la microVM, via son
//! `HTTP_PROXY`/`HTTPS_PROXY`) : verifie la destination contre l'allowlist,
//! journalise, puis relaie les octets si autorise.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::allowlist;
use crate::http::{self, RequestHead};
use crate::upstream::UpstreamProxy;

const FORBIDDEN_RESPONSE: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY_RESPONSE: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone)]
pub struct EgressConfig {
    pub allowlist: Arc<Vec<String>>,
    pub upstream: Option<Arc<UpstreamProxy>>,
    pub no_proxy: Arc<Vec<String>>,
}

pub async fn handle_connection(
    client: TcpStream,
    peer: SocketAddr,
    config: EgressConfig,
) -> anyhow::Result<()> {
    let mut client = BufReader::new(client);

    let Some(head) = http::read_request_head(&mut client).await? else {
        return Ok(());
    };

    let (host, port) = match http::destination(&head) {
        Ok(dest) => dest,
        Err(err) => {
            tracing::warn!(%peer, %err, "requete sans destination exploitable");
            let _ = client.write_all(FORBIDDEN_RESPONSE).await;
            return Ok(());
        }
    };

    if !allowlist::is_allowed(&host, &config.allowlist) {
        tracing::warn!(
            %peer,
            host,
            port,
            method = %head.method,
            allowed = false,
            "egress refuse (hors allowlist)"
        );
        let _ = client.write_all(FORBIDDEN_RESPONSE).await;
        return Ok(());
    }

    tracing::info!(
        %peer,
        host,
        port,
        method = %head.method,
        allowed = true,
        "egress autorise"
    );

    if head.method.eq_ignore_ascii_case("CONNECT") {
        tunnel(client, &host, port, peer, &config).await
    } else {
        forward(client, &head, &host, port, peer, &config).await
    }
}

/// `CONNECT` : etablit un tunnel TCP opaque (le contenu est TLS, net-proxy
/// ne le dechiffre pas — seule la destination est controlee).
async fn tunnel(
    mut client: BufReader<TcpStream>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    config: &EgressConfig,
) -> anyhow::Result<()> {
    let mut upstream = match crate::upstream::connect(
        host,
        port,
        config.upstream.as_deref(),
        &config.no_proxy,
    )
    .await
    {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion a la destination echouee");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .context("envoi de la reponse CONNECT")?;

    let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));
    tracing::debug!(%peer, host, port, sent, received, "tunnel CONNECT ferme");
    Ok(())
}

/// Requete HTTP en clair (proxy explicite, pas de CONNECT) : rejoue la
/// requete telle que recue vers la destination (ou vers le proxy parent,
/// via un tunnel `CONNECT`, si configure), puis relaie le reste.
async fn forward(
    mut client: BufReader<TcpStream>,
    head: &RequestHead,
    host: &str,
    port: u16,
    peer: SocketAddr,
    config: &EgressConfig,
) -> anyhow::Result<()> {
    let mut upstream = match crate::upstream::connect(
        host,
        port,
        config.upstream.as_deref(),
        &config.no_proxy,
    )
    .await
    {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion a la destination echouee");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    upstream
        .write_all(&head.raw)
        .await
        .context("envoi de la requete a la destination")?;

    let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));
    tracing::debug!(%peer, host, port, sent, received, "relai HTTP ferme");
    Ok(())
}
