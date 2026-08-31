//! Traitement d'une connexion cliente (la microVM, via son
//! `HTTP_PROXY`/`HTTPS_PROXY`) : verifie la destination contre l'allowlist,
//! journalise, puis relaie les octets si autorise.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::RwLock;

use crate::allowlist;
use crate::http;
use crate::internal::InternalRoutes;
use crate::tls_sni;
use crate::upstream::UpstreamProxy;

/// Nombre de tentatives de `peek` avant d'abandonner un `ClientHello` qui ne
/// tient decidement pas dans un seul paquet — largement suffisant en
/// pratique (un `ClientHello` reel fait quelques centaines d'octets, un
/// seul segment TCP), voir `handle_transparent_tls_connection`.
const SNI_PEEK_ATTEMPTS: u32 = 20;
const SNI_PEEK_RETRY_DELAY: Duration = Duration::from_millis(50);

const FORBIDDEN_RESPONSE: &[u8] =
    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const BAD_GATEWAY_RESPONSE: &[u8] =
    b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

#[derive(Clone)]
pub struct EgressConfig {
    /// Mutable a chaud : `mcp-gateway` peut y ajouter un hote via
    /// `crate::admin` (`request_egress`), sans redemarrer net-proxy.
    pub allowlist: Arc<RwLock<Vec<String>>>,
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
    /// Alias `simulator` (voir `crate::admin`, tool MCP `enable_simulator`) :
    /// contrairement aux alias de `internal` (toujours actifs des le
    /// demarrage), celui-ci reste `None` (donc soumis a l'allowlist
    /// normale, qui le refuse puisque "simulator" n'est pas un domaine
    /// reel) tant que l'agent n'a pas explicitement demande le simulateur
    /// via MCP — un simulateur AWS local n'a pas vocation a etre joignable
    /// par defaut, seulement quand demande.
    pub simulator: Arc<RwLock<Option<(String, u16)>>>,
    /// Detection d'anomalie reseau (tache 4.2.4) : compte les refus d'egress
    /// et demande le confinement au-dela d'un seuil. `None` en test.
    pub anomaly: Option<Arc<crate::anomaly::AnomalyDetector>>,
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
            // Forme origine, pas verbatim : certains serveurs (constate
            // avec `uvicorn`/LiteLLM, alias `llm-proxy`) ne savent pas
            // parser une cible en forme absolue et renvoient 404 sur tout
            // — voir `http::to_origin_form`. Sans effet sur les alias deja
            // testes (axum/hyper, qui tolerent les deux formes).
            forward_rewriting(client, head, &target_host, target_port, peer).await
        };
    }

    if host.eq_ignore_ascii_case("simulator") {
        if let Some((target_host, target_port)) = config.simulator.read().await.clone() {
            tracing::info!(%peer, target_host, target_port, method = %head.method, "route simulateur (active par enable_simulator)");
            return if head.method.eq_ignore_ascii_case("CONNECT") {
                tunnel(client, &target_host, target_port, peer, None, &[]).await
            } else {
                let raw = http::to_origin_form(&head);
                forward(client, &raw, &target_host, target_port, peer, None, &[]).await
            };
        }
    }

    let allowed = {
        let list = config.allowlist.read().await;
        allowlist::is_allowed(&host, &list)
    };
    if !allowed {
        tracing::warn!(
            %peer,
            host,
            port,
            method = %head.method,
            allowed = false,
            "egress refuse (hors allowlist)"
        );
        if let Some(anomaly) = config.anomaly.as_ref() {
            anomaly.record_denial(&host).await;
        }
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
            &head.raw,
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
            &head.raw,
            &host,
            port,
            peer,
            config.upstream.as_deref(),
            &config.no_proxy,
        )
        .await
    }
}

