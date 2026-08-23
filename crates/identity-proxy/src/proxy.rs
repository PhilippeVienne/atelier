//! Traitement d'une connexion cliente — jamais la VM directement,
//! toujours `net-proxy` qui chaine ici tout l'egress qu'il a deja juge
//! autorise (voir le commentaire de tete de `main.rs`) : injecte un
//! credential si une regle correspond a la destination, puis se connecte
//! directement a la destination — identity-proxy ne decide jamais
//! lui-meme de l'allowlist egress, ce role reste a net-proxy en amont
//! (separation des responsabilites : "quoi" vs "avec quelle identite").
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
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::http::{self, RequestHead};
use crate::rules::{self, InjectionRule, InjectionRuleExt};
use crate::secrets::SecretCache;

const BAD_GATEWAY_RESPONSE: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone)]
pub struct ProxyConfig {
    pub rules: Arc<Vec<InjectionRule>>,
    pub secrets: SecretCache,
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
    _config: &ProxyConfig,
) -> anyhow::Result<()> {
    let mut upstream = match TcpStream::connect((host, port)).await {
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

/// Relaie une (ou plusieurs, voir plus bas) requete(s) HTTP en clair vers la
/// destination, en injectant `rule` sur chacune.
///
/// Boucle sur la **meme** connexion TCP tant que le client (net-proxy, qui
/// chaine ici toute requete HTTP en clair deja autorisee — voir le
/// commentaire de tete de `main.rs`) y enchaine plusieurs requetes en
/// keep-alive : c'est le cas normal du protocole HTTP smart de Git
/// (`GET .../info/refs` suivi de `POST .../git-upload-pack` sur la meme
/// connexion), constate en pratique en testant un vrai `git clone` de bout
/// en bout contre Forgejo (voir docs/PROGRESS.md) — sans cette boucle, seule
/// la toute premiere requete de la connexion recevait l'en-tete injecte, les
/// suivantes n'etant plus que des octets relayes en aveugle
/// (`copy_bidirectional`), jamais rejouees par cette fonction : Forgejo
/// repondait alors `401 Unauthorized` a la deuxieme requete (le corps
/// `git-upload-pack`), faisant echouer le clone malgre une premiere requete
/// reussie.
///
/// Necessite de savoir ou se termine chaque requete/reponse pour continuer
/// a lire sur la connexion (`Content-Length` ou `Transfer-Encoding: chunked`,
/// voir `crate::http`) : si le framing d'une reponse est inconnu (ni l'un ni
/// l'autre — corps jusqu'a fermeture de connexion, ou reponse sans corps
/// type `204`/`304`), on bascule sur l'ancien comportement (relai
/// bidirectionnel aveugle jusqu'a fermeture), correct tant qu'il ne reste
/// plus qu'une requete a relayer sur cette connexion (cas le plus courant
/// pour ce genre de reponse).
async fn forward(
    mut client: BufReader<TcpStream>,
    head: &RequestHead,
    rule: Option<&InjectionRule>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    config: &ProxyConfig,
) -> anyhow::Result<()> {
    let mut upstream = match TcpStream::connect((host, port)).await {
        Ok(stream) => BufReader::new(stream),
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion a la destination echouee");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    let mut current_head = head.clone();
    loop {
        let raw = match rule {
            Some(rule) => match config.secrets.read().await.get(&rule.secret_cache_key()) {
                Some(value) => {
                    tracing::info!(%peer, host, header = %rule.header, "credential injecte");
                    current_head
                        .with_injected_header(&rule.header, &format!("{}{}", rule.prefix, value))
                }
                None => {
                    tracing::warn!(
                        %peer, host, secret_path = %rule.secret_path,
                        "regle d'injection trouvee mais secret pas encore disponible, requete relayee sans injection"
                    );
                    current_head.to_bytes()
                }
            },
            None => current_head.to_bytes(),
        };

        upstream
            .write_all(&raw)
            .await
            .context("envoi de la requete a la destination")?;

        if let Err(err) = relay_body(&mut client, &mut upstream, current_head.headers()).await {
            tracing::warn!(%peer, host, %err, "relai du corps de requete interrompu");
            return Ok(());
        }

        let response_head = match http::read_response_head(&mut upstream).await {
            Ok(Some(response_head)) => response_head,
            Ok(None) => return Ok(()), // destination fermee, fin normale
            Err(err) => {
                tracing::warn!(%peer, host, %err, "lecture de la reponse echouee");
                return Ok(());
            }
        };
        client
            .write_all(&response_head.to_bytes())
            .await
            .context("relai des en-tetes de reponse")?;

        let known_length = http::content_length(response_head.headers());
        let chunked = http::is_chunked(response_head.headers());
        match (known_length, chunked) {
            (Some(0), _) => {}
            (Some(len), _) => {
                if let Err(err) = http::copy_exact(&mut upstream, &mut client, len).await {
                    tracing::warn!(%peer, host, %err, "relai du corps de reponse interrompu");
                    return Ok(());
                }
            }
            (None, true) => {
                if let Err(err) = http::copy_chunked_body(&mut upstream, &mut client).await {
                    tracing::warn!(%peer, host, %err, "relai du corps de reponse (chunked) interrompu");
                    return Ok(());
                }
            }
            (None, false) => {
                let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
                    .await
                    .unwrap_or((0, 0));
                tracing::debug!(%peer, host, port, sent, received, "relai HTTP ferme (framing de reponse inconnu)");
                return Ok(());
            }
        }

        current_head = match http::read_request_head(&mut client).await {
            Ok(Some(next_head)) => next_head,
            Ok(None) => return Ok(()), // client a ferme, fin normale
            Err(err) => {
                tracing::debug!(%peer, host, %err, "fin de la connexion (pas de nouvelle requete valide)");
                return Ok(());
            }
        };
    }
}

/// Relaie le corps d'une requete (ou reponse, meme logique) dont le framing
/// est connu (`Content-Length` ou `chunked`) — `Ok(())` sans rien copier si
/// aucun des deux n'est present (pas de corps, ex: `GET`).
async fn relay_body<R, W>(
    reader: &mut R,
    writer: &mut W,
    headers: &[(String, String)],
) -> anyhow::Result<()>
where
    R: tokio::io::AsyncBufRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    if let Some(len) = http::content_length(headers) {
        if len > 0 {
            http::copy_exact(reader, writer, len).await?;
        }
    } else if http::is_chunked(headers) {
        http::copy_chunked_body(reader, writer).await?;
    }
    Ok(())
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
        // Signale qu'aucune autre requete ne suivra sur cette connexion
        // (demi-fermeture en ecriture uniquement) : depuis que `forward()`
        // boucle pour supporter plusieurs requetes par connexion (voir son
        // commentaire de tete), il attend une eventuelle requete suivante
        // apres avoir relaye la reponse — sans ce signal, `read_to_end`
        // ci-dessous et cette attente se bloqueraient mutuellement.
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        let _ = client.read_to_end(&mut response).await;

        let received = captured.await.unwrap();
        assert!(
            received.contains("Authorization: Bearer s3cr3t"),
            "{received}"
        );
    }
}
