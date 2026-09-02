//! Client OpenBao minimal, partage par tout composant qui doit lire un
//! secret scope a un `Workshop` : authentification via la methode
//! Kubernetes (le pod s'authentifie avec son propre ServiceAccount projete,
//! verifie par OpenBao via l'API Kubernetes, voir
//! `crates/controller/src/openbao.rs`), puis lecture d'un champ KV v2 sous
//! `secret/workshops/<name>/*` (la policy provisionnee par le controller
//! couvre tout ce prefixe, quel que soit le sous-chemin exact).
//!
//! Ne fait aucune mise en cache/rafraichissement : chaque appelant qui a
//! besoin d'un cache periodique (ex: `identity-proxy`, dont les regles
//! d'injection sont relues en continu) le construit au-dessus de ce client.
//!
//! Les valeurs lues ne sont **jamais** journalisees : seuls les chemins/cles
//! le sont.

use anyhow::Context;

const DEFAULT_SA_TOKEN_PATH: &str = "/var/run/secrets/kubernetes.io/serviceaccount/token";

#[derive(Clone)]
pub struct OpenBaoClient {
    addr: String,
    workshop_name: String,
    /// Role OpenBao utilise lors du login (methode Kubernetes-auth). Par
    /// defaut `workshop-<workshop_name>` (voir [`OpenBaoClient::from_env`]),
    /// mais peut etre un role fixe distinct du `workshop_name` pour un
    /// composant cluster-wide (ex: `atelier-api-server`, voir
    /// [`OpenBaoClient::from_env_with_role`]) qui n'est pas scope a un seul
    /// Workshop et doit donc utiliser son propre role, lie a son propre
    /// ServiceAccount plutot qu'a celui d'un pod de Workshop.
    role: String,
    sa_token_path: String,
    http: reqwest::Client,
}

impl OpenBaoClient {
    /// Role `workshop-<workshop_name>`, comme provisionne par
    /// `crates/controller/src/openbao.rs::ensure_workshop_role` — convention
    /// utilisee par tout composant qui tourne DANS le pod d'un Workshop
    /// precis (`identity-proxy`, `mcp-gateway`, `net-proxy`,
    /// `image-builder`).
    pub fn from_env(openbao_addr: String, workshop_name: String) -> Self {
        let role = format!("workshop-{workshop_name}");
        Self::from_env_with_role(openbao_addr, workshop_name, role)
    }

    /// Variante avec un role explicite, distinct de la convention
    /// `workshop-<name>` : necessaire pour un composant cluster-wide (une
    /// seule instance pour tous les Workshops, pas un pod par Workshop,
    /// ex: `api-server`) qui s'authentifie avec son propre role/ServiceAccount
    /// (voir `crates/controller/src/openbao.rs::ensure_api_server_role`) mais
    /// doit tout de meme lire des secrets scopes a un Workshop donne via
    /// [`OpenBaoClient::read_field_for`].
    pub fn from_env_with_role(openbao_addr: String, workshop_name: String, role: String) -> Self {
        let sa_token_path = std::env::var("ATELIER_K8S_SA_TOKEN_PATH")
            .unwrap_or_else(|_| DEFAULT_SA_TOKEN_PATH.to_string());
        Self {
            addr: openbao_addr,
            workshop_name,
            role,
            sa_token_path,
            http: reqwest::Client::new(),
        }
    }

    /// Authentification aupres d'OpenBao via la methode Kubernetes : envoie
    /// le token du ServiceAccount projete dans ce pod, recoit un client
    /// token OpenBao scope par [`Self::role`].
    pub async fn login(&self) -> anyhow::Result<String> {
        let jwt = tokio::fs::read_to_string(&self.sa_token_path)
            .await
            .with_context(|| format!("lecture du token ServiceAccount ({})", self.sa_token_path))?;

        let response: serde_json::Value = self
            .http
            .post(format!("{}/v1/auth/kubernetes/login", self.addr))
            .json(&serde_json::json!({
                "jwt": jwt.trim(),
                "role": self.role,
            }))
            .send()
            .await
            .context("requete de login OpenBao")?
            .error_for_status()
            .context("login OpenBao refuse")?
            .json()
            .await
            .context("reponse de login OpenBao invalide")?;

        response["auth"]["client_token"]
            .as_str()
            .map(str::to_string)
            .context("client_token absent de la reponse de login OpenBao")
    }

