//! Lecture minimale d'une ligne de requete HTTP/1.x + ses en-tetes — copie
//! volontairement proche de `crates/net-proxy/src/http.rs` (pas de crate
//! partagee pour ce parseur intentionnellement minimal), avec un ajout
//! propre a identity-proxy : reconstruire la requete avec un en-tete
//! injecte/remplace, necessaire pour poser `Authorization` (ou equivalent)
//! avant de rejouer la requete vers la destination.

use anyhow::{bail, Context};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

#[derive(Clone)]
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

/// Ligne de statut + en-tetes d'une reponse HTTP/1.x — pendant de
/// [`RequestHead`], necessaire pour reutiliser une meme connexion pour
/// PLUSIEURS requetes (voir le commentaire de tete de `crate::proxy::forward`) :
/// sans decoder ou se termine une reponse, impossible de savoir quand la
/// prochaine requete du client peut etre relue sur la meme connexion.
pub struct ResponseHead {
    status_line: String,
    headers: Vec<(String, String)>,
}

impl ResponseHead {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = format!("{}\r\n", self.status_line).into_bytes();
        for (name, value) in &self.headers {
            out.extend_from_slice(format!("{name}: {value}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out
    }
}

/// `Ok(None)` si la connexion se ferme avant meme la ligne de statut (la
/// destination a ferme la connexion, ce qui arrive normalement en fin de
/// vie d'une connexion gardee ouverte).
pub async fn read_response_head<R>(reader: &mut R) -> anyhow::Result<Option<ResponseHead>>
where
    R: AsyncBufRead + Unpin,
{
    let mut status_line = String::new();
    if reader.read_line(&mut status_line).await? == 0 {
        return Ok(None);
    }
    let status_line = status_line.trim_end_matches(['\r', '\n']).to_string();
    if status_line.is_empty() {
        bail!("ligne de statut vide");
    }

    let mut headers = Vec::new();
    loop {
        let mut header_line = String::new();
        if reader.read_line(&mut header_line).await? == 0 {
            bail!("connexion fermee avant la fin des en-tetes de reponse");
        }
        let trimmed = header_line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            break;
        }
        if let Some((name, value)) = trimmed.split_once(':') {
            headers.push((name.trim().to_string(), value.trim().to_string()));
        }
    }

    Ok(Some(ResponseHead {
        status_line,
        headers,
    }))
}

fn headers_of<'a>(head_headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    head_headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

impl RequestHead {
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

impl ResponseHead {
    pub fn headers(&self) -> &[(String, String)] {
        &self.headers
    }
}

/// Longueur du corps annoncee par `Content-Length`, si l'en-tete est present
/// et valide.
pub fn content_length(headers: &[(String, String)]) -> Option<u64> {
    headers_of(headers, "Content-Length")?.trim().parse().ok()
}

/// `true` si le corps est encode en chunks (`Transfer-Encoding: chunked`) —
/// la RFC 7230 autorise plusieurs valeurs separees par des virgules, seule
/// la derniere importe pour savoir comment le corps se termine.
pub fn is_chunked(headers: &[(String, String)]) -> bool {
    headers_of(headers, "Transfer-Encoding")
        .map(|v| {
            v.split(',')
                .next_back()
                .is_some_and(|last| last.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false)
}

/// Recopie exactement `len` octets de `reader` vers `writer` — utilise pour
/// un corps `Content-Length` connu (requete ou reponse).
pub async fn copy_exact<R, W>(reader: &mut R, writer: &mut W, len: u64) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut limited = reader.take(len);
    tokio::io::copy(&mut limited, writer).await?;
    Ok(())
}

/// Recopie un corps `Transfer-Encoding: chunked` tel quel (taille de chunk,
/// donnees, CRLF, jusqu'au chunk terminal `0` et ses eventuels en-tetes de
/// fin) : ne dechiffre ni ne modifie le contenu, seulement assez de parsing
/// pour savoir ou le corps se termine et pouvoir relire la requete/reponse
/// suivante sur la meme connexion.
pub async fn copy_chunked_body<R, W>(reader: &mut R, writer: &mut W) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    loop {
        let mut size_line = String::new();
        if reader.read_line(&mut size_line).await? == 0 {
            bail!("connexion fermee au milieu d'un corps chunked");
        }
        writer.write_all(size_line.as_bytes()).await?;
        let size_str = size_line.trim().split(';').next().unwrap_or("").trim();
        let size = u64::from_str_radix(size_str, 16)
            .with_context(|| format!("taille de chunk invalide: {size_str:?}"))?;

        if size == 0 {
            // Chunk terminal : en-tetes de fin optionnels jusqu'a la ligne
            // vide (RFC 7230 §4.1.2), rejoues tels quels.
            loop {
                let mut trailer_line = String::new();
                if reader.read_line(&mut trailer_line).await? == 0 {
                    bail!("connexion fermee au milieu des en-tetes de fin (chunked)");
                }
                writer.write_all(trailer_line.as_bytes()).await?;
                if trailer_line.trim_end_matches(['\r', '\n']).is_empty() {
                    break;
                }
            }
            return Ok(());
        }

        copy_exact(reader, writer, size).await?;
        let mut crlf = [0u8; 2];
        reader.read_exact(&mut crlf).await?;
        writer.write_all(&crlf).await?;
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
