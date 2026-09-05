//! Client HTTP pour l'API d'administration de LiteLLM (`deploy/dev/llm-proxy/`,
//! service global du cluster, meme niveau qu'OpenBao — voir
//! `docs/specs/03-litellm-proxy.md`) : provisionne, par Workshop, une
//! Virtual Key isolee a budget plafonne et TTL court.
//!
//! Convention `config_from_env()` -> `Ok(None)`/`None` identique a
//! `crate::openbao::config_from_env` : reutilise les DEUX variables deja
//! definies pour cabler l'alias `net-proxy` `llm-proxy`
//! (`ATELIER_LLM_PROXY_ADDR`/`ATELIER_LLM_PROXY_AUTH_TOKEN`, voir
//! `crates/controller/src/reconcile.rs`) — le meme jeton (`LITELLM_MASTER_KEY`
//! cote LiteLLM) sert a la fois de jeton client (`ANTHROPIC_AUTH_TOKEN`
//! historique, partage) et de jeton d'administration (`Authorization: Bearer
//! <master_key>` sur `/key/generate`/`/key/delete`) : LiteLLM traite tout
//! appel authentifie avec le master key comme administrateur.
//!
//! ## Choix d'injection cote guest (ecart documente vis-a-vis du libelle
//! litteral de la tache 3.1.3, "injecter dans `/etc/environment`")
//!
//! Le seul mecanisme existant qui ecrit reellement dans `/etc/environment`
//! du guest est `crates/image-builder::inject_net_proxy_config`, execute une
//! seule fois **au moment du build de l'image** (rootfs content-addressed,
//! potentiellement partagee entre plusieurs Workshops/reprises ayant le
//! meme devcontainer). Une Virtual Key par Workshop, regeneree a chaque
//! reprise (`resume`), ne peut donc pas y etre re-ecrite sans rebuild —
//! l'objectif de la tache (TTL court renouvele a chaud, sans reconstruire
//! l'image) serait manque. Modifier `net-proxy`/`identity-proxy` pour
//! ouvrir un nouveau canal (a la maniere de `crates/net-proxy/src/metadata.rs`
//! pour `session_auth`) sortirait par ailleurs du perimetre assigne a cet
//! agent pour ce jalon.
//!
//! Solution retenue, a cout d'implementation nul sur ces deux crates :
//! reutiliser le mecanisme **generique** deja en place pour l'injection de
//! credentials Git (`Workshop.spec.identity_injection_rules`,
//! `crates/identity-proxy/src/rules.rs`/`proxy.rs`, deja deployes et non
//! modifies ici). Ce mecanisme injecte un en-tete HTTP (ici `Authorization:
//! Bearer <valeur>`) sur toute requete en clair a destination d'un hote
//! donne, en le lisant depuis OpenBao — et `identity-proxy` REMPLACE tout
//! en-tete `Authorization` deja present (voir
//! `crates/identity-proxy/src/http.rs::with_injected_header`, test
//! `replaces_existing_header`), pas seulement l'ajoute. Concretement :
//!
//! 1. Le controller genere la Virtual Key (ce module) et l'ecrit dans
//!    OpenBao a `secret/workshops/<name>/llm_key` (champ `value`, voir
//!    `crate::openbao::ensure_llm_virtual_key_secret`).
//! 2. Il ajoute une regle d'injection pour l'hote `llm-proxy` (alias interne
//!    resolu par `net-proxy`, jamais un nom DNS reel) a
//!    `ATELIER_IDENTITY_INJECTION_RULES`, au meme titre que la regle Git.
//! 3. Le guest continue de presenter le jeton STATIQUE partage, baked au
//!    build (`ANTHROPIC_AUTH_TOKEN`, inchange) — `identity-proxy`, sur le
//!    chemin de sortie, le remplace transparemment par la Virtual Key reelle
//!    de ce Workshop avant de relayer vers LiteLLM. L'agent dans la microVM
//!    n'a jamais connaissance de la vraie cle, exactement comme pour le
//!    credential Git.
//!
//! Consequence assumee : cette isolation par Workshop necessite qu'OpenBao
//! soit configure (`ReconcileCtx.openbao`) EN PLUS de LiteLLM
//! (`ReconcileCtx.litellm`) — sans OpenBao, aucun canal de livraison sur de
//! la Virtual Key au guest n'existe, le controller se contente alors de
//! generer/reserver la cle (utile pour le budget/l'audit cote LiteLLM) sans
//! pouvoir la faire consommer par l'agent, qui retombe sur le jeton statique
//! partage historique (comportement identique a avant ce jalon).

use serde::Deserialize;

