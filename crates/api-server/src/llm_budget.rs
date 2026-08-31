//! Consommation LLM d'un Workshop, lue depuis LiteLLM.
//!
//! Le controller provisionne une Virtual Key par Workshop, a budget plafonne
//! et TTL court (`crates/controller/src/litellm.rs`). LiteLLM tient lui-meme
//! la comptabilite de ce qui est depense sur chaque cle : ce module ne fait
//! que la RELIRE. Aucun compteur maison — un second decompte finirait par
//! diverger de celui qui fait autorite, et c'est bien LiteLLM qui applique
//! le plafond.
//!
//! Deux cles concernent un meme Workshop et leurs depenses s'additionnent :
//! `atelier-build-<nom>` (construction de l'image devcontainer) et
//! `atelier-wks-<nom>` (l'agent lui-meme). Les presenter separement serait
//! exact mais inutilement subtil : ce qu'on veut savoir, c'est ce que ce
//! Workshop a coute.

use serde::Deserialize;

/// Meme convention de nommage que `atelier_controller::litellm` — dupliquee
/// ici plutot que partagee, `api-server` ne dependant pas du controller.
/// Toute evolution doit rester synchronisee avec lui (voir le test).
fn key_aliases(workshop_name: &str) -> [String; 2] {
    [
        format!("atelier-wks-{workshop_name}"),
        format!("atelier-build-{workshop_name}"),
    ]
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LlmBudget {
    /// Depense cumulee des cles de ce Workshop, en dollars.
    pub spend_usd: f64,
    /// Plafond, s'il en existe un. `None` = pas de plafond configure : ce
    /// n'est pas « zero », et l'interface doit les distinguer.
    pub max_budget_usd: Option<f64>,
    /// Nombre de cles trouvees. `0` signifie qu'aucune Virtual Key n'existe
    /// (encore) pour ce Workshop — la depense affichee vaut alors zero par
    /// absence de donnee, pas par mesure.
    pub key_count: usize,
}

#[derive(Debug, Deserialize)]
struct KeyListResponse {
    #[serde(default)]
    keys: Vec<KeyInfo>,
}

#[derive(Debug, Deserialize)]
struct KeyInfo {
    #[serde(default)]
    spend: Option<f64>,
    #[serde(default)]
    max_budget: Option<f64>,
}

pub struct LlmBudgetClient {
    addr: String,
    master_key: String,
    http: reqwest::Client,
}

impl LlmBudgetClient {
    pub fn new(addr: String, master_key: String) -> Self {
        Self {
            addr,
            master_key,
            http: reqwest::Client::new(),
        }
    }

    /// Depense d'un Workshop, ou `None` si LiteLLM est injoignable.
    ///
    /// `None` plutot qu'une erreur remontee a l'appelant : la consommation
    /// est une information d'appoint, et une passerelle LiteLLM
    /// momentanement indisponible ne doit pas faire echouer l'affichage d'un
    /// Workshop par ailleurs parfaitement sain.
    pub async fn workshop_budget(&self, workshop_name: &str) -> Option<LlmBudget> {
        let mut spend_usd = 0.0;
        let mut max_budget_usd: Option<f64> = None;
        let mut key_count = 0;

        for alias in key_aliases(workshop_name) {
            let response = self
                .http
                .get(format!("http://{}/key/list", self.addr))
                .bearer_auth(&self.master_key)
                .query(&[
                    ("key_alias", alias.as_str()),
                    ("return_full_object", "true"),
                ])
                .send()
                .await
                .ok()?
                .error_for_status()
                .ok()?;
            let parsed: KeyListResponse = response.json().await.ok()?;
            for key in parsed.keys {
                key_count += 1;
                spend_usd += key.spend.unwrap_or(0.0);
                // Le plafond le plus BAS fait foi : c'est lui qui coupera en
                // premier, donc le seul qui renseigne sur la marge reelle.
                if let Some(budget) = key.max_budget {
                    max_budget_usd = Some(match max_budget_usd {
                        Some(current) => current.min(budget),
                        None => budget,
                    });
                }
            }
        }

        Some(LlmBudget {
            spend_usd,
            max_budget_usd,
            key_count,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::key_aliases;

    /// Ces alias doivent rester identiques a ceux que le controller donne
    /// aux Virtual Keys (`workshop_key_alias`/`build_key_alias`) : c'est le
    /// seul lien entre les deux composants, et une divergence se traduirait
    /// par une consommation affichee a zero, sans erreur nulle part.
    #[test]
    fn aliases_match_the_controller_naming_convention() {
        assert_eq!(
            key_aliases("pm-16-task-1"),
            [
                "atelier-wks-pm-16-task-1".to_string(),
                "atelier-build-pm-16-task-1".to_string()
            ]
        );
    }
}