/// Point d'entree pour le port TLS transparent (redirection iptables sur le
/// port 443, voir `crates/firecracker::network::NetworkSetup::enable_transparent_gateway`) :
/// contrairement au port egress classique, la connexion n'est jamais un
/// `CONNECT` — le guest croit parler directement au vrai serveur, il n'y a
/// donc aucune ligne de requete a lire pour connaitre la destination.
/// `TcpStream::peek` (`MSG_PEEK`) permet de lire les premiers octets du
/// `ClientHello` sans les consommer, pour en extraire le SNI (voir
/// `crate::tls_sni`) puis rejouer exactement ces memes octets vers la vraie
/// destination via [`tunnel_transparent`].
pub async fn handle_transparent_tls_connection(
    client: TcpStream,
    peer: SocketAddr,
    config: EgressConfig,
) -> anyhow::Result<()> {
    handle_transparent_tls_connection_on_port(client, peer, config, 443).await
}

/// Meme logique que [`handle_transparent_tls_connection`], port de
/// destination parametrable — separee uniquement pour permettre aux tests
/// de dialoguer avec un `TcpListener` de test (port choisi par l'OS)
/// plutot que le 443 fixe de la vraie redirection iptables.
async fn handle_transparent_tls_connection_on_port(
    client: TcpStream,
    peer: SocketAddr,
    config: EgressConfig,
    port: u16,
) -> anyhow::Result<()> {
    let mut buf = vec![0u8; 4096];
    let Some(host) = peek_sni(&client, &mut buf).await else {
        tracing::warn!(%peer, "TLS transparent : SNI illisible, connexion fermee");
        return Ok(());
    };

    let allowed = {
        let list = config.allowlist.read().await;
        allowlist::is_allowed(&host, &list)
    };
    if !allowed {
        tracing::warn!(%peer, host, port, allowed = false, "egress refuse (hors allowlist, TLS transparent)");
        if let Some(anomaly) = config.anomaly.as_ref() {
            anomaly.record_denial(&host).await;
        }
        return Ok(());
    }
    tracing::info!(%peer, host, port, allowed = true, "egress autorise (TLS transparent)");

    let (next_hop, no_proxy): (Option<&UpstreamProxy>, &[String]) =
        match config.identity_proxy.as_deref() {
            Some(identity_proxy) => (Some(identity_proxy), &[]),
            None => (config.upstream.as_deref(), config.no_proxy.as_slice()),
        };
    tunnel_transparent(
        BufReader::new(client),
        &host,
        port,
        peer,
        next_hop,
        no_proxy,
    )
    .await
}

/// Relit le debut de connexion via `peek` jusqu'a ce qu'un `ClientHello`
/// complet soit disponible ou qu'un delai raisonnable soit depasse. Ne
/// consomme jamais les octets (contrairement a `read`) : ils restent
/// disponibles pour le relai qui suit.
async fn peek_sni(client: &TcpStream, buf: &mut [u8]) -> Option<String> {
    for _ in 0..SNI_PEEK_ATTEMPTS {
        let n = client.peek(buf).await.ok()?;
        if n == 0 {
            return None;
        }
        if let Some(sni) = tls_sni::parse_sni(&buf[..n]) {
            return Some(sni);
        }
        if tls_sni::is_incomplete(&buf[..n]) {
            tokio::time::sleep(SNI_PEEK_RETRY_DELAY).await;
            continue;
        }
        return None;
    }
    None
}

/// `CONNECT` : etablit un tunnel TCP opaque (le contenu est TLS, net-proxy
/// ne le dechiffre pas — seule la destination est controlee).
///
/// `upstream`/`no_proxy` sont `None`/vides pour une route interne
/// (identity-proxy, mcp-gateway) : ces destinations sont toujours jointes
/// en direct, jamais chainees via le proxy parent.
async fn tunnel(
    client: BufReader<TcpStream>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    upstream_proxy: Option<&UpstreamProxy>,
    no_proxy: &[String],
) -> anyhow::Result<()> {
    tunnel_inner(client, host, port, peer, upstream_proxy, no_proxy, true).await
}

/// Meme relai que [`tunnel`], mais sans repondre "200 Connection
/// Established" au client : ce dernier n'a jamais envoye de `CONNECT` (voir
/// [`handle_transparent_tls_connection`]) et attend directement les octets
/// du serveur TLS reel.
async fn tunnel_transparent(
    client: BufReader<TcpStream>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    upstream_proxy: Option<&UpstreamProxy>,
    no_proxy: &[String],
) -> anyhow::Result<()> {
    tunnel_inner(client, host, port, peer, upstream_proxy, no_proxy, false).await
}

