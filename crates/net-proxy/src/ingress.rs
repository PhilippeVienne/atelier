//! Relais TCP entrant vers la microVM, pour les ports applicatifs exportes
//! aux AUTRES Workshops d'une meme campagne (spec docs/specs/16-escouades-
//! multi-agents-swarms-mesh.md §3.2, tache 12.1, `Workshop.spec.
//! exported_services`).
//!
//! Un port applicatif de l'agent (ex: une API backend sur :8080) n'existe
//! QUE dans le netns de la microVM — rien n'ecoute sur l'IP du POD pour ce
//! port (voir `crates/controller/src/guest_probe.rs`, "pas de port expose
//! directement sur l'IP du pod"). Le Service Kubernetes cree par le
//! controller pour un service exporte (`ensure_exported_service`) route
//! donc vers CE net-proxy, pas vers la microVM elle-meme : c'est ce module
//! qui fait le dernier saut, en clair, en TCP simple (pas le protocole
//! websocket multiplexe de `crate::portforward`, reserve au chemin externe
//! api-server -> net-proxy — ici les deux bouts sont dans le MEME pod,
//! aucune raison de passer par un websocket).
//!
//! Contrairement au port de controle (`crate::portforward`, jamais expose
//! au-dela du reseau du pod), ces ports SONT le point d'entree direct des
//! Workshops autorises de la campagne : net-proxy ne fait ici ni
//! authentification ni autorisation applicative — c'est la `NetworkPolicy`
//! generee par le controller (`crates/controller/src/reconcile.rs::
//! campaign_network_policy`) qui restreint, au niveau paquet, quels pods
//! peuvent seulement ETABLIR une connexion TCP vers ce port.

use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};

/// Parse `ATELIER_EXPORTED_SERVICES` (`name=port,name2=port2`, pose par le
/// controller a partir de `Workshop.spec.exported_services`) et lance un
/// relais TCP pour chaque port. Chaque relais est independant : l'echec de
/// l'un (port deja utilise, ex: collision avec un port reserve de net-proxy
/// lui-meme) n'empeche jamais le demarrage des autres.
pub fn spawn_from_env(vm_addr: Arc<str>) {
    let Ok(raw) = std::env::var("ATELIER_EXPORTED_SERVICES") else {
        return;
    };
    for entry in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((name, port)) = entry.split_once('=') else {
            tracing::error!(
                entry,
                "entree ATELIER_EXPORTED_SERVICES invalide, attendu nom=port"
            );
            continue;
        };
        let Ok(port) = port.parse::<u16>() else {
            tracing::error!(entry, "port invalide dans ATELIER_EXPORTED_SERVICES");
            continue;
        };
        let name = name.to_string();
        let vm_addr = Arc::clone(&vm_addr);
        tokio::spawn(async move {
            if let Err(err) = listen_and_relay(&name, port, vm_addr).await {
                tracing::error!(service = %name, port, %err, "relais de service exporte arrete");
            }
        });
    }
}

async fn listen_and_relay(name: &str, port: u16, vm_addr: Arc<str>) -> anyhow::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    tracing::info!(service = name, port, "service exporte en ecoute");
    accept_loop(listener, name, port, vm_addr).await
}

/// Boucle d'acceptation separee de `listen_and_relay` pour rester testable
/// avec un port ephemere (`TcpListener::bind(("127.0.0.1", 0))`) — le port
/// REEL a relayer (`upstream_port`) est alors distinct du port d'ecoute,
/// contrairement au cas de production ou net-proxy relaie le meme numero de
/// port des deux cotes (voir la doc du champ `WorkshopSpec::exported_services::port`).
async fn accept_loop(
    listener: TcpListener,
    name: &str,
    upstream_port: u16,
    vm_addr: Arc<str>,
) -> anyhow::Result<()> {
    loop {
        let (client, peer) = listener.accept().await?;
        let vm_addr = Arc::clone(&vm_addr);
        let name = name.to_string();
        tokio::spawn(async move {
            if let Err(err) = relay_one(client, &vm_addr, upstream_port).await {
                tracing::warn!(service = %name, %peer, port = upstream_port, %err, "relais d'une connexion echoue");
            }
        });
    }
}

async fn relay_one(mut client: TcpStream, vm_addr: &str, port: u16) -> anyhow::Result<()> {
    let mut upstream = TcpStream::connect((vm_addr, port)).await?;
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Bout en bout REEL : un vrai `TcpListener` cote "guest" (echo server),
    /// un vrai relais (`accept_loop`), un vrai client TCP — aucun mock du
    /// reseau. Ports ephemeres (`:0`) pour ne jamais entrer en collision
    /// avec un autre test execute en parallele.
    #[tokio::test]
    async fn relays_bytes_both_ways_between_a_real_client_and_a_real_guest_listener() {
        let guest_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let guest_port = guest_listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (mut guest_conn, _) = guest_listener.accept().await.unwrap();
            let mut buf = [0u8; 5];
            guest_conn.read_exact(&mut buf).await.unwrap();
            assert_eq!(&buf, b"hello");
            guest_conn.write_all(b"world").await.unwrap();
        });

        let ingress_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ingress_addr = ingress_listener.local_addr().unwrap();
        tokio::spawn(accept_loop(
            ingress_listener,
            "test-service",
            guest_port,
            Arc::from("127.0.0.1"),
        ));

        let mut client = TcpStream::connect(ingress_addr).await.unwrap();
        client.write_all(b"hello").await.unwrap();
        let mut response = [0u8; 5];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(&response, b"world");
    }

    /// Un "guest" pas encore pret (port fermé — cas normal juste apres le
    /// boot, systemd n'a pas encore demarre le service applicatif) ne doit
    /// jamais faire planter le relais : seule CETTE connexion echoue, le
    /// listener continue d'accepter les suivantes.
    #[tokio::test]
    async fn a_closed_guest_port_fails_only_that_one_connection() {
        let closed_port = {
            // Reserve un port ephemere puis le libere immediatement : quasi
            // toujours encore ferme au moment du test (rien ne s'y relie
            // entre-temps dans ce test isole).
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap().port()
        };

        let ingress_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ingress_addr = ingress_listener.local_addr().unwrap();
        tokio::spawn(accept_loop(
            ingress_listener,
            "test-service",
            closed_port,
            Arc::from("127.0.0.1"),
        ));

        let mut client = TcpStream::connect(ingress_addr).await.unwrap();
        // Le relais echoue a se connecter au guest et ferme la connexion
        // cliente sans jamais transmettre de donnees ni paniquer.
        let mut buf = [0u8; 1];
        let n = client.read(&mut buf).await.unwrap();
        assert_eq!(
            n, 0,
            "la connexion doit se fermer proprement, pas rester ouverte"
        );
    }
}
