//! Cache content-addressed des images de microVM (rootfs ext4 construits par
//! `image-builder`) : aujourd'hui un PVC Kubernetes partage, monte en
//! lecture-ecriture par les Jobs `image-builder` et (a terme, une fois
//! `vm-supervisor` reellement branche dans le pod parent) en lecture seule
//! par les pods parents. Offload/reload vers un object storage (S3) quand le
//! PVC est trop rempli : envisage plus tard, pas implemente ici.

use k8s_openapi::api::core::v1::{
    PersistentVolumeClaim, PersistentVolumeClaimSpec, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use kube::api::{Api, ObjectMeta, Patch, PatchParams};
use kube::Client;
use std::collections::BTreeMap;

const FIELD_MANAGER: &str = "atelier-controller";
pub const IMAGE_CACHE_PVC_NAME: &str = "atelier-image-cache";
pub const IMAGE_CACHE_MOUNT_PATH: &str = "/cache";

/// Cree le PVC de cache s'il n'existe pas encore. Idempotent (server-side
/// apply) ; ne redimensionne pas un PVC existant. Partage par tous les
/// Workshops d'un namespace, donc **sans** owner reference vers un Workshop
/// en particulier : il survit a la suppression de n'importe lequel d'entre
/// eux.
pub async fn ensure_image_cache_pvc(client: &Client, ns: &str, size: &str) -> anyhow::Result<()> {
    let pvcs: Api<PersistentVolumeClaim> = Api::namespaced(client.clone(), ns);

    let pvc = PersistentVolumeClaim {
        metadata: ObjectMeta {
            name: Some(IMAGE_CACHE_PVC_NAME.to_string()),
            namespace: Some(ns.to_string()),
            ..Default::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".to_string(),
                    Quantity(size.to_string()),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    pvcs.patch(
        IMAGE_CACHE_PVC_NAME,
        &PatchParams::apply(FIELD_MANAGER).force(),
        &Patch::Apply(&pvc),
    )
    .await?;

    Ok(())
}

/// Sous-repertoire du cache pour un digest donne (meme convention que
/// `image-builder`, cf. `crates/image-builder/src/main.rs::publish_to_cache`).
pub fn digest_cache_subdir(digest: &str) -> String {
    digest.replace(':', "_")
}

/// Sous-repertoire du cache ou `vm-supervisor` publie/lit les fichiers de
/// snapshot d'UN Workshop (`snapshot.state`/`snapshot.mem`). Contrairement
/// au cache d'images (`digest_cache_subdir`, partage entre Workshops via le
/// digest du contenu), un snapshot est scope a un seul Workshop a la fois —
/// pas de dedup utile, donc pas de content-addressing : simplement
/// namespace/nom, ecrase a chaque nouvelle suspension.
pub fn snapshot_cache_subdir(ns: &str, name: &str) -> String {
    format!("snapshots/{ns}_{name}")
}
