//! Workspace service boundary. Filesystem and persistence behavior live in later slices.

mod error;
mod root;
mod service;
mod spec;

pub use error::WorkspaceError;
pub use root::PinnedWorkspaceRoot;
pub use service::WorkspaceService;
pub use spec::ScratchCreateSpec;
