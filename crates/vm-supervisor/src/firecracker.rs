//! Client HTTP minimal pour l'API Firecracker, exposee sur un socket Unix.
//!
//! Pas de crate HTTP-sur-socket-Unix externe (`hyperlocal`, etc.) : les
//! quelques requetes necessaires (PUT/PATCH JSON, lecture d'un GET) sont
//! simples a exprimer directement en HTTP/1.1 sur un `UnixStream`, ce qui
//! evite d'ajouter une dependance de plus a un ecosysteme deja marque par
//! des conflits de versions ce trimestre (cf. rustls/opentelemetry).
//!
//! Contrainte a retenir : `sun_path` (chemin d'un socket Unix) est limite a
//! ~108 octets sur Linux. Garder `socket_path` court (ex: sous `/run/`, pas
//! sous un chemin de working directory profond).

use anyhow::{ensure, Context, Result};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

pub struct FirecrackerClient {
    socket_path: PathBuf,
}

impl FirecrackerClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    pub async fn put(&self, path: &str, body: &Value) -> Result<()> {
        self.request("PUT", path, Some(body)).await.map(|_| ())
    }

    pub async fn patch(&self, path: &str, body: &Value) -> Result<()> {
        self.request("PATCH", path, Some(body)).await.map(|_| ())
    }

    pub async fn get(&self, path: &str) -> Result<Value> {
        let body = self.request("GET", path, None).await?;
        serde_json::from_str(&body).context("reponse Firecracker non-JSON")
    }

    async fn request(&self, method: &str, path: &str, body: Option<&Value>) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connexion au socket Firecracker {:?}", self.socket_path))?;

        let body_str = body.map(|b| b.to_string()).unwrap_or_default();
        let request = format!(
            "{method} {path} HTTP/1.1\r\n\
             Host: localhost\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n\
             {body_str}",
            body_str.len()
        );
        stream
            .write_all(request.as_bytes())
            .await
            .context("envoi de la requete Firecracker")?;

        // Ne pas se fier a la fermeture de connexion pour savoir ou s'arrete
        // la reponse (read_to_end) : rien ne garantit que le serveur ferme
        // la connexion malgre le `Connection: close` demande par le client
        // (constate en pratique : `read_to_end` reste bloque indefiniment
        // sur ce point face a l'API Firecracker). Il faut lire les entetes,
        // trouver `Content-Length`, puis lire exactement ce nombre d'octets
        // de corps — c'est ce que fait `curl` et qui fonctionne de maniere
        // fiable.
        let mut buf = Vec::new();
        let headers_end = loop {
            if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
                break pos + 4;
            }
            let mut chunk = [0u8; 4096];
            let n = stream
                .read(&mut chunk)
                .await
                .context("lecture des entetes de la reponse Firecracker")?;
            ensure!(n > 0, "connexion Firecracker fermee avant la fin des entetes");
            buf.extend_from_slice(&chunk[..n]);
        };

        let headers = String::from_utf8_lossy(&buf[..headers_end]);
        let status_line = headers
            .lines()
            .next()
            .context("reponse HTTP Firecracker vide")?;
        let status: u16 = status_line
            .split_whitespace()
            .nth(1)
            .context("ligne de statut HTTP invalide")?
            .parse()
            .context("code de statut HTTP invalide")?;
        let content_length: usize = headers
            .lines()
            .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|v| v.trim().to_string()))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);

        while buf.len() < headers_end + content_length {
            let mut chunk = [0u8; 4096];
            let n = stream
                .read(&mut chunk)
                .await
                .context("lecture du corps de la reponse Firecracker")?;
            ensure!(n > 0, "connexion Firecracker fermee avant la fin du corps");
            buf.extend_from_slice(&chunk[..n]);
        }
        let response_body =
            String::from_utf8_lossy(&buf[headers_end..headers_end + content_length]).to_string();

        ensure!(
            (200..300).contains(&status),
            "Firecracker a repondu {status} sur {method} {path}: {response_body}"
        );

        Ok(response_body)
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Attend que le socket Firecracker existe (le process vient d'etre lance
/// et n'a pas encore eu le temps de le creer).
pub async fn wait_for_socket(socket_path: &Path, timeout: std::time::Duration) -> Result<()> {
    let deadline = tokio::time::Instant::now() + timeout;
    while !socket_path.exists() {
        ensure!(
            tokio::time::Instant::now() < deadline,
            "timeout en attendant le socket Firecracker {:?}",
            socket_path
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    Ok(())
}
