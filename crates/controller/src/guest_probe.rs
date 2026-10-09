//! Sonde legere pour verifier qu'un port du guest (typiquement `ttyd`,
//! canari le plus rapide a demarrer parmi les services embarques) repond
//! reellement, avant de marquer un `Workshop` `Running` — le pod
//! Kubernetes du parent passe `Running` des que le kernel de la microVM a
//! booté, bien avant que systemd, a l'interieur du guest, ait fini de
//! demarrer ce service (constate en pratique : premier clic sur
//! "Terminal"/"Ouvrir VS Code" tombant sur un port pas encore ouvert).
//!
//! Un port n'est tenu pour pret que sur **preuve positive** : des octets
//! recus de l'invite (tache 14.14, spec 19 §3.8). La sonde d'origine
//! concluait « ouvert » du seul silence de `net-proxy`, qui ne signale un
//! echec que quand la connexion est REFUSEE. Or tant que la microVM n'existe
//! pas encore (reprise en cours, instantane en telechargement depuis S3),
//! personne ne repond au SYN : `net-proxy` attend, se tait, et le Workshop
//! passait `Running` jusqu'a 25 s avant d'etre joignable — le premier exec
//! echouait alors avec `connexion SSH echouee: Disconnected`.
//!
//! Reutilise le protocole `portforward` de `net-proxy`
//! (`crates/net-proxy/src/portforward.rs`), le seul chemin reseau vers un
//! port du guest — pas de port expose directement sur l'IP du pod, voir
//! `crates/api-server/src/vscode.rs::open_forwarded_tcp_stream`.

use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// Timeout total, y compris l'etablissement de la connexion WebSocket vers
/// le control-plane `net-proxy` du pod.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Une fois connecte : combien de temps laisser a l'invite pour repondre.
/// `sshd` envoie sa banniere des l'acceptation de la connexion et `ttyd`
/// repond a une requete HTTP en quelques millisecondes ; un port pas encore
/// pret fait simplement attendre la reconciliation suivante.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(2);

/// Ce qu'on attend d'un port de l'invite pour le tenir pour pret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expect {
    /// Le service parle le premier : `sshd` envoie `SSH-2.0-...` sans rien
    /// attendre du client.
    SshBanner,
    /// Le service attend une requete : on envoie un `GET /` et n'importe
    /// quelle reponse HTTP convient, y compris un refus d'authentification
    /// (`ttyd` est derriere un mot de passe de session).
    HttpResponse,
}

impl Expect {
    fn request(self) -> Option<&'static [u8]> {
        match self {
            Expect::SshBanner => None,
            Expect::HttpResponse => Some(b"GET / HTTP/1.0\r\n\r\n"),
        }
    }

    fn prefix(self) -> &'static [u8] {
        match self {
            Expect::SshBanner => b"SSH-",
            Expect::HttpResponse => b"HTTP/",
        }
    }
}

