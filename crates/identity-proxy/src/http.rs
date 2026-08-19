//! Lecture minimale d'une ligne de requete HTTP/1.x + ses en-tetes — copie
//! volontairement proche de `crates/net-proxy/src/http.rs` (pas de crate
//! partagee pour ce parseur intentionnellement minimal), avec un ajout
//! propre a identity-proxy : reconstruire la requete avec un en-tete
//! injecte/remplace, necessaire pour poser `Authorization` (ou equivalent)
//! avant de rejouer la requete vers la destination.

use anyhow::{bail, Context};
use tokio::io::{AsyncBufRead, AsyncBufReadExt};

pub struct RequestHead {
    pub method: String,
    pub target: String,
    version: String,
    headers: Vec<(String, String)>,
}

impl RequestHead {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Serialise la requete telle que recue.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.render(&self.headers)
    }

    /// Serialise la requete avec `name: value` ajoute (ou remplace si deja
    /// present) — c'est le seul point ou identity-proxy modifie le contenu
    /// d'une requete, jamais ailleurs.
    pub fn with_injected_header(&self, name: &str, value: &str) -> Vec<u8> {
        let mut headers: Vec<(String, String)> = self
            .headers
            .iter()
            .filter(|(k, _)| !k.eq_ignore_ascii_case(name))
            .cloned()
            .collect();
        headers.push((name.to_string(), value.to_string()));
        self.render(&headers)
    }

    fn render(&self, headers: &[(String, String)]) -> Vec<u8> {
        let mut out = format!("{} {} {}\r\n", self.method, self.target, self.version).into_bytes();
        for (name, value) in headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out
    }
}

/// Lit la ligne de requete puis les en-tetes jusqu'a la ligne vide.
/// Retourne `Ok(None)` si le client ferme la connexion avant d'envoyer quoi
/// que ce soit (cas normal d'une connexion gardee ouverte puis relachee).
pub async fn read_request_head<R>(reader: &mut R) -> anyhow::Result<Option<RequestHead>>
where
    R: AsyncBufRead + Unpin,
{
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(None);
    }

    let mut parts = line.trim_end().splitn(3, ' ');
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts
        .next()
        .context("ligne de requete sans cible (request-target)")?
        .to_string();
    let version = parts.next().unwrap_or("HTTP/1.1").to_string();
    if method.is_empty() {
        bail!("ligne de requete vide ou malformee");
    }

    let mut headers = Vec::new();
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).await? == 0 {
            bail!("connexion fermee avant la fin des en-tetes");
        }
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
        version,
        headers,
    }))
}

/// Determine l'hote et le port de destination d'une requete (voir
/// `net-proxy::http::destination`, meme logique).
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
    async fn origin_form_uses_host_header() {
        let head = parse("GET /foo HTTP/1.1\r\nHost: api.example.org\r\n\r\n").await;
        assert_eq!(
            destination(&head).unwrap(),
            ("api.example.org".to_string(), 80)
        );
    }

    #[tokio::test]
    async fn injects_new_header() {
        let head = parse("GET /foo HTTP/1.1\r\nHost: api.example.org\r\n\r\n").await;
        let injected = head.with_injected_header("Authorization", "Bearer secret");
        let injected = String::from_utf8(injected).unwrap();
        assert_eq!(
            injected,
            "GET /foo HTTP/1.1\r\nHost: api.example.org\r\nAuthorization: Bearer secret\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn replaces_existing_header() {
        let head =
            parse("GET /foo HTTP/1.1\r\nHost: api.example.org\r\nAuthorization: old\r\n\r\n").await;
        let injected = head.with_injected_header("Authorization", "Bearer secret");
        let injected = String::from_utf8(injected).unwrap();
        assert!(injected.contains("Authorization: Bearer secret"));
        assert!(!injected.contains("old"));
    }
}