    /// Lit un champ d'un secret KV v2 sous
    /// `secret/workshops/<name>/<secret_path>`, ou `<name>` est le
    /// `workshop_name` fourni a la construction (`from_env`/
    /// `from_env_with_role`).
    pub async fn read_field(
        &self,
        client_token: &str,
        secret_path: &str,
        field: &str,
    ) -> anyhow::Result<String> {
        self.read_field_for(client_token, &self.workshop_name, secret_path, field)
            .await
    }

    /// Variante de [`Self::read_field`] avec un `workshop_name` explicite,
    /// independant de celui fourni a la construction : necessaire pour un
    /// composant cluster-wide (ex: `api-server`, role
    /// `atelier-api-server`) dont chaque appel cible un Workshop different
    /// (extrait du chemin de la requete HTTP), contrairement aux composants
    /// qui tournent DANS le pod d'un Workshop precis et n'en lisent jamais
    /// qu'un seul.
    pub async fn read_field_for(
        &self,
        client_token: &str,
        workshop_name: &str,
        secret_path: &str,
        field: &str,
    ) -> anyhow::Result<String> {
        let response = self
            .http
            .get(format!(
                "{}/v1/secret/data/workshops/{}/{}",
                self.addr, workshop_name, secret_path
            ))
            .header("X-Vault-Token", client_token)
            .send()
            .await
            .context("requete de lecture de secret OpenBao")?
            .error_for_status()
            .context("lecture de secret OpenBao refusee")?
            .json::<serde_json::Value>()
            .await
            .context("reponse de lecture OpenBao invalide")?;

        response["data"]["data"][field]
            .as_str()
            .map(str::to_string)
            .with_context(|| format!("champ '{field}' absent du secret '{secret_path}'"))
    }

    /// Ecrit (ou remplace) un secret d'un Workshop.
    ///
    /// Le pendant en ecriture de [`read_field_for`], utilise par l'api-server
    /// pour deposer un credential saisi dans l'interface sans que celui-ci
    /// n'atterrisse ailleurs qu'ici — ni dans la spec du Workshop, ni dans un
    /// journal.
    pub async fn write_field_for(
        &self,
        client_token: &str,
        workshop_name: &str,
        secret_path: &str,
        field: &str,
        value: &str,
    ) -> anyhow::Result<()> {
        self.write_fields_for(client_token, workshop_name, secret_path, &[(field, value)])
            .await
    }

    /// Depose PLUSIEURS champs d'un secret EN UNE SEULE ecriture.
    ///
    /// KV v2 (l'API `secret/data/...` utilisee ici) REMPLACE integralement
    /// le contenu d'un secret a chaque `POST` — ce n'est pas une fusion avec
    /// la version precedente. Deux appels successifs a
    /// [`Self::write_field_for`] sur le MEME secret (un par champ) ecrivent
    /// donc deux VERSIONS DISTINCTES, la seconde ne contenant que son propre
    /// champ : le premier champ ecrit disparait de la version courante,
    /// silencieusement. Un secret a plusieurs champs (`username`+`password`
    /// pour `workshops/<name>/git`, par exemple) doit donc etre ecrit en une
    /// seule fois, tous ses champs ensemble.
    pub async fn write_fields_for(
        &self,
        client_token: &str,
        workshop_name: &str,
        secret_path: &str,
        fields: &[(&str, &str)],
    ) -> anyhow::Result<()> {
        let data: std::collections::HashMap<&str, &str> = fields.iter().copied().collect();
        self.http
            .post(format!(
                "{}/v1/secret/data/workshops/{}/{}",
                self.addr, workshop_name, secret_path
            ))
            .header("X-Vault-Token", client_token)
            .json(&serde_json::json!({ "data": data }))
            .send()
            .await
            .context("requete d'ecriture de secret OpenBao")?
            .error_for_status()
            .context("ecriture de secret OpenBao refusee")?;
        Ok(())
    }

