//! Lecture minimale d'une ligne de requete HTTP/1.x + ses en-tetes, juste
//! assez pour determiner la destination (`CONNECT host:port`, ou une URI
//! absolue / un en-tete `Host` pour une requete HTTP en clair relayee).
//!
//! Volontairement pas un parseur HTTP complet : net-proxy ne fait pas
//! d'inspection de contenu, seulement du controle d'acces sur la
//! destination puis un relai d'octets brut.

use anyhow::{bail, Context};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub struct RequestHead {
    pub method: String,
    pub target: String,
    /// Octets bruts de la ligne de requete + en-tetes (CRLF inclus, jusqu'a
    /// la ligne vide comprise), a rejouer tels quels vers la destination
    /// pour les requetes HTTP en clair relayees (pas pour CONNECT).
    pub raw: Vec<u8>,
    headers: Vec<(String, String)>,
}

impl RequestHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Lit la ligne de requete puis les en-tetes jusqu'a la ligne vide.
/// Retourne `Ok(None)` si le client ferme la connexion avant d'envoyer quoi
/// que ce soit (cas normal d'une connexion gardee ouverte puis relachee).
pub async fn read_request_head<R>(reader: &mut R) -> anyhow::Result<Option<RequestHead>>
where
    R: AsyncBufRead + Unpin,
{
    let mut raw = Vec::new();
    let mut line = String::new();

    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }
    raw.extend_from_slice(line.as_bytes());

    let mut parts = line.trim_end().splitn(3, ' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts
        .next()
        .context("ligne de requete sans cible (request-target)")?
        .to_string();
    if method.is_empty() {
        bail!("ligne de requete vide ou malformee");
    }

    let mut headers = Vec::new();
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).await? == 0 {
            bail!("connexion fermee avant la fin des en-tetes");
        }
        raw.extend_from_slice(header_line.as_bytes());
        let trimmed = header_line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    Ok(Some(RequestHead {
        method,
        target,
        raw,
        headers,
    }))
}

/// Determine l'hote et le port de destination d'une requete.
///
/// - `CONNECT host:port` (tunnel HTTPS) : cible directement `host:port`.
/// - `METHODE http://host[:port]/chemin HTTP/1.1` (proxy HTTP en clair,
///   forme absolue) : extrait l'hote depuis l'URI.
/// - `METHODE /chemin HTTP/1.1` (forme origine) : retombe sur l'en-tete
///   `Host`.
pub fn destination(head: &RequestHead) -> anyhow::Result<(String, u16)> {
    if head.method.eq_ignore_ascii_case("CONNECT") {
        return split_host_port(&head.target, 443);
    }

    if let Some(rest) = head
        .target
        .strip_prefix("http://")
        .or_else(|| head.target.strip_prefix("https://"))
    {
        let authority = rest.split(['/', '?']).next().unwrap_or(rest);
        return split_host_port(authority, 80);
    }

    let host_header = head
        .header("Host")
        .context("requete HTTP en forme origine sans en-tete Host")?;
    split_host_port(host_header, 80)
}

fn split_host_port(authority: &str, default_port: u16) -> anyhow::Result<(String, u16)> {
    if authority.is_empty() {
        bail!("autorite vide");
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            let port: u16 = port.parse().context("port invalide")?;
            Ok((host.to_string(), port))
        }
        _ => Ok((authority.to_string(), default_port)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    async fn parse(input: &str) -> RequestHead {
        let mut reader = BufReader::new(input.as_bytes());
        read_request_head(&mut reader)
            .await
            .unwrap()
            .expect("requete presente")
    }

    #[tokio::test]
    async fn connect_target() {
        let head = parse("CONNECT github.com:443 HTTP/1.1\r\nHost: github.com:443\r\n\r\n").await;
        assert_eq!(destination(&head).unwrap(), ("github.com".to_string(), 443));
    }

    #[tokio::test]
    async fn absolute_form_target() {
        let head = parse("GET http://example.org/foo HTTP/1.1\r\nHost: example.org\r\n\r\n").await;
        assert_eq!(destination(&head).unwrap(), ("example.org".to_string(), 80));
    }

    #[tokio::test]
    async fn origin_form_uses_host_header() {
        let head = parse("GET /foo HTTP/1.1\r\nHost: example.org:8080\r\n\r\n").await;
        assert_eq!(
            destination(&head).unwrap(),
            ("example.org".to_string(), 8080)
        );
    }

    #[tokio::test]
    async fn empty_connection_returns_none() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_request_head(&mut reader).await.unwrap().is_none());
    }
}
