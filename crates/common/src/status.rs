use crate::Workshop;
use kube::api::{Api, Patch, PatchParams};
use kube::Client;

/// Merge-patch partiel du sous-objet `status` d'un `Workshop` (RFC 7386) :
/// seules les cles presentes dans `fields` sont modifiees, le reste du
/// statut est preserve. Utilise par les composants (ex: `image-builder`)
/// qui ne connaissent qu'une partie du statut et ne doivent pas ecraser le
/// reste (ex: `pod_name` gere par le `controller`).
pub async fn patch_workshop_status(
    client: &Client,
    namespace: &str,
    name: &str,
    fields: serde_json::Value,
) -> Result<(), kube::Error> {
    let api: Api<Workshop> = Api::namespaced(client.clone(), namespace);
    let patch = serde_json::json!({ "status": fields });
    api.patch_status(name, &PatchParams::default(), &Patch::Merge(&patch))
        .await?;
    Ok(())
}
