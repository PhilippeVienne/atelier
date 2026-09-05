//! Gestion des contextes multi-environnements (spec `docs/specs/14-devex-cli-simulateurs-hitl.md`
//! §3.3) : `~/.config/atelier/config.yaml` memorise, par contexte nomme,
//! l'URL de l'`api-server` cible et les parametres OIDC generiques
//! necessaires au flux Device Authorization Grant (`crate::oidc`). Les jetons
//! eux-memes ne transitent jamais par ce fichier (voir `crate::tokens`) :
//! seule la configuration, non sensible, y vit.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextConfig {
    /// URL de base de l'`api-server` cible (ex: `https://api.atelier.acme.corp`).
    pub api_url: String,
    /// Issuer OIDC (RFC 8414 `.well-known/openid-configuration` derriere),
    /// jamais un endpoint specifique a un fournisseur : voir
    /// `docs/specs/00-architecture-principles-substitutability.md`.
    pub issuer: String,
    /// `client_id` OAuth2 public utilise pour le Device Authorization Grant.
    pub client_id: String,
    #[serde(default = "default_scope")]
    pub scope: String,
}

fn default_scope() -> String {
    "openid profile email".to_string()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub current: Option<String>,
    #[serde(default)]
    pub contexts: BTreeMap<String, ContextConfig>,
}

impl Config {
    pub fn path() -> Result<PathBuf> {
        let dir = dirs::config_dir()
            .context("impossible de determiner le repertoire de configuration de l'OS")?
            .join("atelier");
        Ok(dir.join("config.yaml"))
    }

    pub fn load() -> Result<Self> {
        let path = Self::path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("lecture de {}", path.display()))?;
        serde_yaml::from_str(&raw).with_context(|| format!("parsing de {}", path.display()))
    }

    /// Ecrit le fichier avec des permissions restreintes (0600 sur Unix) :
    /// ce fichier contient des `client_id`/URLs, pas de secret, mais reste
    /// une cible de choix pour rediriger un client vers un issuer non
    /// desire.
    pub fn save(&self) -> Result<()> {
        let path = Self::path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creation de {}", parent.display()))?;
        }
        let raw = serde_yaml::to_string(self).context("serialisation de la configuration")?;
        std::fs::write(&path, raw).with_context(|| format!("ecriture de {}", path.display()))?;
        set_owner_only_permissions(&path)?;
        Ok(())
    }

    pub fn current_context(&self) -> Result<(&str, &ContextConfig)> {
        let name = self
            .current
            .as_deref()
            .context("aucun contexte actif : `atelier context use <name>`")?;
        let ctx = self
            .contexts
            .get(name)
            .with_context(|| format!("contexte '{name}' introuvable dans la configuration"))?;
        Ok((name, ctx))
    }
}

#[cfg(unix)]
fn set_owner_only_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restriction des permissions de {}", path.display()))
}

#[cfg(not(unix))]
fn set_owner_only_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
