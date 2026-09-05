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
pub fn key_aliases_for(workshop_name: &str) -> [String; 2] {
    key_aliases(workshop_name)
}

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
    /// Identifiant stable attribue par LiteLLM aux entrees de `model_list`
    /// creees dynamiquement (`model_info.id`, verifie empiriquement contre
    /// `/model/new` — voir `docs/specs/11-admin-litellm-model-config.md`
    /// §3.2). `None` pour une entree statique du `config.yaml` (cas du
    /// cluster de dev) : `update`/`delete` n'ont alors rien a cibler.
    pub id: Option<String>,
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
    #[serde(default)]
    model_info: Option<serde_json::Value>,
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
                        let info = entry.model_info.unwrap_or(serde_json::Value::Null);
                        // `model_info.db_model` distingue une entree ajoutee
                        // dynamiquement (`true`, verifie empiriquement) d'une
                        // entree statique du `config.yaml` (`false`) — CETTE
                        // DERNIERE a elle aussi un `id` non-nul dans la
                        // reponse LiteLLM, mais update/delete n'ont aucun
                        // sens dessus (elle revient telle quelle au prochain
                        // redemarrage du pod, un `/model/delete` dessus
                        // laisserait croire a une suppression durable qui ne
                        // l'est pas). N'exposer `id` que si `db_model` est
                        // vrai.
                        let is_db_model =
                            info.get("db_model").and_then(|v| v.as_bool()) == Some(true);
                        Some(LlmModel {
                            id: is_db_model
                                .then(|| {
                                    info.get("id").and_then(|v| v.as_str()).map(str::to_string)
                                })
                                .flatten(),
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

#[derive(Debug, Deserialize)]
struct ModelWriteResponse {
    model_info: ModelWriteInfo,
}

#[derive(Debug, Deserialize)]
struct ModelWriteInfo {
    id: String,
}

impl LlmBudgetClient {
    fn model_payload(
        model_name: &str,
        target: &str,
        api_base: Option<&str>,
        api_key: Option<&str>,
    ) -> serde_json::Value {
        let mut litellm_params = serde_json::json!({ "model": target });
        if let Some(base) = api_base {
            litellm_params["api_base"] = serde_json::Value::String(base.to_string());
        }
        if let Some(key) = api_key {
            litellm_params["api_key"] = serde_json::Value::String(key.to_string());
        }
        serde_json::json!({ "model_name": model_name, "litellm_params": litellm_params })
    }

    async fn post_model_mutation(
        &self,
        path: &str,
        body: &serde_json::Value,
    ) -> Result<reqwest::Response, String> {
        let response = self
            .http
            .post(format!("http://{}{path}", self.addr))
            .bearer_auth(&self.master_key)
            .json(body)
            .send()
            .await
            .map_err(|e| format!("passerelle LiteLLM injoignable : {e}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let text = response.text().await.unwrap_or_default();
            return Err(format!("LiteLLM a repondu {status} : {text}"));
        }
        Ok(response)
    }

    /// Ajoute un modele a `model_list` (`POST /model/new`, verifie
    /// empiriquement contre le cluster de dev — spec 11 §3.1). Renvoie l'`id`
    /// genere par LiteLLM, seule cible valide de `update_model`/`delete_model`
    /// (`model_name` n'est pas garanti unique, voir spec 11 §3.2).
    ///
    /// Precondition (verifiee de la meme facon) : `STORE_MODEL_IN_DB=True`
    /// doit etre positionnee sur le deploiement LiteLLM, sans quoi cet appel
    /// echoue systematiquement avec `500`.
    pub async fn create_model(
        &self,
        model_name: &str,
        target: &str,
        api_base: Option<&str>,
        api_key: &str,
    ) -> Result<String, String> {
        let body = Self::model_payload(model_name, target, api_base, Some(api_key));
        let response = self.post_model_mutation("/model/new", &body).await?;
        let parsed: ModelWriteResponse = response
            .json()
            .await
            .map_err(|e| format!("reponse LiteLLM inattendue : {e}"))?;
        Ok(parsed.model_info.id)
    }

    /// Modifie un modele existant (`POST /model/update`, cible par
    /// `model_info.id` dans le corps — verifie empiriquement, spec 11 §3.1).
    ///
    /// `api_key: None` omet le champ du payload envoye a LiteLLM, qui
    /// PRESERVE alors la cle existante — verifie par un test FONCTIONNEL
    /// (`GET /model/info` ne renvoie jamais ce champ, succes ou echec, donc
    /// inutilisable pour verifier quoi que ce soit ici) : creer un modele
    /// avec une vraie cle, appel reel reussi, `update` sans `api_key`,
    /// meme appel toujours reussi ; a l'inverse, `update` avec une cle
    /// FAUSSE fait bien echouer l'appel suivant en 401. `litellm_params`
    /// est donc fusionne champ par champ par LiteLLM, pas remplace
    /// integralement (spec 11 §5).
    pub async fn update_model(
        &self,
        id: &str,
        model_name: &str,
        target: &str,
        api_base: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<(), String> {
        let mut body = Self::model_payload(model_name, target, api_base, api_key);
        body["model_info"] = serde_json::json!({ "id": id });
        self.post_model_mutation("/model/update", &body).await?;
        Ok(())
    }

    /// Retire un modele (`POST /model/delete`, body `{"id": ...}` — verifie
    /// empiriquement, spec 11 §3.1).
    pub async fn delete_model(&self, id: &str) -> Result<(), String> {
        let body = serde_json::json!({ "id": id });
        self.post_model_mutation("/model/delete", &body).await?;
        Ok(())
    }
}

// --------------------------------------------------------------------------
// Rapport de depense : qui a consomme quoi, et quand
// --------------------------------------------------------------------------
//
// La depense par Workshop (`workshop_budget`) et le total global existaient
// deja ; entre les deux, rien. Impossible de repondre a « combien a coute ce
// groupe cette semaine », ni « qu'est-ce qui consomme le plus », sans lire a
// la main les journaux de LiteLLM — ce que j'ai fait le 2026-09-01, et qui a
// pris une douzaine de requetes et failli produire un chiffre FAUX (voir plus
// bas `TEST_PRICING_MODELS`).
//
// Note pour qui chercherait mieux : `/global/spend/report`, qui agregerait
// tout cela cote LiteLLM, est reserve a l'edition Enterprise (verifie : il
// repond « You must be a LiteLLM Enterprise user »). L'agregation se fait
// donc ici, a partir de `/spend/logs`.

/// Modeles dont le TARIF est fictif : ils servent a exercer l'application des
/// plafonds sans attendre une vraie consommation, et sont donc factures des
/// dollars par requete. Les compter dans une depense presentee a un humain
/// donnerait un chiffre faux de deux ordres de grandeur — le 2026-09-01, les
/// journaux affichaient 211,49 $ dont 210,00 $ venaient de QUATORZE requetes
/// de test a 15 $ piece. La depense reelle etait de 1,49 $.
const TEST_PRICING_MODELS: [&str; 2] = ["openai/atelier-plan-test", "openai/atelier-budget-test"];

/// Ce qu'une requete a coute, tel que LiteLLM le journalise.
#[derive(Debug, Deserialize)]
struct SpendLogEntry {
    #[serde(default)]
    spend: Option<f64>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default, rename = "startTime")]
    start_time: Option<String>,
    /// Hachage de la cle utilisee : c'est LUI qui permet de rattacher une
    /// requete a un Workshop (jointure sur `token` de `/key/list`), et donc a
    /// un groupe. Le jeton statique partage n'y figure pas, d'ou la part
    /// « non rattachee ».
    #[serde(default)]
    api_key: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendBucket {
    pub label: String,
    pub spend_usd: f64,
    pub request_count: usize,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SpendReport {
    /// Depense reelle, hors modeles a tarif fictif.
    pub total_usd: f64,
    /// Ce qui a ete ECARTE du total, dit explicitement plutot que
    /// silencieusement : un montant qu'on retire sans le montrer est un
    /// montant qu'on finira par oublier d'expliquer.
    pub test_pricing_usd: f64,
    /// Depense qu'aucune Virtual Key ne revendique — le jeton statique
    /// partage. C'est la part que les plafonds par Workshop NE GOUVERNENT
    /// PAS : la afficher est tout l'interet de ce rapport.
    pub unattributed_usd: f64,
    pub by_day: Vec<SpendBucket>,
    pub by_group: Vec<SpendBucket>,
    pub by_model: Vec<SpendBucket>,
}

/// Rattachement d'une cle a un groupe.
///
/// DEUX sources, parce qu'aucune ne suffit seule :
///
/// - `group_by_alias` vient des Workshops vivants (`routes.rs` : `llm_budget`
///   ne connait ni les Workshops ni Kubernetes). C'est la source qui fait
///   autorite, mais elle disparait avec le Workshop.
/// - `group_by_token` vient de la metadonnee `owner` que le controller ecrit
///   dans la Virtual Key elle-meme, et qui SURVIT a la suppression du
///   Workshop. Sans elle, supprimer un Workshop ferait basculer toute sa
///   depense passee dans « non rattache » — l'argent ne disparaitrait pas du
///   total, mais on ne saurait plus qui l'a depense.
pub struct KeyOwnership {
    /// Hachage de cle -> alias.
    pub alias_by_token: std::collections::HashMap<String, String>,
    /// Alias -> groupe proprietaire, d'apres les Workshops existants.
    pub group_by_alias: std::collections::HashMap<String, String>,
    /// Hachage de cle -> groupe, d'apres la metadonnee de la cle.
    pub group_by_token: std::collections::HashMap<String, String>,
}

impl KeyOwnership {
    /// Groupe d'une requete. Le Workshop vivant l'emporte sur la metadonnee :
    /// il reflete l'etat courant, la metadonnee celui de la generation.
    fn group_of(&self, token: Option<&String>) -> Option<String> {
        let token = token?;
        self.alias_by_token
            .get(token)
            .and_then(|alias| self.group_by_alias.get(alias))
            .or_else(|| self.group_by_token.get(token))
            .cloned()
    }
}

fn sorted_buckets(map: std::collections::HashMap<String, (f64, usize)>) -> Vec<SpendBucket> {
    let mut buckets: Vec<SpendBucket> = map
        .into_iter()
        .map(|(label, (spend_usd, request_count))| SpendBucket {
            label,
            spend_usd,
            request_count,
        })
        .collect();
    buckets.sort_by(|a, b| {
        b.spend_usd
            .partial_cmp(&a.spend_usd)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });
    buckets
}

/// Agrege des journaux de depense. Separee du client HTTP pour etre testable
/// sur des journaux reels sans LiteLLM.
fn aggregate(entries: &[SpendLogEntry], ownership: &KeyOwnership) -> SpendReport {
    use std::collections::HashMap;
    let mut total = 0.0;
    let mut test_pricing = 0.0;
    let mut unattributed = 0.0;
    let mut by_day: HashMap<String, (f64, usize)> = HashMap::new();
    let mut by_group: HashMap<String, (f64, usize)> = HashMap::new();
    let mut by_model: HashMap<String, (f64, usize)> = HashMap::new();

    for entry in entries {
        let spend = entry.spend.unwrap_or(0.0);
        let model = entry.model.clone().unwrap_or_default();
        if TEST_PRICING_MODELS.contains(&model.as_str()) {
            test_pricing += spend;
            continue;
        }
        total += spend;

        match ownership.group_of(entry.api_key.as_ref()) {
            Some(group) => {
                let slot = by_group.entry(group).or_default();
                slot.0 += spend;
                slot.1 += 1;
            }
            None => unattributed += spend,
        }

        if let Some(day) = entry.start_time.as_deref().and_then(|t| t.get(..10)) {
            let slot = by_day.entry(day.to_string()).or_default();
            slot.0 += spend;
            slot.1 += 1;
        }
        if !model.is_empty() {
            let slot = by_model.entry(model).or_default();
            slot.0 += spend;
            slot.1 += 1;
        }
    }

    // Les jours se lisent dans l'ordre du temps, pas du montant.
    let mut by_day = sorted_buckets(by_day);
    by_day.sort_by(|a, b| a.label.cmp(&b.label));

    SpendReport {
        total_usd: total,
        test_pricing_usd: test_pricing,
        unattributed_usd: unattributed,
        by_day,
        by_group: sorted_buckets(by_group),
        by_model: sorted_buckets(by_model),
    }
}

impl LlmBudgetClient {
    /// Hachage de cle -> (alias, groupe d'apres la metadonnee de la cle).
    #[allow(clippy::type_complexity)]
    pub async fn key_ownership_from_litellm(
        &self,
    ) -> (
        std::collections::HashMap<String, String>,
        std::collections::HashMap<String, String>,
    ) {
        let Some(list) = self
            .get_json::<TokenKeyList>("/key/list?return_full_object=true&size=100")
            .await
        else {
            return Default::default();
        };
        let mut alias_by_token = std::collections::HashMap::new();
        let mut group_by_token = std::collections::HashMap::new();
        for key in list.keys {
            let Some(token) = key.token else { continue };
            if let Some(owner) = key
                .metadata
                .as_ref()
                .and_then(|m| m.get("owner"))
                .and_then(|v| v.as_str())
            {
                group_by_token.insert(token.clone(), owner.to_string());
            }
            if let Some(alias) = key.key_alias {
                alias_by_token.insert(token, alias);
            }
        }
        (alias_by_token, group_by_token)
    }

    /// Rapport de depense, ou `None` si LiteLLM est injoignable.
    pub async fn spend_report(&self, ownership: &KeyOwnership) -> Option<SpendReport> {
        let entries = self.get_json::<Vec<SpendLogEntry>>("/spend/logs").await?;
        Some(aggregate(&entries, ownership))
    }
}

#[derive(Debug, Deserialize)]
struct TokenKeyList {
    #[serde(default)]
    keys: Vec<TokenKeyInfo>,
}

#[derive(Debug, Deserialize)]
struct TokenKeyInfo {
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    key_alias: Option<String>,
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[cfg(test)]
mod spend_tests {
    use super::*;
    use std::collections::HashMap;

    fn entry(spend: f64, model: &str, day: &str, token: Option<&str>) -> SpendLogEntry {
        SpendLogEntry {
            spend: Some(spend),
            model: Some(model.to_string()),
            start_time: Some(format!("{day}T10:00:00.000000Z")),
            api_key: token.map(str::to_string),
        }
    }

    fn ownership() -> KeyOwnership {
        KeyOwnership {
            alias_by_token: HashMap::from([
                ("hash-a".to_string(), "atelier-wks-demo".to_string()),
                ("hash-b".to_string(), "atelier-build-autre".to_string()),
            ]),
            group_by_alias: HashMap::from([
                ("atelier-wks-demo".to_string(), "equipe-a".to_string()),
                ("atelier-build-autre".to_string(), "equipe-b".to_string()),
            ]),
            // Le Workshop `hash-c` a ete supprime : seule sa cle se souvient
            // encore de qui a depense.
            group_by_token: HashMap::from([("hash-c".to_string(), "equipe-disparue".to_string())]),
        }
    }

    /// Le cas qui a motive tout ce module : sur de vrais journaux, 210,00 $
    /// des 211,49 $ affiches venaient de 14 requetes a tarif FICTIF. Les
    /// inclure donnerait un chiffre faux de deux ordres de grandeur.
    #[test]
    fn test_pricing_is_excluded_from_the_total_and_shown_apart() {
        let entries = vec![
            entry(15.0, "openai/atelier-budget-test", "2026-08-31", None),
            entry(0.5, "deepseek/deepseek-chat", "2026-08-31", Some("hash-a")),
        ];
        let report = aggregate(&entries, &ownership());
        assert_eq!(report.total_usd, 0.5);
        assert_eq!(report.test_pricing_usd, 15.0);
        // Et le modele de test ne pollue pas non plus la repartition.
        assert!(report
            .by_model
            .iter()
            .all(|b| b.label != "openai/atelier-budget-test"));
    }

    /// Ce que le rapport doit rendre visible : la depense qu'aucun plafond
    /// par Workshop ne gouverne, parce qu'elle passe par le jeton partage.
    #[test]
    fn spend_on_the_shared_token_is_reported_as_unattributed() {
        let entries = vec![
            entry(1.0, "deepseek/deepseek-chat", "2026-08-31", None),
            entry(0.25, "deepseek/deepseek-chat", "2026-08-31", Some("hash-a")),
        ];
        let report = aggregate(&entries, &ownership());
        assert_eq!(report.total_usd, 1.25);
        assert_eq!(report.unattributed_usd, 1.0);
        assert_eq!(report.by_group.len(), 1);
        assert_eq!(report.by_group[0].label, "equipe-a");
        assert_eq!(report.by_group[0].spend_usd, 0.25);
    }

    /// Une cle dont le Workshop n'existe plus (supprime, cle revoquee) n'est
    /// rattachee a aucun groupe : sa depense doit rester COMPTEE dans le
    /// total, sans quoi le total ne serait plus le total.
    #[test]
    fn an_unknown_key_still_counts_towards_the_total() {
        let entries = vec![entry(
            2.0,
            "deepseek/deepseek-chat",
            "2026-08-31",
            Some("hash-inconnu"),
        )];
        let report = aggregate(&entries, &ownership());
        assert_eq!(report.total_usd, 2.0);
        assert_eq!(report.unattributed_usd, 2.0);
    }

    /// Le Workshop a ete supprime, mais la Virtual Key se souvient du groupe
    /// qui l'a payee. Sans ce repli, supprimer un Workshop ferait basculer
    /// toute sa depense passee dans « non rattache » — et un cout par equipe
    /// qui s'efface quand on fait le menage ne vaut rien.
    #[test]
    fn a_deleted_workshop_is_still_attributed_through_its_key_metadata() {
        let entries = vec![entry(
            0.75,
            "deepseek/deepseek-chat",
            "2026-08-31",
            Some("hash-c"),
        )];
        let report = aggregate(&entries, &ownership());
        assert_eq!(report.unattributed_usd, 0.0);
        assert_eq!(report.by_group[0].label, "equipe-disparue");
        assert_eq!(report.by_group[0].spend_usd, 0.75);
    }

    /// Le Workshop vivant l'emporte sur la metadonnee : il reflete l'etat
    /// courant, la metadonnee celui de la generation de la cle.
    #[test]
    fn a_living_workshop_wins_over_the_key_metadata() {
        let mut own = ownership();
        own.alias_by_token
            .insert("hash-c".to_string(), "atelier-wks-demo".to_string());
        let entries = vec![entry(
            0.75,
            "deepseek/deepseek-chat",
            "2026-08-31",
            Some("hash-c"),
        )];
        let report = aggregate(&entries, &own);
        assert_eq!(report.by_group[0].label, "equipe-a");
    }

    /// Les jours se lisent dans l'ordre du temps ; les groupes et modeles
    /// dans l'ordre du montant (ce qu'on cherche, c'est ce qui coute).
    #[test]
    fn days_are_chronological_and_the_rest_is_by_amount() {
        let entries = vec![
            entry(1.0, "modele-b", "2026-08-31", Some("hash-a")),
            entry(3.0, "modele-a", "2026-08-29", Some("hash-b")),
            entry(2.0, "modele-a", "2026-08-30", Some("hash-b")),
        ];
        let report = aggregate(&entries, &ownership());
        let days: Vec<_> = report.by_day.iter().map(|b| b.label.as_str()).collect();
        assert_eq!(days, ["2026-08-29", "2026-08-30", "2026-08-31"]);
        assert_eq!(report.by_model[0].label, "modele-a");
        assert_eq!(report.by_model[0].spend_usd, 5.0);
        assert_eq!(report.by_group[0].label, "equipe-b");
    }
}
