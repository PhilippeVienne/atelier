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

/// Contexte de test sans Kanidm ni OpenBao configures (comportement par
/// defaut, identique a avant l'introduction du provisioning d'identite).
fn ctx_without_kanidm(client: Client) -> ReconcileCtx {
    ReconcileCtx {
        client,
        kanidm: None,
        openbao: None,
        registry_addr: "localhost:5000".to_string(),
        registry_insecure: true,
        llm_proxy_addr: None,
        llm_proxy_auth_token: None,
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
        },
        egress_allowlist: vec![],
        tools: vec![],
        identity_injection_rules: vec![],
        owner_subject: "test-user".into(),
        desired_state: WorkshopDesiredState::Running,
    }
}

/// Sans `status.imageDigest`, apply() doit declencher un Job image-builder
/// et rester en phase BuildingImage, sans creer de pod parent.
#[tokio::test]
async fn apply_triggers_image_build_job_when_digest_missing() {
    atelier_common::telemetry::ensure_crypto_provider();
    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop-build");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let ctx = ctx_without_kanidm(client.clone());

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
    atelier_common::telemetry::ensure_crypto_provider();
    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop-pod");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);
    let jobs: Api<Job> = Api::namespaced(client.clone(), ns);
    let ctx = ctx_without_kanidm(client.clone());

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    // Premier apply() : declenche le Job image-builder et initialise
    // status.phase=BuildingImage (necessaire avant tout patch partiel, le
    // CRD exige status.phase). Cf. l'autre test pour cette etape en detail.
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
    let ctx = ctx_without_kanidm(client.clone());
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
        kanidm: None,
        openbao: Some(atelier_controller::openbao::OpenBaoConfig {
            addr: openbao_addr.clone(),
            token: openbao_token,
        }),
        registry_addr: "localhost:5000".to_string(),
        registry_insecure: true,
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

/// Necessite en plus une instance Kanidm accessible via KANIDM_URL /
/// KANIDM_API_TOKEN (+ KANIDM_CA_PATH si TLS auto-signe), cf.
/// deploy/dev/kanidm/README.md. Sans ces variables, le test passe sans rien
/// verifier : le provisioning Kanidm est une fonctionnalite optionnelle,
/// pas une dependance dure du controller (cf. ReconcileCtx.kanidm).
#[tokio::test]
async fn apply_provisions_kanidm_entity_when_configured() {
    atelier_common::telemetry::ensure_crypto_provider();
    let Some(kanidm) = atelier_controller::kanidm::client_from_env()
        .await
        .expect("client_from_env ne doit pas echouer si les variables sont coherentes")
        .map(std::sync::Arc::new)
    else {
        eprintln!(
            "KANIDM_URL non defini, test ignore (voir deploy/dev/kanidm/README.md pour le configurer)"
        );
        return;
    };

    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop-kanidm");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let ctx = ReconcileCtx {
        client: client.clone(),
        kanidm: Some(kanidm.clone()),
        openbao: None,
        registry_addr: "localhost:5000".to_string(),
        registry_insecure: true,
    };

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_entity = format!("atelier-workshop-{name}");
    assert_eq!(
        status.kanidm_entity_id.as_deref(),
        Some(expected_entity.as_str())
    );

    let entity = kanidm
        .idm_service_account_get(&expected_entity)
        .await
        .expect("lecture du service account Kanidm")
        .expect("le service account doit avoir ete cree dans Kanidm");
    assert_eq!(
        entity
            .attrs
            .get("name")
            .and_then(|v| v.first())
            .map(String::as_str),
        Some(expected_entity.as_str())
    );

    // Un deuxieme apply() ne doit pas tenter de recreer l'entite (elle
    // existe deja d'apres status.kanidmEntityId).
    atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("un deuxieme apply() doit rester idempotent");

    kanidm
        .idm_service_account_delete(&expected_entity)
        .await
        .ok();
    workshops.delete(&name, &foreground_delete()).await.ok();
}
