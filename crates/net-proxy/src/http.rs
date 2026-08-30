//! Lecture minimale d'une ligne de requete HTTP/1.x + ses en-tetes, juste
//! assez pour determiner la destination (`CONNECT host:port`, ou une URI
//! absolue / un en-tete `Host` pour une requete HTTP en clair relayee).
//!
//! Volontairement pas un parseur HTTP complet : net-proxy ne fait pas
//! d'inspection de contenu, seulement du controle d'acces sur la
//! destination puis un relai d'octets brut.

use anyhow::{bail, Context};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt};

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

/// Comment se delimite le corps d'une requete — necessaire des lors qu'on
/// relaie plus d'une requete par connexion (keep-alive) : il faut savoir ou
/// finit l'une pour reconnaitre la ligne de requete de la suivante.
#[derive(Debug, PartialEq, Eq)]
pub enum BodyFraming {
    /// Pas de corps (`GET` sans `Content-Length`, `Content-Length: 0`).
    None,
    Length(u64),
    Chunked,
}

/// `Transfer-Encoding: chunked` prime sur `Content-Length` (RFC 9112 §6.1).
pub fn body_framing(head: &RequestHead) -> BodyFraming {
    if head
        .header("Transfer-Encoding")
        .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
    {
        return BodyFraming::Chunked;
    }
    match head
        .header("Content-Length")
        .and_then(|v| v.trim().parse::<u64>().ok())
    {
        Some(len) if len > 0 => BodyFraming::Length(len),
        _ => BodyFraming::None,
    }
}

/// Recopie le corps de la requete du client vers la destination, en
/// s'arretant exactement a sa fin pour laisser le lecteur positionne sur la
/// requete suivante.
pub async fn copy_body<R, W>(
    reader: &mut R,
    writer: &mut W,
    framing: BodyFraming,
) -> anyhow::Result<()>
where
    R: AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt;

    match framing {
        BodyFraming::None => {}
        BodyFraming::Length(len) => {
            let copied = tokio::io::copy(&mut reader.take(len), writer)
                .await
                .context("relai du corps de la requete")?;
            if copied != len {
                bail!("corps tronque : {copied} octets relayes sur {len} annonces");
            }
        }
        BodyFraming::Chunked => loop {
            let mut size_line = String::new();
            if reader.read_line(&mut size_line).await? == 0 {
                bail!("connexion fermee au milieu d'un corps chunked");
            }
            writer.write_all(size_line.as_bytes()).await?;
            // La ligne de taille peut porter des extensions (`; nom=valeur`),
            // qu'on relaie telles quelles mais qui ne font pas partie du
            // nombre hexadecimal a interpreter.
            let size_token = size_line.trim_end().split(';').next().unwrap_or_default();
            let size = u64::from_str_radix(size_token.trim(), 16)
                .with_context(|| format!("taille de chunk illisible : {size_token:?}"))?;
            if size > 0 {
                tokio::io::copy(&mut reader.take(size), writer).await?;
            }
            // Le CRLF qui suit chaque chunk (y compris celui de taille 0).
            let mut crlf = String::new();
            reader.read_line(&mut crlf).await?;
            writer.write_all(crlf.as_bytes()).await?;
            if size == 0 {
                break;
            }
        },
    }
    writer.flush().await?;
    Ok(())
}

/// Reecrit la ligne de requete en forme origine (`METHODE /chemin HTTP/1.1`,
/// en-tetes inchanges) a partir d'une requete en forme absolue
/// (`METHODE http://hote/chemin HTTP/1.1`) — pour les destinations de
/// confiance (alias internes `net-proxy`) dont on ne controle pas
/// l'implementation HTTP : certains serveurs ASGI/WSGI (constate en
/// pratique avec `uvicorn`, contrairement a `axum`/`hyper`, qui tolerent
/// les deux formes) ne savent pas parser une cible en forme absolue et
/// renvoient un `404` sur n'importe quel chemin. Pas d'effet si la requete
/// est deja en forme origine ou si c'est un `CONNECT` (jamais appelee dans
/// ce cas).
pub fn to_origin_form(head: &RequestHead) -> Vec<u8> {
    let Some(rest) = head
        .target
        .strip_prefix("http://")
        .or_else(|| head.target.strip_prefix("https://"))
    else {
        return head.raw.clone();
    };
    let path = match rest.find('/') {
        Some(idx) => &rest[idx..],
        None => "/",
    };

    let first_line_end = head
        .raw
        .iter()
        .position(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let first_line = String::from_utf8_lossy(&head.raw[..first_line_end]);
    // Preserve la version HTTP annoncee par le client (3e jeton de la ligne
    // de requete) plutot que d'en supposer une — seule la cible (2e jeton)
    // change, forme absolue -> forme origine.
    let version = first_line
        .trim_end()
        .splitn(3, ' ')
        .nth(2)
        .unwrap_or("HTTP/1.1");
    let mut rewritten = format!("{} {} {}\r\n", head.method, path, version).into_bytes();
    rewritten.extend_from_slice(&head.raw[first_line_end..]);
    rewritten
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
    async fn to_origin_form_rewrites_absolute_target() {
        let head = parse("GET http://llm-proxy/v1/models HTTP/1.1\r\nHost: llm-proxy\r\nAuthorization: Bearer x\r\n\r\n").await;
        let rewritten = to_origin_form(&head);
        assert_eq!(
            rewritten,
            b"GET /v1/models HTTP/1.1\r\nHost: llm-proxy\r\nAuthorization: Bearer x\r\n\r\n"
        );
    }

    #[tokio::test]
    async fn to_origin_form_defaults_to_root_path_without_trailing_slash() {
        let head = parse("GET http://llm-proxy HTTP/1.1\r\nHost: llm-proxy\r\n\r\n").await;
        assert!(to_origin_form(&head).starts_with(b"GET / HTTP/1.1\r\n"));
    }

    #[tokio::test]
    async fn to_origin_form_leaves_origin_form_requests_untouched() {
        let head = parse("GET /foo HTTP/1.1\r\nHost: llm-proxy\r\n\r\n").await;
        assert_eq!(to_origin_form(&head), head.raw);
    }

    #[tokio::test]
    async fn empty_connection_returns_none() {
        let mut reader = BufReader::new(&b""[..]);
        assert!(read_request_head(&mut reader).await.unwrap().is_none());
    }
}
