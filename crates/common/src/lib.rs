pub mod crd;
pub mod status;

pub use crd::{
    DevcontainerSource, Workshop, WorkshopDesiredState, WorkshopPhase, WorkshopResources,
    WorkshopSpec, WorkshopStatus,
};
pub use status::patch_workshop_status;
