//! Moteur Serveur Single-Node (`atelier server ...`, tache 9.8, spec
//! `docs/specs/14-devex-cli-simulateurs-hitl.md` §3.5) : remplace
//! `scripts/install.sh` (spec `docs/specs/10-low-cost-single-node-install.md`)
//! par un moteur Rust type et idempotent — memes etapes exactes que le
//! script shell (k3s sans Traefik, Helm, ingress-nginx, cert-manager,
//! ClusterIssuer Let's Encrypt, identifiants generes une seule fois,
//! `helm upgrade --install` du chart `charts/atelier`), avec un
//! diagnostic prealable (`doctor`) et des sous-commandes dediees
//! `status`/`upgrade`/`uninstall` que le script shell n'offrait pas
//! separement (il fallait le relancer en entier).
//!
//! `install`/`upgrade`/`uninstall` executent des commandes systeme
//! (`k3s`, `helm`, `kubectl`) qui installent/desinstallent un cluster
//! Kubernetes complet sur la machine hote — jamais teste de bout en bout
//! dans cette session (modifierait l'etat systeme de la machine de
//! developpement elle-meme, qui fait deja tourner un cluster `kind`) : voir
//! `docs/specs/PLAN-ACTION-GLOBAL.md`, tache 9.8, pour le detail exact de
//! ce qui a ete verifie (`doctor`, en reel, lecture seule) et ce qui ne l'a
//! pas ete.

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use std::process::Command;
use std::time::Duration;

const INGRESS_NGINX_CHART_VERSION: &str = "4.15.1";
const CERT_MANAGER_CHART_VERSION: &str = "v1.21.1";
const CLUSTER_ISSUER: &str = "letsencrypt-prod";
const DEFAULT_NAMESPACE: &str = "atelier-system";
const DEFAULT_INSTALL_DIR: &str = "/opt/atelier";

fn install_dir() -> String {
    std::env::var("ATELIER_INSTALL_DIR").unwrap_or_else(|_| DEFAULT_INSTALL_DIR.to_string())
}

fn spinner(message: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(
        ProgressStyle::with_template("{spinner:.cyan} {msg}")
            .unwrap()
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.enable_steady_tick(Duration::from_millis(100));
    pb.set_message(message.to_string());
    pb
}

/// Un check de `doctor` : nom, resultat (`Ok(details)` ou `Err(raison)`),
/// et si un echec est bloquant (empeche `install` de fonctionner) ou juste
/// un avertissement.
struct CheckResult {
    name: &'static str,
    outcome: std::result::Result<String, String>,
    blocking: bool,
}

/// `atelier server doctor` : diagnostic pre-vol, LECTURE SEULE — ne modifie
/// jamais l'etat de la machine (contrairement a `install`). Verifie chaque
/// prerequis reellement, jamais par supposition :
/// - Architecture CPU (x86_64/aarch64, memes archs que `scripts/install.sh`).
/// - Acces KVM reel via l'ioctl `KVM_GET_API_VERSION` (`kvm-ioctls`, pas
///   seulement l'existence du fichier `/dev/kvm` — un fichier present sans
///   permissions suffisantes ferait echouer TOUS les Workshops, pas
///   seulement en avertir).
/// - Memoire vive totale (>= 8 Go recommandes).
/// - Ports 80/443 libres (ingress-nginx en a besoin).
/// - `systemd` present (k3s en depend).
pub async fn doctor() -> Result<()> {
    let mut checks = Vec::new();

    let arch = std::env::consts::ARCH;
    checks.push(CheckResult {
        name: "Architecture CPU",
        outcome: if matches!(arch, "x86_64" | "aarch64") {
            Ok(arch.to_string())
        } else {
            Err(format!("{arch} non supportee (x86_64/aarch64 attendus)"))
        },
        blocking: true,
    });

    checks.push(CheckResult {
        name: "Acces KVM (ioctl KVM_GET_API_VERSION)",
        outcome: match kvm_ioctls::Kvm::new() {
            Ok(kvm) => Ok(format!("version API KVM {}", kvm.get_api_version())),
            Err(err) => Err(format!(
                "{err} — /dev/kvm absent, ou permissions insuffisantes (groupe 'kvm' ?), \
                 ou virtualisation materielle non exposee (VPS grand public sans nested-virt)"
            )),
        },
        blocking: true,
    });

    checks.push(CheckResult {
        name: "Memoire vive",
        outcome: match total_memory_gib() {
            Ok(gib) if gib >= 8.0 => Ok(format!("{gib:.1} Go")),
            Ok(gib) => Err(format!("{gib:.1} Go, 8 Go recommandes")),
            Err(err) => Err(err),
        },
        blocking: false,
    });

    for port in [80u16, 443u16] {
        checks.push(CheckResult {
            name: if port == 80 {
                "Port 80 libre"
            } else {
                "Port 443 libre"
            },
            outcome: check_port_free(port),
            blocking: true,
        });
    }

    checks.push(CheckResult {
        name: "systemd present",
        outcome: which("systemctl").map(|_| "trouve".to_string()),
        blocking: true,
    });

    let mut any_blocking_failure = false;
    for check in &checks {
        match &check.outcome {
            Ok(detail) => println!("  OK   {} — {detail}", check.name),
            Err(reason) => {
                let marker = if check.blocking { "ECHEC" } else { "ATTN " };
                println!("  {marker} {} — {reason}", check.name);
                if check.blocking {
                    any_blocking_failure = true;
                }
            }
        }
    }

    if any_blocking_failure {
        bail!("un ou plusieurs prerequis bloquants ne sont pas remplis, voir ci-dessus");
    }
    println!("\nTous les prerequis bloquants sont remplis.");
    Ok(())
}

fn which(program: &str) -> std::result::Result<(), String> {
    Command::new("which")
        .arg(program)
        .output()
        .map_err(|e| e.to_string())
        .and_then(|out| {
            if out.status.success() {
                Ok(())
            } else {
                Err(format!("'{program}' introuvable dans le PATH"))
            }
        })
}

fn total_memory_gib() -> std::result::Result<f64, String> {
    let content = std::fs::read_to_string("/proc/meminfo")
        .map_err(|e| format!("lecture de /proc/meminfo: {e}"))?;
    let line = content
        .lines()
        .find(|l| l.starts_with("MemTotal:"))
        .ok_or_else(|| "ligne MemTotal absente de /proc/meminfo".to_string())?;
    let kib: f64 = line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| "format de /proc/meminfo inattendu".to_string())?
        .parse()
        .map_err(|_| "valeur MemTotal non numerique".to_string())?;
    Ok(kib / 1024.0 / 1024.0)
}