/// Alias interne resolu par `net-proxy` vers LiteLLM (voir
/// `crates/net-proxy/src/internal.rs::LLM_PROXY_ALIAS`) — duplique ici
/// plutot qu'importe (pas de dependance de `atelier-controller` vers
/// `atelier-net-proxy`), mais DOIT rester en phase avec cette constante.
pub const LLM_PROXY_ALIAS_HOST: &str = "llm-proxy";

/// Chemin (relatif au Workshop) et champ sous lesquels la Virtual Key
/// courante d'un Workshop est stockee dans OpenBao — voir
/// `crate::openbao::ensure_llm_virtual_key_secret`.
pub const LLM_VIRTUAL_KEY_SECRET_PATH: &str = "llm_key";
pub const LLM_VIRTUAL_KEY_SECRET_FIELD: &str = "value";

/// TTL court impose a chaque Virtual Key generee (provisioning ou reprise) :
/// voir `docs/specs/03-litellm-proxy.md`, section 1 ("1-2 heures").
pub const VIRTUAL_KEY_TTL: &str = "2h";

/// TTL beaucoup plus court pour les cles de build (`image-builder`, tache
/// 3.1.4) : le Job de build dure typiquement quelques minutes, jamais des
/// heures — une cle de secours si la revocation explicite en fin de Job
/// echouait exceptionnellement.
pub const BUILD_VIRTUAL_KEY_TTL: &str = "30m";

#[derive(Debug, Clone)]
pub struct LiteLlmConfig {
    /// `host:port`, sans schema (meme convention que
    /// `ReconcileCtx::llm_proxy_addr`) — ex.
    /// `atelier-llm-proxy.default.svc.cluster.local:4000`.
    pub addr: String,
    /// `LITELLM_MASTER_KEY` cote LiteLLM : sert ici de jeton
    /// d'administration (`/key/generate`, `/key/delete`), et par ailleurs de
    /// jeton client statique partage (voir le commentaire de tete de ce
    /// module).
    pub master_key: String,
}

/// Renvoie `None` si LiteLLM n'est pas configure (au moins une des deux
/// variables absente) — meme convention que `openbao::config_from_env`,
/// mais sans erreur possible ici : les deux variables sont deja lues
/// separement par `ReconcileCtx` pour cabler l'alias `net-proxy`, ce module
/// se contente de les combiner s'il y a lieu.
pub fn config_from_env(addr: Option<String>, master_key: Option<String>) -> Option<LiteLlmConfig> {
    let addr = addr.filter(|s| !s.trim().is_empty())?;
    let master_key = master_key.filter(|s| !s.trim().is_empty())?;
    Some(LiteLlmConfig { addr, master_key })
}

/// Virtual Key nouvellement generee par [`LiteLlmClient::generate_virtual_key`].
#[derive(Debug, Clone)]
pub struct VirtualKey {
    /// Jeton reel (`sk-...`) a presenter a LiteLLM — ne doit jamais etre
    /// journalise ni ecrit ailleurs que dans OpenBao (voir
    /// `crate::openbao::ensure_llm_virtual_key_secret`).
    pub key: String,
    /// Alias stable (`atelier-wks-<name>` ou `atelier-build-<name>`),
    /// utilise ensuite pour la revocation (`delete_virtual_key`) — jamais le
    /// jeton lui-meme, que le controller ne conserve pas en memoire au-dela
    /// de l'ecriture OpenBao.
    pub key_alias: String,
}

#[derive(Debug, Deserialize)]
struct GenerateKeyResponse {
    key: String,
}

pub struct LiteLlmClient {
    config: LiteLlmConfig,
    http: reqwest::Client,
}

