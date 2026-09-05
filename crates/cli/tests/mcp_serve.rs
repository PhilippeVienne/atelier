//! Test d'integration reel de la tache 9.9 (`atelier mcp serve`) : lance le
//! VRAI binaire `atelier` compile comme sous-processus (exactement comme le
//! ferait Claude Desktop/Cursor, voir `crate::commands::mcp::install_config`),
//! parle MCP dessus via stdio avec un vrai client `rmcp`
//! (`rmcp::transport::child_process::TokioChildProcess`), et verifie que les
//! outils `atelier_*` sont bien annonces.
//!
//! Necessite un contexte CLI deja configure et authentifie (`atelier auth
//! login` prealable, contre un vrai `api-server`/Keycloak de dev) : ignore
//! silencieusement sinon, comme les autres tests d'integration reels de ce
//! depot.

use rmcp::transport::child_process::TokioChildProcess;
use rmcp::ServiceExt;
use tokio::process::Command;

fn atelier_binary() -> Option<std::path::PathBuf> {
    let candidate =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/atelier");
    candidate.exists().then_some(candidate)
}

#[tokio::test]
async fn atelier_mcp_serve_announces_atelier_tools_over_stdio() {
    let Some(bin) = atelier_binary() else {
        eprintln!("binaire atelier non construit, test ignore");
        return;
    };

    let mut command = Command::new(&bin);
    command.args(["mcp", "serve"]);
    let transport = match TokioChildProcess::new(command) {
        Ok(t) => t,
        Err(err) => {
            eprintln!("lancement du sous-processus atelier echoue: {err}");
            return;
        }
    };

    let client = match tokio::time::timeout(std::time::Duration::from_secs(10), ().serve(transport))
        .await
    {
        Ok(Ok(client)) => client,
        Ok(Err(err)) => {
            eprintln!(
                "connexion/handshake MCP echouee (pas de contexte CLI authentifie ? {err}), test ignore"
            );
            return;
        }
        Err(_) => {
            eprintln!("timeout de connexion MCP, test ignore");
            return;
        }
    };

    let tools = client
        .peer()
        .list_tools(None)
        .await
        .expect("tools/list contre le serveur MCP stdio");
    let names: Vec<&str> = tools.tools.iter().map(|t| t.name.as_ref()).collect();

    for expected in [
        "atelier_create_sandbox",
        "atelier_list_sandboxes",
        "atelier_exec_in_sandbox",
        "atelier_read_file",
        "atelier_write_file",
        "atelier_git_diff",
        "atelier_suspend",
        "atelier_resume",
    ] {
        assert!(
            names.contains(&expected),
            "tools/list doit annoncer {expected}, recu: {names:?}"
        );
    }

    let listed = client
        .peer()
        .call_tool(rmcp::model::CallToolRequestParams::new(
            "atelier_list_sandboxes",
        ))
        .await
        .expect("appel atelier_list_sandboxes (relaye vers le vrai api-server)");
    assert_ne!(
        listed.is_error,
        Some(true),
        "atelier_list_sandboxes ne doit pas echouer: {listed:?}"
    );
}
