//! Generic component machinery (ADR-038 C1): the schema-2 `ComponentLock`, the
//! pinned-release downloader, the immutable installer with its hidden-subcommand
//! writer process, and D2a signature verification. Component crates (agent24-idoris,
//! later others) depend on this crate, never the other way round.
//!
//! C3 plugs the component state machine (`installing -> installed`) in at
//! `install::verify_signatures`'s call sites; nothing here reports state yet.

pub mod download;
pub mod install;
pub mod lock;
pub mod sign;
pub mod storage;

use std::path::Path;
use tokio_util::sync::CancellationToken;

pub use download::{
    DownloadError, DownloadOptions, DownloadProgress, MAX_ARCHIVE_BYTES, PinnedAsset,
    RELEASE_HOSTS, VerifiedArchive, download_locked_asset, download_pinned,
};
pub use install::run_install_child;
pub use lock::{ComponentLock, LockError};

/// Fetch a pinned release asset using the fixed GitHub host policy, for callers
/// that carry their own digest instead of a `ComponentLock` (Hyphae, schema 1,
/// until C1b). The cache must live below a managed `components/<id>/` root, which
/// supplies the per-component operation lock and the directory checks.
/// URLs must end in `/<tag>/<file_name>`; digests come from the caller's lock.
pub async fn fetch_verified(
    url: &str,
    expected_sha256: &str,
    file_name: &str,
    cache_dir: &Path,
    limits: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    let (prefix, url_name) = url
        .rsplit_once('/')
        .ok_or(DownloadError::Network("invalid locked URL"))?;
    prefix
        .rsplit_once('/')
        .filter(|(_, tag)| !tag.is_empty())
        .ok_or(DownloadError::Network("invalid locked URL"))?;
    if url_name != file_name {
        return Err(DownloadError::Network("locked asset name differs from URL"));
    }
    download_pinned(
        &PinnedAsset {
            url: url.into(),
            name: file_name.into(),
            sha256: expected_sha256.into(),
        },
        cache_dir,
        limits,
        cancel,
    )
    .await
}

#[cfg(test)]
mod tests;
