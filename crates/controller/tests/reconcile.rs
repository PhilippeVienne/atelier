//! Test d'integration : necessite un vrai cluster Kubernetes accessible via
//! le kubeconfig par defaut, avec le CRD `Workshop` installe
//! (`kubectl apply -f crds/workshop.yaml`). Un cluster kind local suffit :
//!
//!   kind create cluster --name atelier-dev
//!   kubectl apply -f crds/workshop.yaml
//!   cargo test -p atelier-controller

use atelier_common::{
    DevcontainerSource, Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopResources,
    WorkshopSpec,
};
use atelier_controller::reconcile::ReconcileCtx;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams, Patch, PatchParams, PostParams, PropagationPolicy};
use kube::Client;
use std::time::{SystemTime, UNIX_EPOCH};

/// Contexte de test sans OpenBao configure (comportement par defaut,
/// identique a avant l'introduction du provisioning de secrets).
fn ctx_without_openbao(client: Client) -> ReconcileCtx {
    ReconcileCtx {
        client,
        openbao: None,
        registry_addr: "localhost:5000".to_string(),
        registry_insecure: true,
        llm_proxy_addr: None,
        llm_proxy_auth_token: None,
        git_identity: None,
        litellm: None,
    }
}

/// Un `Job` ne supprime pas ses pods en cascade par defaut (contrairement a
/// un `Pod` avec owner reference geree par le garbage collector standard) :
/// il faut demander explicitement une propagation en avant-plan.
fn foreground_delete() -> DeleteParams {
    DeleteParams {
        propagation_policy: Some(PropagationPolicy::Foreground),
        ..DeleteParams::default()
    }
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

fn sample_spec() -> WorkshopSpec {
    WorkshopSpec {
        devcontainer: DevcontainerSource {
            repo: "https://example.invalid/repo.git".into(),
            revision: "HEAD".into(),
            config_path: ".devcontainer/devcontainer.json".into(),
        },
        resources: WorkshopResources {
            cpu: "100m".into(),
            memory: "128Mi".into(),
            disk: None,
            max_llm_budget_usd: None,
        },
        egress_allowlist: vec![],
        tools: vec![],
        identity_injection_rules: vec![],
        owner_subject: "test-user".into(),
        desired_state: WorkshopDesiredState::Running,
    }
}

async fn try_client() -> Option<Client> {
    atelier_common::telemetry::ensure_crypto_provider();
    Client::try_default().await.ok()
}

/// Sans `status.imageDigest`, apply() doit declencher un Job image-builder
/// et rester en phase BuildingImage, sans creer de pod parent.
#[tokio::test]
async fn apply_triggers_image_build_job_when_digest_missing() {
    let Some(client) = try_client().await else {
        eprintln!("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier), test ignore");
        return;
    };

    let ns = "default";
    let name = unique_name("test-workshop-build");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let ctx = ctx_without_openbao(client.clone());

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("apply() ne doit pas echouer");

    assert_eq!(status.phase, WorkshopPhase::BuildingImage);
    assert!(status.pod_name.is_none(), "pas de pod parent avant l'image");
    assert!(status.image_digest.is_none());

    let job_name = format!("{name}-image-build");
    let job = jobs
        .get(&job_name)
        .await
        .expect("le job image-builder doit avoir ete cree");
    let owners = job
        .metadata
        .owner_references
        .clone()
        .expect("le job doit avoir une owner reference vers le Workshop");
    assert_eq!(owners[0].name, name);

    let pod_spec = job.spec.clone().unwrap().template.spec.unwrap();
    let env = pod_spec.containers[0].env.clone().unwrap();
    let repo_env = env
        .iter()
        .find(|e| e.name == "ATELIER_DEVCONTAINER_REPO")
        .expect("ATELIER_DEVCONTAINER_REPO doit etre transmise au job");
    assert_eq!(
        repo_env.value.as_deref(),
        Some("https://example.invalid/repo.git")
    );
    let cache_dir_env = env
        .iter()
        .find(|e| e.name == "ATELIER_IMAGE_CACHE_DIR")
        .expect("ATELIER_IMAGE_CACHE_DIR doit etre transmise au job");
    assert_eq!(cache_dir_env.value.as_deref(), Some("/cache"));

    // Le PVC de cache partage doit avoir ete cree (idempotent, sans owner
    // reference : il survit a la suppression de n'importe quel Workshop).
    let pvcs: Api<k8s_openapi::api::core::v1::PersistentVolumeClaim> =
        Api::namespaced(client.clone(), ns);
    let pvc = pvcs
        .get(atelier_controller::storage::IMAGE_CACHE_PVC_NAME)
        .await
        .expect("le PVC de cache doit avoir ete cree");
    assert!(
        pvc.metadata.owner_references.is_none()
            || pvc.metadata.owner_references.unwrap().is_empty(),
        "le PVC de cache est partage, il ne doit pas etre owned par un Workshop"
    );

    // ServiceAccount du pod parent pas encore cree a ce stade (avant
    // l'image) : le Job doit monter le PVC lui-meme (lecture-ecriture) et
    // avoir un initContainer qui prepare `crane` sur un volume separe.
    let cache_volume = pod_spec
        .volumes
        .as_ref()
        .and_then(|vols| vols.iter().find(|v| v.name == "cache"))
        .expect("le job doit monter le volume de cache");
    assert_eq!(
        cache_volume
            .persistent_volume_claim
            .as_ref()
            .map(|pvc| pvc.claim_name.as_str()),
        Some(atelier_controller::storage::IMAGE_CACHE_PVC_NAME)
    );
    let init_containers = pod_spec
        .init_containers
        .as_ref()
        .expect("le job doit avoir un initContainer pour preparer crane");
    assert!(
        init_containers.iter().any(|c| c.name == "copy-tools"),
        "le job doit avoir un initContainer copy-tools"
    );

    // `net-proxy` est un sidecar natif (initContainer avec
    // `restartPolicy: Always`, cf. reconcile.rs) : sans ca, le Job ne se
    // terminerait jamais puisque net-proxy ne sort jamais de lui-meme.
    let net_proxy = init_containers
        .iter()
        .find(|c| c.name == "net-proxy")
        .expect("le job doit avoir un sidecar net-proxy pour l'egress de la microVM builder");
    assert_eq!(net_proxy.restart_policy.as_deref(), Some("Always"));

    // Le PVC de cache est partage entre tous les Workshops du namespace : on
    // ne le supprime pas en fin de test (d'autres tests, potentiellement en
    // parallele, s'appuient sur sa presence idempotente).
    jobs.delete(&job_name, &foreground_delete()).await.ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Une fois `status.imageDigest` present, apply() doit creer le pod parent
/// (idempotent) plutot que de redeclencher un build.
#[tokio::test]
async fn apply_creates_owned_parent_pod_once_image_ready() {
    let Some(client) = try_client().await else {
        eprintln!("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier), test ignore");
        return;
    };

    let ns = "default";
    let name = unique_name("test-workshop-pod");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let ctx = ctx_without_openbao(client.clone());

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    let building_status = atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("premier apply()");

    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": building_status })),
        )
        .await
        .expect("ecriture du statut initial (simule ce que fait reconcile())");

    // Simule la fin du build (comportement normalement effectue par
    // image-builder lui-meme via patch_workshop_status, en patch partiel).
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": { "imageDigest": "sha256:deadbeef" } })),
        )
        .await
        .expect("patch du statut");
    let with_digest = workshops.get(&name).await.expect("get workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_pod_name = format!("{name}-parent");
    assert_eq!(status.pod_name.as_deref(), Some(expected_pod_name.as_str()));
    assert_eq!(status.image_digest.as_deref(), Some("sha256:deadbeef"));

    let pod = pods
        .get(&expected_pod_name)
        .await
        .expect("le pod parent doit avoir ete cree");
    let owners = pod
        .metadata
        .owner_references
        .expect("le pod doit avoir une owner reference vers le Workshop");
    assert_eq!(owners[0].name, name);
    assert_eq!(
        pod.spec
            .as_ref()
            .and_then(|s| s.service_account_name.clone()),
        Some(expected_pod_name.clone()),
        "le pod parent doit utiliser son propre ServiceAccount dedie"
    );
    let containers = &pod.spec.as_ref().expect("pod spec").containers;
    for expected in [
        "vm-supervisor",
        "net-proxy",
        "identity-proxy",
        "mcp-gateway",
    ] {
        assert!(
            containers.iter().any(|c| c.name == expected),
            "le pod parent doit avoir un conteneur {expected}"
        );
    }

    // apply() doit rester idempotent : un deuxieme appel ne doit pas echouer.
    atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("un deuxieme apply() doit rester idempotent");

    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    service_accounts
        .delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    jobs.delete(&format!("{name}-image-build"), &foreground_delete())
        .await
        .ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Test reel de la tache 2.2.2 (Jalon M2, section 5.2) : quand
/// `ReconcileCtx::git_identity` est configure, `apply()` doit calculer une
/// regle d'injection Git (jamais ecrite dans `Workshop.spec` lui-meme),
/// resoudre le vrai ClusterIP du Service Forgejo de dev via l'API
/// Kubernetes (`crate::git_identity::resolve_cluster_ip`, jamais une
/// resolution DNS classique — voir le commentaire de tete de ce module) et
/// la poser en `hostAliases` sur le pod parent, pour que
/// `atelier_common::GIT_ALIAS_HOST` soit reellement resolvable par
/// `identity-proxy` une fois le pod demarre.
#[tokio::test]
async fn apply_wires_the_git_identity_injection_rule_when_configured() {
    let Some(client) = try_client().await else {
        eprintln!("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier), test ignore");
        return;
    };

    let ns = "default";
    let services: Api<k8s_openapi::api::core::v1::Service> = Api::namespaced(client.clone(), ns);
    let Ok(forgejo_service) = services.get("atelier-forgejo-dev").await else {
        eprintln!(
            "Service atelier-forgejo-dev absent (voir deploy/dev/forgejo/README.md), test ignore"
        );
        return;
    };
    let expected_cluster_ip = forgejo_service
        .spec
        .and_then(|s| s.cluster_ip)
        .expect("le Service atelier-forgejo-dev doit avoir un ClusterIP");

    let name = unique_name("test-workshop-git-identity");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let mut ctx = ctx_without_openbao(client.clone());
    ctx.git_identity = Some(atelier_controller::git_identity::GitIdentityConfig {
        service_name: "atelier-forgejo-dev".to_string(),
        service_namespace: ns.to_string(),
        port: 3000,
        header: "Authorization".to_string(),
        prefix: "token ".to_string(),
    });

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    let building_status = atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("premier apply()");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": building_status })),
        )
        .await
        .expect("ecriture du statut initial");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": { "imageDigest": "sha256:deadbeef" } })),
        )
        .await
        .expect("patch du statut");
    let with_digest = workshops.get(&name).await.expect("get workshop");

    atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_pod_name = format!("{name}-parent");
    let pod = pods
        .get(&expected_pod_name)
        .await
        .expect("le pod parent doit avoir ete cree");
    let pod_spec = pod.spec.as_ref().expect("pod spec");

    let host_aliases = pod_spec
        .host_aliases
        .as_ref()
        .expect("hostAliases doit etre pose quand git_identity est configure");
    let git_alias = host_aliases
        .iter()
        .find(|a| {
            a.hostnames
                .as_ref()
                .is_some_and(|h| h.iter().any(|n| n == atelier_common::GIT_ALIAS_HOST))
        })
        .expect("une entree hostAliases doit cibler atelier_common::GIT_ALIAS_HOST");
    assert_eq!(
        git_alias.ip, expected_cluster_ip,
        "l'IP posee doit etre le vrai ClusterIP du Service Forgejo, lu via l'API Kubernetes"
    );

    let identity_proxy = pod_spec
        .containers
        .iter()
        .find(|c| c.name == "identity-proxy")
        .expect("conteneur identity-proxy");
    let rules_env = identity_proxy
        .env
        .as_ref()
        .and_then(|env| {
            env.iter()
                .find(|e| e.name == "ATELIER_IDENTITY_INJECTION_RULES")
        })
        .and_then(|e| e.value.clone())
        .expect("ATELIER_IDENTITY_INJECTION_RULES doit etre pose");
    let rules: Vec<atelier_common::IdentityInjectionRule> =
        serde_json::from_str(&rules_env).expect("JSON valide");
    let git_rule = rules
        .iter()
        .find(|r| r.host == atelier_common::GIT_ALIAS_HOST)
        .expect("la regle d'injection Git calculee doit etre presente");
    assert_eq!(git_rule.header, "Authorization");
    assert_eq!(git_rule.prefix, "token ");
    // Meme chemin OpenBao que `crates/image-builder/src/main.rs::resolve_git_credentials`
    // (decision documentee dans `crates/controller/src/git_identity.rs`).
    assert_eq!(git_rule.secret_path, "git");
    assert_eq!(git_rule.field, "password");

    let net_proxy = pod_spec
        .containers
        .iter()
        .find(|c| c.name == "net-proxy")
        .expect("conteneur net-proxy");
    let git_alias_addr = net_proxy
        .env
        .as_ref()
        .and_then(|env| env.iter().find(|e| e.name == "ATELIER_GIT_ALIAS_ADDR"))
        .and_then(|e| e.value.clone())
        .expect("ATELIER_GIT_ALIAS_ADDR doit etre pose sur net-proxy");
    assert_eq!(git_alias_addr, "127.0.0.1:3129");

    // `Workshop.spec` lui-meme ne doit jamais avoir ete modifie (source de
    // verite declarative de l'utilisateur) : la regle est calculee a la
    // volee, jamais persistee dans le CRD.
    let refetched = workshops.get(&name).await.expect("get workshop");
    assert!(
        refetched.spec.identity_injection_rules.is_empty(),
        "la regle Git calculee ne doit jamais etre ecrite dans Workshop.spec"
    );

    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    service_accounts
        .delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    jobs.delete(&format!("{name}-image-build"), &foreground_delete())
        .await
        .ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Jalon M6, tache 6.4.2 : un pod parent deja en place, cree avec un
/// template different de celui que le controller construirait aujourd'hui
/// (simule ici un `helm upgrade` qui a change l'image d'un des conteneurs
/// entre deux reconciles), doit faire passer `status.upgradeState` a
/// `NeedsRestartForUpgrade` SANS que le pod (donc la microVM active qu'il
/// heberge) ne soit jamais supprime ni recree. Un pod fraichement cree, lui,
/// ne doit jamais porter cet etat.
#[tokio::test]
async fn apply_flags_needs_restart_for_upgrade_without_recreating_pod() {
    let Some(client) = try_client().await else {
        eprintln!("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier), test ignore");
        return;
    };

    let ns = "default";
    let name = unique_name("test-workshop-upgrade");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let ctx = ctx_without_openbao(client.clone());

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    let building_status = atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("premier apply()");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": building_status })),
        )
        .await
        .expect("ecriture du statut initial");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": { "imageDigest": "sha256:deadbeef" } })),
        )
        .await
        .expect("patch du statut");
    let with_digest = workshops.get(&name).await.expect("get workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("apply() ne doit pas echouer");
    let expected_pod_name = format!("{name}-parent");
    assert_eq!(
        status.upgrade_state, None,
        "un pod fraichement cree ne doit jamais avoir besoin d'un redemarrage"
    );

    let pod_before = pods
        .get(&expected_pod_name)
        .await
        .expect("le pod parent doit avoir ete cree");
    let uid_before = pod_before.metadata.uid.clone();

    // Simule un `helm upgrade` du controller : le pod deja en place porte
    // desormais un hash de template obsolete par rapport a ce que le
    // controller construirait aujourd'hui (patch direct de l'annotation,
    // le seul champ mutable du pod une fois cree).
    pods.patch(
        &expected_pod_name,
        &PatchParams::default(),
        &Patch::Merge(&serde_json::json!({
            "metadata": { "annotations": { "atelier.dev/template-hash": "stale-hash-from-an-older-controller" } }
        })),
    )
    .await
    .expect("patch de l'annotation de hash");

    let with_digest = workshops.get(&name).await.expect("get workshop");
    let status_after = atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("apply() ne doit pas echouer suite au hash obsolete");

    assert_eq!(
        status_after.upgrade_state,
        Some(atelier_common::WorkshopUpgradeState::NeedsRestartForUpgrade),
        "un hash de template divergent doit positionner NeedsRestartForUpgrade"
    );

    let pod_after = pods
        .get(&expected_pod_name)
        .await
        .expect("le pod parent doit toujours exister, jamais recree de force");
    assert_eq!(
        pod_after.metadata.uid, uid_before,
        "le pod (et donc la microVM active qu'il heberge) ne doit jamais etre recree pour un simple changement de template"
    );

    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    service_accounts
        .delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    jobs.delete(&format!("{name}-image-build"), &foreground_delete())
        .await
        .ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Suspendre un Workshop doit liberer son pod parent sans toucher a son
/// ServiceAccount ; le reprendre doit recreer le pod (phase `Resuming` puis
/// `Running`) sans reconstruire l'image.
#[tokio::test]
async fn apply_suspend_then_resume_releases_and_recreates_pod_only() {
    atelier_common::telemetry::ensure_crypto_provider();
    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop-suspend");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    let ctx = ctx_without_openbao(client.clone());
    let pod_name = format!("{name}-parent");

    workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": { "phase": "Pending", "imageDigest": "sha256:deadbeef" } })),
        )
        .await
        .expect("ecriture du statut initial");

    let running = workshops.get(&name).await.expect("get workshop");
    let status = atelier_controller::reconcile::apply(&ctx, &running)
        .await
        .expect("apply() initial (creation du pod)");
    assert_ne!(status.phase, WorkshopPhase::Suspended);
    pods.get(&pod_name)
        .await
        .expect("le pod parent doit exister avant toute suspension");

    // Suspendre : patch de spec.desiredState (pas status).
    workshops
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "spec": { "desiredState": "Suspended" } })),
        )
        .await
        .expect("passage en desiredState=Suspended");
    let suspending = workshops.get(&name).await.expect("get workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &suspending)
        .await
        .expect("apply() de suspension");
    assert!(
        matches!(
            status.phase,
            WorkshopPhase::Suspending | WorkshopPhase::Suspended
        ),
        "phase attendue Suspending ou Suspended, obtenu {:?}",
        status.phase
    );
    assert!(status.pod_name.is_none());

    // Le ServiceAccount doit survivre a la suspension.
    service_accounts
        .get(&pod_name)
        .await
        .expect("le ServiceAccount ne doit pas etre supprime par la suspension");

    // La suppression du pod n'est pas instantanee (grace period par
    // defaut) : on reapplique en boucle jusqu'a confirmation Suspended,
    // comme le ferait le controller reel au fil de ses cycles de requeue.
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": status })),
        )
        .await
        .expect("ecriture du statut de suspension");

    let mut status = status;
    for _ in 0..30 {
        let current = workshops.get(&name).await.expect("get workshop");
        status = atelier_controller::reconcile::apply(&ctx, &current)
            .await
            .expect("apply() de confirmation de suspension");
        workshops
            .patch_status(
                &name,
                &PatchParams::default(),
                &Patch::Merge(&serde_json::json!({ "status": status })),
            )
            .await
            .expect("ecriture du statut de suspension");
        if status.phase == WorkshopPhase::Suspended {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
    assert_eq!(
        status.phase,
        WorkshopPhase::Suspended,
        "timeout en attendant Suspended"
    );
    assert!(
        pods.get_opt(&pod_name)
            .await
            .expect("get_opt pod")
            .is_none(),
        "le pod parent doit avoir disparu une fois suspendu"
    );

    // Reprendre : patch de spec.desiredState vers Running (le statut
    // Suspended a deja ete ecrit par la boucle ci-dessus).
    workshops
        .patch(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "spec": { "desiredState": "Running" } })),
        )
        .await
        .expect("passage en desiredState=Running");
    let resuming = workshops.get(&name).await.expect("get workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &resuming)
        .await
        .expect("apply() de reprise");
    assert!(
        matches!(
            status.phase,
            WorkshopPhase::Resuming | WorkshopPhase::Running
        ),
        "phase attendue Resuming ou Running, obtenu {:?}",
        status.phase
    );
    assert_eq!(status.image_digest.as_deref(), Some("sha256:deadbeef"));
    pods.get(&pod_name)
        .await
        .expect("le pod parent doit avoir ete recree a la reprise");

    service_accounts
        .delete(&pod_name, &DeleteParams::default())
        .await
        .ok();
    pods.delete(&pod_name, &DeleteParams::default()).await.ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Necessite en plus une instance OpenBao accessible via OPENBAO_ADDR /
/// OPENBAO_TOKEN, cf. deploy/dev/openbao/README.md. Sans ces variables, le
/// test passe sans rien verifier : le provisioning OpenBao est une
/// fonctionnalite optionnelle (cf. ReconcileCtx.openbao).
///
/// Verifie la chaine complete : le controller provisionne un role
/// kubernetes-auth scope au ServiceAccount du pod parent, et ce
/// ServiceAccount (via un vrai token, obtenu comme le ferait Kubernetes en
/// le projetant dans le pod) peut effectivement se logger aupres d'OpenBao
/// et n'obtenir que les policies de son propre Workshop.
#[tokio::test]
async fn apply_provisions_openbao_role_when_configured() {
    atelier_common::telemetry::ensure_crypto_provider();

    let (Ok(openbao_addr), Ok(openbao_token)) = (
        std::env::var("OPENBAO_ADDR"),
        std::env::var("OPENBAO_TOKEN"),
    ) else {
        eprintln!(
            "OPENBAO_ADDR/OPENBAO_TOKEN non definis, test ignore (voir deploy/dev/openbao/README.md)"
        );
        return;
    };

    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop-openbao");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    let ctx = ReconcileCtx {
        client: client.clone(),
        openbao: Some(atelier_controller::openbao::OpenBaoConfig {
            addr: openbao_addr.clone(),
            token: openbao_token,
        }),
        registry_addr: "localhost:5000".to_string(),
        registry_insecure: true,
        llm_proxy_addr: None,
        llm_proxy_auth_token: None,
        git_identity: None,
        litellm: None,
    };

    workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": { "phase": "Pending", "imageDigest": "sha256:deadbeef" } })),
        )
        .await
        .expect("ecriture du statut initial");
    let with_digest = workshops.get(&name).await.expect("get workshop");

    atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_pod_name = format!("{name}-parent");
    let sa = service_accounts
        .get(&expected_pod_name)
        .await
        .expect("le ServiceAccount dedie doit avoir ete cree");
    assert_eq!(
        sa.metadata.name.as_deref(),
        Some(expected_pod_name.as_str())
    );

    // Obtient un vrai token pour ce ServiceAccount (equivalent a ce que
    // Kubernetes projette automatiquement dans un pod qui l'utilise).
    let output = std::process::Command::new("kubectl")
        .args(["create", "token", &expected_pod_name, "-n", ns])
        .output()
        .expect("kubectl doit etre disponible");
    assert!(
        output.status.success(),
        "kubectl create token a echoue: {output:?}"
    );
    let sa_token = String::from_utf8(output.stdout).unwrap().trim().to_string();

    let http = reqwest::Client::new();
    let login: serde_json::Value = http
        .post(format!("{openbao_addr}/v1/auth/kubernetes/login"))
        .json(&serde_json::json!({ "jwt": sa_token, "role": format!("workshop-{name}") }))
        .send()
        .await
        .expect("requete de login OpenBao")
        .error_for_status()
        .expect("le login OpenBao doit reussir avec le token du ServiceAccount dedie")
        .json()
        .await
        .expect("reponse JSON de login OpenBao");

    let policies = login["auth"]["policies"]
        .as_array()
        .expect("policies dans la reponse de login")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(
        policies.contains(&format!("workshop-{name}")),
        "le token OpenBao obtenu doit porter la policy scopee a ce Workshop: {policies:?}"
    );

    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    service_accounts
        .delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}

