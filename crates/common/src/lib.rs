pub mod crd;
pub mod openbao_client;
pub mod squad_token;
pub mod status;
pub mod storage;
pub mod telemetry;
pub mod tls_client;

pub use crd::{
    DevcontainerSource, ExportedService, IdentityInjectionRule, SimulatorSpec, SimulatorType,
    Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopResources, WorkshopSpec, WorkshopStatus,
    WorkshopUpgradeState, GIT_ALIAS_HOST,
};
pub use openbao_client::OpenBaoClient;
pub use status::patch_workshop_status;
