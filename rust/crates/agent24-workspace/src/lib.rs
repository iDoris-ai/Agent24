//! Workspace service boundary. Filesystem and persistence behavior live in later slices.

mod allocation;
mod error;
mod service;
mod spec;

pub use allocation::{AllocationId, AllocationPhase};
pub use error::WorkspaceError;
pub use service::WorkspaceService;
pub use spec::ScratchCreateSpec;
