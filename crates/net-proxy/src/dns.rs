//! Proxy DNS pour la microVM : meme allowlist que le proxy egress
//! (`Workshop.spec.egress_allowlist`), une seule source de verite pour la
//! politique reseau. Une requete pour un nom hors allowlist recoit
//! `REFUSED` sans jamais etre transmise a l'upstream — la VM ne doit pas
//! pouvoir se servir du DNS comme canal de decouverte ou d'exfiltration
//! pour des noms qu'elle ne pourra de toute facon pas joindre via
//! `net-proxy`.
//!
//! Parseur DNS volontairement minimal : assez pour lire le nom et le type
//! de l'unique question attendue, pas un parseur RFC 1035 complet. Le
//! message est ensuite relaye tel quel (octets bruts) vers l'upstream si
//! autorise — net-proxy ne reconstruit jamais de reponse lui-meme, sauf le
//! refus.
//!
//! Filtrage applique, dans l'ordre :
//! 1. `QDCOUNT` doit valoir exactement 1 — un resolveur stub normal
//!    n'envoie jamais plus d'une question par message ; en accepter
//!    plusieurs ouvrirait un contournement (nom autorise en premiere
//!    question, nom interdit en deuxieme, silencieusement relaye avec).
//! 2. Le type de la question ne doit pas etre `ANY` (255), `AXFR` (252) ou
//!    `IXFR` (251) : trois types concus pour retourner bien plus qu'un
//!    enregistrement, qui n'ont pas de raison d'etre poses par un
//!    resolveur applicatif normal et permettraient de recuperer plus
//!    d'information qu'un nom autorise individuellement ne devrait en
//!    exposer (jusqu'a un transfert de zone entier).
//! 3. Le nom doit correspondre a l'allowlist egress (`allowlist::is_allowed`,
//!    meme logique que pour le proxy HTTP(S) — une seule politique).

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use tokio::sync::RwLock;

use crate::allowlist;

const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_MESSAGE_SIZE: usize = 4096;

/// Types de question a refuser inconditionnellement, meme pour un nom
/// autorise (RFC 1035 §3.2.3 pour ANY, RFC 1035 §3.2.4/RFC 1995 pour les
/// transferts de zone).
const QTYPE_ANY: u16 = 255;
const QTYPE_AXFR: u16 = 252;
const QTYPE_IXFR: u16 = 251;
const FORBIDDEN_QTYPES: [u16; 3] = [QTYPE_ANY, QTYPE_AXFR, QTYPE_IXFR];

