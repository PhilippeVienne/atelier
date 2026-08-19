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
    let _ = reboot(RebootMode::RB_POWER_OFF);
}

fn run() -> Result<()> {
    mount_pseudo_filesystems()?;
    let params = parse_cmdline_params()?;
    configure_network(&params)?;
    run_envbuilder(&params)?;
    Ok(())
}

fn mount_pseudo_filesystems() -> Result<()> {
    mount(Some("proc"), "/proc", Some("proc"), MsFlags::empty(), None::<&str>)
        .context("montage de /proc")?;
    mount(Some("sysfs"), "/sys", Some("sysfs"), MsFlags::empty(), None::<&str>)
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
/// `firecracker::network::NetworkSetup`). Pas de route par defaut ni de
/// resolveur DNS : le seul voisin joignable est `net-proxy`
/// (`atelier.host_ip:atelier.net_proxy_port`, directement sur le lien
/// point-a-point, donc sans route necessaire), configure ci-dessous comme
/// `HTTP_PROXY`/`HTTPS_PROXY` pour `envbuilder` — c'est lui, pas ce guest,
/// qui resout les noms et applique l'allowlist d'egress (voir
/// `crates/net-proxy`, "seul chemin de sortie reseau autorise pour la
/// microVM", docs/ARCHITECTURE.md).
fn configure_network(params: &HashMap<String, String>) -> Result<()> {
    let guest_ip = require(params, "guest_ip")?;
    let prefix = require(params, "prefix")?;

    run_cmd("ip", &["link", "set", "lo", "up"])?;
    run_cmd("ip", &["addr", "add", &format!("{guest_ip}/{prefix}"), "dev", "eth0"])?;
    run_cmd("ip", &["link", "set", "eth0", "up"])?;
    Ok(())
}

fn net_proxy_url(params: &HashMap<String, String>) -> Result<String> {
    let host_ip = require(params, "host_ip")?;
    let port = params.get("net_proxy_port").map(String::as_str).unwrap_or("3128");
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
    let cache_repo = image_ref.rsplit_once(':').map(|(repo, _tag)| repo).unwrap_or(image_ref);
    let proxy_url = net_proxy_url(params)?;

    // Chemin canonique attendu par envbuilder lui-meme (il s'auto-embarque
    // dans l'image construite depuis ce chemin exact, cf. Dockerfile) —
    // meme contrainte que dans `crates/image-builder`.
    let mut cmd = Command::new("/.envbuilder/bin/envbuilder");
    cmd.env("ENVBUILDER_GIT_URL", git_url)
        .env("ENVBUILDER_DEVCONTAINER_JSON_PATH", devcontainer_json_filename)
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
