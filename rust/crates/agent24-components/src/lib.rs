//! Component downloads reuse iDoris's reviewed downloader, including its managed
//! directory checks, operation lock, cancellation and progress reporting.

pub use agent24_idoris::download::{
    DownloadError, DownloadOptions, DownloadProgress, MAX_ARCHIVE_BYTES, RELEASE_HOSTS,
    VerifiedArchive,
};
use agent24_idoris::{IdorisLock, LockedAsset, download::download_locked_asset};
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// Fetch a pinned release asset using the fixed GitHub host policy. The cache
/// must live inside an `idoris` tree so the existing installation lock and
/// directory checks apply. This adapter does not install an iDoris launcher.
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
    let (base, tag) = prefix
        .rsplit_once('/')
        .filter(|(_, tag)| !tag.is_empty())
        .ok_or(DownloadError::Network("invalid locked URL"))?;
    if url_name != file_name {
        return Err(DownloadError::Network("locked asset name differs from URL"));
    }
    // Only these release fields are read by download_locked_asset. Preserve
    // its API and implementation rather than splitting its security boundary.
    let mut lock = IdorisLock::embedded().clone();
    lock.release_url_base = base.into();
    lock.tag = tag.into();
    lock.assets.clear();
    lock.assets.insert(
        "component".into(),
        LockedAsset {
            name: file_name.into(),
            sha256: expected_sha256.into(),
            files: Vec::new(),
        },
    );
    download_locked_asset(&lock, "component", cache_dir, limits, cancel).await
}

#[cfg(test)]
mod tests;
