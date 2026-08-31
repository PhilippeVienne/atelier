//! Credentials d'un Workshop : regles d'injection saisies depuis
//! l'interface, secrets deposes dans OpenBao par CE serveur.
//!
//! Le principe est celui d'`identity-proxy` (voir
//! `atelier_common::crd::IdentityInjectionRule`) : l'agent appelle une API
//! tierce sans jamais detenir le credential, c'est le proxy qui pose
//! l'en-tete au passage. Ce module ne fait qu'exposer ce mecanisme deja
//! eprouve — il sert deja a la forge Git et a la Virtual Key LiteLLM.
//!
//! **Ce que ce module ne fait jamais** : relire un secret. La valeur saisie
//! part directement dans OpenBao et n'en ressort plus, ni par cette API, ni
//! par la spec du Workshop, ni par un journal. La policy OpenBao de
//! l'api-server accorde d'ailleurs `create`/`update` mais PAS `read` sur ces
//! chemins (voir `atelier_controller::openbao::ensure_api_server_role`) :
//! l'interdiction est appliquee par OpenBao lui-meme, pas seulement par la
//! discipline de ce code.

use crate::session_auth::SessionAuthClient;
use atelier_common::IdentityInjectionRule;

/// Champ du secret KV. Un seul par credential : la valeur de l'en-tete.
pub const CREDENTIAL_FIELD: &str = "value";

/// Chemin du secret d'un credential, derive de l'hote.
///
/// Derive plutot que choisi par le client : deux regles pour un meme hote
/// n'auraient pas de sens (identity-proxy en applique une seule), et laisser
/// le client nommer le chemin ouvrirait la porte a un `../` vers un autre
/// secret du Workshop. Les caracteres hors `[a-z0-9.-]` sont remplaces, ce
/// qui rend le chemin sûr par construction.
pub fn secret_path(host: &str) -> String {
    let mut slug: String = host
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();

    // Le point est legitime dans un nom d'hote, mais DEUX points de suite
    // forment une remontee de chemin : `secret_path("..")` donnerait
    // `credentials/..`, qui designe le repertoire des secrets du Workshop —
    // dont sa cle SSH. On les ecrase, ainsi que les points en bordure.
    while slug.contains("..") {
        slug = slug.replace("..", "-");
    }
    let slug = slug.trim_matches('.').to_string();

    // Un hote entierement compose de caracteres remplaces ne doit pas
    // produire un chemin vide, qui viserait le repertoire parent.
    let slug = if slug.is_empty() {
        "sans-nom".to_string()
    } else {
        slug
    };
    format!("credentials/{slug}")
}

/// Une regle telle qu'elle est presentee au client : jamais la valeur.
#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct CredentialSummary {
    pub host: String,
    pub header: String,
    pub prefix: String,
    /// Chemin OpenBao, montre a titre indicatif : il dit ou le secret vit,
    /// pas ce qu'il vaut.
    pub secret_path: String,
}

impl From<&IdentityInjectionRule> for CredentialSummary {
    fn from(rule: &IdentityInjectionRule) -> Self {
        Self {
            host: rule.host.clone(),
            header: rule.header.clone(),
            prefix: rule.prefix.clone(),
            secret_path: rule.secret_path.clone(),
        }
    }
}

/// Depose la valeur d'un credential dans OpenBao.
pub async fn store_secret(
    client: &SessionAuthClient,
    workshop_name: &str,
    host: &str,
    value: &str,
) -> anyhow::Result<()> {
    client
        .write_secret_field(workshop_name, &secret_path(host), CREDENTIAL_FIELD, value)
        .await
}

/// Supprime definitivement la valeur d'un credential.
pub async fn remove_secret(
    client: &SessionAuthClient,
    workshop_name: &str,
    host: &str,
) -> anyhow::Result<()> {
    client
        .delete_secret(workshop_name, &secret_path(host))
        .await
}

#[cfg(test)]
mod tests {
    use super::secret_path;

    /// Le chemin est DERIVE de l'hote : un client ne le choisit pas, et ne
    /// peut donc pas viser `../session_auth` ni la cle SSH du Workshop.
    #[test]
    fn secret_path_is_derived_and_cannot_escape() {
        assert_eq!(secret_path("api.stripe.com"), "credentials/api.stripe.com");
        assert_eq!(secret_path("API.Stripe.COM"), "credentials/api.stripe.com");
        // `..` ecrase : sans cela le chemin remonterait au repertoire des
        // secrets du Workshop, ou vit sa cle SSH.
        assert_eq!(secret_path("../ssh_key"), "credentials/--ssh-key");
        assert_eq!(secret_path(".."), "credentials/-");
        assert_eq!(secret_path("..."), "credentials/-");
        assert_eq!(secret_path("/"), "credentials/-");
        assert_eq!(secret_path("a/b"), "credentials/a-b");
        assert!(!secret_path("../../etc").contains(".."));
    }
}
