//! Test d'integration : necessite un vrai cluster Kubernetes accessible via
//! le kubeconfig par defaut, avec le CRD `Workshop` installe
//! (`kubectl apply -f crds/workshop.yaml`). Un cluster kind local suffit :
//!
//!   kind create cluster --name atelier-dev
//!   kubectl apply -f crds/workshop.yaml
//!   cargo test -p atelier-controller

use atelier_common::{
    DevcontainerSource, Workshop, WorkshopDesiredState, WorkshopResources, WorkshopSpec,
};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Api, DeleteParams, PostParams};
use kube::Client;
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{prefix}-{nanos}")
}

#[tokio::test]
async fn apply_creates_owned_placeholder_pod() {
    let client = Client::try_default()
        .await
        .expect("kubeconfig requis (cluster kind local, cf. commentaire en tete de fichier)");

    let ns = "default";
    let name = unique_name("test-workshop");
    let workshops: Api<Workshop> = Api::namespaced(client.clone(), ns);
    let pods: Api<Pod> = Api::namespaced(client.clone(), ns);

    let spec = WorkshopSpec {
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
    };

    let created = workshops
        .create(&PostParams::default(), &Workshop::new(&name, spec))
        .await
        .expect("creation du Workshop");

    let status = atelier_controller::reconcile::apply(&client, &created)
        .await
        .expect("apply() ne doit pas echouer");

    let expected_pod_name = format!("{name}-parent");
    assert_eq!(status.pod_name.as_deref(), Some(expected_pod_name.as_str()));

    let pod = pods
        .get(&expected_pod_name)
        .await
        .expect("le pod parent doit avoir ete cree");
    let owners = pod
        .metadata
        .owner_references
        .expect("le pod doit avoir une owner reference vers le Workshop");
    assert_eq!(owners.len(), 1);
    assert_eq!(owners[0].name, name);
    assert_eq!(owners[0].kind, "Workshop");

    // apply() doit rester idempotent : un deuxieme appel ne doit pas echouer.
    atelier_controller::reconcile::apply(&client, &created)
        .await
        .expect("un deuxieme apply() doit rester idempotent");

    pods.delete(&expected_pod_name, &DeleteParams::default())
        .await
        .ok();
    workshops.delete(&name, &DeleteParams::default()).await.ok();
}
