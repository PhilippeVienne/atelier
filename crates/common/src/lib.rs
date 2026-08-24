pub mod crd;
pub mod openbao_client;
pub mod status;
pub mod telemetry;

pub use crd::{
    DevcontainerSource, IdentityInjectionRule, Workshop, WorkshopDesiredState, WorkshopPhase,
    WorkshopResources, WorkshopSpec, WorkshopStatus, WorkshopUpgradeState,
};
pub use openbao_client::OpenBaoClient;
pub use status::patch_workshop_status;
