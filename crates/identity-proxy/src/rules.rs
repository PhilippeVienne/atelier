//! Regles d'injection : quel en-tete poser, avec quel secret OpenBao,
//! pour les requetes a destination de quel hote.
//!
//! Pas encore alimente par le controller depuis `Workshop.spec` (voir
//! PROGRESS.md, "Prochaines etapes" #3) : pour l'instant configure via une
//! variable d'environnement JSON, sur le meme modele que
//! `ATELIER_EGRESS_ALLOWLIST` cote net-proxy avant son propre branchement.

use serde::Deserialize;

/// Une regle : les requetes dont l'hote correspond a `host` (correspondance
/// exacte ou prefixe `*.`, meme syntaxe que l'allowlist de net-proxy)
/// recoivent l'en-tete `header` construit comme `prefix` + la valeur du
/// champ `field` du secret KV v2 stocke sous
/// `secret/workshops/<name>/<secret_path>`.
#[derive(Debug, Clone, Deserialize)]
pub struct InjectionRule {
    pub host: String,
    pub header: String,
    #[serde(default)]
    pub prefix: String,
    pub secret_path: String,
    #[serde(default = "default_field")]
    pub field: String,
}

fn default_field() -> String {
    "value".to_string()
}

impl InjectionRule {
    /// Cle utilisee pour retrouver la valeur en cache (voir `secrets.rs`) :
    /// un meme secret peut alimenter plusieurs regles (hotes differents),
    /// donc indexe par `(secret_path, field)` plutot que par hote.
    pub fn secret_cache_key(&self) -> String {
        format!("{}#{}", self.secret_path, self.field)
    }
}

/// Charge les regles depuis `ATELIER_IDENTITY_INJECTION_RULES` (JSON,
/// tableau d'objets au format de [`InjectionRule`]). Absente ou vide :
/// aucune regle, identity-proxy relaie tel quel sans jamais injecter.
pub fn from_env() -> anyhow::Result<Vec<InjectionRule>> {
    let raw = std::env::var("ATELIER_IDENTITY_INJECTION_RULES").unwrap_or_default();
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let rules: Vec<InjectionRule> = serde_json::from_str(&raw).map_err(|err| {
        anyhow::anyhow!("regles d'injection invalides (ATELIER_IDENTITY_INJECTION_RULES) : {err}")
    })?;
    Ok(rules)
}

/// Trouve la premiere regle dont `host` correspond a la destination de la
/// requete (meme syntaxe de correspondance que l'allowlist de net-proxy :
/// exacte ou wildcard `*.domaine`).
pub fn matching<'a>(rules: &'a [InjectionRule], host: &str) -> Option<&'a InjectionRule> {
    rules.iter().find(|rule| host_matches(&rule.host, host))
}

fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.ends_with(suffix) && host.len() > suffix.len(),
        None => pattern.eq_ignore_ascii_case(host),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match() {
        let rule = InjectionRule {
            host: "api.github.com".to_string(),
            header: "Authorization".to_string(),
            prefix: "Bearer ".to_string(),
            secret_path: "github".to_string(),
            field: "token".to_string(),
        };
        assert!(matching(std::slice::from_ref(&rule), "api.github.com").is_some());
        assert!(matching(std::slice::from_ref(&rule), "evil.com").is_none());
    }

    #[test]
    fn wildcard_match() {
        let rule = InjectionRule {
            host: "*.example.org".to_string(),
            header: "Authorization".to_string(),
            prefix: String::new(),
            secret_path: "example".to_string(),
            field: "value".to_string(),
        };
        assert!(matching(std::slice::from_ref(&rule), "api.example.org").is_some());
        assert!(matching(std::slice::from_ref(&rule), "example.org").is_none());
    }

    #[test]
    fn parses_from_json() {
        let json = r#"[{"host":"api.github.com","header":"Authorization","prefix":"Bearer ","secret_path":"github","field":"token"}]"#;
        std::env::set_var("ATELIER_IDENTITY_INJECTION_RULES", json);
        let rules = from_env().unwrap();
        std::env::remove_var("ATELIER_IDENTITY_INJECTION_RULES");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].host, "api.github.com");
        assert_eq!(rules[0].field, "token");
    }
}