    /// Supprime definitivement un secret d'un Workshop (metadonnees
    /// comprises : sans cela, KV v2 conserve les versions precedentes, et un
    /// credential « supprime » resterait lisible par qui aurait le droit de
    /// lire son historique).
    pub async fn delete_secret_for(
        &self,
        client_token: &str,
        workshop_name: &str,
        secret_path: &str,
    ) -> anyhow::Result<()> {
        self.http
            .delete(format!(
                "{}/v1/secret/metadata/workshops/{}/{}",
                self.addr, workshop_name, secret_path
            ))
            .header("X-Vault-Token", client_token)
            .send()
            .await
            .context("requete de suppression de secret OpenBao")?
            .error_for_status()
            .context("suppression de secret OpenBao refusee")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::OpenBaoClient;

    /// Ignore si aucun OpenBao de dev n'est joignable (le token racine, seul
    /// utilisable ici sans authentification Kubernetes reelle, n'existe que
    /// sur une instance de dev) — meme convention que les tests pm-engine
    /// contre une vraie infra (`OPENBAO_TEST_TOKEN` absent = test ignore,
    /// jamais un mock).
    /// Nom de Workshop unique pour isoler chaque test (evite qu'ils se
    /// marchent dessus s'ils tournent en parallele, ou d'une execution a
    /// l'autre) — un compteur atomique suffit, pas besoin d'une dependance
    /// `uuid` (absente du workspace) pour un simple test.
    fn unique_test_workshop_name(prefix: &str) -> String {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        format!("{prefix}-{nanos}-{n}")
    }

    fn client_and_token() -> Option<(OpenBaoClient, String)> {
        let token = std::env::var("OPENBAO_TEST_TOKEN").ok()?;
        let addr =
            std::env::var("OPENBAO_ADDR").unwrap_or_else(|_| "http://127.0.0.1:8200".to_string());
        Some((
            OpenBaoClient::from_env_with_role(addr, "test".to_string(), "unused".to_string()),
            token,
        ))
    }

    /// Regression (2026-09-02) : `write_field_for` appele deux fois de
    /// suite sur le MEME secret (un par champ) perdait le premier champ,
    /// KV v2 remplacant tout le contenu a chaque ecriture plutot que de
    /// fusionner. `write_fields_for` doit ecrire les deux ensemble et les
    /// deux doivent survivre a une relecture.
    #[tokio::test]
    async fn write_fields_for_writes_both_fields_atomically() {
        let Some((client, token)) = client_and_token() else {
            eprintln!("OPENBAO_TEST_TOKEN absent, test ignore");
            return;
        };
        let workshop = unique_test_workshop_name("test-atomic");

        client
            .write_fields_for(
                &token,
                &workshop,
                "git",
                &[("username", "u"), ("password", "p")],
            )
            .await
            .expect("ecriture atomique des deux champs");

        assert_eq!(
            client
                .read_field_for(&token, &workshop, "git", "username")
                .await
                .expect("username doit avoir survecu"),
            "u"
        );
        assert_eq!(
            client
                .read_field_for(&token, &workshop, "git", "password")
                .await
                .expect("password doit avoir survecu"),
            "p"
        );
    }

    /// Preuve du bug que `write_fields_for` corrige : deux appels SEPARES a
    /// `write_field_for` sur le meme secret perdent bien le premier champ.
    /// Sans ce test, rien ne garantit que la comprehension du comportement
    /// KV v2 documentee ci-dessus reste vraie si OpenBao change de version.
    #[tokio::test]
    async fn two_separate_writes_lose_the_first_field_proving_the_bug_it_fixes() {
        let Some((client, token)) = client_and_token() else {
            eprintln!("OPENBAO_TEST_TOKEN absent, test ignore");
            return;
        };
        let workshop = unique_test_workshop_name("test-separate");

        client
            .write_field_for(&token, &workshop, "git", "username", "u")
            .await
            .expect("premiere ecriture");
        client
            .write_field_for(&token, &workshop, "git", "password", "p")
            .await
            .expect("seconde ecriture");

        let username_after = client
            .read_field_for(&token, &workshop, "git", "username")
            .await;
        assert!(
            username_after.is_err(),
            "le bug documente doit encore reproduire : username devrait avoir disparu, \
             a obtenu {username_after:?}"
        );
    }
}
