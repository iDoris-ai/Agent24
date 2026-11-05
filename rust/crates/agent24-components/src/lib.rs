//! Generic component machinery (ADR-038 C1): the schema-2 `ComponentLock`, the
//! pinned-release downloader, the immutable installer with its hidden-subcommand
//! writer process, and D2a signature verification. Component crates (agent24-idoris)
//! and the daemon's Hyphae installer depend on this crate, never the other way round.
//!
//! C3 plugs the component state machine (`installing -> installed`) in at
//! `install::verify_signatures`'s call sites; nothing here reports state yet.

pub mod download;
pub mod install;
pub mod integrity;
pub mod lock;
pub mod sign;
pub mod state;
pub mod storage;

pub use download::{
    DownloadError, DownloadOptions, DownloadProgress, MAX_ARCHIVE_BYTES, PinnedAsset,
    RELEASE_HOSTS, VerifiedArchive, download_locked_asset, download_pinned,
};
pub use install::run_install_child;
pub use lock::{ComponentLock, LockError};
pub use sign::{SignatureVerifier, SigningRequirement, VerifyMode, signing_requirement};
pub use state::{ComponentState, Form};

#[cfg(test)]
mod tests;
