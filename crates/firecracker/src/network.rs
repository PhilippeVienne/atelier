//! Reseau minimal pour la microVM "builder" (seul usage a avoir besoin de
//! reseau aujourd'hui : `vm-supervisor` boote l'agent sans interface reseau
//! pour l'instant, cf. `docs/PROGRESS.md`). Pas de NAT/forwarding vers
//! Internet : un simple lien point-a-point link-local (169.254.0.0/16,
//! `/30` par VM) entre un device TAP cote hote et l'interface `eth0` du
//! guest, dont l'unique voisin direct est `net-proxy` (deja "Fonctionnel",
//! `crates/net-proxy`, allowlist + tunnel `CONNECT`) — c'est lui, pas ce
//! module, qui gere la sortie reelle vers Internet et l'allowlist de
//! domaines. Le guest est configure en HTTP_PROXY/HTTPS_PROXY vers
//! `<host_ip>:<net-proxy-port>` (voir `crates/builder-vm-init`), jamais en
//! acces reseau direct/NAT : coherent avec le modele de securite du projet
//! ("net-proxy = seul chemin de sortie reseau autorise pour la microVM",
//! voir docs/ARCHITECTURE.md), applique ici aussi a la microVM "builder"
//! et pas seulement a celle de l'agent.
//!
//! `fctools` fournit l'arithmetique de sous-adressage
//! (`fctools::extension::link_local::LinkLocalSubnet`) mais ne cree pas le
//! TAP lui-meme : c'est ce module qui s'en charge, en shellant vers `ip`
//! (deja un outil etabli ailleurs dans le projet, ex. `crane`, `mke2fs`,
//! plutot que reimplementer en netlink pur).
//!
//! Suppose un pod deja privilegie (comme `vm-supervisor` aujourd'hui pour
//! `/dev/kvm`) : creer un TAP requiert `CAP_NET_ADMIN`, exerce ici par du
//! code first-party, jamais par le contenu du depot cible du Workshop (voir
//! docs/PROGRESS.md, section "Reseau kind ↔ registre").

use anyhow::{ensure, Context, Result};
use fctools::extension::link_local::LinkLocalSubnet;
use std::net::Ipv4Addr;
use tokio::process::Command;

/// Reseau prepare pour une microVM : device TAP cree cote hote, IP
/// hote/guest calculees. [`NetworkSetup::teardown`] doit etre appele une
/// fois la VM eteinte pour ne pas laisser trainer le TAP.
pub struct NetworkSetup {
    pub iface_id: String,
    pub tap_name: String,
    pub guest_mac: String,
    pub host_ip: Ipv4Addr,
    pub guest_ip: Ipv4Addr,
    pub network_length: u8,
}

/// Cree un device TAP + un sous-reseau link-local `/30` dedie.
///
/// `subnet_index` identifie le sous-reseau (donc l'IP hote/guest et
/// l'adresse MAC generee) : a choisir unique par appelant concurrent dans
/// le meme pod (ex: un compteur atomique ou le suffixe du nom du Job), sous
/// peine de collision entre deux microVMs "builder" simultanees.
pub async fn setup_link_local_tap(tap_name: &str, subnet_index: u16) -> Result<NetworkSetup> {
    // IFNAMSIZ (Linux) est 16 octets, terminateur nul inclus : un nom de 16
    // caracteres ou plus est silencieusement tronque par le kernel, et `ip
    // tuntap add <nom-trop-long> mode tap` echoue avec un message trompeur
    // ("... not a valid ifname") qui ne mentionne pas la longueur —
    // constate en pratique en validant ce module.
    ensure!(
        tap_name.len() <= 15,
        "nom de device TAP trop long ({} caracteres, max 15): {tap_name}",
        tap_name.len()
    );

    let subnet = LinkLocalSubnet::new(subnet_index, 30)
        .context("sous-reseau link-local /30 invalide pour cet index")?;
    let host_ip = subnet.get_host_ip(0).context("IP hote du sous-reseau")?;
    let guest_ip = subnet.get_host_ip(1).context("IP guest du sous-reseau")?;

    let ip_bin = ip_bin();
    run(&ip_bin, &["tuntap", "add", tap_name, "mode", "tap"]).await?;
    run(
        &ip_bin,
        &[
            "addr",
            "add",
            &format!("{}/{}", host_ip.address(), subnet.network_length()),
            "dev",
            tap_name,
        ],
    )
    .await?;
    run(&ip_bin, &["link", "set", tap_name, "up"]).await?;

    // Adresse MAC deterministe a partir de l'index de sous-reseau (prefixe
    // localement administre `06:00`) : evite toute collision entre VMs
    // concurrentes du meme pod sans avoir a generer/tracker un pool
    // d'adresses a part.
    let guest_mac = format!(
        "06:00:00:00:{:02x}:{:02x}",
        (subnet_index >> 8) as u8,
        subnet_index as u8
    );

    Ok(NetworkSetup {
        iface_id: "eth0".to_string(),
        tap_name: tap_name.to_string(),
        guest_mac,
        host_ip: host_ip.address(),
        guest_ip: guest_ip.address(),
        network_length: subnet.network_length(),
    })
}

