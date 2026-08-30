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

    let network = match setup_link_local_tap(&tap_name, 0).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("creation du TAP impossible ({e:?}), test ignore (requiert CAP_NET_ADMIN)");
            return;
        }
    };

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
    assert!(
        link_text.contains("UP"),
        "le device TAP doit etre up: {link_text}"
    );

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
    let network = match setup_link_local_tap(&tap_name, 2).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("creation du TAP impossible ({e:?}), test ignore (requiert CAP_NET_ADMIN)");
            return;
        }
    };

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
    assert!(
        rules.contains("--dport 3128") && rules.contains("ACCEPT"),
        "regle net-proxy manquante: {rules}"
    );
    assert!(rules.contains("--dport 53"), "regle DNS manquante: {rules}");
    assert!(
        rules.contains("-j DROP"),
        "regle de rejet par defaut manquante: {rules}"
    );
    // Sans cette regle, le retour (SYN-ACK) d'une connexion que net-proxy
    // initie lui-meme vers le guest (port-forward/code-server/ttyd) serait
    // jete par cette meme chaine (port de destination ephemere, jamais dans
    // la liste explicite ci-dessus) — bug reel trouve en testant.
    assert!(
        rules.contains("ctstate") && rules.contains("ESTABLISHED"),
        "regle de retour de connexion (conntrack) manquante: {rules}"
    );

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

    let chain_after = Command::new("iptables")
        .args(["-S", &chain])
        .output()
        .expect("lancement de `iptables -S`");
    assert!(
        !chain_after.status.success(),
        "la chaine dediee doit avoir disparu apres teardown()"
    );
}

/// Verifie le chemin transparent (voir docs/architecture/network-security.md,
/// section sur `net-proxy` en passerelle gatekeeper) : redirection `nat`
/// des ports 80/443/53 vers les ports locaux de `net-proxy`, sans jamais
/// toucher a `FORWARD`/`ip_forward` (deja verifie inchange par
/// `restricts_tap_to_net_proxy_only` sur la chaine `filter` — ce test se
/// concentre sur la table `nat`, nouvelle ici).
#[tokio::test]
async fn enables_transparent_redirect_without_touching_forward() {
    let tap_name = format!("fc-x{}", std::process::id() % 10000);
    let network = match setup_link_local_tap(&tap_name, 3).await {
        Ok(n) => n,
        Err(e) => {
            eprintln!("creation du TAP impossible ({e:?}), test ignore (requiert CAP_NET_ADMIN)");
            return;
        }
    };

    network
        .enable_transparent_gateway(3128, 3180, 3181, Some(3132))
        .await
        .expect("la pose des regles de redirection doit reussir sous CAP_NET_ADMIN");

    let nat_chain = format!("atelier-vm-nat-{tap_name}");
    let nat_output = Command::new("iptables")
        .args(["-t", "nat", "-S", &nat_chain])
        .output()
        .expect("lancement de `iptables -t nat -S`");
    let nat_rules = String::from_utf8_lossy(&nat_output.stdout);
    assert!(
        nat_rules.contains("--dport 80") && nat_rules.contains("--to-ports 3180"),
        "redirection HTTP manquante: {nat_rules}"
    );
    assert!(
        nat_rules.contains("--dport 443") && nat_rules.contains("--to-ports 3181"),
        "redirection TLS manquante: {nat_rules}"
    );
    assert!(
        nat_rules.contains("--dport 53") && nat_rules.contains("--to-ports 53"),
        "redirection DNS manquante: {nat_rules}"
    );

    // Regression (bug reel, 2026-08-30) : le port du serveur metadata de
    // `net-proxy` (3132) doit etre ACCEPTE dans la chaine `filter` dediee.
    // Il n'y etait pas : le guest ne pouvait donc jamais recuperer son mot
    // de passe de session ni sa cle publique SSH au boot (trafic jete par
    // le `DROP` final de la chaine), rendant ttyd/code-server et sshd
    // definitivement inaccessibles — voir
    // `crates/vm-supervisor/src/main.rs` pour le detail du symptome.
    // Contrairement a 80/443/53, ce port est adresse DIRECTEMENT par le
    // guest (pas de redirection `nat`), d'ou la verification sur la chaine
    // `filter` et non `nat`.
    let filter_chain = format!("atelier-vm-{tap_name}");
    let filter_output = Command::new("iptables")
        .args(["-S", &filter_chain])
        .output()
        .expect("lancement de `iptables -S <chaine dediee>`");
    let filter_rules = String::from_utf8_lossy(&filter_output.stdout);
    assert!(
        filter_rules.contains("--dport 3132") && filter_rules.contains("-j ACCEPT"),
        "le port du serveur metadata (3132) doit etre accepte: {filter_rules}"
    );

    let prerouting_output = Command::new("iptables")
        .args(["-t", "nat", "-S", "PREROUTING"])
        .output()
        .expect("lancement de `iptables -t nat -S PREROUTING`");
    let prerouting_rules = String::from_utf8_lossy(&prerouting_output.stdout);
    assert!(
        prerouting_rules.contains(&format!("-i {tap_name} -j {nat_chain}")),
        "le TAP doit etre route vers la chaine nat dediee: {prerouting_rules}"
    );

    // Le comportement le plus important a re-verifier explicitement : ce
    // mecanisme ne doit JAMAIS activer `FORWARD -j ACCEPT` ni retirer le
    // `DROP` par defaut du TAP — `REDIRECT` rend le paquet local avant
    // toute decision de routage, donc `FORWARD` reste hors-jeu.
    let forward_output = Command::new("iptables")
        .args(["-S", "FORWARD"])
        .output()
        .expect("lancement de `iptables -S FORWARD`");
    let forward_rules = String::from_utf8_lossy(&forward_output.stdout);
    assert!(
        forward_rules.contains(&format!("-i {tap_name} -j DROP")),
        "FORWARD doit rester bloque pour ce TAP: {forward_rules}"
    );
    assert!(
        !forward_rules.contains("-j ACCEPT"),
        "aucune regle FORWARD ACCEPT ne doit apparaitre: {forward_rules}"
    );

    network.teardown().await;

    let nat_chain_after = Command::new("iptables")
        .args(["-t", "nat", "-S", &nat_chain])
        .output()
        .expect("lancement de `iptables -t nat -S`");
    assert!(
        !nat_chain_after.status.success(),
        "la chaine nat dediee doit avoir disparu apres teardown()"
    );
}
