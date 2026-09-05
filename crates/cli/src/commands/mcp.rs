//! `atelier mcp` (tache 9.9, spec `docs/specs/14-devex-cli-simulateurs-hitl.md`
//! §3.4) : serveur MCP local (stdio) pour agents desktop (Claude Desktop,
//! Cursor) qui n'ont pas de notion de contexte Atelier — expose les outils
//! `atelier_*` de la spec, chacun relayant vers le vrai serveur MCP externe
//! deja expose par `api-server` (`/v1/mcp`), jamais reimplemente
//! (`crate::mcp_client::connect`, meme client que
//! `crates/api-server/tests/mcp.rs`).
//!
//! `atelier_exec_in_sandbox`/`atelier_read_file`/`atelier_write_file`/
//! `atelier_git_diff` s'appuient tous sur le SEUL mecanisme d'execution
//! reellement expose cote serveur (`exec_in_workshop`, SSH bufferise) :
//! lire un fichier est un `cat`, en ecrire un est une redirection shell, un
//! diff Git est... un `git diff` — aucune nouvelle capacite cote
//! `api-server`, seulement des raccourcis pratiques pour un agent desktop.

use crate::commands::auth::ensure_access_token;
use crate::config::Config;
use crate::mcp_client::{self, UpstreamService};
use anyhow::{Context, Result};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolRequestParams, ServerCapabilities, ServerInfo};
use rmcp::transport::io::stdio;
use rmcp::{schemars, tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use serde_json::json;

struct Proxy {
    tool_router: ToolRouter<Self>,
    upstream: UpstreamService,
    api_url: String,
    access_token: String,
}

impl Proxy {
    async fn call_upstream(
        &self,
        name: &str,
        args: serde_json::Value,
    ) -> Result<String, ErrorData> {
        let result = self
            .upstream
            .peer()
            .call_tool(
                CallToolRequestParams::new(name.to_string())
                    .with_arguments(args.as_object().cloned().unwrap_or_default()),
            )
            .await
            .map_err(|err| {
                ErrorData::internal_error(format!("api-server injoignable: {err}"), None)
            })?;
        if result.is_error == Some(true) {
            let text = result
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| t.text.clone())
                .unwrap_or_else(|| "erreur inconnue".to_string());
            return Err(ErrorData::internal_error(text, None));
        }
        Ok(result
            .content
            .first()
            .and_then(|c| c.as_text())
            .map(|t| t.text.clone())
            .unwrap_or_default())
    }

    /// Execute `command` dans le Workshop et attend sa fin en consommant le
    /// flux SSE `/v1/workshops/{name}/exec/{id}/stream` — meme mecanisme
    /// que la commande `atelier_exec_in_sandbox`, reutilise par
    /// `atelier_read_file`/`atelier_write_file`/`atelier_git_diff` (voir
    /// doc de tete de module).
    async fn exec_and_wait(&self, workshop: &str, command: &str) -> Result<String, ErrorData> {
        let raw = self
            .call_upstream(
                "exec_in_workshop",
                json!({ "name": workshop, "command": command }),
            )
            .await?;
        let parsed: serde_json::Value = serde_json::from_str(&raw)
            .map_err(|err| ErrorData::internal_error(format!("reponse invalide: {err}"), None))?;
        let stream_url = parsed["streamUrl"]
            .as_str()
            .ok_or_else(|| ErrorData::internal_error("streamUrl absente de la reponse", None))?;

        let http = reqwest::Client::new();
        let resp = http
            .get(format!(
                "{}{}",
                self.api_url.trim_end_matches('/'),
                stream_url
            ))
            .bearer_auth(&self.access_token)
            .send()
            .await
            .map_err(|err| {
                ErrorData::internal_error(format!("flux d'execution injoignable: {err}"), None)
            })?;
        let body = resp.text().await.map_err(|err| {
            ErrorData::internal_error(format!("lecture du flux echouee: {err}"), None)
        })?;

        let mut stdout = String::new();
        let mut stderr = String::new();
        let mut status: Option<serde_json::Value> = None;
        for block in body.split("\n\n") {
            let mut event = None;
            let mut data = String::new();
            for line in block.lines() {
                if let Some(v) = line.strip_prefix("event: ") {
                    event = Some(v);
                } else if let Some(v) = line.strip_prefix("data: ") {
                    if !data.is_empty() {
                        data.push('\n');
                    }
                    data.push_str(v);
                }
            }
            match event {
                Some("stdout") => stdout.push_str(&data),
                Some("stderr") => stderr.push_str(&data),
                Some("status") => status = serde_json::from_str(&data).ok(),
                _ => {}
            }
        }

        Ok(json!({
            "stdout": stdout,
            "stderr": stderr,
            "status": status,
        })
        .to_string())
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct CreateSandboxParams {
    repo_url: String,
    #[serde(default)]
    devcontainer_path: Option<String>,
    #[serde(default)]
    max_budget_usd: Option<f64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WorkshopIdParams {
    workshop_id: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ExecParams {
    workshop_id: String,
    command: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct ReadFileParams {
    workshop_id: String,
    path: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
struct WriteFileParams {
    workshop_id: String,
    path: String,
    content: String,
}

#[tool_router]
impl Proxy {
    #[tool(
        description = "Provisionne une microVM Firecracker isolee dediee a la tache pour l'utilisateur authentifie."
    )]
    async fn atelier_create_sandbox(
        &self,
        Parameters(p): Parameters<CreateSandboxParams>,
    ) -> Result<String, ErrorData> {
        let name = format!(
            "desktop-{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(8)
                .collect::<String>()
        );
        self.call_upstream(
            "create_workshop",
            json!({
                "name": name,
                "devcontainerRepo": p.repo_url,
                "devcontainerConfigPath": p.devcontainer_path,
                "cpu": "1",
                "memory": "2Gi",
                "maxLlmBudgetUsd": p.max_budget_usd,
            }),
        )
        .await
    }

    #[tool(description = "Liste les sandboxes (Workshops) actives de l'utilisateur.")]
    async fn atelier_list_sandboxes(&self) -> Result<String, ErrorData> {
        self.call_upstream("list_workshops", json!({})).await
    }

    #[tool(
        description = "Execute une commande shell hermetique dans la sandbox et retourne stdout/stderr/statut."
    )]
    async fn atelier_exec_in_sandbox(
        &self,
        Parameters(p): Parameters<ExecParams>,
    ) -> Result<String, ErrorData> {
        self.exec_and_wait(&p.workshop_id, &p.command).await
    }

    #[tool(description = "Lit un fichier du workspace distant (via exec_in_sandbox + cat).")]
    async fn atelier_read_file(
        &self,
        Parameters(p): Parameters<ReadFileParams>,
    ) -> Result<String, ErrorData> {
        let quoted = shell_quote(&p.path);
        self.exec_and_wait(&p.workshop_id, &format!("cat {quoted}"))
            .await
    }

    #[tool(
        description = "Ecrit ou modifie un fichier dans la microVM (contenu transmis encode en base64, decode cote guest pour eviter tout probleme d'echappement shell)."
    )]
    async fn atelier_write_file(
        &self,
        Parameters(p): Parameters<WriteFileParams>,
    ) -> Result<String, ErrorData> {
        use base64::engine::general_purpose::STANDARD;
        use base64::Engine;
        let encoded = STANDARD.encode(p.content.as_bytes());
        let quoted = shell_quote(&p.path);
        let command =
            format!("mkdir -p \"$(dirname {quoted})\" && echo {encoded} | base64 -d > {quoted}");
        self.exec_and_wait(&p.workshop_id, &command).await
    }

    #[tool(
        description = "Inspecte les modifications de code effectuees in-VM (git diff dans /workspace)."
    )]
    async fn atelier_git_diff(
        &self,
        Parameters(p): Parameters<WorkshopIdParams>,
    ) -> Result<String, ErrorData> {
        self.exec_and_wait(&p.workshop_id, "cd /workspace && git diff")
            .await
    }

    #[tool(description = "Met un Workshop en veille (snapshot memoire).")]
    async fn atelier_suspend(
        &self,
        Parameters(p): Parameters<WorkshopIdParams>,
    ) -> Result<String, ErrorData> {
        self.call_upstream("suspend_workshop", json!({ "name": p.workshop_id }))
            .await
    }

    #[tool(description = "Reprend un Workshop suspendu.")]
    async fn atelier_resume(
        &self,
        Parameters(p): Parameters<WorkshopIdParams>,
    ) -> Result<String, ErrorData> {
        self.call_upstream("resume_workshop", json!({ "name": p.workshop_id }))
            .await
    }
}

