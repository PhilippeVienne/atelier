pub mod crd;
pub mod openbao_client;
pub mod status;
pub mod storage;
pub mod telemetry;

pub use crd::{
    DevcontainerSource, IdentityInjectionRule, Workshop, WorkshopDesiredState, WorkshopPhase,
    WorkshopResources, WorkshopSpec, WorkshopStatus, WorkshopUpgradeState, GIT_ALIAS_HOST,
};
pub use openbao_client::OpenBaoClient;
pub use status::patch_workshop_status;
