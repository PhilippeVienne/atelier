//! Client CLI unifie `atelier` (spec `docs/specs/14-devex-cli-simulateurs-hitl.md`
//! §3, tache 9.1) : gestion de contextes multi-environnements, authentification
//! OIDC (Device Authorization Grant, RFC 8628) et CRUD `Workshop` depuis le
//! terminal, sans passer par le Dashboard web.

mod api;
mod commands;
mod config;
mod mcp_client;
mod oidc;
mod tokens;
mod tunnel;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "atelier", version, about = "Client CLI Atelier")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Gestion des contextes multi-environnements (cluster local ou distant).
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    /// Authentification OIDC (Device Authorization Grant).
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Gestion des Workshops sur le contexte actif.
    Workshops {
        #[command(subcommand)]
        command: WorkshopsCommand,
    },
    /// Tunnel port-forward brut vers un port du Workshop (spec §3.7).
    PortForward {
        /// Utilisable comme `ProxyCommand` SSH : relaie stdin/stdout au
        /// lieu d'ecouter sur un port local.
        #[arg(long)]
        stdio: bool,
        name: String,
        /// `local:remote` en mode ecoute, `remote` seul en mode `--stdio`.
        mapping: String,
    },
    /// Terminal SSH interactif dans le Workshop (spec §3.7).
    Ssh {
        name: String,
        #[arg(long, default_value = "root")]
        user: String,
    },
    /// Connecte un IDE local (VS Code/Cursor) en Remote-SSH au Workshop.
    Code {
        name: String,
        #[arg(long, default_value = "root")]
        user: String,
        #[arg(long, default_value = "code")]
        editor: String,
    },
    /// Demandes d'approbation Human-in-the-Loop (HITL, spec §5).
    Approvals {
        #[command(subcommand)]
        command: ApprovalsCommand,
    },
    /// Gestion du cycle de vie du serveur (hote single-node, spec §3.5).
    Server {
        #[command(subcommand)]
        command: ServerCommand,
    },
    /// Serveur MCP local pour agents desktop (spec §3.4).
    Mcp {
        #[command(subcommand)]
        command: McpCommand,
    },
}

#[derive(Subcommand)]
enum McpCommand {
    /// Lance le serveur MCP (stdio) pour Claude Desktop/Cursor.
    Serve {
        /// Contexte a utiliser (par defaut, le contexte actif) — permet a
        /// `install_config` de lancer un agent desktop sur un contexte
        /// distinct de celui utilise par `atelier` en interactif.
        #[arg(long)]
        context: Option<String>,
    },
    /// Injecte la configuration MCP dans Claude Desktop ou Cursor.
    InstallConfig {
        #[arg(long, value_parser = ["claude-desktop", "cursor"])]
        target: String,
        #[arg(long)]
        context: Option<String>,
    },
}

#[derive(Subcommand)]
enum ServerCommand {
    /// Diagnostic pre-vol (KVM, memoire, ports, systemd) — lecture seule.
    Doctor,
    /// Installation complete (k3s, ingress-nginx, cert-manager, chart Atelier).
    Install {
        #[arg(long)]
        domain: String,
        #[arg(long)]
        email: String,
        #[arg(long)]
        openbao_production: bool,
        /// Active l'inference GPU locale (backend vLLM, spec
        /// docs/specs/15-souverainete-airgap-inference-gpu.md §3.1, tache
        /// 11.3) : echoue explicitement si aucun GPU NVIDIA n'est reellement
        /// detecte (`nvidia-smi`, pas seulement `/dev/nvidia*`) plutot que
        /// d'installer silencieusement un backend qui ne demarrera jamais.
        #[arg(long)]
        enable_gpu: bool,
    },
    /// Etat des pods du namespace Atelier.
    Status,
    /// Met a jour le depot et reapplique le chart.
    Upgrade,
    /// Desinstalle le chart Atelier puis k3s (destructif).
    Uninstall,
}

