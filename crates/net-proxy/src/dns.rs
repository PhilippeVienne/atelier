//! Proxy DNS pour la microVM : meme allowlist que le proxy egress
//! (`Workshop.spec.egress_allowlist`), une seule source de verite pour la
//! politique reseau. Une requete pour un nom hors allowlist recoit
//! `REFUSED` sans jamais etre transmise a l'upstream — la VM ne doit pas
//! pouvoir se servir du DNS comme canal de decouverte ou d'exfiltration
//! pour des noms qu'elle ne pourra de toute facon pas joindre via
//! `net-proxy`.
//!
//! Parseur DNS volontairement minimal : assez pour lire le nom de la
//! premiere question (`QDCOUNT` suppose == 1, cas normal d'un resolveur
//! stub), pas un parseur RFC 1035 complet. Le message est ensuite relaye
//! tel quel (octets bruts) vers l'upstream si autorise — net-proxy ne
//! reconstruit jamais de reponse lui-meme, sauf le refus.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::allowlist;

const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE_SIZE: usize = 4096;

#[derive(Clone)]
pub struct DnsConfig {
    pub listen_addr: String,
    pub upstream: String,
    pub allowlist: Arc<Vec<String>>,
}

pub async fn run(config: DnsConfig) -> anyhow::Result<()> {
    tracing::info!(
        listen_addr = %config.listen_addr,
        upstream = %config.upstream,
        "proxy DNS en ecoute (UDP + TCP)"
    );
    let udp = tokio::spawn(run_udp(config.clone()));
    let tcp = tokio::spawn(run_tcp(config));
    tokio::select! {
        res = udp => res.context("tache DNS UDP")??,
        res = tcp => res.context("tache DNS TCP")??,
    }
    Ok(())
}

/// Nameserver du fichier `resolv.conf` local, sinon un resolveur public par
/// defaut — appele une seule fois au demarrage, avant que la VM n'existe.
pub fn default_upstream() -> String {
    if let Ok(contents) = std::fs::read_to_string("/etc/resolv.conf") {
        for line in contents.lines() {
            if let Some(ip) = line.strip_prefix("nameserver") {
                let ip = ip.trim();
                if !ip.is_empty() {
                    return format!("{ip}:53");
                }
            }
        }
    }
    "1.1.1.1:53".to_string()
}

async fn run_udp(config: DnsConfig) -> anyhow::Result<()> {
    let listen = Arc::new(UdpSocket::bind(&config.listen_addr).await?);
    let mut buf = vec![0u8; MAX_MESSAGE_SIZE];
    loop {
        let (len, client_addr) = listen.recv_from(&mut buf).await?;
        let message = buf[..len].to_vec();
        let listen = Arc::clone(&listen);
        let allowlist = Arc::clone(&config.allowlist);
        let upstream = config.upstream.clone();
        tokio::spawn(async move {
            if let Some(response) = handle_query(&message, &allowlist, &upstream, client_addr).await {
                let _ = listen.send_to(&response, client_addr).await;
            }
        });
    }
}

async fn run_tcp(config: DnsConfig) -> anyhow::Result<()> {
    let listener = TcpListener::bind(&config.listen_addr).await?;
    loop {
        let (socket, peer) = listener.accept().await?;
        let allowlist = Arc::clone(&config.allowlist);
        let upstream = config.upstream.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_tcp_connection(socket, allowlist, upstream, peer).await {
                tracing::debug!(%peer, %err, "connexion DNS (TCP) terminee");
            }
        });
    }
}

async fn handle_tcp_connection(
    mut socket: TcpStream,
    allowlist: Arc<Vec<String>>,
    upstream: String,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    loop {
        let mut len_buf = [0u8; 2];
        if socket.read_exact(&mut len_buf).await.is_err() {
            return Ok(()); // client a ferme la connexion, cas normal
        }
        let len = u16::from_be_bytes(len_buf) as usize;
        let mut message = vec![0u8; len];
        socket.read_exact(&mut message).await?;

        let response = handle_query(&message, &allowlist, &upstream, peer)
            .await
            .unwrap_or_else(|| refusal(&message));

        socket
            .write_all(&(response.len() as u16).to_be_bytes())
            .await?;
        socket.write_all(&response).await?;
    }
}

/// Verifie l'allowlist puis relaie a l'upstream si autorise. `None`
/// seulement en cas d'echec de l'upstream lui-meme (deja journalise) — un
/// nom hors allowlist produit toujours une reponse (`REFUSED`), jamais
/// `None`.
async fn handle_query(
    message: &[u8],
    allowlist: &[String],
    upstream: &str,
    peer: SocketAddr,
) -> Option<Vec<u8>> {
    let name = match parse_question_name(message) {
        Ok(name) => name,
        Err(err) => {
            tracing::warn!(%peer, %err, "requete DNS malformee");
            return None;
        }
    };

    if !allowlist::is_allowed(&name, allowlist) {
        tracing::warn!(%peer, name, allowed = false, "DNS refuse (hors allowlist)");
        return Some(refusal(message));
    }

    tracing::info!(%peer, name, allowed = true, "DNS relaye");
    match query_upstream(upstream, message).await {
        Ok(response) => Some(response),
        Err(err) => {
            tracing::warn!(%peer, name, %err, "requete DNS vers l'upstream echouee");
            None
        }
    }
}

