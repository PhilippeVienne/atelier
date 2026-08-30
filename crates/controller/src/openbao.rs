//! Provisioning OpenBao par Workshop : une policy + un role d'authentification
//! Kubernetes scopes au ServiceAccount du pod parent de ce Workshop.
//!
//! Pont d'identite retenu : la methode d'auth **Kubernetes** d'OpenBao, pas
//! une federation JWT/OIDC. Le pod parent s'authentifie avec son propre
//! ServiceAccount (token projete, verifie par OpenBao via TokenReview
//! aupres de l'API Kubernetes) : aucun secret a distribuer/stocker pour
//! amorcer la confiance. L'identite humaine/utilisateur
//! (`WorkshopSpec.owner_subject`) reste portee par le fournisseur OIDC
//! (Keycloak ou equivalent, voir `crates/api-server/src/auth.rs`), separement
//! de ce pont-la.
//!
//! Le secret que `identity-proxy` recupere ensuite via ce chemin est
//! lui-meme l'identite de sortie de l'environnement (ex: la cle d'API que
//! l'environnement presente aux services externes) : seul `identity-proxy`
//! peut l'obtenir (il est le seul a detenir le ServiceAccount du pod
//! parent) et donc le seul composant capable d'agir "en tant que"
//! l'environnement auprès de ces services. L'agent dans la microVM n'y a
//! jamais acces directement.

pub struct OpenBaoConfig {
    /// Adresse utilisee par CE controller pour ses propres appels
    /// (provisioning de policy/role) — en dev, un port-forward host
    /// (`http://127.0.0.1:8200`, le controller tournant hors cluster, voir
    /// `deploy/dev/local-stack.sh`).
    pub addr: String,
    pub token: String,
    /// Adresse injectee dans `OPENBAO_ADDR` des pods Workshop
    /// (`net-proxy`, qui s'authentifie lui-meme via son ServiceAccount,
    /// jamais avec `token` ci-dessus) : DOIT etre joignable depuis
    /// l'interieur du cluster, contrairement a `addr` — bug reel constate
    /// en pratique (session de debug 2026-08-30, premier vrai Workshop
    /// Firecracker de ce depot) : reutiliser `addr` telle quelle faisait
    /// echouer indefiniment le login Kubernetes de `net-proxy`
    /// ("127.0.0.1" a l'interieur d'un pod ne designe jamais OpenBao),
    /// bloquant `atelier-terminal.service`/`atelier-code-server.service`
    /// (mot de passe de session jamais recupere). Distinct de `addr` en
    /// dev (`ATELIER_OPENBAO_POD_ADDR`, Service K8s
    /// `atelier-openbao-dev`) ; identique a `addr` par defaut si absent
    /// (cas de production ou le controller tourne lui-meme dans le
    /// cluster).
    pub pod_addr: String,
}

/// Renvoie `Ok(None)` si `OPENBAO_ADDR` est absent (fonctionnalite
/// desactivee), une erreur si elle est partiellement configuree.
pub fn config_from_env() -> anyhow::Result<Option<OpenBaoConfig>> {
    let Ok(addr) = std::env::var("OPENBAO_ADDR") else {
        tracing::info!("OPENBAO_ADDR absent, provisioning OpenBao desactive");
        return Ok(None);
    };
    let token = std::env::var("OPENBAO_TOKEN")
        .map_err(|_| anyhow::anyhow!("OPENBAO_ADDR est defini mais OPENBAO_TOKEN est absent"))?;
    let pod_addr = std::env::var("ATELIER_OPENBAO_POD_ADDR").unwrap_or_else(|_| addr.clone());
    Ok(Some(OpenBaoConfig {
        addr,
        token,
        pod_addr,
    }))
}

/// Chemins KV (v2) sous lesquels vivent les secrets d'un Workshop. KV v2
/// separe le chemin de lecture (`data/`) de celui de listing (`metadata/`) :
/// une policy pour ce Workshop doit couvrir les deux.
pub fn secrets_data_path(workshop_name: &str) -> String {
    format!("secret/data/workshops/{workshop_name}/*")
}

pub fn secrets_metadata_path(workshop_name: &str) -> String {
    format!("secret/metadata/workshops/{workshop_name}/*")
}

