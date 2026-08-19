//! Traitement d'une connexion cliente (la microVM, via son
//! `HTTP_PROXY`/`HTTPS_PROXY`) : verifie la destination contre l'allowlist,
//! journalise, puis relaie les octets si autorise.

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

use crate::allowlist;
use crate::http::{self, RequestHead};
use crate::internal::InternalRoutes;
use crate::upstream::UpstreamProxy;

const FORBIDDEN_RESPONSE: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY_RESPONSE: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone)]
pub struct EgressConfig {
    pub allowlist: Arc<Vec<String>>,
    pub upstream: Option<Arc<UpstreamProxy>>,
    pub no_proxy: Arc<Vec<String>>,
    pub internal: Arc<InternalRoutes>,
    /// `identity-proxy`, saut obligatoire pour tout le trafic egress
    /// autorise (hors alias internes eux-memes) quand configure : c'est le
    /// **seul** chemin par lequel la VM peut atteindre identity-proxy — au
    /// contraire de l'alias `identity-proxy` (adressage explicite par nom),
    /// ce chainage s'applique a n'importe quel hote autorise, pour que
    /// l'injection de credentials (decidee par identity-proxy lui-meme,
    /// selon ses propres regles) puisse s'appliquer sans que net-proxy ait
    /// besoin de connaitre ces regles. identity-proxy relaie tel quel les
    /// hotes sans regle correspondante, donc ce chainage est transparent
    /// dans le cas courant. Prend le pas sur `upstream` (proxy parent
    /// externe) : voir la note sur `tunnel`/`forward` plus bas.
    pub identity_proxy: Option<Arc<UpstreamProxy>>,
}

pub async fn handle_connection(
    client: TcpStream,
    peer: SocketAddr,
    config: EgressConfig,
) -> anyhow::Result<()> {
    let mut client = BufReader::new(client);

    let Some(head) = http::read_request_head(&mut client).await? else {
        return Ok(());
    };

    let (host, port) = match http::destination(&head) {
        Ok(dest) => dest,
        Err(err) => {
            tracing::warn!(%peer, %err, "requete sans destination exploitable");
            let _ = client.write_all(FORBIDDEN_RESPONSE).await;
            return Ok(());
        }
    };

    // Alias internes (identity-proxy, mcp-gateway) : toujours joignables,
    // sans passer par l'allowlist egress ni par un eventuel proxy parent —
    // ce ne sont pas de l'egress vers l'exterieur, voir `crate::internal`.
    if let Some((target_host, target_port)) = config.internal.resolve(&host) {
        tracing::info!(
            %peer,
            alias = host,
            target_host,
            target_port,
            method = %head.method,
            "route interne (bypass allowlist)"
        );
        return if head.method.eq_ignore_ascii_case("CONNECT") {
            tunnel(client, &target_host, target_port, peer, None, &[]).await
        } else {
            forward(client, &head, &target_host, target_port, peer, None, &[]).await
        };
    }

    if !allowlist::is_allowed(&host, &config.allowlist) {
        tracing::warn!(
            %peer,
            host,
            port,
            method = %head.method,
            allowed = false,
            "egress refuse (hors allowlist)"
        );
        let _ = client.write_all(FORBIDDEN_RESPONSE).await;
        return Ok(());
    }

    tracing::info!(
        %peer,
        host,
        port,
        method = %head.method,
        allowed = true,
        "egress autorise"
    );

    // identity-proxy est un saut obligatoire, pas une alternative au proxy
    // parent externe : s'il est configure, tout le trafic autorise y
    // transite (c'est lui qui, apres injection eventuelle, se charge de la
    // sortie reelle — voir `crates/identity-proxy`). Le proxy parent
    // externe (`config.upstream`) ne s'applique alors plus ici.
    //
    // Attention, les deux methodes ne se chainent pas de la meme facon :
    // - CONNECT : identity-proxy doit recevoir un `CONNECT host:port`
    //   comme n'importe quel proxy parent (son propre handler `tunnel()`
    //   attend exactement ca) — reutilise `upstream::connect`, deja fait
    //   pour le proxy parent externe.
    // - HTTP en clair : identity-proxy doit recevoir la requete telle
    //   quelle (forme absolue), PAS enveloppee dans un tunnel `CONNECT`
    //   supplementaire — sinon son propre handler la voit comme un
    //   `CONNECT` et la relaie en aveugle, sans jamais la reinterpreter
    //   pour y injecter un en-tete (bug constate en testant reellement :
    //   la regle d'injection ne se declenchait jamais). On se connecte
    //   donc en direct a identity-proxy et on lui rejoue `head.raw`
    //   verbatim, exactement comme pour un alias interne — c'est lui qui
    //   extrait le vrai hote de l'URI absolue.
    if head.method.eq_ignore_ascii_case("CONNECT") {
        let (next_hop, no_proxy): (Option<&UpstreamProxy>, &[String]) =
            match config.identity_proxy.as_deref() {
                Some(identity_proxy) => (Some(identity_proxy), &[]),
                None => (config.upstream.as_deref(), config.no_proxy.as_slice()),
            };
        tunnel(client, &host, port, peer, next_hop, no_proxy).await
    } else if let Some(identity_proxy) = config.identity_proxy.as_deref() {
        forward(
            client,
            &head,
            identity_proxy.host(),
            identity_proxy.port(),
            peer,
            None,
            &[],
        )
        .await
    } else {
        forward(
            client,
            &head,
            &host,
            port,
            peer,
            config.upstream.as_deref(),
            &config.no_proxy,
        )
        .await
    }
}

