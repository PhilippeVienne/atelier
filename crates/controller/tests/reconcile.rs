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

/// Contexte de test sans Kanidm configure (comportement par defaut,
/// identique a avant l'introduction du provisioning d'identite).
fn ctx_without_kanidm(client: Client) -> ReconcileCtx {
    ReconcileCtx {
        client,
        kanidm: None,
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
        .expect("le job doit avoir une owner reference vers le Workshop");
    assert_eq!(owners[0].name, name);

    let env = job.spec.unwrap().template.spec.unwrap().containers[0]
        .env
        .clone()
        .unwrap();
    let repo_env = env
        .iter()
        .find(|e| e.name == "ATELIER_DEVCONTAINER_REPO")
        .expect("ATELIER_DEVCONTAINER_REPO doit etre transmise au job");
    assert_eq!(repo_env.value.as_deref(), Some("https://example.invalid/repo.git"));

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

    // apply() doit rester idempotent : un deuxieme appel ne doit pas echouer.
    atelier_controller::reconcile::apply(&ctx, &with_digest)
        .await
        .expect("un deuxieme apply() doit rester idempotent");

    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    jobs.delete(&format!("{name}-image-build"), &foreground_delete())
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
    };

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, sample_spec()))
        .await
        .expect("creation du Workshop");

    let status = atelier_controller::reconcile::apply(&ctx, &created)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_entity = format!("atelier-workshop-{name}");
    assert_eq!(status.kanidm_entity_id.as_deref(), Some(expected_entity.as_str()));

    let entity = kanidm
        .idm_service_account_get(&expected_entity)
        .await
        .expect("lecture du service account Kanidm")
        .expect("le service account doit avoir ete cree dans Kanidm");
    assert_eq!(
        entity.attrs.get("name").and_then(|v| v.first()).map(String::as_str),
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
    workshops
        .delete(&name, &foreground_delete())
        .await
        .ok();
}
