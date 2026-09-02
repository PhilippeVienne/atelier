//! Init minimal (PID 1) pour un devcontainer SANS systemd.
//!
//! `image-builder` (`ensure_init_system`, `crates/image-builder/src/main.rs`)
//! ne pose ce binaire comme `/sbin/init` du rootfs QUE quand il detecte
//! l'absence de `systemd` dans l'image de base — les images qui en ont un
//! gardent leur init habituel, les unites `atelier-*.service` injectees
//! ailleurs (`inject_terminal_and_ide`, `inject_workspace_refresh`) restent
//! le mecanisme normal dans ce cas.
//!
//! Contrairement a `atelier-builder-vm-init` (microVM "builder" jetable,
//! run-once puis reboot), celui-ci doit survivre indefiniment : c'est le
//! PID 1 d'un Workshop qui reste allume tant que l'agent travaille. Il
//! mounte les pseudo-filesystems, lance les services atelier en
//! arriere-plan (relances s'ils sortent), puis boucle sur `wait()` —
//! responsabilite standard d'un PID 1 (recolter les zombies, y compris ceux
//! d'un `git`/process quelconque lance puis abandonne par l'agent).
//!
//! Le reseau n'est PAS configure ici, volontairement : le noyau le fait
//! lui-meme au boot via le parametre `ip=` (`CONFIG_IP_PNP`), pose par
//! `vm-supervisor::kernel_ip_boot_arg` — rien a faire cote init.

use nix::mount::{mount, MsFlags};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::path::Path;
use std::process::{Child, Command};
use std::time::Duration;

/// Services optionnels : chacun n'existe que si le noeud `image-builder`
/// correspondant a tourne pour ce Workshop (`inject_terminal_and_ide`,
/// `inject_sshd`) — un binaire absent est simplement ignore, pas une
/// erreur.
const SUPERVISED_SERVICES: &[&str] = &[
    "/usr/local/bin/atelier-start-ttyd.sh",
    "/usr/local/bin/atelier-start-code-server.sh",
    "/usr/local/bin/atelier-start-sshd.sh",
];

fn main() {
    step("demarrage (init sans systemd)");
    mount_pseudo_filesystems();
    step("pseudo-filesystems montes (/proc, /sys, /dev)");

    // Une seule fois, avant les services persistants — meme script que sous
    // systemd (`inject_workspace_refresh`), lance en synchrone : les
    // services ci-dessous ouvrent le workspace, il doit etre a jour avant.
    run_once("/usr/local/bin/atelier-refresh-workspace.sh");
    step("rafraichissement du workspace termine");

    let mut children: Vec<(&str, Option<Child>)> = SUPERVISED_SERVICES
        .iter()
        .map(|&path| (path, spawn_if_present(path)))
        .collect();
    step("services demarres");

    // Boucle de reap infinie : responsabilite non negociable d'un PID 1
    // (tout process orphelin est reparente ici a sa sortie). Les services
    // supervises sont en plus relances s'ils meurent — un `ttyd`/
    // `code-server` qui crashe ne doit pas laisser le Workshop sans
    // terminal/IDE pour le reste de sa vie.
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                for (path, child) in children.iter_mut() {
                    let matches = child.as_ref().map(|c| c.id() as i32) == Some(pid.as_raw());
                    if matches {
                        eprintln!("atelier-guest-init: {path} s'est arrete, relance dans 2s");
                        std::thread::sleep(Duration::from_secs(2));
                        *child = spawn_if_present(path);
                    }
                }
            }
            Ok(WaitStatus::StillAlive) | Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => {}
        }
    }
}

fn step(label: &str) {
    println!("atelier-guest-init: {label}");
}

fn mount_pseudo_filesystems() {
    // Chacun best-effort : deja monte par le noyau (`devtmpfs` l'est
    // frequemment d'office) ou par une image de base qui fait quand meme
    // un peu d'init avant de nous ceder la main — une erreur ici (EBUSY en
    // particulier) ne doit jamais empecher le reste de demarrer.
    for (fstype, target) in [("proc", "/proc"), ("sysfs", "/sys"), ("devtmpfs", "/dev")] {
        let _ = mount(
            Some(fstype),
            target,
            Some(fstype),
            MsFlags::empty(),
            None::<&str>,
        );
    }
}

fn run_once(path: &str) {
    if !Path::new(path).exists() {
        return;
    }
    let _ = Command::new(path).status();
}

fn spawn_if_present(path: &str) -> Option<Child> {
    if !Path::new(path).exists() {
        return None;
    }
    Command::new(path).spawn().ok()
}