/// Cree (ou met a jour, idempotent) la policy et le role Kubernetes-auth
/// scopant l'acces d'un Workshop a ses seuls secrets. Renvoie le nom du role
/// (deterministe : `workshop-{name}`), a utiliser comme `role` lors du login
/// (`identity-proxy`, `mcp-gateway`, et desormais `image-builder` pour lire
/// d'eventuels identifiants git — voir `crates/image-builder/src/main.rs`).
///
/// Accepte plusieurs ServiceAccounts (pas un seul) : le pod parent et le Job
/// `image-builder` d'un meme Workshop ont chacun le leur, mais partagent le
/// meme role/policy OpenBao (memes secrets, `secret/workshops/<name>/*`) —
/// un seul appel avec la liste complete, plutot que d'ecraser
/// `bound_service_account_names` a chaque appel avec un seul nom (PUT
/// remplace tout le champ, pas un ajout).
pub async fn ensure_workshop_role(
    config: &OpenBaoConfig,
    workshop_name: &str,
    namespace: &str,
    service_accounts: &[&str],
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let role_name = format!("workshop-{workshop_name}");

    let policy_hcl = format!(
        "path \"{}\" {{ capabilities = [\"read\"] }}\npath \"{}\" {{ capabilities = [\"read\", \"list\"] }}",
        secrets_data_path(workshop_name),
        secrets_metadata_path(workshop_name),
    );

    client
        .put(format!("{}/v1/sys/policy/{role_name}", config.addr))
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({ "policy": policy_hcl }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture de la policy OpenBao: {e}"))?;

    client
        .put(format!(
            "{}/v1/auth/kubernetes/role/{role_name}",
            config.addr
        ))
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({
            "bound_service_account_names": service_accounts,
            "bound_service_account_namespaces": [namespace],
            "policies": [role_name],
            "ttl": "15m",
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture du role kubernetes-auth OpenBao: {e}"))?;

    Ok(role_name)
}

/// Role OpenBao cluster-wide (pas scope a un seul Workshop) utilise par
/// `api-server` pour lire le secret `session_auth` de N'IMPORTE QUEL
/// Workshop (voir `crates/common/src/openbao_client.rs::OpenBaoClient::
/// from_env_with_role`) — `api-server` est une seule instance partagee par
/// tous les Workshops (pas un pod par Workshop), il ne peut donc pas
/// s'authentifier avec le role `workshop-<name>` d'un Workshop precis comme
/// le font `identity-proxy`/`mcp-gateway`/`net-proxy`, qui tournent chacun
/// DANS le pod du Workshop concerne.
pub const API_SERVER_ROLE: &str = "atelier-api-server";

/// Cree (ou met a jour, idempotent) la policy et le role Kubernetes-auth
/// cluster-wide utilise par `api-server` (voir [`API_SERVER_ROLE`]) :
/// bound au ServiceAccount du Deployment `api-server` (pas a celui d'un
/// Workshop), avec une policy `read` seule (jamais d'ecriture) sur
/// `secret/data|metadata/workshops/+/{session_auth,ssh_key}` — le `+` est
/// un wildcard OpenBao/Vault KV v2 pour un seul segment de chemin : couvre
/// `session_auth`/`ssh_key` de n'importe quel Workshop, mais rien d'autre
/// (`secret/workshops/<name>/git`, `secret/workshops/<name>/<injection
/// rule>` restent hors de portee, reserves aux composants qui tournent
/// dans le pod du Workshop concerne). `ssh_key` ajoute pour `exec_in_workshop`
/// (Jalon M4, tache 4.2.3, voir `crate::openbao::ensure_ssh_key`) :
/// `api-server` y lit la cle PRIVEE pour s'authentifier en SSH aupres du
/// guest, jamais la cle publique seule (deja servie au guest par net-proxy).
///
/// A appeler une seule fois au demarrage du controller (`main.rs`), pas a
/// chaque reconciliation : ce role ne depend d'aucun Workshop particulier.
pub async fn ensure_api_server_role(
    config: &OpenBaoConfig,
    api_server_namespace: &str,
    api_server_service_account: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();

    let policy_hcl = [
        "session_auth",
        "ssh_key",
    ]
    .iter()
    .map(|secret| {
        format!(
            "path \"secret/data/workshops/+/{secret}\" {{ capabilities = [\"read\"] }}\npath \"secret/metadata/workshops/+/{secret}\" {{ capabilities = [\"read\"] }}"
        )
    })
    .collect::<Vec<_>>()
    .join("\n");

    client
        .put(format!("{}/v1/sys/policy/{API_SERVER_ROLE}", config.addr))
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({ "policy": policy_hcl }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture de la policy OpenBao (api-server): {e}"))?;

    client
        .put(format!(
            "{}/v1/auth/kubernetes/role/{API_SERVER_ROLE}",
            config.addr
        ))
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({
            "bound_service_account_names": [api_server_service_account],
            "bound_service_account_namespaces": [api_server_namespace],
            "policies": [API_SERVER_ROLE],
            "ttl": "15m",
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| {
            anyhow::anyhow!("ecriture du role kubernetes-auth OpenBao (api-server): {e}")
        })?;

    Ok(())
}

/// Supprime le role kubernetes-auth et la policy d'un Workshop. Idempotent :
/// un 404 (deja absent) n'est pas une erreur ; toute autre erreur est
/// remontee pour que le finalizer retente plutot que de laisser un role
/// orphelin (surface d'acces residuelle a des secrets).
pub async fn delete_workshop_role(
    config: &OpenBaoConfig,
    workshop_name: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let role_name = format!("workshop-{workshop_name}");

    delete_if_present(
        &client,
        &format!("{}/v1/auth/kubernetes/role/{role_name}", config.addr),
        &config.token,
    )
    .await?;
    delete_if_present(
        &client,
        &format!("{}/v1/sys/policy/{role_name}", config.addr),
        &config.token,
    )
    .await?;

    Ok(())
}

/// S'assure qu'un secret `session_auth` (mot de passe aleatoire de 32
/// caracteres) existe pour ce Workshop sous
/// `secret/data/workshops/<name>/session_auth`, champ `password`, et
/// renvoie sa valeur. Idempotent : si le secret existe deja (cas courant a
/// chaque reconcile), il est relu tel quel plutot que regenere — un
/// resume/reprovisioning ne doit pas invalider le mot de passe deja
/// communique/utilise par une session en cours.
///
/// Ce secret est ensuite lu directement par `net-proxy` (pas transmis en
/// clair par le controller dans la spec du pod) : `net-proxy` s'authentifie
/// aupres d'OpenBao avec le meme role Kubernetes-auth que le reste du
/// Workshop (voir `ensure_workshop_role`, `secret/data/workshops/<name>/*`
/// couvre deja ce chemin) et l'expose au guest via son endpoint metadata
/// (`crates/net-proxy/src/session_auth.rs`), a l'adresse link-local
/// `169.254.0.1` — jamais via une variable d'environnement du pod, qui
/// serait lisible par quiconque peut lire la spec du pod (`kubectl get pod
/// -o yaml`), pas seulement le guest.
pub async fn ensure_session_auth(
    config: &OpenBaoConfig,
    workshop_name: &str,
) -> anyhow::Result<String> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/secret/data/workshops/{workshop_name}/session_auth",
        config.addr
    );

    let existing = client
        .get(&url)
        .header("X-Vault-Token", &config.token)
        .send()
        .await?;

    if existing.status().is_success() {
        let body: serde_json::Value = existing.json().await.map_err(|e| {
            anyhow::anyhow!("reponse de lecture du secret session_auth invalide: {e}")
        })?;
        if let Some(password) = body["data"]["data"]["password"].as_str() {
            return Ok(password.to_string());
        }
    }

    let password = generate_session_password();
    client
        .put(&url)
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({ "data": { "password": password } }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture du secret session_auth OpenBao: {e}"))?;

    Ok(password)
}

/// Idempotent-preservant comme [`ensure_session_auth`] (une paire de cles
/// generee une seule fois par Workshop, jamais regeneree tant qu'elle
/// existe) : provisionne la paire de cles SSH (Ed25519) utilisee par
/// `api-server` (`crates/api-server/src/exec.rs`) pour `exec_in_workshop`
/// (Jalon M4, tache 4.2.3) — canal separe de `ttyd`, dedie a l'execution de
/// commandes fiables (exit code explicite), pas a une session interactive.
///
/// La cle PRIVEE reste dans OpenBao, lue uniquement par `api-server` (meme
/// role cluster-wide dedie que `session_auth`, voir
/// `crate::session_auth::SessionAuthClient` cote api-server) ; seule la cle
/// PUBLIQUE est servie au guest par `net-proxy`
/// (`crates/net-proxy/src/ssh_authorized_key.rs`), qui l'installe dans
/// `~vscode/.ssh/authorized_keys` avant que `sshd` ne demarre (voir le
/// script `atelier-fetch-ssh-authorized-key.sh` du depot `atelier-workspace`).
pub async fn ensure_ssh_key(config: &OpenBaoConfig, workshop_name: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/secret/data/workshops/{workshop_name}/ssh_key",
        config.addr
    );

    let existing = client
        .get(&url)
        .header("X-Vault-Token", &config.token)
        .send()
        .await?;
    if existing.status().is_success() {
        let body: serde_json::Value = existing
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("reponse de lecture du secret ssh_key invalide: {e}"))?;
        if body["data"]["data"]["privateKey"].as_str().is_some()
            && body["data"]["data"]["publicKey"].as_str().is_some()
        {
            return Ok(());
        }
    }

    let (private_key, public_key) = generate_ssh_keypair(workshop_name)?;
    client
        .put(&url)
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({
            "data": { "privateKey": private_key, "publicKey": public_key }
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture du secret ssh_key OpenBao: {e}"))?;

    Ok(())
}

/// `comment` = le nom du Workshop : purement informatif (visible dans
/// `authorized_keys`/lors d'un `ssh -v`, aucun role de securite), utile pour
/// diagnostiquer rapidement quelle cle correspond a quel Workshop.
fn generate_ssh_keypair(comment: &str) -> anyhow::Result<(String, String)> {
    let private_key =
        ssh_key::PrivateKey::random(&mut rand::rngs::OsRng, ssh_key::Algorithm::Ed25519)
            .map_err(|e| anyhow::anyhow!("generation de la paire de cles SSH: {e}"))?;
    let mut public_key = private_key.public_key().clone();
    public_key.set_comment(comment);
    let private_pem = private_key
        .to_openssh(ssh_key::LineEnding::LF)
        .map_err(|e| anyhow::anyhow!("serialisation de la cle privee SSH: {e}"))?
        .to_string();
    let public_line = public_key
        .to_openssh()
        .map_err(|e| anyhow::anyhow!("serialisation de la cle publique SSH: {e}"))?;
    Ok((private_pem, public_line))
}

/// Ecrit (ou remplace) la Virtual Key LiteLLM courante d'un Workshop sous
/// `secret/data/workshops/<name>/llm_key`, champ `value` (voir
/// `crate::litellm::LLM_VIRTUAL_KEY_SECRET_PATH`/`_FIELD`). Contrairement a
/// [`ensure_session_auth`], PAS idempotent-preservant : une Virtual Key est
/// volontairement regeneree a chaque creation du pod parent (provisioning
/// initial ou reprise post-suspension, TTL court renouvele a chaud — voir
/// `docs/specs/03-litellm-proxy.md`), donc toujours ecrasee ici plutot que
/// relue si presente.
///
/// C'est `identity-proxy` qui relit ensuite ce secret (meme role
/// Kubernetes-auth que le reste du Workshop, `secret/workshops/<name>/*` est
/// deja couvert par `ensure_workshop_role`) pour remplacer, sur le chemin de
/// sortie vers l'alias interne `llm-proxy`, l'en-tete `Authorization`
/// statique baked dans l'image par la vraie Virtual Key de ce Workshop —
/// voir le commentaire de tete de `crate::litellm` pour la justification
/// complete de ce choix d'injection.
pub async fn ensure_llm_virtual_key_secret(
    config: &OpenBaoConfig,
    workshop_name: &str,
    virtual_key: &str,
) -> anyhow::Result<()> {
    let client = reqwest::Client::new();
    let url = format!(
        "{}/v1/secret/data/workshops/{workshop_name}/{}",
        config.addr,
        crate::litellm::LLM_VIRTUAL_KEY_SECRET_PATH
    );

    client
        .put(&url)
        .header("X-Vault-Token", &config.token)
        .json(&serde_json::json!({
            "data": { crate::litellm::LLM_VIRTUAL_KEY_SECRET_FIELD: virtual_key }
        }))
        .send()
        .await?
        .error_for_status()
        .map_err(|e| anyhow::anyhow!("ecriture du secret llm_key OpenBao: {e}"))?;

    Ok(())
}

fn generate_session_password() -> String {
    use rand::Rng;
    const CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut rng = rand::thread_rng();
    (0..32)
        .map(|_| CHARSET[rng.gen_range(0..CHARSET.len())] as char)
        .collect()
}

async fn delete_if_present(client: &reqwest::Client, url: &str, token: &str) -> anyhow::Result<()> {
    let response = client
        .delete(url)
        .header("X-Vault-Token", token)
        .send()
        .await?;

    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(());
    }

    Err(anyhow::anyhow!(
        "suppression OpenBao ({url}) a echoue: {}",
        response.status()
    ))
}