/// Tente une vraie liaison TCP sur le port : distingue "deja utilise"
/// (`AddrInUse`) de "permissions insuffisantes" (`PermissionDenied`, ports
/// < 1024 sans privilege) plutot que de tout confondre en un echec generique.
fn check_port_free(port: u16) -> std::result::Result<String, String> {
    match std::net::TcpListener::bind(("0.0.0.0", port)) {
        Ok(_) => Ok("libre".to_string()),
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => Err(format!(
            "permission refusee sur le port {port} (execute `doctor` en root pour un diagnostic fiable)"
        )),
        Err(_) => Err(format!("port {port} deja utilise par un autre processus")),
    }
}

fn run(pb: &ProgressBar, program: &str, args: &[&str]) -> Result<()> {
    pb.set_message(format!("{program} {}", args.join(" ")));
    let status = Command::new(program)
        .args(args)
        .status()
        .with_context(|| format!("lancement de '{program}'"))?;
    if !status.success() {
        bail!(
            "'{program} {}' a echoue (code {:?})",
            args.join(" "),
            status.code()
        );
    }
    Ok(())
}

/// `atelier server install --domain <d> --email <e>` : memes etapes que
/// `scripts/install.sh`, jamais executees de bout en bout dans cette
/// session (voir la doc de tete de module) — implementation portee du
/// script shell, a verifier contre un vrai serveur/VM avant de s'y fier en
/// production.
pub async fn install(domain: String, email: String, openbao_production: bool) -> Result<()> {
    if !nix_is_root() {
        bail!("`atelier server install` doit s'executer en root (ou via sudo)");
    }
    doctor().await.context(
        "prerequis non remplis (`atelier server doctor`) — installation annulee avant toute modification",
    )?;

    let dir = install_dir();
    std::fs::create_dir_all(&dir).with_context(|| format!("creation de {dir}"))?;

    let pb = spinner("installation de k3s (sans Traefik)...");
    if which("k3s").is_err() {
        let status = Command::new("sh")
            .arg("-c")
            .arg("curl -sfL https://get.k3s.io | INSTALL_K3S_EXEC=\"--disable=traefik\" sh -")
            .status()
            .context("installation de k3s")?;
        if !status.success() {
            bail!("installation de k3s echouee");
        }
    }
    pb.finish_with_message("k3s installe.");

    let pb = spinner("attente du noeud k3s...");
    run(
        &pb,
        "kubectl",
        &[
            "wait",
            "--for=condition=Ready",
            "node",
            "--all",
            "--timeout=300s",
        ],
    )?;
    pb.finish_with_message("noeud k3s pret.");

    let pb = spinner("Helm...");
    if which("helm").is_err() {
        let status = Command::new("sh")
            .arg("-c")
            .arg("curl -fsSL https://raw.githubusercontent.com/helm/helm/main/scripts/get-helm-3 | bash")
            .status()
            .context("installation de Helm")?;
        if !status.success() {
            bail!("installation de Helm echouee");
        }
    }
    pb.finish_with_message("Helm pret.");

    let pb = spinner("ingress-nginx...");
    let _ = run(
        &pb,
        "helm",
        &[
            "repo",
            "add",
            "ingress-nginx",
            "https://kubernetes.github.io/ingress-nginx",
        ],
    );
    let _ = run(
        &pb,
        "helm",
        &["repo", "add", "jetstack", "https://charts.jetstack.io"],
    );
    run(&pb, "helm", &["repo", "update"])?;
    run(
        &pb,
        "helm",
        &[
            "upgrade",
            "--install",
            "ingress-nginx",
            "ingress-nginx/ingress-nginx",
            "--version",
            INGRESS_NGINX_CHART_VERSION,
            "--namespace",
            "ingress-nginx",
            "--create-namespace",
            "--set",
            "controller.ingressClassResource.default=true",
            "--wait",
            "--timeout",
            "5m",
        ],
    )?;
    pb.finish_with_message("ingress-nginx installe.");

    let pb = spinner("cert-manager...");
    run(
        &pb,
        "helm",
        &[
            "upgrade",
            "--install",
            "cert-manager",
            "jetstack/cert-manager",
            "--version",
            CERT_MANAGER_CHART_VERSION,
            "--namespace",
            "cert-manager",
            "--create-namespace",
            "--set",
            "crds.enabled=true",
            "--wait",
            "--timeout",
            "5m",
        ],
    )?;
    pb.finish_with_message("cert-manager installe.");

    let issuer_yaml = format!(
        "apiVersion: cert-manager.io/v1\nkind: ClusterIssuer\nmetadata:\n  name: {CLUSTER_ISSUER}\nspec:\n  acme:\n    server: https://acme-v02.api.letsencrypt.org/directory\n    email: {email}\n    privateKeySecretRef:\n      name: {CLUSTER_ISSUER}-account-key\n    solvers:\n      - http01:\n          ingress:\n            ingressClassName: nginx\n"
    );
    apply_stdin(&issuer_yaml).context("application du ClusterIssuer")?;

    let credentials = ensure_credentials(&dir, openbao_production)?;
    let values_path = format!("{dir}/values-generated.yaml");
    write_values_file(&values_path, &domain, &credentials, openbao_production)?;

    let repo_dir = format!("{dir}/src");
    ensure_repo_cloned(&repo_dir)?;

    let pb = spinner("CRD Workshop...");
    run(
        &pb,
        "kubectl",
        &["apply", "-f", &format!("{repo_dir}/crds/workshop.yaml")],
    )?;
    pb.finish_with_message("CRD applique.");

    let pb = spinner("chart atelier (helm upgrade --install)...");
    let chart_dir = format!("{repo_dir}/charts/atelier");
    let outcome = run(
        &pb,
        "helm",
        &[
            "upgrade",
            "--install",
            "atelier",
            &chart_dir,
            "--namespace",
            DEFAULT_NAMESPACE,
            "--create-namespace",
            "-f",
            &values_path,
            "--wait",
            "--timeout",
            "10m",
        ],
    );
    if outcome.is_err() {
        pb.finish_with_message(
            "helm upgrade --install a echoue ou depasse son delai — un CrashLoopBackOff \
             transitoire de quelques minutes au premier demarrage est NORMAL, verifie \
             `kubectl get pods -n atelier-system -w` avant de conclure a un echec reel.",
        );
    } else {
        pb.finish_with_message("chart atelier installe.");
    }

    println!("\nInstallation terminee (ou en cours de stabilisation).");
    println!("Dashboard  : https://app.{domain}");
    println!("API Server : https://api.{domain}");
    println!("Keycloak   : https://auth.{domain}");
    println!("Forgejo    : https://git.{domain}");
    println!("Identifiants generes : {dir}/credentials.txt (chmod 600)");
    Ok(())
}

