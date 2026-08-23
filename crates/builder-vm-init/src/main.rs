//! Init minimal (PID 1) de la microVM "builder" : monte `/proc` et `/sys`,
//! configure un lien reseau point-a-point minimal (parametres fournis par
//! l'hote via les `boot_args` du kernel), execute `envbuilder` pour
//! construire et pousser le devcontainer cible, puis eteint la VM. Pas de
//! systemd : demarrage rapide et previsible pour une VM jetable a usage
//! unique. Lance via `boot_args: "... init=/sbin/atelier-builder-vm-init"`
//! (cote hote, voir `crates/firecracker::vm::Vm::boot_with_network` et
//! `crates/firecracker::network`).
//!
//! Isolation : faire tourner `envbuilder` ICI, dans le noyau dedie de cette
//! microVM jetable, plutot que dans le conteneur Kubernetes du Job
//! `image-builder`, evite d'avoir a accorder `CAP_SYS_ADMIN` (necessaire au
//! remount qu'envbuilder fait apres avoir vide son propre systeme de
//! fichiers) a un conteneur qui execute des instructions de build issues du
//! depot cible du Workshop — potentiellement non fiable. Voir
//! `docs/PROGRESS.md`, section "Reseau kind ↔ registre".

use anyhow::{bail, Context, Result};
use nix::mount::{mount, MsFlags};
use nix::sys::reboot::{reboot, RebootMode};
use std::collections::HashMap;
use std::process::Command;

fn main() {
    if let Err(err) = run() {
        eprintln!("atelier-builder-vm-init: echec fatal: {err:#}");
    }
    // Toujours tenter l'extinction, succes ou echec : une microVM jetable
    // qui reste allumee bloque indefiniment le hote, qui attend cette
    // extinction pour savoir que le build est termine (pas de canal de
    // controle vsock dans ce MVP — voir docs/PROGRESS.md).
    //
    // RB_AUTOBOOT (reboot), PAS RB_POWER_OFF : cette machine minimale n'a
    // pas d'ACPI (`pci=off` dans les boot_args), donc RB_POWER_OFF n'a
    // aucun handler `pm_power_off` a invoquer — le noyau se contente d'un
    // `halt` ("System halted"), le process Firecracker cote hote continue
    // de tourner indefiniment (`is_running()` ne devient jamais faux).
    // `reboot=k` (deja dans les boot_args) demande au noyau d'utiliser le
    // reset via le controleur clavier i8042 pour un vrai reboot — signal
    // que Firecracker intercepte lui-meme comme fin de VM (pattern standard
    // des inits minimaux Firecracker, cf. documentation upstream). Constate
    // en pratique : sans ce changement, le guest affichait bien "envbuilder
    // termine avec succes" puis "reboot: System halted", mais le hote
    // attendait la VM pendant les 180s du deadline de test sans jamais la
    // voir s'eteindre.
    let _ = reboot(RebootMode::RB_AUTOBOOT);
}

/// Marqueurs de progression sur stdout : la console serie du guest
/// (`console=ttyS0`) est le seul canal de diagnostic disponible dans ce MVP
/// (pas de vsock), et elle n'est aujourd'hui drainee cote hote que vers
/// `tracing::debug!` — invisible sans subscriber configure. Ces lignes
/// permettent de localiser un blocage par elimination (quelle est la
/// derniere etape atteinte) meme sans lire le detail d'`envbuilder`.
fn step(label: &str) {
    println!("atelier-builder-vm-init: {label}");
}

fn run() -> Result<()> {
    step("demarrage");
    mount_pseudo_filesystems()?;
    step("pseudo-filesystems montes (/proc, /sys)");
    let params = parse_cmdline_params()?;
    step("parametres de boot lus");
    configure_network(&params)?;
    step("reseau configure (lo, eth0)");
    run_envbuilder(&params)?;
    step("envbuilder termine avec succes");
    Ok(())
}