/// Necessite EN PLUS une vraie instance LiteLLM (ATELIER_LLM_PROXY_ADDR /
/// ATELIER_LLM_PROXY_AUTH_TOKEN, voir deploy/dev/llm-proxy/README.md), au
/// meme titre qu'OpenBao (OPENBAO_ADDR / OPENBAO_TOKEN) — sans l'une des
/// deux, silencieusement ignore.
///
/// Taches 3.1.3/3.2.1 (Jalon M3) : verifie que `apply()` genere une vraie
/// Virtual Key LiteLLM pour ce Workshop, l'ecrit dans OpenBao
/// (`secret/workshops/<name>/llm_key`), et cable la regle d'injection
/// `identity-proxy` correspondante (host `llm-proxy`) dans la spec du pod
/// parent cree — puis que le finalizer `atelier.dev/cleanup` revoque
/// effectivement cette cle cote LiteLLM (verifie via `/key/info`, 404 apres
/// suppression).
#[tokio::test]
async fn apply_wires_the_llm_virtual_key_injection_rule_when_configured() {
    atelier_common::telemetry::ensure_crypto_provider();

    let (Ok(openbao_addr), Ok(openbao_token)) = (
        std::env::var("OPENBAO_ADDR"),
        std::env::var("OPENBAO_TOKEN"),
    ) else {
        eprintln!(
            "OPENBAO_ADDR/OPENBAO_TOKEN non definis, test ignore (voir deploy/dev/openbao/README.md)"
        );
        return;
    };
    let Some(litellm_config) = atelier_controller::litellm::config_from_env(
        std::env::var("ATELIER_LLM_PROXY_ADDR").ok(),
        std::env::var("ATELIER_LLM_PROXY_AUTH_TOKEN").ok(),
    ) else {
        eprintln!(
            "ATELIER_LLM_PROXY_ADDR/ATELIER_LLM_PROXY_AUTH_TOKEN non definis, test ignore (voir deploy/dev/llm-proxy/README.md)"
        );
        return;
    };

    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop-llm");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    let base_url = format!("http://{}", litellm_config.addr);
    let master_key = litellm_config.master_key.clone();
    let ctx = ReconcileCtx {
        client: client.clone(),
        openbao: Some(atelier_controller::openbao::OpenBaoConfig {
            addr: openbao_addr.clone(),
            token: openbao_token.clone(),
        }),
        registry_addr: "localhost:5000".to_string(),
        registry_insecure: true,
        llm_proxy_addr: None,
        llm_proxy_auth_token: None,
        git_identity: None,
        litellm: Some(litellm_config),
    };

    workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");
    workshops
        .patch_status(
            &name,
            &PatchParams::default(),
            &Patch::Merge(&serde_json::json!({ "status": { "phase": "Pending", "imageDigest": "sha256:deadbeef" } })),
        )
        .await
        .expect("ecriture du statut initial");
    let with_digest = workshops.get(&name).await.expect("get workshop");

    atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_pod_name = format!("{name}-parent");
    let pod = pods
        .get(&expected_pod_name)
        .await
        .expect("le pod parent doit avoir ete cree");
    let identity_proxy = pod
        .spec
        .as_ref()
        .and_then(|s| s.containers.iter().find(|c| c.name == "identity-proxy"))
        .expect("conteneur identity-proxy present");
    let rules_json = identity_proxy
        .env
        .as_ref()
        .and_then(|env| {
            env.iter()
                .find(|e| e.name == "ATELIER_IDENTITY_INJECTION_RULES")
        })
        .and_then(|e| e.value.clone())
        .expect("ATELIER_IDENTITY_INJECTION_RULES doit etre defini");
    assert!(
        rules_json.contains("\"llm-proxy\""),
        "la regle d'injection pour l'alias llm-proxy doit etre presente: {rules_json}"
    );

    // La Virtual Key doit avoir ete ecrite dans OpenBao, exploitable par
    // identity-proxy (meme role Kubernetes-auth que le reste du Workshop).
    let http = reqwest::Client::new();
    let secret: serde_json::Value = http
        .get(format!(
            "{openbao_addr}/v1/secret/data/workshops/{name}/llm_key"
        ))
        .header("X-Vault-Token", &openbao_token)
        .send()
        .await
        .expect("lecture du secret llm_key")
        .error_for_status()
        .expect("le secret llm_key doit avoir ete ecrit par apply()")
        .json()
        .await
        .expect("reponse JSON de lecture llm_key");
    let virtual_key = secret["data"]["data"]["value"]
        .as_str()
        .expect("champ value du secret llm_key")
        .to_string();
    assert!(virtual_key.starts_with("sk-"), "{virtual_key}");

    // La cle doit exister cote LiteLLM (pas seulement dans OpenBao).
    let info = http
        .get(format!("{base_url}/key/info"))
        .bearer_auth(&master_key)
        .query(&[("key", &virtual_key)])
        .send()
        .await
        .expect("appel /key/info");
    assert_eq!(
        info.status(),
        reqwest::StatusCode::OK,
        "la Virtual Key doit exister cote LiteLLM juste apres provisioning"
    );

    // Le finalizer `atelier.dev/cleanup` (tache 3.2.1) doit la revoquer a la
    // suppression du Workshop : appelle directement `cleanup()` (la logique
    // executee par le handler `Event::Cleanup` du finalizer, voir
    // `reconcile()`) plutot que d'attendre un `Controller` complet en cours
    // d'execution (aucun n'est demarre dans ce test), meme approche que les
    // tests OpenBao ci-dessus qui exercent `apply()` directement.
    atelier_controller::reconcile::cleanup(&ctx, &with_digest)
        .await
        .expect("cleanup() ne doit pas echouer");
    workshops.delete(&name, &DeleteParams::default()).await.ok();

    let info_after_delete = http
        .get(format!("{base_url}/key/info"))
        .bearer_auth(&master_key)
        .query(&[("key", &virtual_key)])
        .send()
        .await
        .expect("appel /key/info apres suppression");
    assert_ne!(
        info_after_delete.status(),
        reqwest::StatusCode::OK,
        "la Virtual Key ne doit plus exister cote LiteLLM apres suppression"
    );

    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    service_accounts
        .delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
}