impl LiteLlmClient {
    pub fn new(config: LiteLlmConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::new(),
        }
    }

    fn base_url(&self) -> String {
        format!("http://{}", self.config.addr)
    }

    /// `POST /key/generate` : cree une Virtual Key nommee `key_alias`, avec
    /// un budget plafond optionnel (`max_budget_usd`, voir
    /// `WorkshopResources.max_llm_budget_usd` — absent = pas de plafond
    /// specifique, comportement par defaut de LiteLLM) et un TTL
    /// (`duration`, ex. [`VIRTUAL_KEY_TTL`]). `owner` est propage en
    /// metadonnee pour l'observabilite cote LiteLLM (`/key/info`), pas
    /// utilise par LiteLLM lui-meme pour l'enforcement.
    ///
    /// C'est le GROUPE proprietaire qui y est ecrit, pas le createur : la
    /// depense d'un Workshop est celle du groupe qui le porte, et c'est cette
    /// cle-la qui permet a la console d'administration d'agreger un cout par
    /// equipe plutot que par individu.
    ///
    /// Pas idempotent au sens strict : un appel avec un `key_alias` deja
    /// utilise cree une Virtual Key SUPPLEMENTAIRE (constate en pratique
    /// contre une vraie instance LiteLLM) — a l'appelant de ne generer
    /// qu'une fois par cycle de vie (creation du pod parent, reprise), voir
    /// `crate::reconcile::ensure_parent_pod`.
    pub async fn generate_virtual_key(
        &self,
        key_alias: &str,
        owner: &str,
        max_budget_usd: Option<f64>,
        ttl: &str,
    ) -> anyhow::Result<VirtualKey> {
        self.generate_virtual_key_in_team(key_alias, owner, max_budget_usd, ttl, None)
            .await
    }

    /// Comme [`Self::generate_virtual_key`], avec en plus `team_id` (tache
    /// 12.5, spec docs/specs/16-escouades-multi-agents-swarms-mesh.md §3.4,
    /// "Virtual Key Parente") : une cle rattachee a une Team LiteLLM (voir
    /// [`Self::ensure_team`]) partage la depense agregee de CETTE Team avec
    /// toutes les autres cles qui y sont rattachees — c'est le coupe-circuit
    /// collectif d'une campagne multi-Workshops, pas un plafond individuel
    /// supplementaire (`max_budget_usd` reste independant et s'applique EN
    /// PLUS, au niveau de la cle elle-meme, verifie empiriquement contre le
    /// cluster de dev reel : une cle liee a une Team garde son propre champ
    /// `max_budget`, LiteLLM applique les deux plafonds).
    pub async fn generate_virtual_key_in_team(
        &self,
        key_alias: &str,
        owner: &str,
        max_budget_usd: Option<f64>,
        ttl: &str,
        team_id: Option<&str>,
    ) -> anyhow::Result<VirtualKey> {
        let mut body = serde_json::json!({
            "key_alias": key_alias,
            "duration": ttl,
            "metadata": { "owner": owner },
        });
        if let Some(budget) = max_budget_usd {
            body["max_budget"] = serde_json::json!(budget);
        }
        if let Some(team_id) = team_id {
            body["team_id"] = serde_json::json!(team_id);
        }

        let response = self
            .http
            .post(format!("{}/key/generate", self.base_url()))
            .bearer_auth(&self.config.master_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()
            .map_err(|err| {
                anyhow::anyhow!("generation de la Virtual Key LiteLLM ({key_alias}): {err}")
            })?;

        let parsed: GenerateKeyResponse = response.json().await.map_err(|err| {
            anyhow::anyhow!("reponse /key/generate LiteLLM illisible ({key_alias}): {err}")
        })?;

        Ok(VirtualKey {
            key: parsed.key,
            key_alias: key_alias.to_string(),
        })
    }

    /// `POST /key/delete` par `key_aliases` (pas par jeton : le controller
    /// ne conserve jamais le jeton lui-meme au-dela de son ecriture dans
    /// OpenBao, voir [`VirtualKey`]). Idempotent : LiteLLM renvoie `404`
    /// (`"No keys found"`) pour un alias deja absent, traite ici comme un
    /// succes plutot qu'une erreur — evite de faire echouer indefiniment le
    /// finalizer `atelier.dev/cleanup` sur une cle deja nettoyee (ex: second
    /// passage apres un redemarrage du controller entre les deux).
    pub async fn delete_virtual_key(&self, key_alias: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/key/delete", self.base_url()))
            .bearer_auth(&self.config.master_key)
            .json(&serde_json::json!({ "key_aliases": [key_alias] }))
            .send()
            .await?;

        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }

        Err(anyhow::anyhow!(
            "suppression de la Virtual Key LiteLLM ({key_alias}) a echoue: {}",
            response.status()
        ))
    }

    /// `POST /team/new` (tache 12.5) : cree la Team LiteLLM d'une campagne
    /// multi-Workshops, porteuse du plafond financier GLOBAL partage par
    /// toutes ses Virtual Keys (voir [`Self::generate_virtual_key_in_team`]).
    ///
    /// **Idempotence geree explicitement** (contrairement a
    /// `generate_virtual_key`, ou l'appelant garantit lui-meme l'unicite) :
    /// PLUSIEURS Workshops de la MEME campagne appellent chacun cette
    /// methode a leur propre provisioning, sans coordination entre eux — le
    /// premier arrive cree reellement la Team, les suivants doivent voir un
    /// succes. Verifie empiriquement contre le cluster de dev reel :
    /// `POST /team/new` sur un `team_id` deja existant renvoie `400` avec
    /// `{"error": "Team id = <id> already exists..."}` (pas de `409`
    /// standard) — ce message precis est la seule facon fiable de
    /// distinguer "deja cree, rien a faire" d'un autre echec 400.
    pub async fn ensure_team(
        &self,
        team_id: &str,
        max_budget_usd: Option<f64>,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::json!({ "team_id": team_id });
        if let Some(budget) = max_budget_usd {
            body["max_budget"] = serde_json::json!(budget);
        }

        let response = self
            .http
            .post(format!("{}/team/new", self.base_url()))
            .bearer_auth(&self.config.master_key)
            .json(&body)
            .send()
            .await?;

        if response.status().is_success() {
            return Ok(());
        }
        if response.status() == reqwest::StatusCode::BAD_REQUEST {
            let text = response.text().await.unwrap_or_default();
            if text.contains("already exists") {
                return Ok(());
            }
            return Err(anyhow::anyhow!(
                "creation de la Team LiteLLM ({team_id}) a echoue: 400 {text}"
            ));
        }

        Err(anyhow::anyhow!(
            "creation de la Team LiteLLM ({team_id}) a echoue: {}",
            response.status()
        ))
    }

    /// `POST /team/delete` : idempotent, meme convention que
    /// [`Self::delete_virtual_key`] — verifie empiriquement qu'un `team_id`
    /// absent renvoie `404`, traite ici comme un succes.
    pub async fn delete_team(&self, team_id: &str) -> anyhow::Result<()> {
        let response = self
            .http
            .post(format!("{}/team/delete", self.base_url()))
            .bearer_auth(&self.config.master_key)
            .json(&serde_json::json!({ "team_ids": [team_id] }))
            .send()
            .await?;

        if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }

        Err(anyhow::anyhow!(
            "suppression de la Team LiteLLM ({team_id}) a echoue: {}",
            response.status()
        ))
    }
}