fn mount_pseudo_filesystems() -> Result<()> {
    mount(
        Some("proc"),
        "/proc",
        Some("proc"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("montage de /proc")?;
    mount(
        Some("sysfs"),
        "/sys",
        Some("sysfs"),
        MsFlags::empty(),
        None::<&str>,
    )
    .context("montage de /sys")?;
    Ok(())
}

/// Parametres passes par l'hote via les `boot_args` du kernel, convention
/// `atelier.<clef>=<valeur>` (pas de MMDS : plus simple pour une VM jetable
/// a usage unique). Les valeurs ne doivent pas contenir d'espace
/// (limitation de `/proc/cmdline`, qui separe sur les espaces).
fn parse_cmdline_params() -> Result<HashMap<String, String>> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").context("lecture de /proc/cmdline")?;
    let mut params = HashMap::new();
    for token in cmdline.split_whitespace() {
        if let Some(rest) = token.strip_prefix("atelier.") {
            if let Some((key, value)) = rest.split_once('=') {
                params.insert(key.to_string(), value.to_string());
            }
        }
    }
    Ok(params)
}

fn require<'a>(params: &'a HashMap<String, String>, key: &str) -> Result<&'a str> {
    params
        .get(key)
        .map(String::as_str)
        .with_context(|| format!("parametre de boot manquant: atelier.{key}"))
}

/// Configure `lo` et `eth0` avec l'IP link-local attribuee par l'hote
/// (`atelier.guest_ip`/`atelier.host_ip`/`atelier.prefix`, cf.
/// `firecracker::network::NetworkSetup`), plus une route par defaut vers
/// `net-proxy` (`atelier.host_ip`) : necessaire pour que le trafic vers une
/// IP externe arbitraire (pas seulement `net-proxy` lui-meme) traverse le
/// TAP et atteigne les regles de redirection transparente posees cote hote
/// (`NetworkSetup::enable_transparent_gateway`) — c'est ce mecanisme, pas
/// `HTTP_PROXY`/`HTTPS_PROXY` (garde plus bas en filet de securite), qui
/// permet a `envbuilder` de resoudre des noms et d'appliquer l'allowlist
/// d'egress sans qu'aucun outil execute pendant le build (`RUN` d'un
/// Dockerfile, `apt-get`, etc.) n'ait besoin de connaitre ces variables —
/// voir docs/architecture/network-security.md. Cette route ne concerne que
/// ce petit init de plateforme (jamais le contenu du devcontainer cible),
/// contrairement a la VM agent qui l'obtient deja gratuitement via
/// l'autoconfiguration IP du kernel (`ip=`, voir `crates/vm-supervisor`).
fn configure_network(params: &HashMap<String, String>) -> Result<()> {
    let guest_ip = require(params, "guest_ip")?;
    let prefix = require(params, "prefix")?;
    let host_ip = require(params, "host_ip")?;

    step("lancement de: ip link set lo up");
    run_cmd("ip", &["link", "set", "lo", "up"])?;
    step("lancement de: ip addr add ... dev eth0");
    run_cmd(
        "ip",
        &[
            "addr",
            "add",
            &format!("{guest_ip}/{prefix}"),
            "dev",
            "eth0",
        ],
    )?;
    step("lancement de: ip link set eth0 up");
    run_cmd("ip", &["link", "set", "eth0", "up"])?;
    step("lancement de: ip route add default via <host_ip>");
    run_cmd("ip", &["route", "add", "default", "via", host_ip])?;
    Ok(())
}

fn net_proxy_url(params: &HashMap<String, String>) -> Result<String> {
    let host_ip = require(params, "host_ip")?;
    let port = params
        .get("net_proxy_port")
        .map(String::as_str)
        .unwrap_or("3128");
    Ok(format!("http://{host_ip}:{port}"))
}

