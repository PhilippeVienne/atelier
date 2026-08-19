//! Test d'integration reel de `atelier_firecracker::network` : cree un
//! vrai device TAP, verifie sa presence/configuration, puis le demonte.
//! Necessite `CAP_NET_ADMIN` : sur un poste de dev sans root, lancer via un
//! espace de noms reseau non privilegie plutot que `sudo` :
//!
//!   unshare --net --map-root-user -- \
//!     cargo test -p atelier-firecracker --test network -- --nocapture
//!
//! (en pod parent reel, le pod est deja `privileged: true` — meme
//! justification que pour `/dev/kvm`, voir docs/PROGRESS.md.)
//!
//! Sans `CAP_NET_ADMIN`, le test echoue tot et clairement (`ip tuntap add`
//! renvoie "Operation not permitted") plutot que d'etre silencieusement
//! ignore : contrairement au test Firecracker (`tests/vm.rs`), il ne
//! depend d'aucun binaire externe a telecharger, donc il n'y a pas de
//! raison de le sauter silencieusement en environnement de dev normal — il
//! ne faut juste pas l'appeler sans capacite reseau elevee.

use atelier_firecracker::network::setup_link_local_tap;
use std::process::Command;

#[tokio::test]
async fn creates_tap_then_tears_it_down() {
    let tap_name = format!("fc-t{}", std::process::id() % 10000);

    let network = setup_link_local_tap(&tap_name, 0)
        .await
        .expect("la creation du TAP doit reussir sous CAP_NET_ADMIN");

    assert_eq!(network.host_ip.to_string(), "169.254.0.1");
    assert_eq!(network.guest_ip.to_string(), "169.254.0.2");

    let link_output = Command::new("ip")
        .args(["addr", "show", &tap_name])
        .output()
        .expect("lancement de `ip addr show`");
    let link_text = String::from_utf8_lossy(&link_output.stdout);
    assert!(
        link_text.contains("169.254.0.1/30"),
        "le device TAP doit porter l'IP hote attendue: {link_text}"
    );
    assert!(link_text.contains("UP"), "le device TAP doit etre up: {link_text}");

    network.teardown().await;

    let link_after = Command::new("ip")
        .args(["link", "show", &tap_name])
        .output()
        .expect("lancement de `ip link show`");
    assert!(
        !link_after.status.success(),
        "le device TAP doit avoir disparu apres teardown()"
    );
}

/// Verifie la defense en profondeur au niveau paquet (voir
/// docs/architecture/network-security.md) : seule la destination
/// `net-proxy` (port applicatif + DNS) doit etre acceptee sur le TAP de la
/// VM de l'agent, tout le reste jete — et la chaine dediee doit disparaitre
/// proprement au `teardown()`.
#[tokio::test]
async fn restricts_tap_to_net_proxy_only() {
    let tap_name = format!("fc-r{}", std::process::id() % 10000);
    let network = setup_link_local_tap(&tap_name, 2)
        .await
        .expect("la creation du TAP doit reussir sous CAP_NET_ADMIN");

    network
        .restrict_to_net_proxy(3128)
        .await
        .expect("la pose des regles iptables doit reussir sous CAP_NET_ADMIN");

    let chain = format!("atelier-vm-{tap_name}");
    let list_output = Command::new("iptables")
        .args(["-S", &chain])
        .output()
        .expect("lancement de `iptables -S`");
    let rules = String::from_utf8_lossy(&list_output.stdout);
    assert!(rules.contains("--dport 3128") && rules.contains("ACCEPT"), "regle net-proxy manquante: {rules}");
    assert!(rules.contains("--dport 53"), "regle DNS manquante: {rules}");
    assert!(rules.contains("-j DROP"), "regle de rejet par defaut manquante: {rules}");

    let input_output = Command::new("iptables")
        .args(["-S", "INPUT"])
        .output()
        .expect("lancement de `iptables -S INPUT`");
    let input_rules = String::from_utf8_lossy(&input_output.stdout);
    assert!(
        input_rules.contains(&format!("-i {tap_name} -j {chain}")),
        "le TAP doit etre route vers la chaine dediee: {input_rules}"
    );

    network.teardown().await;

    let chain_after = Command::new("iptables").args(["-S", &chain]).output().expect("lancement de `iptables -S`");
    assert!(
        !chain_after.status.success(),
        "la chaine dediee doit avoir disparu apres teardown()"
    );
}