async fn tunnel_inner(
    mut client: BufReader<TcpStream>,
    host: &str,
    port: u16,
    peer: SocketAddr,
    upstream_proxy: Option<&UpstreamProxy>,
    no_proxy: &[String],
    send_connect_ack: bool,
) -> anyhow::Result<()> {
    let mut upstream = match crate::upstream::connect(host, port, upstream_proxy, no_proxy).await {
        Ok(stream) => stream,
        Err(err) => {
            tracing::warn!(%peer, host, port, %err, "connexion a la destination echouee");
            if send_connect_ack {
                let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            }
            return Ok(());
        }
    };

    if send_connect_ack {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .context("envoi de la reponse CONNECT")?;
    }

    let (sent, received) = tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .unwrap_or((0, 0));
    tracing::debug!(%peer, host, port, sent, received, "tunnel ferme");
    Ok(())
}

/// Requete HTTP en clair (proxy explicite, pas de CONNECT) : rejoue la
/// requete telle que recue vers la destination (ou vers le proxy parent,
/// via un tunnel `CONNECT`, si configure), puis relaie le reste.
/// Relai HTTP en clair vers un alias interne, en reecrivant en forme origine
/// **chaque** requete de la connexion, pas seulement la premiere.
///
/// Un client configure avec `HTTP_PROXY` (c'est le cas dans les Workshops,
/// voir `builder-vm-init`) garde sa connexion ouverte et envoie toutes ses
/// requetes suivantes en forme absolue sur la meme socket. Un simple
/// `copy_bidirectional` apres la premiere requete les relaie donc telles
/// quelles : `uvicorn` repondait `404` a partir du 2e echange, ce qui se
/// manifestait par un Claude Code qui repond au premier tour puis echoue
/// (`api_error_status: 404`, `num_turns: 2`) sans ecrire aucun fichier.
async fn forward_rewriting(
    client: BufReader<TcpStream>,
    first: http::RequestHead,
    host: &str,
    port: u16,
    peer: SocketAddr,
) -> anyhow::Result<()> {
    let upstream = match crate::upstream::connect(host, port, None, &[]).await {
        Ok(stream) => stream,
        Err(err) => {
            let mut client = client;
            tracing::warn!(%peer, host, port, %err, "connexion a la destination echouee");
            let _ = client.write_all(BAD_GATEWAY_RESPONSE).await;
            return Ok(());
        }
    };

    let (client_read, mut client_write) = tokio::io::split(client);
    let (mut upstream_read, mut upstream_write) = tokio::io::split(upstream);
    let mut client_read = BufReader::new(client_read);

    // Les reponses ne sont pas inspectees : elles repartent telles quelles,
    // en parallele des requetes (une reponse peut arriver pendant que le
    // client envoie deja la suivante).
    let downstream =
        tokio::spawn(async move { tokio::io::copy(&mut upstream_read, &mut client_write).await });

    let mut pending = Some(first);
    while let Some(current) = match pending.take() {
        Some(head) => Some(head),
        None => http::read_request_head(&mut client_read).await?,
    } {
        upstream_write
            .write_all(&http::to_origin_form(&current))
            .await
            .context("envoi de la requete a la destination")?;
        http::copy_body(
            &mut client_read,
            &mut upstream_write,
            http::body_framing(&current),
        )
        .await?;
    }

    // Fin des requetes : on ferme le sens montant pour que la destination
    // sache que plus rien n'arrive, et on laisse la reponse en cours finir.
    let _ = upstream_write.shutdown().await;
    let _ = downstream.await;
    tracing::debug!(%peer, host, port, "relai HTTP interne ferme");
    Ok(())
}

