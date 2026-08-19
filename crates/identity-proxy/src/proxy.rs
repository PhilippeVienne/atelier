//! Traitement d'une connexion cliente (l'agent, via son
//! `HTTP_PROXY`/`HTTPS_PROXY` pointant sur identity-proxy) : injecte un
//! credential si une regle correspond a la destination, puis relaie vers
//! `net-proxy` — identity-proxy ne decide jamais lui-meme de l'allowlist
//! egress, ce role reste a net-proxy (separation des responsabilites :
//! "quoi" vs "avec quelle identite").
//!
//! Limite connue (voir `docs/ARCHITECTURE.md`, section isolation reseau,
//! TODO ouvert) : un `CONNECT` (HTTPS) est un tunnel TCP opaque, le contenu
//! est chiffre bout-a-bout entre l'agent et la destination — identity-proxy
//! ne peut donc **pas** y injecter d'en-tete sans devenir un MITM TLS actif,
//! ce qui n'est pas fait ici. L'injection ne fonctionne aujourd'hui que pour
//! les requetes HTTP en clair relayees en forme absolue.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::http::{self, RequestHead};
use crate::rules::{self, InjectionRule};
use crate::secrets::SecretCache;

const BAD_GATEWAY_RESPONSE: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone)]
pub struct ProxyConfig {
    pub rules: Arc<Vec<InjectionRule>>,
    pub secrets: SecretCache,
    /// `host:port` de `net-proxy`, seul chemin de sortie reseau autorise
    /// pour la microVM. Absent : connexion directe (dev/test uniquement).
    pub next_hop: Option<Arc<str>>,
}

pub async fn handle_connection(
    client: TcpStream,
    peer: SocketAddr,
    config: ProxyConfig,
) -> anyhow::Result<()> {
    let mut client = BufReader::new(client);

    let Some(head) = http::read_request_head(&mut client).await? else {
        return Ok(());
    };

    let (host, port) = match http::destination(&head) {
        Ok(dest) => dest,
        Err(err) => {
            tracing::warn!(%peer, %err, "requete sans destination exploitable");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    let rule = rules::matching(&config.rules, &host);

    if head.method.eq_ignore_ascii_case("CONNECT") {
        if let Some(rule) = rule {
            tracing::warn!(
                %peer, host, header = %rule.header,
                "regle d'injection ignoree : destination jointe en HTTPS (CONNECT), \
                 identity-proxy ne dechiffre pas le tunnel"
            );
        }
        tunnel(client, &host, port, peer, &config).await
    } else {
        forward(client, &head, rule, &host, port, peer, &config).await
    }
}

async fn tunnel(
    mut client: BufReader<TcpStream>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    config: &ProxyConfig,
) -> anyhow::Result<()> {
    let mut upstream = match connect_next_hop(host, port, config.next_hop.as_deref()).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion au saut suivant echouee");
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

async fn forward(
    mut client: BufReader<TcpStream>,
    head: &RequestHead,
    rule: Option<&InjectionRule>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    config: &ProxyConfig,
) -> anyhow::Result<()> {
    let mut upstream = match connect_next_hop(host, port, config.next_hop.as_deref()).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion au saut suivant echouee");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    let raw = match rule {
        Some(rule) => match config.secrets.read().await.get(&rule.secret_cache_key()) {
            Some(value) => {
                tracing::info!(%peer, host, header = %rule.header, "credential injecte");
                head.with_injected_header(&rule.header, &format!("{}{}", rule.prefix, value))
            }
            None => {
                tracing::warn!(
                    %peer, host, secret_path = %rule.secret_path,
                    "regle d'injection trouvee mais secret pas encore disponible, requete relayee sans injection"
                );
                head.to_bytes()
            }
        },
        None => head.to_bytes(),
    };

    upstream
        .write_all(&raw)
        .await
        .context("envoi de la requete au saut suivant")?;

    let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));
    tracing::debug!(%peer, host, port, sent, received, "relai HTTP ferme");
    Ok(())
}

/// Rejoint `net-proxy` (via un tunnel `CONNECT`, quel que soit le protocole
/// d'origine — HTTP en clair inclus, pour beneficier de son allowlist et de
/// son eventuel chainage vers un proxy parent) si configure, sinon se
/// connecte directement a la destination (dev/test uniquement : en
/// production la VM ne peut de toute facon joindre qu'identity-proxy et
/// net-proxy, voir le pare-feu TAP dans `docs/ARCHITECTURE.md`).
async fn connect_next_hop(
    host: &str,
    port: u16,
    next_hop: Option<&str>,
) -> anyhow::Result<TcpStream> {
    match next_hop {
        Some(addr) => connect_via_connect_tunnel(addr, host, port)
            .await
            .with_context(|| format!("connexion via net-proxy ({addr})")),
        None => TcpStream::connect((host, port))
            .await
            .with_context(|| format!("connexion directe a {host}:{port}")),
    }
}

async fn connect_via_connect_tunnel(
    proxy_addr: &str,
    host: &str,
    port: u16,
) -> anyhow::Result<TcpStream> {
    let stream = TcpStream::connect(proxy_addr)
        .await
        .context("connexion TCP a net-proxy")?;
    let mut reader = BufReader::new(stream);

    let request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\nProxy-Connection: keep-alive\r\n\r\n");
    reader
        .write_all(request.as_bytes())
        .await
        .context("envoi du CONNECT a net-proxy")?;

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .context("lecture de la reponse CONNECT de net-proxy")?;
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .map(|code| code.starts_with('2'))
        .unwrap_or(false);
    if !ok {
        anyhow::bail!("net-proxy a refuse le CONNECT: {}", status_line.trim());
    }

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            anyhow::bail!("net-proxy a ferme la connexion avant la fin de la reponse CONNECT");
        }
        if line == "\r\n" {
            break;
        }
    }

    Ok(reader.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::RwLock;

    /// Bout-en-bout sur de vraies sockets TCP (pas de mock) : un client
    /// envoie une requete HTTP en clair a identity-proxy, une regle
    /// correspond et un secret est en cache — la "destination" (un vrai
    /// listener TCP local) doit recevoir l'en-tete injecte.
    #[tokio::test]
    async fn injects_configured_header_into_plain_http_request() {
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_addr = destination.local_addr().unwrap();

        let captured = tokio::spawn(async move {
            let (mut socket, _) = destination.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
                .await;
            String::from_utf8(buf).unwrap()
        });

        let rule = InjectionRule {
            host: destination_addr.ip().to_string(),
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
            secret_path: "github".to_string(),
            field: "token".to_string(),
        };
        let mut secrets = HashMap::new();
        secrets.insert(rule.secret_cache_key(), "s3cr3t".to_string());

        let config = ProxyConfig {
            rules: Arc::new(vec![rule]),
            secrets: Arc::new(RwLock::new(secrets)),
            next_hop: None,
        };

        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = proxy_listener.accept().await.unwrap();
            handle_connection(socket, peer, config).await.unwrap();
        });

        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let request = format!("GET / HTTP/1.1\r\nHost: {destination_addr}\r\n\r\n");
        client.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response).await;

        let received = captured.await.unwrap();
        assert!(
            received.contains("Authorization: Bearer s3cr3t"),
            "{received}"
        );
    }
}
