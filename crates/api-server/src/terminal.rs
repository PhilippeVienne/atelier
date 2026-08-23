//! Pont HTTP+WebSocket vers `ttyd` (port 7681 dans la microVM agent) —
//! terminal riche dans le navigateur (xterm.js, deja embarque par `ttyd`,
//! voir github.com/tsl0922/ttyd) pour un acces shell direct au devcontainer
//! depuis le dashboard, sans avoir a ouvrir tout `code-server`. Reutilise le
//! meme pont generique que `crate::vscode` (`proxy_to_guest_port`) : seul le
//! port cible differe, tout le protocole (portforward -> duplex -> hyper
//! client avec upgrades) est deja teste par le chemin `code-server`.

use crate::auth::AuthenticatedUser;
use crate::routes::{ApiError, AppState};
use crate::vscode::proxy_to_guest_port;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::Request;
use axum::response::Response;

/// Port sur lequel `ttyd` ecoute dans la microVM agent (voir
/// github.com/PhilippeVienne/atelier-workspace `.devcontainer/atelier-terminal.service`) —
/// meme convention que `code_server_port()` : fixe par Workshop pour ce lot,
/// `ATELIER_TERMINAL_PORT` overridable pour les tests.
fn terminal_port() -> u16 {
    std::env::var("ATELIER_TERMINAL_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(7681)
}

pub async fn terminal_proxy_root(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path(name): Path<String>,
    req: Request<Body>,
) -> Result<Response, ApiError> {
    proxy_to_guest_port(
        state,
        user,
        name,
        String::new(),
        terminal_port(),
        "terminal",
        req,
    )
    .await
}

pub async fn terminal_proxy(
    State(state): State<AppState>,
    Extension(user): Extension<AuthenticatedUser>,
    Path((name, path)): Path<(String, String)>,
    req: Request<Body>,
) -> Result<Response, ApiError> {
    proxy_to_guest_port(state, user, name, path, terminal_port(), "terminal", req).await
}