/// Necessite OPENBAO_ADDR/OPENBAO_TOKEN (voir deploy/dev/openbao/README.md),
/// silencieusement ignore sans ces variables.
///
/// Tache 1.2.6 (docs/specs/PLAN-ACTION-GLOBAL.md) : verifie le role OpenBao
/// cluster-wide `atelier-api-server` (pas scope a un seul Workshop, voir
/// `crates/controller/src/openbao.rs::ensure_api_server_role`). Contrairement
/// au role `workshop-<name>` teste ci-dessus, celui-ci doit permettre de
/// lire le secret `session_auth` de N'IMPORTE QUEL Workshop (a partir d'un
/// seul ServiceAccount cluster-wide, celui du Deployment `api-server`), mais
/// rien d'autre (pas les autres secrets d'un Workshop, ex: `git`).
#[tokio::test]
async fn ensure_api_server_role_reads_any_workshop_session_auth_but_nothing_else() {
    atelier_common::telemetry::ensure_crypto_provider();

    let (Ok(openbao_addr), Ok(openbao_token)) = (
        std::env::var("OPENBAO_ADDR"),
        std::env::var("OPENBAO_TOKEN"),
    ) else {
        eprintln!(
            "OPENBAO_ADDR/OPENBAO_TOKEN non definis, test ignore (voir deploy/dev/openbao/README.md)"
        );
        return;
    };

    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "atelier-system-test";
    let sa_name = "atelier-api-server-test";
    let namespaces: Api<k8s_openapi::api::core::v1::Namespace> = Api::all(client.clone());
    let _ = namespaces
        .create(
            &PostParams::default(),
            &k8s_openapi::api::core::v1::Namespace {
                metadata: kube::api::ObjectMeta {
                    name: Some(ns.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;
    let service_accounts: Api<k8s_openapi::api::core::v1::ServiceAccount> =
        Api::namespaced(client.clone(), ns);
    let _ = service_accounts
        .create(
            &PostParams::default(),
            &k8s_openapi::api::core::v1::ServiceAccount {
                metadata: kube::api::ObjectMeta {
                    name: Some(sa_name.to_string()),
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await;

    let openbao_config = atelier_controller::openbao::OpenBaoConfig {
        addr: openbao_addr.clone(),
        token: openbao_token.clone(),
    };
    atelier_controller::openbao::ensure_api_server_role(&openbao_config, ns, sa_name)
        .await
        .expect("ensure_api_server_role ne doit pas echouer");

    // Deux Workshops distincts, avec un vrai secret session_auth chacun
    // (ecrit directement via le token d'administration OpenBao, comme le
    // ferait le controller via `ensure_session_auth` en conditions
    // reelles) : le role cluster-wide doit pouvoir lire les DEUX, avec le
    // meme token client.
    let http = reqwest::Client::new();
    for (name, password) in [
        ("api-server-role-test-a", "password-a"),
        ("api-server-role-test-b", "password-b"),
    ] {
        http.put(format!(
            "{openbao_addr}/v1/secret/data/workshops/{name}/session_auth"
        ))
        .header("X-Vault-Token", &openbao_token)
        .json(&serde_json::json!({ "data": { "password": password } }))
        .send()
        .await
        .expect("ecriture du secret session_auth de test")
        .error_for_status()
        .expect("ecriture du secret session_auth de test refusee");
    }
    // Un secret HORS `session_auth` sous le meme Workshop : le wildcard `+`
    // de la policy `atelier-api-server` ne couvre que le dernier segment de
    // chemin (`session_auth` precis), pas les autres secrets d'un Workshop.
    http.put(format!(
        "{openbao_addr}/v1/secret/data/workshops/api-server-role-test-a/git"
    ))
    .header("X-Vault-Token", &openbao_token)
    .json(&serde_json::json!({ "data": { "password": "git-secret-should-stay-out-of-reach" } }))
    .send()
    .await
    .expect("ecriture du secret git de test")
    .error_for_status()
    .expect("ecriture du secret git de test refusee");

    let output = std::process::Command::new("kubectl")
        .args(["create", "token", sa_name, "-n", ns])
        .output()
        .expect("kubectl doit etre disponible");
    assert!(
        output.status.success(),
        "kubectl create token a echoue: {output:?}"
    );
    let sa_token = String::from_utf8(output.stdout).unwrap().trim().to_string();

    let login: serde_json::Value = http
        .post(format!("{openbao_addr}/v1/auth/kubernetes/login"))
        .json(&serde_json::json!({
            "jwt": sa_token,
            "role": atelier_controller::openbao::API_SERVER_ROLE,
        }))
        .send()
        .await
        .expect("requete de login OpenBao")
        .error_for_status()
        .expect("le login OpenBao doit reussir avec le token du ServiceAccount api-server")
        .json()
        .await
        .expect("reponse JSON de login OpenBao");
    let client_token = login["auth"]["client_token"]
        .as_str()
        .expect("client_token dans la reponse de login")
        .to_string();

    for (name, expected_password) in [
        ("api-server-role-test-a", "password-a"),
        ("api-server-role-test-b", "password-b"),
    ] {
        let secret: serde_json::Value = http
            .get(format!(
                "{openbao_addr}/v1/secret/data/workshops/{name}/session_auth"
            ))
            .header("X-Vault-Token", &client_token)
            .send()
            .await
            .expect("requete de lecture session_auth")
            .error_for_status()
            .expect("le role api-server doit pouvoir lire session_auth de n'importe quel Workshop")
            .json()
            .await
            .expect("reponse JSON de lecture session_auth");
        assert_eq!(
            secret["data"]["data"]["password"].as_str(),
            Some(expected_password),
            "mot de passe session_auth inattendu pour {name}"
        );
    }

    let git_secret_denied = http
        .get(format!(
            "{openbao_addr}/v1/secret/data/workshops/api-server-role-test-a/git"
        ))
        .header("X-Vault-Token", &client_token)
        .send()
        .await
        .expect("requete de lecture du secret git");
    assert_eq!(
        git_secret_denied.status(),
        reqwest::StatusCode::FORBIDDEN,
        "le role api-server ne doit PAS pouvoir lire un secret autre que session_auth"
    );

    let _ = service_accounts
        .delete(sa_name, &DeleteParams::default())
        .await;
    let _ = namespaces.delete(ns, &DeleteParams::default()).await;
}
