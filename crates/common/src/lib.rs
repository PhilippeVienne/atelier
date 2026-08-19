pub mod crd;
pub mod status;
pub mod telemetry;

pub use crd::{
    DevcontainerSource, IdentityInjectionRule, Workshop, WorkshopDesiredState, WorkshopPhase,
    WorkshopResources, WorkshopSpec, WorkshopStatus,
};
pub use status::patch_workshop_status;
