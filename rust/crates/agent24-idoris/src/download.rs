//! iDoris's use of the generic downloader (agent24-components).

use crate::{IdorisLock, platform_of};
pub use agent24_components::download::{
    DownloadError, DownloadOptions, DownloadProgress, MAX_ARCHIVE_BYTES, RELEASE_HOSTS,
    VerifiedArchive,
};
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Fetch or reverify the pinned archive for a Rust target triple.
pub async fn download_locked_asset(
    lock: &IdorisLock,
    triple: &str,
    cache_dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    let platform = platform_of(triple).ok_or(DownloadError::UnsupportedPlatform)?;
    agent24_components::download_locked_asset(
        &lock.to_component(),
        platform,
        cache_dir,
        opts,
        cancel,
    )
    .await
}