/// `atelier server status` : etat des pods du namespace Atelier — lecture
/// seule, safe a executer n'importe quand.
pub async fn status() -> Result<()> {
    let status = Command::new("kubectl")
        .args(["get", "pods", "-n", DEFAULT_NAMESPACE, "-o", "wide"])
        .status()
        .context("lancement de kubectl")?;
    if !status.success() {
        bail!("kubectl get pods a echoue");
    }
    Ok(())
}

/// `atelier server upgrade` : reapplique le chart avec les valeurs deja
/// generees (idempotent, meme commande que la derniere etape d'`install`).
pub async fn upgrade() -> Result<()> {
    let dir = install_dir();
    let values_path = format!("{dir}/values-generated.yaml");
    if !std::path::Path::new(&values_path).exists() {
        bail!("{values_path} introuvable — lance d'abord `atelier server install`");
    }
    let repo_dir = format!("{dir}/src");
    if std::path::Path::new(&format!("{repo_dir}/.git")).exists() {
        let pb = spinner("mise a jour du depot Atelier...");
        run(
            &pb,
            "git",
            &["-C", &repo_dir, "fetch", "--depth", "1", "origin", "main"],
        )?;
        run(
            &pb,
            "git",
            &["-C", &repo_dir, "reset", "--hard", "origin/main"],
        )?;
        pb.finish_with_message("depot a jour.");
    }
    let pb = spinner("helm upgrade --install atelier...");
    let chart_dir = format!("{repo_dir}/charts/atelier");
    run(
        &pb,
        "helm",
        &[
            "upgrade",
            "--install",
            "atelier",
            &chart_dir,
            "--namespace",
            DEFAULT_NAMESPACE,
            "-f",
            &values_path,
            "--wait",
            "--timeout",
            "10m",
        ],
    )?;
    pb.finish_with_message("mise a niveau terminee.");
    Ok(())
}

