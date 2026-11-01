//! Pinned component installation owned by the daemon, outside comm's no-HTTP boundary.
use agent24_comm::{BinaryError, HyphaeLock, VerifiedBinary, release::HyphaeRelease};
use agent24_components::{DownloadError, DownloadOptions, fetch_verified};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error(transparent)]
    Binary(#[from] BinaryError),
    #[error("component installation I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Callers serialize installs and supply a lock that cannot be overridden in production.
pub async fn install(
    release: &HyphaeRelease,
    lock: &HyphaeLock,
    platform: &str,
    state_dir: &Path,
    cancel: &CancellationToken,
) -> Result<PathBuf, InstallError> {
    let asset = release
        .asset_for(lock, platform)
        .map_err(|_| DownloadError::UnsupportedPlatform)?;
    let installed = release.installed_path(state_dir);
    let install_dir = release.install_dir(state_dir);
    // Reinstall is idempotent, but an existing binary is never trusted by name.
    if tokio::fs::try_exists(&installed).await? {
        VerifiedBinary::install(&installed, asset.expected, &install_dir).await?;
        return Ok(installed);
    }
    let archive = fetch_verified(
        &asset.url,
        &asset.expected.to_hex(),
        &asset.name,
        &state_dir.join("idoris/component-cache/hyphae"),
        &DownloadOptions::default(),
        cancel,
    )
    .await?;
    let verified = VerifiedBinary::install(&archive.path, asset.expected, &install_dir).await?;
    // Publish the stable path atomically only after VerifiedBinary has verified
    // and installed its executable copy. The private hashed copy remains the runner input.
    let staged = install_dir.join("hyphae.installing");
    match tokio::fs::remove_file(&staged).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    tokio::fs::hard_link(verified.path(), &staged).await?;
    tokio::fs::rename(&staged, &installed).await?;
    Ok(installed)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    #[tokio::test]
    #[ignore = "downloads the pinned v0.27.2 darwin-arm64 binary from GitHub"]
    async fn real_github_v0_27_2_darwin_arm64_matches_lock() {
        let dir = tempfile::tempdir().unwrap();
        let release = HyphaeRelease::embedded().unwrap();
        let lock = HyphaeLock::embedded().unwrap();
        let path = install(
            &release,
            &lock,
            "darwin-arm64",
            dir.path(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let bytes = tokio::fs::read(path).await.unwrap();
        let digest = agent24_comm::binary::sha256_of(&bytes);
        assert_eq!(digest, lock.expected_for("darwin-arm64").unwrap());
        eprintln!(
            "verified Hyphae v0.27.2 darwin-arm64: {} bytes, sha256 {}",
            bytes.len(),
            digest.to_hex()
        );
    }
}

#[cfg(test)]
mod route_tests {
    use crate::capabilities::Operation;

    #[test]
    fn install_route_is_host_only_and_excluded_from_creative() {
        let op = crate::capabilities::required_operation(
            &axum::http::Method::POST,
            "/api/v1/comm/install",
        );
        assert_eq!(op, Some(Operation::ComponentInstall));
        let op = Operation::ComponentInstall;
        assert_eq!(
            op.assurance_level(),
            Some(crate::capabilities::AssuranceLevel::Elevated)
        );
        assert!(!Operation::creative_allowlist().contains(&op));
        assert_eq!(Operation::from_action("component.install"), Some(op));
    }
}
