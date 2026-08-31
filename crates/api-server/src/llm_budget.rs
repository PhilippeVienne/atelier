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

// --------------------------------------------------------------------------
// Vue d'administration
// --------------------------------------------------------------------------

/// Etat de la passerelle LiteLLM, destine aux seuls administrateurs.
///
/// Ce qui est volontairement ABSENT : le jeton des cles. LiteLLM ne le rend
/// d'ailleurs pas (`/key/list` renvoie un hachage et un nom tronque), et
/// c'est tres bien ainsi — une console d'administration n'a aucune raison de
/// pouvoir rejouer les appels d'un Workshop. Les cles d'API des fournisseurs
/// ne sont pas exposees non plus : `/model/info` ne renvoie que `model` et
/// `api_base`.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmOverview {
    /// Depense cumulee de TOUTES les cles, y compris le jeton statique
    /// partage — donc superieure a la somme des Workshops.
    pub global_spend_usd: Option<f64>,
    pub models: Vec<LlmModel>,
    pub keys: Vec<LlmKey>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmModel {
    /// Nom expose aux clients (`claude-3-5-sonnet-20241022`, `*`...).
    pub name: String,
    /// Modele reellement appele derriere cet alias.
    pub target: Option<String>,
    pub api_base: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LlmKey {
    pub alias: String,
    /// Sujet OIDC proprietaire, propage en metadonnee a la generation.
    pub owner: Option<String>,
    pub spend_usd: f64,
    pub max_budget_usd: Option<f64>,
    pub expires_at: Option<String>,
    /// Calcule ici plutot que dans l'interface : la comparaison depend de
    /// l'horloge, et celle du serveur fait davantage autorite que celle du
    /// navigateur.
    pub expired: bool,
}

#[derive(Debug, Deserialize)]
struct AdminKeyInfo {
    #[serde(default)]
    key_alias: Option<String>,
    #[serde(default)]
    spend: Option<f64>,
    #[serde(default)]
    max_budget: Option<f64>,
    #[serde(default)]
    expires: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
struct AdminKeyList {
    #[serde(default)]
    keys: Vec<AdminKeyInfo>,
}

#[derive(Debug, Deserialize)]
struct GlobalSpend {
    #[serde(default)]
    spend: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct ModelInfoList {
    #[serde(default)]
    data: Vec<ModelInfoEntry>,
}

#[derive(Debug, Deserialize)]
struct ModelInfoEntry {
    #[serde(default)]
    model_name: Option<String>,
    #[serde(default)]
    litellm_params: Option<serde_json::Value>,
}

impl LlmBudgetClient {
    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Option<T> {
        self.http
            .get(format!("http://{}{path}", self.addr))
            .bearer_auth(&self.master_key)
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()
    }

    /// Vue d'ensemble de la passerelle.
    ///
    /// Chaque partie est recuperee independamment et son absence est toleree
    /// (`Option`, listes vides) : une console d'administration qui ne
    /// s'affiche pas du tout parce qu'un seul de ses trois panneaux est
    /// indisponible est moins utile qu'une console partielle qui le dit.
    pub async fn overview(&self) -> LlmOverview {
        let global_spend_usd = self
            .get_json::<GlobalSpend>("/global/spend")
            .await
            .and_then(|s| s.spend);

        let models = self
            .get_json::<ModelInfoList>("/model/info")
            .await
            .map(|list| {
                list.data
                    .into_iter()
                    .filter_map(|entry| {
                        let params = entry.litellm_params.unwrap_or(serde_json::Value::Null);
                        Some(LlmModel {
                            name: entry.model_name?,
                            target: params
                                .get("model")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            api_base: params
                                .get("api_base")
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        let now = chrono::Utc::now();
        // `size` est plafonne a 100 par LiteLLM : au-dela il repond `422`, que
        // `get_json` transformait silencieusement en liste vide. Les Virtual
        // Keys ayant un TTL court, cette premiere page couvre tout ce qui est
        // actif ; c'est l'historique expire qui serait tronque.
        let keys = self
            .get_json::<AdminKeyList>("/key/list?return_full_object=true&size=100")
            .await
            .map(|list| {
                let mut keys: Vec<LlmKey> = list
                    .keys
                    .into_iter()
                    .filter_map(|key| {
                        let alias = key.key_alias?;
                        let expires_at = key.expires;
                        let expired = expires_at
                            .as_deref()
                            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
                            .is_some_and(|d| d < now);
                        Some(LlmKey {
                            alias,
                            owner: key
                                .metadata
                                .as_ref()
                                .and_then(|m| m.get("owner"))
                                .and_then(|v| v.as_str())
                                .map(str::to_string),
                            spend_usd: key.spend.unwrap_or(0.0),
                            max_budget_usd: key.max_budget,
                            expires_at,
                            expired,
                        })
                    })
                    .collect();
                // Les cles actives d'abord, puis par depense decroissante :
                // ce qu'un administrateur cherche, c'est ce qui consomme
                // maintenant, pas l'ordre interne de LiteLLM.
                keys.sort_by(|a, b| {
                    a.expired.cmp(&b.expired).then(
                        b.spend_usd
                            .partial_cmp(&a.spend_usd)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                });
                keys
            })
            .unwrap_or_default();

        LlmOverview {
            global_spend_usd,
            models,
            keys,
        }
    }
}