/// `atelier server uninstall` : desinstalle le chart Atelier puis k3s
/// lui-meme — DESTRUCTIF (perd toutes les donnees non sauvegardees des
/// Workshops et de leurs secrets). Ne supprime jamais
/// `{install_dir}/credentials.txt` de son propre chef.
pub async fn uninstall() -> Result<()> {
    if !nix_is_root() {
        bail!("`atelier server uninstall` doit s'executer en root (ou via sudo)");
    }
    let pb = spinner("desinstallation du chart atelier...");
    let _ = run(
        &pb,
        "helm",
        &["uninstall", "atelier", "--namespace", DEFAULT_NAMESPACE],
    );
    pb.finish_with_message("chart desinstalle.");

    if which("k3s-uninstall.sh").is_ok() {
        let pb = spinner("desinstallation de k3s...");
        run(&pb, "k3s-uninstall.sh", &[])?;
        pb.finish_with_message("k3s desinstalle.");
    } else {
        println!("k3s-uninstall.sh introuvable (k3s deja absent ?), rien a faire pour k3s.");
    }
    Ok(())
}

fn nix_is_root() -> bool {
    // Pas de crate `nix`/`users` supplementaire pour une seule valeur :
    // `id -u` est deja invoque partout ailleurs dans ce projet en shell
    // (voir `scripts/install.sh`), meme verification ici.
    Command::new("id")
        .arg("-u")
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).trim() == "0")
        .unwrap_or(false)
}