async fn query_upstream(upstream: &str, message: &[u8]) -> anyhow::Result<Vec<u8>> {
    let socket = UdpSocket::bind("0.0.0.0:0")
        .await
        .context("bind socket UDP ephemere")?;
    socket
        .connect(upstream)
        .await
        .context("connexion a l'upstream DNS")?;
    socket
        .send(message)
        .await
        .context("envoi de la requete a l'upstream DNS")?;

    let mut buf = vec![0u8; MAX_MESSAGE_SIZE];
    let n = tokio::time::timeout(UPSTREAM_TIMEOUT, socket.recv(&mut buf))
        .await
        .context("upstream DNS: timeout")?
        .context("upstream DNS: lecture de la reponse")?;
    Ok(buf[..n].to_vec())
}

/// Construit une reponse `REFUSED` (RCODE 5) en reutilisant tel quel
/// l'en-tete et la question de la requete d'origine.
fn refusal(message: &[u8]) -> Vec<u8> {
    let mut response = message.to_vec();
    if response.len() >= 4 {
        response[2] |= 0b1000_0000; // QR = 1 (reponse)
        response[3] = 0b0000_0101; // RA = 0, RCODE = 5 (REFUSED)
    }
    response
}

/// Extrait le nom (en minuscules, notation pointee) de la premiere
/// question d'un message DNS. Ne gere pas la compression (RFC 1035
/// §4.1.4) : non pertinent ici, la section Question d'une requete n'en
/// contient jamais (seules les sections Answer/Authority/Additional le
/// peuvent, qu'on ne parse pas).
fn parse_question_name(message: &[u8]) -> anyhow::Result<String> {
    if message.len() < 12 {
        bail!("message DNS trop court pour un en-tete valide");
    }
    let mut labels = Vec::new();
    let mut offset = 12usize;
    loop {
        let len = *message
            .get(offset)
            .context("QNAME tronque (longueur de label manquante)")? as usize;
        offset += 1;
        if len == 0 {
            break;
        }
        if len & 0xC0 != 0 {
            bail!("compression DNS inattendue dans la section Question");
        }
        let label = message
            .get(offset..offset + len)
            .context("QNAME tronque (label incomplet)")?;
        labels.push(String::from_utf8_lossy(label).to_ascii_lowercase());
        offset += len;
    }
    if labels.is_empty() {
        bail!("QNAME vide");
    }
    Ok(labels.join("."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_query(id: u16, name: &str) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&id.to_be_bytes());
        msg.extend_from_slice(&[0x01, 0x00]); // flags: RD=1
        msg.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
        msg.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // AN/NS/AR COUNT = 0
        for label in name.split('.') {
            msg.push(label.len() as u8);
            msg.extend_from_slice(label.as_bytes());
        }
        msg.push(0); // fin du QNAME
        msg.extend_from_slice(&1u16.to_be_bytes()); // QTYPE = A
        msg.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
        msg
    }

    #[test]
    fn parses_simple_name() {
        let query = build_query(0x1234, "github.com");
        assert_eq!(parse_question_name(&query).unwrap(), "github.com");
    }

    #[test]
    fn parses_subdomain() {
        let query = build_query(1, "raw.githubusercontent.com");
        assert_eq!(
            parse_question_name(&query).unwrap(),
            "raw.githubusercontent.com"
        );
    }

    #[test]
    fn rejects_short_message() {
        assert!(parse_question_name(&[0u8; 5]).is_err());
    }

    #[test]
    fn refusal_sets_qr_and_rcode() {
        let query = build_query(0xabcd, "denied.example");
        let response = refusal(&query);
        assert_eq!(&response[0..2], &0xabcdu16.to_be_bytes());
        assert_eq!(response[2] & 0b1000_0000, 0b1000_0000, "QR doit etre a 1");
        assert_eq!(response[3], 5, "RCODE doit etre REFUSED (5)");
    }

    #[tokio::test]
    async fn allowed_query_is_relayed_to_upstream() {
        let fake_upstream = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = fake_upstream.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = vec![0u8; 512];
            let (len, from) = fake_upstream.recv_from(&mut buf).await.unwrap();
            let mut canned_response = buf[..len].to_vec();
            canned_response[2] |= 0b1000_0000; // simule une reponse
            fake_upstream.send_to(&canned_response, from).await.unwrap();
        });

        let query = build_query(42, "github.com");
        let allowlist = vec!["github.com".to_string()];
        let response = handle_query(
            &query,
            &allowlist,
            &upstream_addr.to_string(),
            "127.0.0.1:1".parse().unwrap(),
        )
        .await
        .expect("reponse relayee");
        assert_eq!(&response[0..2], &42u16.to_be_bytes());
        assert_eq!(response[2] & 0b1000_0000, 0b1000_0000);
    }

    #[tokio::test]
    async fn denied_query_never_reaches_upstream() {
        let fake_upstream = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let upstream_addr = fake_upstream.local_addr().unwrap();
        let touched = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let touched_clone = Arc::clone(&touched);
        tokio::spawn(async move {
            let mut buf = vec![0u8; 512];
            if fake_upstream.recv_from(&mut buf).await.is_ok() {
                touched_clone.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        });

        let query = build_query(7, "evil.example");
        let allowlist = vec!["github.com".to_string()];
        let response = handle_query(
            &query,
            &allowlist,
            &upstream_addr.to_string(),
            "127.0.0.1:1".parse().unwrap(),
        )
        .await
        .expect("reponse REFUSED locale");
        assert_eq!(response[3], 5, "RCODE doit etre REFUSED");

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !touched.load(std::sync::atomic::Ordering::SeqCst),
            "l'upstream ne doit jamais recevoir une requete hors allowlist"
        );
    }
}
