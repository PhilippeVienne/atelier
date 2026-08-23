//! Provisioning de l'entite machine Kanidm propre a chaque Workshop
//! (`WorkshopStatus.kanidm_entity_id`), distincte du sujet humain
//! proprietaire. Optionnel : si Kanidm n'est pas configure (variables
//! d'environnement absentes), le controller continue sans provisionner
//! d'identite plutot que d'echouer — utile pour le dev/tests qui n'ont pas
//! besoin de cette brique.

use kanidm_client::{ClientError, KanidmClient, KanidmClientBuilder};

/// Construit le client Kanidm a partir de l'environnement :
/// - `KANIDM_URL` (requis pour activer le provisioning)
/// - `KANIDM_API_TOKEN` (requis, token de service account en lecture-ecriture)
/// - `KANIDM_CA_PATH` (optionnel, certificat CA pour un serveur en TLS auto-signe)
///
/// Renvoie `Ok(None)` si `KANIDM_URL` est absent (fonctionnalite desactivee),
/// et une erreur si elle est partiellement configuree ou si la construction
/// du client echoue.
pub async fn client_from_env() -> anyhow::Result<Option<KanidmClient>> {
    let Ok(url) = std::env::var("KANIDM_URL") else {
        tracing::info!("KANIDM_URL absent, provisioning d'identite Kanidm desactive");
        return Ok(None);
    };
    let api_token = std::env::var("KANIDM_API_TOKEN")
        .map_err(|_| anyhow::anyhow!("KANIDM_URL est defini mais KANIDM_API_TOKEN est absent"))?;

    let mut builder = KanidmClientBuilder::new().address(url);
    if let Ok(ca_path) = std::env::var("KANIDM_CA_PATH") {
        builder = builder
            .add_root_certificate_filepath(&ca_path)
            .map_err(|e| anyhow::anyhow!("chargement du CA Kanidm ({ca_path}): {e:?}"))?;
    }

    let client = builder
        .build()
        .map_err(|e| anyhow::anyhow!("construction du client Kanidm: {e:?}"))?;
    client.set_token(api_token).await;

    Ok(Some(client))
}

/// Cree (si absent) le service account Kanidm de ce Workshop et renvoie son
/// identifiant stable (le nom du compte, deterministe a partir du nom du
/// Workshop).
pub async fn ensure_workshop_entity(
    client: &KanidmClient,
    workshop_name: &str,
) -> anyhow::Result<String> {
    let account_name = format!("atelier-workshop-{workshop_name}");

    let existing = client
        .idm_service_account_get(&account_name)
        .await
        .map_err(|e| anyhow::anyhow!("lecture du service account Kanidm: {e:?}"))?;

    if existing.is_none() {
        client
            .idm_service_account_create(
                &account_name,
                &format!("Atelier Workshop {workshop_name}"),
                "idm_admin",
            )
            .await
            .map_err(|e| anyhow::anyhow!("creation du service account Kanidm: {e:?}"))?;
    }

    Ok(account_name)
}

/// Supprime le service account Kanidm d'un Workshop. Idempotent : un 404
/// (deja absent) n'est pas une erreur, pour ne pas bloquer indefiniment la
/// suppression d'un Workshop dont l'entite a deja ete nettoyee ; toute autre
/// erreur (reseau, auth, ...) est remontee pour que le finalizer retente
/// plutot que de risquer une entite orpheline.
pub async fn delete_workshop_entity(
    client: &KanidmClient,
    workshop_name: &str,
) -> anyhow::Result<()> {
    let account_name = format!("atelier-workshop-{workshop_name}");
    match client.idm_service_account_delete(&account_name).await {
        Ok(()) => Ok(()),
        Err(ClientError::Http(status, _, _)) if status == reqwest::StatusCode::NOT_FOUND => Ok(()),
        Err(err) => Err(anyhow::anyhow!(
            "suppression du service account Kanidm: {err:?}"
        )),
    }
}