impl NetworkSetup {
    /// Supprime le device TAP. Best-effort : ne remonte pas d'erreur s'il a
    /// deja disparu (ex: pod en cours de terminaison) — a appeler une fois
    /// la VM eteinte, en compagnon de [`crate::vm::Vm::shutdown`].
    pub async fn teardown(&self) {
        let _ = run(&ip_bin(), &["tuntap", "del", &self.tap_name, "mode", "tap"]).await;
        // Chaine dediee (voir `restrict_to_net_proxy`) : supprimee ici pour
        // ne rien laisser trainer, meme si `restrict_to_net_proxy` n'a
        // jamais ete appelee (no-op dans ce cas, la chaine n'existe pas).
        let chain = self.iptables_chain_name();
        let _ = run("iptables", &["-D", "INPUT", "-i", &self.tap_name, "-j", &chain]).await;
        let _ = run("iptables", &["-D", "FORWARD", "-i", &self.tap_name, "-j", "DROP"]).await;
        let _ = run("iptables", &["-F", &chain]).await;
        let _ = run("iptables", &["-X", &chain]).await;
    }

    fn iptables_chain_name(&self) -> String {
        format!("atelier-vm-{}", self.tap_name)
    }

    /// Defense en profondeur au niveau paquet, en complement de l'allowlist
    /// applicative de `net-proxy` : sans ca, rien n'empeche techniquement le
    /// guest d'ouvrir une connexion TCP brute vers l'IP du pod (`eth0`),
    /// l'API server Kubernetes ou un service de metadata cloud, en
    /// contournant `net-proxy` entierement — le `sysctl
    /// net.ipv4.ip_forward` est global au netns du pod, une autre microVM
    /// du meme pod qui l'active ne doit pas rouvrir cette voie par
    /// accident. Autorise uniquement `net-proxy` (port HTTP proxy + DNS) et
    /// jette tout le reste — voir docs/architecture/network-security.md
    /// pour le detail. A appeler apres [`setup_link_local_tap`], avant de
    /// booter la VM.
    pub async fn restrict_to_net_proxy(&self, net_proxy_port: u16) -> Result<()> {
        let chain = self.iptables_chain_name();
        let host_ip = self.host_ip.to_string();

        run("iptables", &["-N", &chain]).await?;
        run(
            "iptables",
            &[
                "-A",
                &chain,
                "-p",
                "tcp",
                "-d",
                &host_ip,
                "--dport",
                &net_proxy_port.to_string(),
                "-j",
                "ACCEPT",
            ],
        )
        .await?;
        run("iptables", &["-A", &chain, "-p", "udp", "-d", &host_ip, "--dport", "53", "-j", "ACCEPT"]).await?;
        run("iptables", &["-A", &chain, "-p", "tcp", "-d", &host_ip, "--dport", "53", "-j", "ACCEPT"]).await?;
        run("iptables", &["-A", &chain, "-j", "DROP"]).await?;
        run("iptables", &["-A", "INPUT", "-i", &self.tap_name, "-j", &chain]).await?;
        run("iptables", &["-A", "FORWARD", "-i", &self.tap_name, "-j", "DROP"]).await?;
        Ok(())
    }
}

/// Chemin du binaire `ip` a utiliser. Sur un poste de dev sans root, une
/// copie dediee de `ip` avec `CAP_NET_ADMIN` positionnee via `setcap`
/// (meme pattern que `jailer`, voir docs/PROGRESS.md) evite d'avoir besoin
/// de `sudo` : `ATELIER_IP_BIN=/usr/local/bin/atelier-ip`. En pod parent
/// reel (deja `privileged: true`), le `ip` systeme suffit (defaut).
fn ip_bin() -> String {
    std::env::var("ATELIER_IP_BIN").unwrap_or_else(|_| "ip".to_string())
}

async fn run(bin: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .await
        .with_context(|| format!("lancement de {bin}"))?;
    ensure!(status.success(), "{bin} {args:?} a echoue avec le statut {status}");
    Ok(())
}
