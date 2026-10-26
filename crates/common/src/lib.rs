pub mod crd;
pub mod openbao_client;
pub mod squad_token;
pub mod status;
pub mod storage;
pub mod telemetry;
pub mod tls_client;

/// Code de sortie d'`image-builder` pour un refus qu'aucune nouvelle tentative
/// ne changera (image qui ne peut pas demarrer). Le controller l'inscrit dans
/// la `podFailurePolicy` du Job de build : un build complet n'est pas refait
/// pour rien (tache 14.12).
pub const IMAGE_BUILD_REFUSED_EXIT_CODE: i32 = 65;

/// Cle de `status.conditions` qui porte la raison de l'echec du build.
pub const IMAGE_BUILD_CONDITION: &str = "ImageBuildFailed";

pub use crd::{
    DevcontainerSource, ExportedService, GuestPorts, GuestServices, IdentityInjectionRule,
    SimulatorSpec, SimulatorType, Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopResources,
    WorkshopSpec, WorkshopStatus, WorkshopUpgradeState, GIT_ALIAS_HOST,
};
pub use openbao_client::OpenBaoClient;
pub use status::patch_workshop_status;
