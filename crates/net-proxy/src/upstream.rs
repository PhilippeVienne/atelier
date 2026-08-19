//! Chainage vers un proxy HTTP parent (ex: proxy sortant impose par le
//! reseau du cluster/de l'entreprise), avec une liste `no_proxy` de
//! destinations a joindre en direct malgre le proxy parent configure.
//!
//! net-proxy reste le seul point de decision de l'allowlist egress ; le
//! proxy parent, s'il existe, n'intervient qu'*apres* qu'une destination a
//! deja ete jugee autorisee.

use anyhow::{bail, Context};
use base64::Engine;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::allowlist;

#[derive(Debug, Clone)]
pub struct UpstreamProxy {
    /// Adresse `host:port` du proxy parent (jointe en clair, TCP).
    pub addr: String,
    /// Valeur pretee a l'en-tete `Proxy-Authorization` (ex: `Basic xxx`),
    /// si le proxy parent exige une authentification.
    pub auth_header: Option<String>,
}

impl UpstreamProxy {
    pub fn from_env() -> Option<Self> {
        let addr = std::env::var("ATELIER_UPSTREAM_PROXY").ok()?;
        let addr = addr.trim();
        if addr.is_empty() {
            return None;
        }
        let auth_header = std::env::var("ATELIER_UPSTREAM_PROXY_AUTH")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|creds| {
                let encoded = base64::engine::general_purpose::STANDARD.encode(creds.trim());
                format!("Basic {encoded}")
            });
        Some(Self {
            addr: addr.to_string(),
            auth_header,
        })
    }

    /// Composantes hote/port de `addr`, pour un appelant qui doit se
    /// connecter directement a ce pair sans passer par le handshake
    /// `CONNECT` (ex: net-proxy relayant une requete HTTP en clair vers
    /// identity-proxy plutot que vers la destination finale — voir
    /// `crate::proxy`).
    pub fn host(&self) -> &str {
        self.addr.rsplit_once(':').map_or(&self.addr, |(h, _)| h)
    }

    pub fn port(&self) -> u16 {
        self.addr
            .rsplit_once(':')
            .and_then(|(_, p)| p.parse().ok())
            .unwrap_or(0)
    }
}

pub fn no_proxy_from_env() -> Vec<String> {
    std::env::var("ATELIER_NO_PROXY")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

/// Ouvre une connexion TCP vers `host:port`, en passant par `upstream` sauf
/// si `host` correspond a une entree de `no_proxy` (meme syntaxe que
/// l'allowlist egress : correspondance exacte ou `*.domaine`).
pub async fn connect(
    host: &str,
    port: u16,
    upstream: Option<&UpstreamProxy>,
    no_proxy: &[String],
) -> anyhow::Result<TcpStream> {
    match upstream {
        Some(proxy) if !allowlist::is_allowed(host, no_proxy) => {
            connect_via_upstream(proxy, host, port)
                .await
                .with_context(|| format!("connexion via le proxy parent {}", proxy.addr))
        }
        _ => TcpStream::connect((host, port))
            .await
            .with_context(|| format!("connexion directe a {host}:{port}")),
    }
}

/// Etablit un tunnel `CONNECT` aupres du proxy parent puis renvoie le socket
/// TCP brut resultant, pret a relayer des octets vers `host:port` — que la
/// requete d'origine du client soit elle-meme un `CONNECT` (HTTPS) ou une
/// requete HTTP en clair (auquel cas l'appelant rejoue la requete telle
/// quelle a travers ce tunnel, exactement comme pour une connexion directe).
async fn connect_via_upstream(
    proxy: &UpstreamProxy,
    host: &str,
    port: u16,
) -> anyhow::Result<TcpStream> {
    let stream = TcpStream::connect(&proxy.addr)
        .await
        .context("connexion TCP au proxy parent")?;
    let mut reader = BufReader::new(stream);

    let mut request = format!("CONNECT {host}:{port} HTTP/1.1\r\nHost: {host}:{port}\r\n");
    if let Some(auth) = &proxy.auth_header {
        request.push_str("Proxy-Authorization: ");
        request.push_str(auth);
        request.push_str("\r\n");
    }
    request.push_str("Proxy-Connection: keep-alive\r\n\r\n");
    reader
        .write_all(request.as_bytes())
        .await
        .context("envoi du CONNECT au proxy parent")?;

    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .await
        .context("lecture de la reponse du proxy parent")?;
    let ok = status_line
        .split_whitespace()
        .nth(1)
        .map(|code| code.starts_with('2'))
        .unwrap_or(false);
    if !ok {
        bail!("proxy parent a refuse le CONNECT: {}", status_line.trim());
    }

    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            bail!("proxy parent a ferme la connexion avant la fin de la reponse CONNECT");
        }
        if line == "\r\n" {
            break;
        }
    }

    Ok(reader.into_inner())
}