#[derive(Clone)]
pub struct DnsConfig {
    pub listen_addr: String,
    pub upstream: String,
    /// Mutable a chaud (voir `crate::admin`) : `request_egress` de
    /// `mcp-gateway` peut elargir cette liste sans redemarrer le process.
    pub allowlist: Arc<RwLock<Vec<String>>>,
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
            let snapshot = allowlist.read().await.clone();
            if let Some(response) = handle_query(&message, &snapshot, &upstream, client_addr).await {
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
    allowlist: Arc<RwLock<Vec<String>>>,
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

        let snapshot = allowlist.read().await.clone();
        let response = handle_query(&message, &snapshot, &upstream, peer)
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
/// nom ou type refuse produit toujours une reponse (`REFUSED`), jamais
/// `None`.
async fn handle_query(
    message: &[u8],
    allowlist: &[String],
    upstream: &str,
    peer: SocketAddr,
) -> Option<Vec<u8>> {
    let question = match parse_question(message) {
        Ok(question) => question,
        Err(err) => {
            tracing::warn!(%peer, %err, "requete DNS malformee");
            return None;
        }
    };

    if FORBIDDEN_QTYPES.contains(&question.qtype) {
        tracing::warn!(
            %peer,
            name = question.name,
            qtype = question.qtype,
            allowed = false,
            "DNS refuse (type de requete interdit)"
        );
        return Some(refusal(message));
    }

    if !allowlist::is_allowed(&question.name, allowlist) {
        tracing::warn!(%peer, name = question.name, allowed = false, "DNS refuse (hors allowlist)");
        return Some(refusal(message));
    }

    tracing::info!(%peer, name = question.name, allowed = true, "DNS relaye");
    match query_upstream(upstream, message).await {
        Ok(response) => Some(response),
        Err(err) => {
            tracing::warn!(%peer, name = question.name, %err, "requete DNS vers l'upstream echouee");
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

struct Question {
    name: String,
    qtype: u16,
}

/// Extrait le nom (en minuscules, notation pointee) et le type de l'unique
/// question attendue dans un message DNS — refuse tout message qui n'en
/// declare pas exactement une (`QDCOUNT != 1`, voir le commentaire de tete
/// du module). Ne gere pas la compression (RFC 1035 §4.1.4) : non
/// pertinent ici, la section Question d'une requete n'en contient jamais
/// (seules les sections Answer/Authority/Additional le peuvent, qu'on ne
/// parse pas).
fn parse_question(message: &[u8]) -> anyhow::Result<Question> {
    if message.len() < 12 {
        bail!("message DNS trop court pour un en-tete valide");
    }
    let qdcount = u16::from_be_bytes([message[4], message[5]]);
    if qdcount != 1 {
        bail!("QDCOUNT={qdcount} inattendu (une seule question par requete attendue)");
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

    let qtype_bytes = message
        .get(offset..offset + 2)
        .context("QTYPE tronque")?;
    let qtype = u16::from_be_bytes([qtype_bytes[0], qtype_bytes[1]]);

    Ok(Question {
        name: labels.join("."),
        qtype,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_query_with_type(id: u16, name: &str, qtype: u16) -> Vec<u8> {
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
        msg.extend_from_slice(&qtype.to_be_bytes());
        msg.extend_from_slice(&1u16.to_be_bytes()); // QCLASS = IN
        msg
    }

    fn build_query(id: u16, name: &str) -> Vec<u8> {
        build_query_with_type(id, name, 1) // QTYPE = A
    }

    fn build_query_with_qdcount(name: &str, qdcount: u16) -> Vec<u8> {
        let mut msg = Vec::new();
        msg.extend_from_slice(&1u16.to_be_bytes());
        msg.extend_from_slice(&[0x01, 0x00]);
        msg.extend_from_slice(&qdcount.to_be_bytes());
        msg.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            msg.push(label.len() as u8);
            msg.extend_from_slice(label.as_bytes());
        }
        msg.push(0);
        msg.extend_from_slice(&1u16.to_be_bytes());
        msg.extend_from_slice(&1u16.to_be_bytes());
        msg
    }

    #[test]
    fn parses_simple_name() {
        let query = build_query(0x1234, "github.com");
        assert_eq!(parse_question(&query).unwrap().name, "github.com");
    }

    #[test]
    fn parses_subdomain() {
        let query = build_query(1, "raw.githubusercontent.com");
        assert_eq!(
            parse_question(&query).unwrap().name,
            "raw.githubusercontent.com"
        );
    }

    #[test]
    fn parses_qtype() {
        let query = build_query_with_type(1, "github.com", 28); // AAAA
        assert_eq!(parse_question(&query).unwrap().qtype, 28);
    }

    #[test]
    fn rejects_short_message() {
        assert!(parse_question(&[0u8; 5]).is_err());
    }

    #[test]
    fn rejects_qdcount_other_than_one() {
        assert!(parse_question(&build_query_with_qdcount("github.com", 0)).is_err());
        assert!(parse_question(&build_query_with_qdcount("github.com", 2)).is_err());
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

    #[tokio::test]
    async fn forbidden_qtype_is_refused_even_for_an_allowed_name() {
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

        for qtype in FORBIDDEN_QTYPES {
            let query = build_query_with_type(9, "github.com", qtype);
            let allowlist = vec!["github.com".to_string()];
            let response = handle_query(
                &query,
                &allowlist,
                &upstream_addr.to_string(),
                "127.0.0.1:1".parse().unwrap(),
            )
            .await
            .unwrap_or_else(|| panic!("reponse REFUSED locale attendue pour qtype={qtype}"));
            assert_eq!(response[3], 5, "RCODE doit etre REFUSED pour qtype={qtype}");
        }

        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            !touched.load(std::sync::atomic::Ordering::SeqCst),
            "l'upstream ne doit jamais recevoir une requete a type interdit"
        );
    }
}
