//! Injecte des identites/tokens (ex: credentials cloud, tokens d'API) dans
//! les appels sortants de l'agent, sans jamais exposer le secret brut a la VM.
//!
//! Les secrets destines aux environnements (pas ceux du cluster Kubernetes
//! sous-jacent, qui restent geres par les mecanismes k8s standards) sont
//! stockes dans [OpenBao](https://openbao.org/). identity-proxy s'y
//! authentifie avec l'entite machine Kanidm provisionnee pour ce Workshop
//! (`WorkshopStatus.kanidm_entity_id`, distincte du sujet humain
//! proprietaire) et ne recupere que les secrets scopes a ce Workshop.

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    tracing::info!("atelier-identity-proxy starting");
    // TODO: s'authentifier aupres d'OpenBao avec l'identite Kanidm du Workshop
    // TODO: signer/injecter les credentials a la volee dans les requetes proxiees
    Ok(())
}
