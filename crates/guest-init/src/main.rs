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
//! arriere-plan (relances s'ils sortent), reap les zombies (responsabilite
//! standard d'un PID 1), et surveille en plus les process ORPHELINS restes
//! coinces (voir `kill_stale_orphans`) — un complement au plafond `timeout`
//! de `crate::exec` (`atelier-api-server`), pas un remplacement : voir son
//! commentaire pour la difference exacte de perimetre.
//!
//! Le reseau n'est PAS configure ici, volontairement : le noyau le fait
//! lui-meme au boot via le parametre `ip=` (`CONFIG_IP_PNP`), pose par
//! `vm-supervisor::kernel_ip_boot_arg` — rien a faire cote init, seul
//! `send_heartbeat` en depend (l'adresse link-local du TAP est fixe, voir
//! `crates/firecracker/src/network.rs`).

use nix::mount::{mount, MsFlags};
use nix::sys::signal::{kill, Signal};
use nix::sys::wait::{waitpid, WaitPidFlag, WaitStatus};
use nix::unistd::Pid;
use std::fs;
use std::io::Write;
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Services optionnels : chacun n'existe que si le noeud `image-builder`
/// correspondant a tourne pour ce Workshop (`inject_terminal_and_ide`,
/// `inject_sshd`) — un binaire absent est simplement ignore, pas une
/// erreur.
const SUPERVISED_SERVICES: &[&str] = &[
    "/usr/local/bin/atelier-start-ttyd.sh",
    "/usr/local/bin/atelier-start-code-server.sh",
    "/usr/local/bin/atelier-start-sshd.sh",
];

/// Adresse du serveur metadata de `net-proxy`, cote guest — meme convention
/// que les scripts de demarrage `atelier-start-*.sh` (`session-auth`,
/// `ssh-authorized-key`).
const METADATA_ADDR: &str = "169.254.0.1:3132";

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Age au-dela duquel un process directement rattache a PID 1 (deja
/// orphelin, donc — un `ppid` different signifierait un parent encore
/// vivant, hors du perimetre de ce watchdog) et INCONNU (ni nous-memes, ni
/// un des services supervises) est considere coince plutot que legitime.
/// Volontairement au-dela du plafond `timeout` par defaut cote
/// `crate::exec` (`atelier-api-server`, `ATELIER_EXEC_CEILING_SECS`,
/// 20 min) : cet age ne devrait, sauf echec DE ce plafond, jamais etre
/// atteint pour un exec normal.
const STALE_ORPHAN_AGE: Duration = Duration::from_secs(25 * 60);

/// Ticks d'horloge par seconde (`sysconf(_SC_CLK_TCK)`) : universellement
/// 100 sur Linux x86_64/arm64 depuis des decennies, aucune distribution
/// courante n'y deroge. Evite de tirer une dependance supplementaire (ou le
/// FFI direct vers `sysconf`) pour un seul appel, dans un binaire
/// deliberement minimal.
const CLK_TCK: f64 = 100.0;

struct StaleOrphan {
    pid: i32,
    command: String,
    age_secs: u64,
}

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

    let start = Instant::now();
    let mut zombies_reaped_total: u64 = 0;
    // Premier heartbeat immediat plutot que d'attendre HEARTBEAT_INTERVAL :
    // savoir que le guest vient de demarrer (et est deja injoignable, le cas
    // echeant) sans attendre 15s pour la premiere preuve de vie.
    let mut last_heartbeat = Instant::now() - HEARTBEAT_INTERVAL;

    // Boucle de reap infinie : responsabilite non negociable d'un PID 1
    // (tout process orphelin est reparente ici a sa sortie). Les services
    // supervises sont en plus relances s'ils meurent — un `ttyd`/
    // `code-server` qui crashe ne doit pas laisser le Workshop sans
    // terminal/IDE pour le reste de sa vie.
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                zombies_reaped_total += 1;
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

        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat = Instant::now();
            let supervised_pids: Vec<i32> = children
                .iter()
                .filter_map(|(_, c)| c.as_ref().map(|c| c.id() as i32))
                .collect();
            let killed = kill_stale_orphans(&supervised_pids, STALE_ORPHAN_AGE);
            for orphan in &killed {
                eprintln!(
                    "atelier-guest-init: orphelin coince tue pid={} age={}s cmd={:?}",
                    orphan.pid, orphan.age_secs, orphan.command
                );
            }
            send_heartbeat(start.elapsed().as_secs(), zombies_reaped_total, &killed);
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

/// Parcourt `/proc` a la recherche de processus DIRECTEMENT rattaches a
/// PID 1 (deja orphelins), inconnus de nous (absents de `supervised_pids`),
/// vivants depuis plus de `max_age` — et les tue (`SIGKILL`, sans grace :
/// un process dans cet etat n'a deja rien produit depuis tres longtemps,
/// une terminaison propre n'a plus de sens a esperer).
///
/// Complement du plafond `timeout` cote `crate::exec`, PAS un remplacement :
/// celui-ci ne voit QUE les processus deja reparentes ici — pas un process
/// encore rattache a un parent vivant plus haut dans l'arbre, le cas le
/// plus courant d'un exec SSH bloque (la session `sshd` qui l'a lance reste
/// en vie tout du long, rien n'est donc jamais orphelin de notre point de
/// vue). Ce watchdog couvre le reste : un `disown`/`nohup` dont le parent
/// direct est deja sorti, par exemple.
fn kill_stale_orphans(supervised_pids: &[i32], max_age: Duration) -> Vec<StaleOrphan> {
    let uptime_secs = read_uptime_secs().unwrap_or(0.0);
    let mut killed = Vec::new();

    let Ok(entries) = fs::read_dir("/proc") else {
        return killed;
    };
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<i32>().ok())
        else {
            continue;
        };
        if pid <= 1 || supervised_pids.contains(&pid) {
            continue;
        }
        let Some((ppid, start_ticks)) = read_stat(pid) else {
            continue;
        };
        if ppid != 1 {
            continue;
        }
        let age_secs = (uptime_secs - (start_ticks / CLK_TCK)).max(0.0) as u64;
        if age_secs < max_age.as_secs() {
            continue;
        }
        let command = fs::read_to_string(format!("/proc/{pid}/cmdline"))
            .map(|raw| raw.replace('\0', " ").trim().to_string())
            .unwrap_or_else(|_| "?".to_string());
        let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
        killed.push(StaleOrphan {
            pid,
            command,
            age_secs,
        });
    }
    killed
}

