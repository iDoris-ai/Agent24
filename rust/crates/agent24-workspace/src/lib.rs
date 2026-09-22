//! Workspace service boundary. Filesystem and persistence behavior live in later slices.

mod error;
mod service;
mod spec;

pub use error::WorkspaceError;
pub use service::WorkspaceService;
pub use spec::ScratchCreateSpec;
