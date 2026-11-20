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

/// Verify the app-carried archive against the same platform asset pinned in the lock.
pub fn bundled_locked_asset(
    lock: &IdorisLock,
    triple: &str,
    path: &Path,
) -> Result<VerifiedArchive, DownloadError> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    platform_of(triple).ok_or(DownloadError::UnsupportedPlatform)?;
    let asset = lock
        .asset_for(triple)
        .ok_or(DownloadError::UnsupportedPlatform)?;
    let archive_path = if path.is_dir() {
        path.join(&asset.name)
    } else {
        path.to_owned()
    };
    let mut file = std::fs::File::open(&archive_path)?;
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        size += read as u64;
        if size > MAX_ARCHIVE_BYTES {
            return Err(DownloadError::TooLarge);
        }
        hash.update(&buffer[..read]);
    }
    let sha256 = format!("{:x}", hash.finalize());
    if sha256 != asset.sha256 {
        return Err(DownloadError::HashMismatch);
    }
    Ok(VerifiedArchive {
        path: archive_path,
        sha256,
    })
}

#[cfg(test)]
mod bundled_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn bundled_archive_with_wrong_sha_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let archive = dir.path().join("wrong.tar.gz");
        std::fs::write(&archive, b"not the locked iDoris archive").unwrap();
        let error = bundled_locked_asset(IdorisLock::embedded(), "aarch64-apple-darwin", &archive)
            .unwrap_err();
        assert!(matches!(error, DownloadError::HashMismatch));
    }
}
