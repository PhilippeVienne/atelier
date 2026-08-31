//! Regles d'injection : quel en-tete poser, avec quel secret OpenBao,
//! pour les requetes a destination de quel hote.
//!
//! Le type de regle est defini une seule fois, dans `atelier_common::crd`
//! (partagee avec `Workshop.spec.identity_injection_rules`, dont le
//! controller serialise le contenu tel quel vers cette variable
//! d'environnement — voir `crates/controller/src/reconcile.rs`) : ce module
//! ne fait plus que charger/faire correspondre ces regles.

pub use atelier_common::IdentityInjectionRule as InjectionRule;

/// Cle utilisee pour retrouver la valeur en cache (voir `secrets.rs`) : un
/// meme secret peut alimenter plusieurs regles (hotes differents), donc
/// indexe par `(secret_path, field)` plutot que par hote. Extension plutot
/// que methode inherente : `InjectionRule` est definie dans `atelier_common`,
/// pas dans ce crate.
pub trait InjectionRuleExt {
    fn secret_cache_key(&self) -> String;
}

impl InjectionRuleExt for InjectionRule {
    fn secret_cache_key(&self) -> String {
        format!("{}#{}", self.secret_path, self.field)
    }
}

/// Charge les regles depuis `ATELIER_IDENTITY_INJECTION_RULES` (JSON,
/// tableau d'objets au format de [`InjectionRule`]). Absente ou vide :
/// aucune regle, identity-proxy relaie tel quel sans jamais injecter.
pub fn from_env() -> anyhow::Result<Vec<InjectionRule>> {
    let raw = std::env::var("ATELIER_IDENTITY_INJECTION_RULES").unwrap_or_default();
    parse(&raw, "ATELIER_IDENTITY_INJECTION_RULES")
}

/// Chemin d'un FICHIER de regles, relu periodiquement.
///
/// L'interet sur la variable d'environnement : une variable est figee a la
/// creation du pod, si bien qu'ajouter un credential depuis l'interface
/// n'avait d'effet qu'apres une mise en veille puis une reprise du Workshop.
/// Un fichier monte depuis une ConfigMap, lui, est mis a jour par kubelet
/// sans redemarrage.
pub const RULES_FILE_ENV: &str = "ATELIER_IDENTITY_INJECTION_RULES_FILE";

/// Charge les regles depuis le fichier designe par [`RULES_FILE_ENV`], ou
/// depuis la variable d'environnement si aucun fichier n'est configure.
///
/// Un fichier ABSENT n'est pas une erreur et ne vide pas les regles : kubelet
/// remplace le contenu d'un volume de ConfigMap de facon atomique, mais le
/// montage peut n'etre pas encore la au tout premier instant. Renvoyer une
/// liste vide dans ce cas ferait relayer sans injecter — un credential
/// silencieusement ignore vaut moins qu'une erreur bruyante.
pub fn load() -> anyhow::Result<Option<Vec<InjectionRule>>> {
    let Some(path) = std::env::var(RULES_FILE_ENV).ok().filter(|p| !p.is_empty()) else {
        return from_env().map(Some);
    };
    match std::fs::read_to_string(&path) {
        Ok(raw) => parse(&raw, &path).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::anyhow!("lecture de {path} : {err}")),
    }
}

fn parse(raw: &str, source: &str) -> anyhow::Result<Vec<InjectionRule>> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    serde_json::from_str(raw)
        .map_err(|err| anyhow::anyhow!("regles d'injection invalides ({source}) : {err}"))
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
        let json = r#"[{"host":"api.github.com","header":"Authorization","prefix":"Bearer ","secretPath":"github","field":"token"}]"#;
        std::env::set_var("ATELIER_IDENTITY_INJECTION_RULES", json);
        let rules = from_env().unwrap();
        std::env::remove_var("ATELIER_IDENTITY_INJECTION_RULES");
        assert_eq!(rules.len(), 1);
        assert_eq!(rules[0].host, "api.github.com");
        assert_eq!(rules[0].field, "token");
    }
}
