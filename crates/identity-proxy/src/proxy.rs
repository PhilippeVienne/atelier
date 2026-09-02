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
    /// Partagees et mutables : rechargees en tache de fond, donc lues a
    /// chaque connexion plutot que capturees une fois pour toutes.
    pub rules: crate::secrets::SharedRules,
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

    // Instantane des regles au moment de la requete : elles peuvent avoir
    // change depuis le demarrage (rechargement a chaud), et on ne garde pas
    // le verrou pendant tout le relai.
    let current_rules = config.rules.read().await.clone();
    let rule = rules::matching(&current_rules, &host);

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
        // La destination applique son propre delai d'inactivite, souvent
        // bien plus court que la duree de vie de la connexion cliente
        // (`net-proxy`, qui chaine ici toute une session d'agent — des
        // dizaines de minutes, avec de longues pauses de reflexion). Sans ce
        // controle, la requete suivante etait ecrite dans une socket morte :
        // `relai du corps de requete interrompu ... Broken pipe`, la fonction
        // rendait alors la main SANS reponse ni erreur HTTP — le client
        // restait bloque a attendre une reponse qui n'arriverait jamais,
        // pendant que net-proxy, lui, attendait une requete suivante qui
        // n'arriverait pas davantage (chacun des deux cotes de ce
        // relai attendait l'autre). Constate en Workshop reel le 2026-09-02,
        // un agent reste ainsi suspendu 45 minutes (garde-fou cote PM,
        // `pm_engine.exec_client.DEFAULT_TOTAL_TIMEOUT_S`) avant d'echouer.
        //
        // Le controle a lieu ICI, avant d'ecrire le moindre octet de la
        // requete courante — jamais apres un echec d'ecriture partiel, qui
        // rendrait un rejeu incorrect (le corps deja consomme depuis
        // `client` ne peut pas etre relu). `try_read` avec un tampon d'1
        // octet est l'idiome standard pour detecter un FIN sans bloquer :
        // `Ok(0)` signifie que la destination a ferme, toute autre valeur
        // (donnee reelle imprevue, ou `WouldBlock`) signifie une connexion
        // toujours vivante.
        if let Ok(0) = upstream.get_ref().try_read(&mut [0u8; 1]) {
            tracing::debug!(%peer, host, port, "upstream ferme entre deux requetes (keep-alive expire cote destination), reconnexion");
            upstream = match TcpStream::connect((host, port)).await {
                Ok(stream) => BufReader::new(stream),
                Err(err) => {
                    tracing::warn!(%peer, host, port, %err, "reconnexion a la destination echouee");
                    let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
                    return Ok(());
                }
            };
        }

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
    use std::sync::Arc;
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
            rules: Arc::new(RwLock::new(vec![rule])),
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

    /// Regression : la destination applique son propre delai d'inactivite,
    /// bien plus court que la duree de vie de la connexion cliente (une
    /// session d'agent dure des dizaines de minutes, avec de longues pauses
    /// de reflexion). Quand elle raccroche entre deux requetes,
    /// identity-proxy doit rouvrir une connexion plutot que d'ecrire dans
    /// une socket morte et abandonner en silence — sans quoi le client
    /// reste bloque a attendre une reponse qui n'arrivera jamais (constate
    /// en Workshop reel le 2026-09-02, 45 minutes avant que le garde-fou
    /// cote PM ne finisse par l'interrompre).
    #[tokio::test]
    async fn a_destination_that_hangs_up_between_requests_is_reconnected() {
        let destination = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_addr = destination.local_addr().unwrap();

        // Deux connexions distinctes attendues : la premiere est fermee par
        // la destination juste apres avoir repondu, comme le ferait uvicorn
        // au bout de son `timeout-keep-alive`.
        let captured = tokio::spawn(async move {
            let mut seen = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = destination.accept().await.unwrap();
                let mut buf = vec![0u8; 4096];
                let n = socket.read(&mut buf).await.unwrap();
                buf.truncate(n);
                socket
                    .write_all(
                        b"HTTP/1.1 200 OK
Content-Length: 0

",
                    )
                    .await
                    .unwrap();
                socket.shutdown().await.unwrap();
                seen.push(String::from_utf8(buf).unwrap());
            }
            seen
        });

        let config = ProxyConfig {
            rules: Arc::new(RwLock::new(Vec::new())),
            secrets: Arc::new(RwLock::new(HashMap::new())),
        };

        let proxy_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy_addr = proxy_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = proxy_listener.accept().await.unwrap();
            let _ = handle_connection(socket, peer, config).await;
        });

        let mut client = TcpStream::connect(proxy_addr).await.unwrap();
        let first = format!(
            "GET /first HTTP/1.1
Host: {destination_addr}

"
        );
        client.write_all(first.as_bytes()).await.unwrap();

        let mut response = vec![0u8; 4096];
        let n = client.read(&mut response).await.unwrap();
        assert!(n > 0, "la premiere reponse doit arriver");

        // Laisse la destination raccrocher avant d'envoyer la suite : c'est
        // exactement la situation d'une pause de reflexion du modele plus
        // longue que le keep-alive de la destination.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let second = format!(
            "GET /second HTTP/1.1
Host: {destination_addr}

"
        );
        client.write_all(second.as_bytes()).await.unwrap();
        client.shutdown().await.unwrap();

        let mut second_response = Vec::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            client.read_to_end(&mut second_response),
        )
        .await
        .expect(
            "la deuxieme requete doit obtenir une reponse avant timeout, pas un blocage silencieux",
        );
        read.unwrap();

        assert!(
            String::from_utf8_lossy(&second_response).starts_with("HTTP/1.1 200 OK"),
            "reponse inattendue : {:?}",
            String::from_utf8_lossy(&second_response)
        );

        let seen = captured.await.unwrap();
        assert_eq!(
            seen.len(),
            2,
            "les deux requetes doivent atteindre la destination, sur deux connexions distinctes"
        );
    }
}