/// `CONNECT` : etablit un tunnel TCP opaque (le contenu est TLS, net-proxy
/// ne le dechiffre pas — seule la destination est controlee).
///
/// `upstream`/`no_proxy` sont `None`/vides pour une route interne
/// (identity-proxy, mcp-gateway) : ces destinations sont toujours jointes
/// en direct, jamais chainees via le proxy parent.
async fn tunnel(
    mut client: BufReader<TcpStream>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    upstream_proxy: Option<&UpstreamProxy>,
    no_proxy: &[String],
) -> anyhow::Result<()> {
    let mut upstream = match crate::upstream::connect(host, port, upstream_proxy, no_proxy).await {
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

/// Requete HTTP en clair (proxy explicite, pas de CONNECT) : rejoue la
/// requete telle que recue vers la destination (ou vers le proxy parent,
/// via un tunnel `CONNECT`, si configure), puis relaie le reste.
async fn forward(
    mut client: BufReader<TcpStream>,
    head: &RequestHead,
    host: &str,
    port: u16,
    peer: SocketAddr,
    upstream_proxy: Option<&UpstreamProxy>,
    no_proxy: &[String],
) -> anyhow::Result<()> {
    let mut upstream = match crate::upstream::connect(host, port, upstream_proxy, no_proxy).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion a la destination echouee");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    upstream
        .write_all(&head.raw)
        .await
        .context("envoi de la requete a la destination")?;

    let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));
    tracing::debug!(%peer, host, port, sent, received, "relai HTTP ferme");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;

    fn base_config(identity_proxy_addr: Option<String>) -> EgressConfig {
        EgressConfig {
            allowlist: Arc::new(vec!["127.0.0.1".to_string()]),
            upstream: None,
            no_proxy: Arc::new(Vec::new()),
            internal: Arc::new(InternalRoutes::default()),
            identity_proxy: identity_proxy_addr.map(|addr| {
                Arc::new(UpstreamProxy {
                    addr,
                    auth_header: None,
                })
            }),
        }
    }

    /// Regression : quand identity-proxy est configure comme saut
    /// obligatoire, une requete HTTP en clair doit lui arriver telle
    /// quelle (forme absolue), jamais enveloppee dans un `CONNECT` — sinon
    /// son propre handler ne la reinterprete jamais pour y injecter un
    /// en-tete (bug constate en testant reellement avant ce correctif).
    #[tokio::test]
    async fn plain_http_reaches_identity_proxy_unwrapped_in_connect() {
        let identity_proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let identity_proxy_addr = identity_proxy.local_addr().unwrap();

        let captured = tokio::spawn(async move {
            let (mut socket, _) = identity_proxy.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let config = base_config(Some(identity_proxy_addr.to_string()));
        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = client_listener.accept().await.unwrap();
            let _ = handle_connection(socket, peer, config).await;
        });

        let mut client = TcpStream::connect(client_addr).await.unwrap();
        client
            .write_all(b"GET http://127.0.0.1:9/foo HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
            .await
            .expect("identity-proxy doit recevoir quelque chose avant timeout")
            .unwrap();

        assert!(
            received.starts_with("GET http://127.0.0.1:9/foo"),
            "identity-proxy doit recevoir la requete GET telle quelle, pas un CONNECT : {received:?}"
        );
    }

    /// Le cas CONNECT (HTTPS), lui, doit bien chainer via un `CONNECT
    /// host:port` classique — identity-proxy ne peut de toute facon pas
    /// injecter dans du TLS opaque, autant garder le tunnel standard.
    #[tokio::test]
    async fn connect_reaches_identity_proxy_as_a_connect_tunnel() {
        let identity_proxy = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let identity_proxy_addr = identity_proxy.local_addr().unwrap();

        let captured = tokio::spawn(async move {
            let (mut socket, _) = identity_proxy.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let config = base_config(Some(identity_proxy_addr.to_string()));
        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = client_listener.accept().await.unwrap();
            let _ = handle_connection(socket, peer, config).await;
        });

        let mut client = TcpStream::connect(client_addr).await.unwrap();
        client
            .write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1:9\r\n\r\n")
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
            .await
            .expect("identity-proxy doit recevoir quelque chose avant timeout")
            .unwrap();

        assert!(
            received.starts_with("CONNECT 127.0.0.1:9"),
            "identity-proxy doit recevoir un CONNECT vers la vraie destination : {received:?}"
        );
    }
}