fn read_uptime_secs() -> Option<f64> {
    let raw = fs::read_to_string("/proc/uptime").ok()?;
    raw.split_whitespace().next()?.parse().ok()
}

/// `ppid` (champ 4) et `starttime` (champ 22, en ticks depuis le boot) de
/// `/proc/[pid]/stat`. Le nom de commande (champ 2, entre parentheses) PEUT
/// CONTENIR DES ESPACES ET DES PARENTHESES — on coupe apres la DERNIERE
/// occurrence de `") "` pour ne jamais se faire piegier par un nom de
/// process choisi par le process lui-meme (`prctl(PR_SET_NAME)`), puis on
/// decoupe le reste normalement (le champ `state`, immediatement apres,
/// occupe alors l'index 0).
fn read_stat(pid: i32) -> Option<(i32, f64)> {
    let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let after_comm = raw.rsplit_once(") ")?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let ppid: i32 = fields.get(1)?.parse().ok()?;
    let starttime: f64 = fields.get(19)?.parse().ok()?;
    Some((ppid, starttime))
}

/// Best-effort, sans retry : un heartbeat perdu n'a pas besoin d'etre
/// rejoue, le suivant arrive dans `HEARTBEAT_INTERVAL`. Timeouts courts pour
/// ne jamais laisser cette boucle (qui reap aussi les zombies) bloquee par
/// un `net-proxy` momentanement injoignable.
fn send_heartbeat(uptime_secs: u64, zombies_reaped_total: u64, killed: &[StaleOrphan]) {
    let orphans_json: String = killed
        .iter()
        .map(|o| {
            format!(
                r#"{{"pid":{},"command":{},"age_secs":{}}}"#,
                o.pid,
                json_escape(&o.command),
                o.age_secs
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    let body = format!(
        r#"{{"uptime_secs":{uptime_secs},"zombies_reaped_total":{zombies_reaped_total},"killed_stale_orphans":[{orphans_json}]}}"#
    );
    let request = format!(
        "POST /heartbeat HTTP/1.1\r\nHost: {METADATA_ADDR}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );

    let Ok(addr) = METADATA_ADDR.parse() else {
        return;
    };
    if let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
        let _ = stream.write_all(request.as_bytes());
    }
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if (c as u32) < 0x20 => {}
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::json_escape;

    #[test]
    fn json_escape_handles_quotes_backslashes_and_control_chars() {
        assert_eq!(json_escape("simple"), "\"simple\"");
        assert_eq!(
            json_escape(r#"node "server.js""#),
            r#""node \"server.js\"""#
        );
        assert_eq!(json_escape("a\\b"), r#""a\\b""#);
        assert_eq!(json_escape("a\nb"), r#""a\nb""#);
    }
}
