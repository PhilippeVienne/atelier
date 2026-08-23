//! Sonde legere pour verifier qu'un port du guest (typiquement `ttyd`,
//! canari le plus rapide a demarrer parmi les services embarques) repond
//! reellement, avant de marquer un `Workshop` `Running` — le pod
//! Kubernetes du parent passe `Running` des que le kernel de la microVM a
//! booté, bien avant que systemd, a l'interieur du guest, ait fini de
//! demarrer ce service (constate en pratique : premier clic sur
//! "Terminal"/"Ouvrir VS Code" tombant sur un port pas encore ouvert).
//!
//! Reutilise le protocole `portforward` de `net-proxy`
//! (`crates/net-proxy/src/portforward.rs`), le seul chemin reseau vers un
//! port du guest — pas de port expose directement sur l'IP du pod, voir
//! `crates/api-server/src/vscode.rs::open_forwarded_tcp_stream`.

use futures::StreamExt;
use std::time::Duration;
use tokio_tungstenite::tungstenite::Message;

/// Timeout total, y compris l'etablissement de la connexion WebSocket vers
/// le control-plane `net-proxy` du pod.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
/// Une fois connecte : combien de temps laisser a `net-proxy` pour signaler
/// un echec de connexion TCP vers le guest sur le canal d'erreur dedie
/// (`report_error`, quasi instantane en pratique des qu'un `connect()`
/// echoue) avant de considerer que le silence signifie que le port est bien
/// ouvert — une connexion TCP reussie ne genere elle-meme aucun message
/// immediat (`open_port` ne renvoie rien tant qu'aucune donnee ne transite).
const SILENCE_MEANS_OPEN: Duration = Duration::from_millis(750);

/// `true` si le port TCP `remote_port` du guest, derriere `pod_ip`, accepte
/// une connexion — `false` pour toute erreur (control-plane `net-proxy`
/// injoignable, connexion TCP refusee cote guest, etc.), jamais de panique
/// ni d'erreur remontee : c'est une sonde de readiness, un port pas encore
/// pret est l'etat normal juste apres le boot, pas une erreur a traiter.
pub async fn guest_tcp_port_open(
    pod_ip: &str,
    net_proxy_control_port: u16,
    remote_port: u16,
) -> bool {
    let url = format!("ws://{pod_ip}:{net_proxy_control_port}/portforward?ports=tcp:{remote_port}");

    let connected =
        tokio::time::timeout(CONNECT_TIMEOUT, tokio_tungstenite::connect_async(&url)).await;
    let Ok(Ok((mut ws, _response))) = connected else {
        return false;
    };

    // Un seul port demande (`ports=tcp:{remote_port}`) : index 0, donc
    // canal de donnees 0 et canal d'erreur 1 (`channel_byte` cote
    // net-proxy).
    const ERROR_CHANNEL: u8 = 1;
    let outcome = tokio::time::timeout(SILENCE_MEANS_OPEN, ws.next()).await;
    let _ = ws.close(None).await;

    match outcome {
        // Frame recue sur le canal d'erreur : connexion TCP refusee cote
        // guest (port pas encore ouvert par systemd).
        Ok(Some(Ok(Message::Binary(data)))) if data.first() == Some(&ERROR_CHANNEL) => false,
        // N'importe quelle autre frame recue (canal de donnees, ou tout
        // autre type de message) implique que la connexion TCP a reussi.
        Ok(Some(Ok(_))) => true,
        Ok(Some(Err(_))) | Ok(None) => false,
        // Timeout ecoule sans aucune frame : silence, donc port ouvert.
        Err(_) => true,
    }
}