fn shell_quote(path: &str) -> String {
    format!("'{}'", path.replace('\'', "'\\''"))
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Proxy {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build()).with_instructions(
            "Outils Atelier pour agents desktop (Claude Desktop, Cursor) : provisionne et pilote \
             des microVMs isolees distantes sans quitter l'interface habituelle. Voir \
             docs/specs/14-devex-cli-simulateurs-hitl.md §3.4.",
        )
    }
}

/// `atelier mcp serve` : ouvre la connexion amont vers `api-server`
/// (`/v1/mcp`) avec le jeton du contexte actif, puis sert les outils
/// `atelier_*` sur stdio — c'est ce que Claude Desktop/Cursor lancent comme
/// sous-processus (voir `install_config`).
pub async fn serve(context: Option<String>) -> Result<()> {
    let config = Config::load()?;
    let (api_url, access_token) = match &context {
        Some(name) => {
            let ctx = config
                .contexts
                .get(name)
                .with_context(|| format!("contexte '{name}' inconnu (`atelier context list`)"))?;
            (
                ctx.api_url.clone(),
                crate::commands::auth::ensure_access_token_for_named_context(name).await?,
            )
        }
        None => {
            let (_, ctx) = config.current_context()?;
            (ctx.api_url.clone(), ensure_access_token().await?)
        }
    };

    let upstream = mcp_client::connect(&api_url, &access_token).await?;
    let proxy = Proxy {
        tool_router: Proxy::tool_router(),
        upstream,
        api_url,
        access_token,
    };
    let running = proxy
        .serve(stdio())
        .await
        .context("demarrage du serveur MCP stdio")?;
    running
        .waiting()
        .await
        .context("serveur MCP stdio arrete en erreur")?;
    Ok(())
}