async fn forward(
    mut client: BufReader<TcpStream>,
    request_bytes: &[u8],
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
        .write_all(request_bytes)
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
            allowlist: Arc::new(RwLock::new(vec!["127.0.0.1".to_string()])),
            upstream: None,
            no_proxy: Arc::new(Vec::new()),
            internal: Arc::new(InternalRoutes::default()),
            identity_proxy: identity_proxy_addr.map(|addr| {
                Arc::new(UpstreamProxy {
                    addr,
                    auth_header: None,
                })
            }),
            simulator: Arc::new(RwLock::new(None)),
            anomaly: None,
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

    /// Regression : un client derriere `HTTP_PROXY` reutilise sa connexion
    /// et envoie TOUTES ses requetes en forme absolue. Chacune doit etre
    /// reecrite en forme origine, pas seulement la premiere — sans quoi
    /// `uvicorn`/LiteLLM repond `404` a partir du 2e echange (Claude Code
    /// repondait au 1er tour puis echouait sans ecrire de fichier).
    #[tokio::test]
    async fn every_keep_alive_request_to_an_internal_alias_is_rewritten() {
        let alias_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let alias_addr = alias_server.local_addr().unwrap();

        let captured = tokio::spawn(async move {
            let (socket, _) = alias_server.accept().await.unwrap();
            let mut reader = BufReader::new(socket);
            let mut seen = Vec::new();
            // Deux requetes successives sur la MEME connexion, corps compris.
            for _ in 0..2 {
                let head = crate::http::read_request_head(&mut reader)
                    .await
                    .unwrap()
                    .expect("requete attendue");
                let framing = crate::http::body_framing(&head);
                let mut body = Vec::new();
                crate::http::copy_body(&mut reader, &mut body, framing)
                    .await
                    .unwrap();
                seen.push((
                    head.target.clone(),
                    String::from_utf8_lossy(&body).to_string(),
                ));
            }
            seen
        });

        let mut internal = InternalRoutes::default();
        internal.insert_for_test(
            "llm-proxy",
            (alias_addr.ip().to_string(), alias_addr.port()),
        );
        let mut config = base_config(None);
        config.internal = Arc::new(internal);

        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = client_listener.accept().await.unwrap();
            let _ = handle_connection(socket, peer, config).await;
        });

        let mut client = TcpStream::connect(client_addr).await.unwrap();
        client
            .write_all(
                b"POST http://llm-proxy/v1/messages HTTP/1.1\r\nHost: llm-proxy\r\nContent-Length: 5\r\n\r\nfirst\
                  POST http://llm-proxy/v1/messages?beta=true HTTP/1.1\r\nHost: llm-proxy\r\nContent-Length: 6\r\n\r\nsecond",
            )
            .await
            .unwrap();

        let seen = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
            .await
            .expect("l'alias doit recevoir les deux requetes avant timeout")
            .unwrap();

        assert_eq!(
            seen,
            vec![
                ("/v1/messages".to_string(), "first".to_string()),
                ("/v1/messages?beta=true".to_string(), "second".to_string()),
            ],
            "les deux requetes du keep-alive doivent arriver en forme origine, corps intact"
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

    /// Le port HTTP transparent reutilise `handle_connection` tel quel :
    /// une requete origin-form avec `Host:` (jamais de `CONNECT`, jamais de
    /// cible en forme absolue) doit deja etre relayee correctement, exactement
    /// comme si elle etait arrivee sur le port egress classique.
    #[tokio::test]
    async fn transparent_http_relays_an_origin_form_request_without_connect() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_addr = destination.local_addr().unwrap();

        let captured = tokio::spawn(async move {
            let (mut socket, _) = destination.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });

        let config = EgressConfig {
            allowlist: Arc::new(RwLock::new(vec!["127.0.0.1".to_string()])),
            upstream: None,
            no_proxy: Arc::new(Vec::new()),
            internal: Arc::new(InternalRoutes::default()),
            identity_proxy: None,
            simulator: Arc::new(RwLock::new(None)),
            anomaly: None,
        };
        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = client_listener.accept().await.unwrap();
            let _ = handle_connection(socket, peer, config).await;
        });

        let mut client = TcpStream::connect(client_addr).await.unwrap();
        // Forme origine (chemin seul, pas d'URI absolue) + `Host:` : c'est
        // exactement ce qu'un client redirige par iptables envoie, puisqu'il
        // croit parler directement au serveur — jamais de `CONNECT`.
        client
            .write_all(
                format!("GET /probe HTTP/1.1\r\nHost: {destination_addr}\r\n\r\n").as_bytes(),
            )
            .await
            .unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
            .await
            .expect("la destination doit recevoir quelque chose avant timeout")
            .unwrap();

        assert!(
            received.starts_with("GET /probe"),
            "la requete origin-form doit etre relayee telle quelle : {received:?}"
        );
    }

    /// Le port TLS transparent doit lire le SNI d'un vrai `ClientHello`
    /// (sans jamais dechiffrer) et relayer les octets (deja peekes, donc
    /// rejoues) vers la destination si elle est autorisee.
    #[tokio::test]
    async fn transparent_tls_relays_when_sni_is_allowed() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_addr = destination.local_addr().unwrap();

        let captured = tokio::spawn(async move {
            let (mut socket, _) = destination.accept().await.unwrap();
            let mut buf = vec![0u8; 4096];
            let n = socket.read(&mut buf).await.unwrap();
            buf.truncate(n);
            buf
        });

        let config = EgressConfig {
            allowlist: Arc::new(RwLock::new(vec!["127.0.0.1".to_string()])),
            upstream: None,
            no_proxy: Arc::new(Vec::new()),
            internal: Arc::new(InternalRoutes::default()),
            identity_proxy: None,
            simulator: Arc::new(RwLock::new(None)),
            anomaly: None,
        };

        // Le SNI ("127.0.0.1") sert a l'allowlist ; la connexion sortante
        // reelle utilise le port de la destination de test (au lieu du 443
        // fixe de la redirection iptables reelle) via la variante
        // parametree, pour pouvoir dialoguer avec un `TcpListener` de test.
        let hello = tls_sni::build_client_hello("127.0.0.1");
        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = client_listener.accept().await.unwrap();
            let _ = handle_transparent_tls_connection_on_port(
                socket,
                peer,
                config,
                destination_addr.port(),
            )
            .await;
        });

        let mut client = TcpStream::connect(client_addr).await.unwrap();
        client.write_all(&hello).await.unwrap();

        let received = tokio::time::timeout(std::time::Duration::from_secs(5), captured)
            .await
            .expect("la destination doit recevoir le ClientHello rejoue avant timeout")
            .unwrap();

        assert_eq!(
            received, hello,
            "le ClientHello doit etre rejoue octet pour octet vers la vraie destination"
        );
    }

    /// La decision d'autoriser/refuser se prend bien sur le SNI extrait, pas
    /// sur autre chose : un hote hors allowlist ne doit jamais atteindre la
    /// destination, meme si la connexion TCP elle-meme reussirait.
    #[tokio::test]
    async fn transparent_tls_is_refused_when_sni_is_not_allowed() {
        let destination = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let destination_addr = destination.local_addr().unwrap();
        let never_reached = tokio::spawn(async move {
            let _ = destination.accept().await;
        });

        let config = EgressConfig {
            allowlist: Arc::new(RwLock::new(vec!["allowed.example".to_string()])),
            upstream: None,
            no_proxy: Arc::new(Vec::new()),
            internal: Arc::new(InternalRoutes::default()),
            identity_proxy: None,
            simulator: Arc::new(RwLock::new(None)),
            anomaly: None,
        };
        let client_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let client_addr = client_listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, peer) = client_listener.accept().await.unwrap();
            let _ = handle_transparent_tls_connection(socket, peer, config).await;
        });

        let mut client = TcpStream::connect(client_addr).await.unwrap();
        client
            .write_all(&tls_sni::build_client_hello("blocked.example"))
            .await
            .unwrap();
        // La connexion doit se fermer (refus) sans jamais que la
        // destination ne recoive quoi que ce soit.
        let mut buf = [0u8; 1];
        let n = client.read(&mut buf).await.unwrap_or(0);
        assert_eq!(n, 0, "la connexion doit etre fermee, pas relayee");

        never_reached.abort();
        let _ = destination_addr; // uniquement pour que le port soit reserve pendant le test
    }
}
