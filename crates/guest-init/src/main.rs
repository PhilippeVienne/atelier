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
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Services optionnels : chacun n'existe que si le noeud `image-builder`
/// correspondant a tourne pour ce Workshop (`inject_terminal_and_ide`,
/// `inject_sshd`) — un binaire absent est simplement ignore, pas une
/// erreur.
///
/// Le booleen dit si le service tourne sous le compte de la session (comme
/// `User=vscode` des unites systemd) ou en root. Seul `sshd` a besoin de
/// root, pour ouvrir des sessions sous un autre compte ; un terminal et un
/// IDE en root donnaient a l'utilisateur un acces qu'il n'a pas ailleurs
/// (tache 14.3, spec 19 constat 4).
const SUPERVISED_SERVICES: &[(&str, bool)] = &[
    ("/usr/local/bin/atelier-start-ttyd.sh", true),
    ("/usr/local/bin/atelier-start-code-server.sh", true),
    ("/usr/local/bin/atelier-start-sshd.sh", false),
];

/// Le compte de la session : `vscode`, que `image-builder` cree ou complete
/// avec l'uid 1000 (`ensure_vscode_user`).
const SESSION_USER: &str = "vscode";

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionAccount {
    uid: u32,
    gid: u32,
    home: String,
}

/// Cherche `name` dans le contenu d'un `/etc/passwd`.
fn parse_passwd_entry(passwd: &str, name: &str) -> Option<SessionAccount> {
    passwd.lines().find_map(|line| {
        let mut fields = line.split(':');
        if fields.next()? != name {
            return None;
        }
        let _password = fields.next()?;
        let uid = fields.next()?.parse().ok()?;
        let gid = fields.next()?.parse().ok()?;
        let _gecos = fields.next()?;
        let home = fields.next()?.to_string();
        Some(SessionAccount { uid, gid, home })
    })
}

/// Le compte de la session. A defaut d'entree lisible, l'uid 1000 que
/// `image-builder` garantit : jamais root.
fn session_account() -> SessionAccount {
    fs::read_to_string("/etc/passwd")
        .ok()
        .and_then(|passwd| parse_passwd_entry(&passwd, SESSION_USER))
        .unwrap_or(SessionAccount {
            uid: 1000,
            gid: 1000,
            home: format!("/home/{SESSION_USER}"),
        })
}

fn command_for(path: &str, as_session: bool) -> Command {
    let mut command = Command::new(path);
    // `uid` seul laisserait les groupes supplementaires de root : la
    // bibliotheque standard les vide quand un processus root change d'uid
    // sans liste explicite.
    if as_session {
        let account = session_account();
        command
            .uid(account.uid)
            .gid(account.gid)
            .env("HOME", &account.home)
            .env("USER", SESSION_USER)
            .env("LOGNAME", SESSION_USER);
    }
    command
}

/// Adresse du serveur metadata de `net-proxy`, cote guest — meme convention
/// que les scripts de demarrage `atelier-start-*.sh` (`session-auth`,
/// `ssh-authorized-key`).
const METADATA_ADDR: &str = "169.254.0.1:3132";

const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

/// Periode de la verification de l'horloge (tache 14.18) : apres une reprise
/// d'instantane, l'invite retrouve l'heure de sa mise en veille et retarde.
const CLOCK_CHECK_INTERVAL: Duration = Duration::from_secs(5);
/// Ecart tolere avant de remettre l'horloge a l'heure : en dessous, un saut
/// ferait plus de mal (horodatages de fichiers, `make`) que le retard.
const CLOCK_TOLERANCE_SECS: f64 = 2.0;

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
    run_once("/usr/local/bin/atelier-refresh-workspace.sh", true);
    step("rafraichissement du workspace termine");

    let mut children: Vec<(&str, bool, Option<Child>)> = SUPERVISED_SERVICES
        .iter()
        .map(|&(path, as_session)| (path, as_session, spawn_if_present(path, as_session)))
        .collect();
    step("services demarres");

    let start = Instant::now();
    let mut zombies_reaped_total: u64 = 0;
    // Premier heartbeat immediat plutot que d'attendre HEARTBEAT_INTERVAL :
    // savoir que le guest vient de demarrer (et est deja injoignable, le cas
    // echeant) sans attendre 15s pour la premiere preuve de vie.
    let mut last_heartbeat = Instant::now() - HEARTBEAT_INTERVAL;
    let mut last_clock_check = Instant::now() - CLOCK_CHECK_INTERVAL;

    // Boucle de reap infinie : responsabilite non negociable d'un PID 1
    // (tout process orphelin est reparente ici a sa sortie). Les services
    // supervises sont en plus relances s'ils meurent — un `ttyd`/
    // `code-server` qui crashe ne doit pas laisser le Workshop sans
    // terminal/IDE pour le reste de sa vie.
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, _)) | Ok(WaitStatus::Signaled(pid, _, _)) => {
                zombies_reaped_total += 1;
                for (path, as_session, child) in children.iter_mut() {
                    let matches = child.as_ref().map(|c| c.id() as i32) == Some(pid.as_raw());
                    if matches {
                        eprintln!("atelier-guest-init: {path} s'est arrete, relance dans 2s");
                        std::thread::sleep(Duration::from_secs(2));
                        *child = spawn_if_present(path, *as_session);
                    }
                }
            }
            Ok(WaitStatus::StillAlive) | Err(_) => {
                std::thread::sleep(Duration::from_millis(500));
            }
            _ => {}
        }

        if last_clock_check.elapsed() >= CLOCK_CHECK_INTERVAL {
            last_clock_check = Instant::now();
            sync_clock();
        }

        if last_heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
            last_heartbeat = Instant::now();
            let supervised_pids: Vec<i32> = children
                .iter()
                .filter_map(|(_, _, c)| c.as_ref().map(|c| c.id() as i32))
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

