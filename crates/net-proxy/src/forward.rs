//! Primitives partagees par [`crate::portforward`] : le protocole cible
//! d'un port de la microVM, et le parsing de la liste de ports demandee par
//! le client d'une session de port-forward (`?ports=tcp:8443,udp:53`).

use anyhow::{bail, Context};

/// Nombre maximum de ports forwardes sur une seule connexion websocket —
/// borne le nombre de canaux multiplexes (et donc de sockets ouvertes cote
/// microVM) qu'un client peut faire ouvrir en une seule requete.
pub const MAX_PORTS_PER_SESSION: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PortSpec {
    pub protocol: Protocol,
    pub port: u16,
}

/// Parse le parametre `ports` d'une requete de port-forward : une liste
/// separee par des virgules d'entrees `port` (TCP implicite, comme
/// `kubectl port-forward`) ou `proto:port`.
pub fn parse_ports_query(raw: &str) -> anyhow::Result<Vec<PortSpec>> {
    let specs: Vec<PortSpec> = raw
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(parse_one)
        .collect::<anyhow::Result<_>>()?;

    if specs.is_empty() {
        bail!("parametre 'ports' manquant ou vide");
    }
    if specs.len() > MAX_PORTS_PER_SESSION {
        bail!(
            "trop de ports demandes ({}), maximum {MAX_PORTS_PER_SESSION} par session",
            specs.len()
        );
    }
    Ok(specs)
}

fn parse_one(entry: &str) -> anyhow::Result<PortSpec> {
    let (protocol, port) = match entry.split_once(':') {
        Some((proto, port)) => {
            let protocol = match proto.to_ascii_lowercase().as_str() {
                "tcp" => Protocol::Tcp,
                "udp" => Protocol::Udp,
                other => bail!("protocole de port-forward inconnu: {other:?} (tcp ou udp)"),
            };
            (protocol, port)
        }
        None => (Protocol::Tcp, entry),
    };
    let port: u16 = port
        .parse()
        .with_context(|| format!("port invalide dans l'entree {entry:?}"))?;
    Ok(PortSpec { protocol, port })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_tcp() {
        let specs = parse_ports_query("8443").unwrap();
        assert_eq!(specs, vec![PortSpec { protocol: Protocol::Tcp, port: 8443 }]);
    }

    #[test]
    fn explicit_protocols() {
        let specs = parse_ports_query("tcp:8443,udp:53").unwrap();
        assert_eq!(
            specs,
            vec![
                PortSpec { protocol: Protocol::Tcp, port: 8443 },
                PortSpec { protocol: Protocol::Udp, port: 53 },
            ]
        );
    }

    #[test]
    fn rejects_empty() {
        assert!(parse_ports_query("").is_err());
        assert!(parse_ports_query("  ").is_err());
    }

    #[test]
    fn rejects_unknown_protocol() {
        assert!(parse_ports_query("sctp:80").is_err());
    }

    #[test]
    fn rejects_too_many_ports() {
        let many = (0..=MAX_PORTS_PER_SESSION)
            .map(|i| (9000 + i as u16).to_string())
            .collect::<Vec<_>>()
            .join(",");
        assert!(parse_ports_query(&many).is_err());
    }
}