#[derive(Subcommand)]
enum ApprovalsCommand {
    /// Liste les demandes d'approbation d'un Workshop.
    List { workshop: String },
    /// Approuve une demande en attente.
    Approve {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
    /// Rejette une demande en attente.
    Reject {
        id: String,
        #[arg(long)]
        reason: Option<String>,
    },
}

#[derive(Subcommand)]
enum ContextCommand {
    /// Enregistre un nouveau contexte.
    Add {
        name: String,
        #[arg(long)]
        api_url: String,
        #[arg(long)]
        issuer: String,
        #[arg(long)]
        client_id: String,
        #[arg(long)]
        scope: Option<String>,
    },
    /// Bascule le contexte actif.
    Use { name: String },
    /// Liste les contextes configures.
    List,
    /// Affiche le contexte actif.
    Current,
}

#[derive(Subcommand)]
enum AuthCommand {
    /// Flux OAuth2 Device Code : ouvre l'URL de connexion et stocke le jeton.
    Login,
    /// Revoque la session locale (trousseau de cles).
    Logout,
    /// Affiche l'etat du jeton du contexte actif.
    Status,
}

#[derive(Subcommand)]
// Enum d'arguments clap, jamais construit en boucle chaude ni stocke en
// masse : la difference de taille entre variantes (le variant `Create`
// porte tous les flags de creation) est sans impact ici.
#[allow(clippy::large_enum_variant)]
enum WorkshopsCommand {
    /// Liste les Workshops visibles pour l'utilisateur courant.
    List,
    /// Cree un Workshop.
    Create {
        name: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        revision: Option<String>,
        #[arg(long = "config-path")]
        config_path: Option<String>,
        #[arg(long, default_value = "1")]
        cpu: String,
        #[arg(long, default_value = "2Gi")]
        memory: String,
        #[arg(long)]
        disk: Option<String>,
        #[arg(long = "egress", value_delimiter = ',')]
        egress_allowlist: Vec<String>,
        #[arg(long, value_delimiter = ',')]
        tools: Vec<String>,
        #[arg(long = "owner-group")]
        owner_group: Option<String>,
    },
    /// Affiche l'etat detaille d'un Workshop.
    Status { name: String },
    /// Suspend un Workshop (snapshot memoire, cout compute nul).
    Stop { name: String },
    /// Reprend un Workshop suspendu.
    Resume { name: String },
    /// Supprime un Workshop.
    Delete { name: String },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cli = Cli::parse();
    match cli.command {
        Command::Context { command } => match command {
            ContextCommand::Add {
                name,
                api_url,
                issuer,
                client_id,
                scope,
            } => commands::context::add(name, api_url, issuer, client_id, scope),
            ContextCommand::Use { name } => commands::context::use_context(name),
            ContextCommand::List => commands::context::list(),
            ContextCommand::Current => commands::context::current(),
        },
        Command::Auth { command } => match command {
            AuthCommand::Login => commands::auth::login().await,
            AuthCommand::Logout => commands::auth::logout(),
            AuthCommand::Status => commands::auth::status(),
        },
        Command::Workshops { command } => match command {
            WorkshopsCommand::List => commands::workshops::list().await,
            WorkshopsCommand::Create {
                name,
                repo,
                revision,
                config_path,
                cpu,
                memory,
                disk,
                egress_allowlist,
                tools,
                owner_group,
            } => {
                commands::workshops::create(
                    name,
                    repo,
                    revision,
                    config_path,
                    cpu,
                    memory,
                    disk,
                    egress_allowlist,
                    tools,
                    owner_group,
                )
                .await
            }
            WorkshopsCommand::Status { name } => commands::workshops::status(name).await,
            WorkshopsCommand::Stop { name } => commands::workshops::stop(name).await,
            WorkshopsCommand::Resume { name } => commands::workshops::resume(name).await,
            WorkshopsCommand::Delete { name } => commands::workshops::delete(name).await,
        },
        Command::PortForward {
            stdio,
            name,
            mapping,
        } => commands::tunnels::port_forward(name, mapping, stdio).await,
        Command::Ssh { name, user } => commands::tunnels::ssh(name, user).await,
        Command::Code { name, user, editor } => commands::tunnels::code(name, user, editor).await,
        Command::Approvals { command } => match command {
            ApprovalsCommand::List { workshop } => commands::approvals::list(workshop).await,
            ApprovalsCommand::Approve { id, reason } => {
                commands::approvals::approve(id, reason).await
            }
            ApprovalsCommand::Reject { id, reason } => {
                commands::approvals::reject(id, reason).await
            }
        },
        Command::Server { command } => match command {
            ServerCommand::Doctor => commands::server::doctor().await,
            ServerCommand::Install {
                domain,
                email,
                openbao_production,
                enable_gpu,
            } => commands::server::install(domain, email, openbao_production, enable_gpu).await,
            ServerCommand::Status => commands::server::status().await,
            ServerCommand::Upgrade => commands::server::upgrade().await,
            ServerCommand::Uninstall => commands::server::uninstall().await,
        },
        Command::Mcp { command } => match command {
            McpCommand::Serve { context } => commands::mcp::serve(context).await,
            McpCommand::InstallConfig { target, context } => {
                commands::mcp::install_config(target, context)
            }
        },
    }
}
