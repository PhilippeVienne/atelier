use crate::commands::auth::ensure_access_token;
use crate::config::Config;
use crate::tunnel;
use anyhow::{Context, Result};

async fn resolve() -> Result<(String, String)> {
    let config = Config::load()?;
    let (_, ctx) = config.current_context()?;
    let token = ensure_access_token().await?;
    Ok((ctx.api_url.clone(), token))
}

/// `atelier port-forward <id> <local:remote>` (mode ecoute) ou
/// `atelier port-forward --stdio <id> <remote>` (relais stdin/stdout,
/// utilisable comme `ProxyCommand` SSH — spec §3.7).
pub async fn port_forward(name: String, mapping: String, stdio: bool) -> Result<()> {
    let (api_url, token) = resolve().await?;

    if stdio {
        let remote_port: u16 = mapping
            .parse()
            .with_context(|| format!("port distant invalide: '{mapping}'"))?;
        return tunnel::relay_stdio(&api_url, &token, &name, remote_port).await;
    }

    let (local, remote) = mapping
        .split_once(':')
        .with_context(|| format!("mapping attendu 'local:remote', recu '{mapping}'"))?;
    let remote_port: u16 = remote
        .parse()
        .with_context(|| format!("port distant invalide: '{remote}'"))?;
    let local_addr = if local.contains(':') {
        local.to_string()
    } else {
        format!("127.0.0.1:{local}")
    };
    tunnel::listen_and_forward(&api_url, &token, &name, &local_addr, remote_port).await
}

/// `atelier ssh <id>` : delegue a un client SSH systeme reel, avec un
/// `ProxyCommand` qui relaie le port 22 du Workshop via `atelier
/// port-forward --stdio` (spec §3.7) — pas de reimplementation d'un client
/// SSH, seulement le tunnel de transport.
pub async fn ssh(name: String, user: String) -> Result<()> {
    let self_exe = std::env::current_exe().context("chemin du binaire atelier introuvable")?;
    let proxy_command = format!("{} port-forward --stdio {name} 22", self_exe.display());
    let status = std::process::Command::new("ssh")
        .arg("-o")
        .arg(format!("ProxyCommand={proxy_command}"))
        .arg("-o")
        .arg("StrictHostKeyChecking=no")
        .arg("-o")
        .arg("UserKnownHostsFile=/dev/null")
        .arg(format!("{user}@{name}"))
        .status()
        .context("lancement du client ssh systeme")?;
    if !status.success() {
        anyhow::bail!("ssh a quitte avec le code {:?}", status.code());
    }
    Ok(())
}

/// `atelier code <id>` : meme principe que `ssh`, mais invoque l'IDE local
/// en mode Remote-SSH (spec §3.7) via un alias SSH dedie
/// (`atelier-<workshop-id>`) ecrit dans `~/.ssh/config` avec le meme
/// `ProxyCommand`.
pub async fn code(name: String, user: String, editor: String) -> Result<()> {
    let self_exe = std::env::current_exe().context("chemin du binaire atelier introuvable")?;
    let host_alias = format!("atelier-{name}");
    let proxy_command = format!("{} port-forward --stdio {name} 22", self_exe.display());

    write_ssh_config_entry(&host_alias, &user, &proxy_command)?;

    let status = std::process::Command::new(&editor)
        .arg("--remote")
        .arg(format!("ssh-remote+{host_alias}"))
        .arg("/workspace")
        .status()
        .with_context(|| format!("lancement de '{editor}'"))?;
    if !status.success() {
        anyhow::bail!("'{editor}' a quitte avec le code {:?}", status.code());
    }
    Ok(())
}

/// Ecrit (ou remplace) un bloc `Host atelier-<id>` dans `~/.ssh/config`,
/// delimite par des marqueurs pour ne jamais toucher au reste du fichier
/// gere par l'utilisateur.
fn write_ssh_config_entry(host_alias: &str, user: &str, proxy_command: &str) -> Result<()> {
    let ssh_dir = dirs::home_dir()
        .context("repertoire personnel introuvable")?
        .join(".ssh");
    std::fs::create_dir_all(&ssh_dir)
        .with_context(|| format!("creation de {}", ssh_dir.display()))?;
    let config_path = ssh_dir.join("config");

    let marker_start = format!("# atelier:{host_alias}:start");
    let marker_end = format!("# atelier:{host_alias}:end");
    let block = format!(
        "{marker_start}\nHost {host_alias}\n    User {user}\n    ProxyCommand {proxy_command}\n{marker_end}\n"
    );

    let existing = std::fs::read_to_string(&config_path).unwrap_or_default();
    let mut lines: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in existing.lines() {
        if line == marker_start {
            skipping = true;
            continue;
        }
        if line == marker_end {
            skipping = false;
            continue;
        }
        if !skipping {
            lines.push(line);
        }
    }
    let mut new_content = lines.join("\n");
    if !new_content.is_empty() && !new_content.ends_with('\n') {
        new_content.push('\n');
    }
    new_content.push_str(&block);

    std::fs::write(&config_path, new_content)
        .with_context(|| format!("ecriture de {}", config_path.display()))
}