fn apply_stdin(yaml: &str) -> Result<()> {
    use std::io::Write;
    let mut child = Command::new("kubectl")
        .args(["apply", "-f", "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("lancement de kubectl apply")?;
    child
        .stdin
        .take()
        .context("stdin de kubectl indisponible")?
        .write_all(yaml.as_bytes())
        .context("ecriture du YAML vers kubectl")?;
    let status = child.wait().context("attente de kubectl apply")?;
    if !status.success() {
        bail!("kubectl apply a echoue");
    }
    Ok(())
}

struct Credentials {
    postgres_admin_password: String,
    postgres_migrator_password: String,
    keycloak_admin_password: String,
    litellm_master_key: String,
    litellm_salt_key: String,
}

fn random_hex(bytes: usize) -> String {
    use rand::RngCore;
    let mut buf = vec![0u8; bytes];
    rand::thread_rng().fill_bytes(&mut buf);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Genere les identifiants une seule fois (fichier `credentials.txt`,
/// chmod 600) — memes variables que `scripts/install.sh`, relues telles
/// quelles si le fichier existe deja (idempotent).
fn ensure_credentials(dir: &str, _openbao_production: bool) -> Result<Credentials> {
    let path = format!("{dir}/credentials.txt");
    if let Ok(content) = std::fs::read_to_string(&path) {
        let get = |key: &str| -> Result<String> {
            content
                .lines()
                .find_map(|l| l.strip_prefix(&format!("{key}=\"")))
                .and_then(|v| v.strip_suffix('"'))
                .map(str::to_string)
                .with_context(|| format!("{key} absente de {path}"))
        };
        return Ok(Credentials {
            postgres_admin_password: get("POSTGRES_ADMIN_PASSWORD")?,
            postgres_migrator_password: get("POSTGRES_MIGRATOR_PASSWORD")?,
            keycloak_admin_password: get("KEYCLOAK_ADMIN_PASSWORD")?,
            litellm_master_key: get("LITELLM_MASTER_KEY")?,
            litellm_salt_key: get("LITELLM_SALT_KEY")?,
        });
    }

    let creds = Credentials {
        postgres_admin_password: random_hex(24),
        postgres_migrator_password: random_hex(24),
        keycloak_admin_password: random_hex(24),
        litellm_master_key: random_hex(24),
        litellm_salt_key: random_hex(24),
    };
    let content = format!(
        "# Genere par `atelier server install` — a garder confidentiel.\n\
         POSTGRES_ADMIN_PASSWORD=\"{}\"\n\
         POSTGRES_MIGRATOR_PASSWORD=\"{}\"\n\
         KEYCLOAK_ADMIN_PASSWORD=\"{}\"\n\
         LITELLM_MASTER_KEY=\"{}\"\n\
         LITELLM_SALT_KEY=\"{}\"\n",
        creds.postgres_admin_password,
        creds.postgres_migrator_password,
        creds.keycloak_admin_password,
        creds.litellm_master_key,
        creds.litellm_salt_key,
    );
    std::fs::write(&path, content).with_context(|| format!("ecriture de {path}"))?;
    set_owner_only(&path)?;
    Ok(creds)
}

#[cfg(unix)]
fn set_owner_only(path: &str) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restriction des permissions de {path}"))
}

#[cfg(not(unix))]
fn set_owner_only(_path: &str) -> Result<()> {
    Ok(())
}

fn write_values_file(
    path: &str,
    domain: &str,
    creds: &Credentials,
    openbao_production: bool,
) -> Result<()> {
    let content = format!(
        "# Genere par `atelier server install` — ne pas committer, ne pas partager.\n\
         domains:\n  keycloak: \"auth.{domain}\"\n  forgejo: \"git.{domain}\"\n  dashboard: \"app.{domain}\"\n  apiServer: \"api.{domain}\"\n\n\
         ingress:\n  className: \"nginx\"\n\n\
         tls:\n  enabled: true\n  certManager:\n    enabled: true\n    issuer: \"{CLUSTER_ISSUER}\"\n    issuerKind: \"ClusterIssuer\"\n\n\
         postgresql:\n  auth:\n    adminPassword: \"{}\"\n    migratorPassword: \"{}\"\n\n\
         keycloak:\n  auth:\n    adminPassword: \"{}\"\n\n\
         litellm:\n  masterKey: \"{}\"\n  saltKey: \"{}\"\n\n\
         openbao:\n  devMode: {}\n",
        creds.postgres_admin_password,
        creds.postgres_migrator_password,
        creds.keycloak_admin_password,
        creds.litellm_master_key,
        creds.litellm_salt_key,
        !openbao_production,
    );
    std::fs::write(path, content).with_context(|| format!("ecriture de {path}"))?;
    set_owner_only(path)
}

fn ensure_repo_cloned(repo_dir: &str) -> Result<()> {
    const REPO_URL: &str = "https://github.com/PhilippeVienne/atelier.git";
    if std::path::Path::new(&format!("{repo_dir}/.git")).exists() {
        let pb = spinner("mise a jour du depot Atelier...");
        run(
            &pb,
            "git",
            &["-C", repo_dir, "fetch", "--depth", "1", "origin", "main"],
        )?;
        run(
            &pb,
            "git",
            &["-C", repo_dir, "reset", "--hard", "origin/main"],
        )?;
        pb.finish_with_message("depot a jour.");
    } else {
        let pb = spinner("clonage du depot Atelier...");
        run(&pb, "git", &["clone", "--depth", "1", REPO_URL, repo_dir])?;
        pb.finish_with_message("depot clone.");
    }
    Ok(())
}