/// `true` si le service attendu repond sur le port TCP `remote_port` du
/// guest, derriere `pod_ip` — `false` dans tous les autres cas
/// (control-plane `net-proxy` injoignable, connexion refusee, microVM pas
/// encore la, reponse d'un autre protocole), jamais de panique ni d'erreur
/// remontee : c'est une sonde de readiness, un port pas encore pret est
/// l'etat normal juste apres le boot, pas une erreur a traiter.
pub async fn guest_port_answers(
    pod_ip: &str,
    net_proxy_control_port: u16,
    remote_port: u16,
    expect: Expect,
) -> bool {
    let url = format!("ws://{pod_ip}:{net_proxy_control_port}/portforward?ports=tcp:{remote_port}");

    let connected =
        tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&url)).await;
    let Ok(Ok((mut ws, _response))) = connected else {
        return false;
    };

    let answered = tokio::time::timeout(ANSWER_TIMEOUT, async {
        // Un seul port demande (`ports=tcp:{remote_port}`) : index 0, donc
        // canal de donnees 0 et canal d'erreur 1 (`channel_byte` cote
        // net-proxy).
        const DATA_CHANNEL: u8 = 0;
        if let Some(request) = expect.request() {
            let mut frame = vec![DATA_CHANNEL];
            frame.extend_from_slice(request);
            if ws.send(Message::Binary(frame.into())).await.is_err() {
                return false;
            }
        }
        // Les premiers octets peuvent arriver en plusieurs trames.
        let mut received = Vec::new();
        while let Some(Ok(message)) = ws.next().await {
            let Message::Binary(data) = message else {
                continue;
            };
            match data.split_first() {
                Some((&DATA_CHANNEL, payload)) => received.extend_from_slice(payload),
                // Canal d'erreur : connexion refusee cote guest (port pas
                // encore ouvert par systemd).
                _ => return false,
            }
            let prefix = expect.prefix();
            if received.len() >= prefix.len() {
                return received.starts_with(prefix);
            }
        }
        false
    })
    .await
    // Silence : ni refus ni reponse, donc pas de preuve. C'est le cas d'une
    // microVM qui n'existe pas encore.
    .unwrap_or(false);

    let _ = ws.close(None).await;
    answered
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    /// Ce que fait le faux `net-proxy` une fois le WebSocket ouvert.
    #[derive(Clone, Copy)]
    enum Guest {
        /// Ne dit rien : la microVM n'existe pas encore.
        Silent,
        /// Connexion refusee, signalee sur le canal d'erreur.
        Refused,
        /// Envoie ces octets d'emblee sur le canal de donnees, en deux
        /// trames.
        Speaks(&'static [u8]),
        /// Attend une requete sur le canal de donnees puis repond.
        Replies(&'static [u8]),
    }

    fn frame(channel: u8, payload: &[u8]) -> Message {
        let mut data = vec![channel];
        data.extend_from_slice(payload);
        Message::Binary(data.into())
    }

    async fn fake_net_proxy(guest: Guest) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            match guest {
                Guest::Silent => {}
                Guest::Refused => {
                    ws.send(frame(1, b"Connection refused")).await.ok();
                }
                Guest::Speaks(bytes) => {
                    let (first, rest) = bytes.split_at(2);
                    ws.send(frame(0, first)).await.ok();
                    ws.send(frame(0, rest)).await.ok();
                }
                Guest::Replies(bytes) => {
                    if let Some(Ok(Message::Binary(request))) = ws.next().await {
                        assert_eq!(request[0], 0, "la requete part sur le canal de donnees");
                        assert!(request[1..].starts_with(b"GET / HTTP/1.0"));
                        ws.send(frame(0, bytes)).await.ok();
                    }
                }
            }
            // Garde la connexion ouverte jusqu'a ce que la sonde la ferme.
            while let Some(Ok(_)) = ws.next().await {}
        });
        port
    }

    async fn probe(guest: Guest, expect: Expect) -> bool {
        let port = fake_net_proxy(guest).await;
        guest_port_answers("127.0.0.1", port, 2222, expect).await
    }

    #[tokio::test]
    async fn a_silent_guest_is_not_ready() {
        // Le defaut d'origine : ce cas rendait `true`.
        assert!(!probe(Guest::Silent, Expect::SshBanner).await);
        assert!(!probe(Guest::Silent, Expect::HttpResponse).await);
    }

    #[tokio::test]
    async fn a_refused_connection_is_not_ready() {
        assert!(!probe(Guest::Refused, Expect::SshBanner).await);
        assert!(!probe(Guest::Refused, Expect::HttpResponse).await);
    }

    #[tokio::test]
    async fn an_ssh_banner_proves_sshd_is_up() {
        assert!(
            probe(
                Guest::Speaks(b"SSH-2.0-OpenSSH_9.2p1\r\n"),
                Expect::SshBanner
            )
            .await
        );
    }

    #[tokio::test]
    async fn any_http_response_proves_the_terminal_is_up() {
        let unauthorized = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
        assert!(probe(Guest::Replies(unauthorized), Expect::HttpResponse).await);
    }

    #[tokio::test]
    async fn another_protocol_on_the_port_is_not_ready() {
        assert!(!probe(Guest::Speaks(b"220 smtp ready\r\n"), Expect::SshBanner).await);
        assert!(!probe(Guest::Replies(b"SSH-2.0-x\r\n"), Expect::HttpResponse).await);
    }

    #[tokio::test]
    async fn an_unreachable_net_proxy_is_not_ready() {
        // Port ferme : personne n'ecoute.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(!guest_port_answers("127.0.0.1", port, 2222, Expect::SshBanner).await);
    }
}