/// Reprend telle quelle la logique de `image-builder`
/// (`crates/image-builder/src/main.rs::build_and_push`), executee ici dans
/// le noyau dedie de la microVM : le remount qu'envbuilder fait pour
/// survivre a son propre vidage de filesystem y est sans risque (equivalent
/// a n'importe quel process root sur une machine dediee), a la difference
/// d'un conteneur Kubernetes partageant le noyau hote.
fn run_envbuilder(params: &HashMap<String, String>) -> Result<()> {
    let repo = require(params, "repo")?;
    let revision = params.get("revision").map(String::as_str).unwrap_or("HEAD");
    let devcontainer_dir = params.get("devcontainer_dir").map(String::as_str);
    let devcontainer_json_filename = require(params, "devcontainer_json_filename")?;
    let image_ref = require(params, "image_ref")?;
    let registry_insecure = params
        .get("registry_insecure")
        .map(|v| v == "true")
        .unwrap_or(false);

    let git_url = if revision.is_empty() || revision == "HEAD" {
        repo.to_string()
    } else {
        format!("{repo}#{revision}")
    };
    let cache_repo = image_ref
        .rsplit_once(':')
        .map(|(repo, _tag)| repo)
        .unwrap_or(image_ref);
    let proxy_url = net_proxy_url(params)?;

    // Chemin canonique attendu par envbuilder lui-meme (il s'auto-embarque
    // dans l'image construite depuis ce chemin exact, cf. Dockerfile) —
    // meme contrainte que dans `crates/image-builder`.
    let mut cmd = Command::new("/.envbuilder/bin/envbuilder");
    cmd
        // Le `Dockerfile` de cette image fixe `ENV KANIKO_DIR=/.envbuilder`
        // (garde-fou interne d'envbuilder/Kaniko avant de vider le
        // filesystem), mais cette metadonnee OCI n'est interpretee que par
        // un runtime de conteneur — perdue lors de la conversion en
        // `rootfs.ext4` brut (crane export + mke2fs), puisque ce guest n'a
        // pas de runtime de conteneur, seulement ce process PID 1. Sans ce
        // rappel explicite, envbuilder refuse de demarrer
        // ("KANIKO_DIR is not set to /.envbuilder. Bailing!").
        .env("KANIKO_DIR", "/.envbuilder")
        .env("ENVBUILDER_GIT_URL", git_url)
        .env(
            "ENVBUILDER_DEVCONTAINER_JSON_PATH",
            devcontainer_json_filename,
        )
        .env("ENVBUILDER_PUSH_IMAGE", "true")
        .env("ENVBUILDER_CACHE_REPO", cache_repo)
        .env("ENVBUILDER_EXIT_ON_BUILD_FAILURE", "true")
        .env("ENVBUILDER_INIT_COMMAND", "/bin/true")
        .env("ENVBUILDER_INSECURE", registry_insecure.to_string())
        // Seul chemin de sortie reseau autorise : `net-proxy`, joignable en
        // point-a-point (pas de route par defaut, voir configure_network).
        // Respecte par le client HTTP standard de Go (`envbuilder`) et par
        // git (clone du depot cible).
        .env("HTTP_PROXY", &proxy_url)
        .env("HTTPS_PROXY", &proxy_url)
        .env("http_proxy", &proxy_url)
        .env("https_proxy", &proxy_url);
    if let Some(devcontainer_dir) = devcontainer_dir.filter(|d| !d.is_empty()) {
        cmd.env("ENVBUILDER_DEVCONTAINER_DIR", devcontainer_dir);
    }
    // Identifiants git optionnels (depot prive), lus par `image-builder`
    // depuis OpenBao et transmis via les boot_args du kernel — voir le
    // commentaire de `build_via_microvm` cote `crates/image-builder` pour la
    // limite assumee (visibles en clair dans les logs debug de la console).
    if let Some(username) = params.get("git_username") {
        cmd.env("ENVBUILDER_GIT_USERNAME", username);
    }
    if let Some(password) = params.get("git_password") {
        cmd.env("ENVBUILDER_GIT_PASSWORD", password);
    }

    step("lancement de /.envbuilder/bin/envbuilder");
    let status = cmd.status().context("lancement du binaire envbuilder")?;
    if !status.success() {
        bail!("envbuilder a echoue avec le statut {status}");
    }
    Ok(())
}

fn run_cmd(bin: &str, args: &[&str]) -> Result<()> {
    let status = Command::new(bin)
        .args(args)
        .status()
        .with_context(|| format!("lancement de {bin}"))?;
    if !status.success() {
        bail!("{bin} {args:?} a echoue avec le statut {status}");
    }
    Ok(())
}
