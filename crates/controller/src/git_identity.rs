//! Injection automatique d'un credential Git pour l'**agent** en cours
//! d'execution dans la microVM (Jalon M2, section 5.2 du plan) — distinct de
//! `crates/image-builder/src/main.rs::resolve_git_credentials`, qui lit le
//! **meme** secret OpenBao mais a un tout autre moment (le **build** du
//! devcontainer, dans un tout autre process/composant, avant meme que la
//! microVM de l'agent n'existe).
//!
//! Decision de conception (2.2.1) : reutiliser le **meme** chemin OpenBao
//! `secret/data/workshops/<name>/git` (champs `username`/`password`) pour
//! les deux usages, plutot que d'introduire un second chemin `git_token`
//! distinct. Justification : un meme PAT (Personal Access Token)
//! Forgejo/GitHub/GitLab donne generalement acces aux memes depots pour les
//! deux usages (cloner le devcontainer au moment du build, cloner/pousser
//! d'autres depots au runtime de l'agent) — l'utilisateur ne doit donc
//! provisionner qu'un seul secret par Workshop. Un chemin distinct
//! n'apporterait de valeur que si l'on voulait un jour permettre des scopes
//! differents (ex: lecture seule pour le build, lecture-ecriture pour
//! l'agent) ; rien dans le plan ni dans les besoins actuels ne le justifie,
//! et ce module reste independant du reste (`config_from_env`/
//! `injection_rule`) : durcir cette separation plus tard (nouveau
//! `secret_path`) resterait un changement localise si le besoin apparait.
//!
//! Ce module ne lit **jamais** le secret lui-meme (contrairement a
//! `resolve_git_credentials` cote `image-builder`) : il se contente de
//! calculer la regle d'injection `IdentityInjectionRule` (2.2.2) — c'est
//! `identity-proxy` (voir `crates/identity-proxy/src/secrets.rs`) qui la
//! lira ensuite via son propre role Kubernetes-auth (deja provisionne par
//! `crate::openbao::ensure_workshop_role`, `secret/data/workshops/<name>/*`
//! couvre deja ce chemin), jamais le controller.
//!
//! Resolution de l'adresse reelle de la forge (2.2.2/2.2.3) : **pas** de
//! resolution DNS classique du Service Kubernetes vise
//! (`<service>.<namespace>.svc.cluster.local`), qui echouerait si le
//! controller tourne hors du cluster (cas du dev local, voir
//! `docs/PROGRESS.md` — le controller se connecte alors a l'API Kubernetes
//! via kubeconfig mais n'a pas acces au DNS interne du cluster). A la place,
//! le ClusterIP du Service est lu **directement via l'API Kubernetes**
//! (`Api<Service>::get`), qui reste toujours accessible : c'est deja le
//! canal utilise par le controller pour tout le reste. Cette IP est ensuite
//! injectee dans `Pod.spec.hostAliases` du pod parent (`/etc/hosts` de tous
//! ses conteneurs, pose par Kubernetes lui-meme) pour l'entree
//! `atelier_common::GIT_ALIAS_HOST` — c'est ce qui rend ce nom
//! effectivement resolvable par `identity-proxy` au moment de relayer la
//! requete vers la vraie destination (voir `crates/identity-proxy/src/proxy.rs`,
//! qui se connecte directement au `host` de la requete recue, sans jamais
//! passer par `net-proxy`).

use atelier_common::{IdentityInjectionRule, GIT_ALIAS_HOST};
use k8s_openapi::api::core::v1::Service;
use kube::api::Api;
use std::net::IpAddr;

/// Configuration lue une seule fois au demarrage du controller (meme
/// convention que `ReconcileCtx::openbao`/`llm_proxy_addr` : `None`
/// desactive entierement la fonctionnalite, sans bloquer le reste du
/// controller — aucune Workshop n'a besoin de cette fonctionnalite pour
/// fonctionner, elle reste strictement additive).
#[derive(Debug, Clone)]
pub struct GitIdentityConfig {
    /// Nom du Service Kubernetes de la forge Git (Forgejo/Gitea/etc).
    /// Defaut de dev : `atelier-forgejo-dev` (voir
    /// `deploy/dev/forgejo/dev-pod.yaml`).
    pub service_name: String,
    pub service_namespace: String,
    /// Port HTTP de la forge (celui du Service, pas necessairement celui du
    /// pod). Defaut de dev : `3000` (Forgejo).
    pub port: u16,
    /// En-tete injecte par `identity-proxy`. Defaut : `Authorization`
    /// (convention Forgejo/Gitea/GitHub `Authorization: token <PAT>`).
    pub header: String,
    /// Prefixe de la valeur de l'en-tete. Defaut : `"token "` (Forgejo/Gitea/
    /// GitHub). Pour GitLab, l'utilisateur peut configurer
    /// `ATELIER_GIT_INJECTION_HEADER=PRIVATE-TOKEN` et
    /// `ATELIER_GIT_INJECTION_PREFIX=` (vide) — le champ `header`/`prefix`
    /// d'`IdentityInjectionRule` est deja generique, voir
    /// `atelier_common::crd::IdentityInjectionRule`.
    pub prefix: String,
}