fn run_once(path: &str, as_session: bool) {
    if !Path::new(path).exists() {
        return;
    }
    let _ = command_for(path, as_session).status();
}

fn spawn_if_present(path: &str, as_session: bool) -> Option<Child> {
    if !Path::new(path).exists() {
        return None;
    }
    command_for(path, as_session).spawn().ok()
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

/// Corps d'une reponse HTTP `GET /time` : les secondes de l'epoque Unix de
/// l'hote, avec fraction.
fn parse_host_time(response: &str) -> Option<f64> {
    let body = response.split_once("\r\n\r\n")?.1.trim();
    body.parse::<f64>()
        .ok()
        .filter(|t| t.is_finite() && *t > 0.0)
}

/// L'horloge locale s'ecarte-t-elle assez de celle de l'hote pour etre remise
/// a l'heure ?
fn clock_needs_correction(local_secs: f64, host_secs: f64) -> bool {
    (host_secs - local_secs).abs() > CLOCK_TOLERANCE_SECS
}

/// Remet l'horloge de l'invite a l'heure de l'hote (`net-proxy`, serveur de
/// metadonnees). Sans effet si l'hote ne repond pas : une horloge un peu en
/// retard vaut mieux qu'un init qui attend.
fn sync_clock() {
    use nix::sys::time::TimeSpec;
    use nix::time::{clock_settime, ClockId};
    use std::io::Read;

    let Ok(addr) = METADATA_ADDR.parse() else {
        return;
    };
    let Ok(mut stream) = TcpStream::connect_timeout(&addr, Duration::from_secs(2)) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let request =
        format!("GET /time HTTP/1.1\r\nHost: {METADATA_ADDR}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return;
    }
    let mut response = String::new();
    if stream.read_to_string(&mut response).is_err() {
        return;
    }
    let Some(host) = parse_host_time(&response) else {
        return;
    };
    let local = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0);
    if !clock_needs_correction(local, host) {
        return;
    }
    let target = TimeSpec::new(host.trunc() as i64, (host.fract() * 1e9) as i64);
    match clock_settime(ClockId::CLOCK_REALTIME, target) {
        Ok(()) => eprintln!(
            "atelier-guest-init: horloge remise a l'heure de l'hote (ecart {:+.1}s)",
            host - local
        ),
        Err(err) => eprintln!("atelier-guest-init: horloge non corrigee: {err}"),
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
    use super::{
        clock_needs_correction, json_escape, parse_host_time, parse_passwd_entry, SessionAccount,
    };

    #[test]
    fn the_host_time_is_read_from_the_http_body() {
        let ok = "HTTP/1.1 200 OK\r\ncontent-length: 14\r\n\r\n1793000000.123";
        assert_eq!(parse_host_time(ok), Some(1793000000.123));
        assert_eq!(parse_host_time("HTTP/1.1 503 X\r\n\r\n"), None);
        assert_eq!(parse_host_time("pas du http"), None);
        assert_eq!(parse_host_time("HTTP/1.1 200 OK\r\n\r\nNaN"), None);
    }

    #[test]
    fn only_a_gap_beyond_the_tolerance_corrects_the_clock() {
        assert!(!clock_needs_correction(100.0, 101.9));
        assert!(!clock_needs_correction(100.0, 98.1));
        assert!(clock_needs_correction(100.0, 102.1));
        assert!(clock_needs_correction(146.0, 100.0), "horloge en avance");
    }

    #[test]
    fn the_session_account_is_read_from_passwd() {
        let passwd =
            "root:x:0:0:root:/root:/bin/bash\nvscode:x:1000:1000:vscode:/home/vscode:/bin/bash\n";
        assert_eq!(
            parse_passwd_entry(passwd, "vscode"),
            Some(SessionAccount {
                uid: 1000,
                gid: 1000,
                home: "/home/vscode".into()
            })
        );
        assert_eq!(parse_passwd_entry(passwd, "absent"), None);
        // Une entree tronquee ne donne jamais un compte a moitie lu.
        assert_eq!(parse_passwd_entry("vscode:x:1000", "vscode"), None);
    }

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