/// `atelier mcp install-config --target claude-desktop|cursor` : injecte la
/// configuration MCP dans le fichier attendu par l'agent local (spec §3.4).
pub fn install_config(target: String, context: Option<String>) -> Result<()> {
    let self_exe = std::env::current_exe().context("chemin du binaire atelier introuvable")?;
    let mut args = vec!["mcp".to_string(), "serve".to_string()];
    if let Some(ctx) = &context {
        args.push("--context".to_string());
        args.push(ctx.clone());
    }

    let path = match target.as_str() {
        "claude-desktop" => claude_desktop_config_path()?,
        "cursor" => cursor_config_path()?,
        other => anyhow::bail!("cible inconnue '{other}', attendu 'claude-desktop' ou 'cursor'"),
    };

    let mut root: serde_json::Value = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!({}));
    let servers_key = "mcpServers";
    root.as_object_mut()
        .context("configuration existante invalide (racine non-objet)")?
        .entry(servers_key)
        .or_insert_with(|| json!({}));
    root[servers_key]["atelier"] = json!({
        "command": self_exe.display().to_string(),
        "args": args,
    });

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creation de {}", parent.display()))?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&root)?)
        .with_context(|| format!("ecriture de {}", path.display()))?;
    println!("Configuration MCP ecrite dans {}", path.display());
    Ok(())
}

fn claude_desktop_config_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("repertoire personnel introuvable")?;
    Ok(if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Claude/claude_desktop_config.json")
    } else {
        dirs::config_dir()
            .context("repertoire de configuration introuvable")?
            .join("Claude/claude_desktop_config.json")
    })
}

fn cursor_config_path() -> Result<std::path::PathBuf> {
    let home = dirs::home_dir().context("repertoire personnel introuvable")?;
    Ok(home.join(".cursor/mcp.json"))
}
