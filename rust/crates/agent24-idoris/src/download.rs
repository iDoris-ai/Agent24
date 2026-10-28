//! iDoris lock adapter for the shared verified component downloader.
use crate::IdorisLock;
pub use agent24_components::{
    DownloadError, DownloadOptions, MAX_ARCHIVE_BYTES, RELEASE_HOSTS, VerifiedArchive,
};
use std::path::Path;
use tokio_util::sync::CancellationToken;

pub async fn download_locked_asset(
    lock: &IdorisLock,
    triple: &str,
    cache_dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    let asset = lock
        .asset_for(triple)
        .ok_or(DownloadError::UnsupportedPlatform)?;
    agent24_components::fetch_verified(
        &lock.asset_url(asset),
        &asset.sha256,
        &asset.name,
        cache_dir,
        RELEASE_HOSTS,
        opts,
        cancel,
    )
    .await
}

#[cfg(test)]
mod tests;