/// `None` si `ATELIER_GIT_HOST_SERVICE` est absent ou vide : fonctionnalite
/// desactivee (comportement par defaut si l'administrateur du cluster n'a
/// pas explicitement configure de forge Git pour ce Jalon).
pub fn config_from_env() -> Option<GitIdentityConfig> {
    let service_name = std::env::var("ATELIER_GIT_HOST_SERVICE")
        .ok()
        .filter(|s| !s.trim().is_empty())?;
    let service_namespace = std::env::var("ATELIER_GIT_HOST_SERVICE_NAMESPACE")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "default".to_string());
    let port = std::env::var("ATELIER_GIT_HOST_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3000);
    let header = std::env::var("ATELIER_GIT_INJECTION_HEADER")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Authorization".to_string());
    let prefix =
        std::env::var("ATELIER_GIT_INJECTION_PREFIX").unwrap_or_else(|_| "token ".to_string());
    Some(GitIdentityConfig {
        service_name,
        service_namespace,
        port,
        header,
        prefix,
    })
}

/// Lit le ClusterIP du Service Kubernetes de la forge Git via l'API
/// Kubernetes (voir le commentaire de tete du module — jamais une
/// resolution DNS classique). Erreur si le Service est absent ou de type
/// `Headless` (`ClusterIP: None`, sans IP unique a inscrire dans
/// `hostAliases`) : appelant attendu de traiter cela comme un echec
/// non-bloquant (log + skip pour ce cycle de reconciliation), pas une
/// erreur fatale — voir `crate::reconcile::ensure_parent_pod`.
pub async fn resolve_cluster_ip(
    client: &kube::Client,
    config: &GitIdentityConfig,
) -> anyhow::Result<IpAddr> {
    let services: Api<Service> = Api::namespaced(client.clone(), &config.service_namespace);
    let service = services.get(&config.service_name).await.map_err(|err| {
        anyhow::anyhow!(
            "lecture du Service {}/{} echouee: {err}",
            config.service_namespace,
            config.service_name
        )
    })?;
    let cluster_ip = service
        .spec
        .and_then(|spec| spec.cluster_ip)
        .filter(|ip| ip != "None")
        .ok_or_else(|| {
            anyhow::anyhow!(
                "Service {}/{} sans ClusterIP exploitable",
                config.service_namespace,
                config.service_name
            )
        })?;
    cluster_ip
        .parse()
        .map_err(|err| anyhow::anyhow!("ClusterIP invalide ({cluster_ip:?}): {err}"))
}

/// Regle d'injection calculee (2.2.2) : jamais ecrite dans le CRD
/// `Workshop` lui-meme (qui reste la source de verite declarative de
/// l'utilisateur), seulement ajoutee a la liste serialisee vers
/// `ATELIER_IDENTITY_INJECTION_RULES` au moment de construire le pod parent
/// — voir `crate::reconcile::ensure_parent_pod`.
pub fn injection_rule(config: &GitIdentityConfig) -> IdentityInjectionRule {
    IdentityInjectionRule {
        host: GIT_ALIAS_HOST.to_string(),
        header: config.header.clone(),
        prefix: config.prefix.clone(),
        secret_path: "git".to_string(),
        field: "password".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_from_env_disabled_without_service_name() {
        // SAFETY: aucun autre test de ce module ne lit ces variables en
        // parallele (chaque `#[test]` a son propre nom de variable
        // implicitement partage par process, mais ce module ne les modifie
        // qu'ici, jamais concurremment avec d'autres tests de ce fichier).
        unsafe {
            std::env::remove_var("ATELIER_GIT_HOST_SERVICE");
        }
        assert!(config_from_env().is_none());
    }

    #[test]
    fn injection_rule_targets_the_shared_git_secret_path() {
        let config = GitIdentityConfig {
            service_name: "atelier-forgejo-dev".to_string(),
            service_namespace: "default".to_string(),
            port: 3000,
            header: "Authorization".to_string(),
            prefix: "token ".to_string(),
        };
        let rule = injection_rule(&config);
        assert_eq!(rule.host, GIT_ALIAS_HOST);
        assert_eq!(rule.header, "Authorization");
        assert_eq!(rule.prefix, "token ");
        // Meme convention que `crates/image-builder/src/main.rs::resolve_git_credentials`
        // (secret_path="git", field="password") : voir le commentaire de
        // tete de ce module pour la justification du chemin partage.
        assert_eq!(rule.secret_path, "git");
        assert_eq!(rule.field, "password");
    }
}