/// Alias de Virtual Key du pod parent (session runtime) d'un Workshop —
/// utilise a la fois a la generation (`ensure_parent_pod`) et a la
/// revocation (finalizer `atelier.dev/cleanup`), voir
/// `docs/archive/PLAN-ACTION-M1-M6.md`, tache 3.2.1.
pub fn workshop_key_alias(workshop_name: &str) -> String {
    format!("atelier-wks-{workshop_name}")
}

/// Alias de Virtual Key ephemere dediee au Job `image-builder` d'un Workshop
/// (tache 3.1.4) — distinct de [`workshop_key_alias`] : cycle de vie propre
/// (generee et revoquee autour d'un seul Job), jamais reutilisee pour le pod
/// parent.
pub fn build_key_alias(workshop_name: &str) -> String {
    format!("atelier-build-{workshop_name}")
}

/// Team LiteLLM d'une campagne multi-Workshops (tache 12.5) — prefixee pour
/// ne jamais collisionner avec un `team_id` sans rapport cree ailleurs
/// (LiteLLM n'impose aucun espace de noms par application), `campaign_id`
/// restant un champ libre du CRD `Workshop`.
///
/// **Limite assumee** : le plafond financier de la Team est celui du
/// PREMIER Workshop de la campagne a etre reconcilie (`ensure_team` est
/// idempotent : un `campaign_id` deja vu ignore silencieusement le budget
/// demande par les Workshops suivants) — il n'existe pas de champ CRD dedie
/// a un budget de campagne INDEPENDANT du budget individuel d'un Workshop
/// particulier. Documente plutot que corrige dans cette tache : un champ
/// `Workshop.spec.campaign_budget_usd` dedie resterait un changement
/// localise si le besoin s'en fait sentir.
pub fn campaign_team_id(campaign_id: &str) -> String {
    format!("atelier-campaign-{campaign_id}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_requires_both_values() {
        assert!(config_from_env(None, None).is_none());
        assert!(config_from_env(Some("addr:4000".into()), None).is_none());
        assert!(config_from_env(None, Some("key".into())).is_none());
        assert!(config_from_env(Some(String::new()), Some("key".into())).is_none());
    }

    #[test]
    fn config_from_env_accepts_both_values() {
        let config = config_from_env(Some("addr:4000".into()), Some("key".into())).unwrap();
        assert_eq!(config.addr, "addr:4000");
        assert_eq!(config.master_key, "key");
    }

    #[test]
    fn alias_naming_is_deterministic_and_distinct() {
        assert_eq!(workshop_key_alias("demo"), "atelier-wks-demo");
        assert_eq!(build_key_alias("demo"), "atelier-build-demo");
        assert_ne!(workshop_key_alias("demo"), build_key_alias("demo"));
    }

    #[test]
    fn campaign_team_id_is_prefixed_and_deterministic() {
        assert_eq!(campaign_team_id("pm-42"), "atelier-campaign-pm-42");
        assert_eq!(campaign_team_id("pm-42"), campaign_team_id("pm-42"));
        assert_ne!(campaign_team_id("pm-42"), campaign_team_id("pm-43"));
    }
}
