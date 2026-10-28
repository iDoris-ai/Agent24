#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use sha2::{Digest, Sha256};
use std::fs;
use tempfile::TempDir;
const TRIPLE: &str = "aarch64-apple-darwin";
#[tokio::test]
async fn unsupported_platform_does_not_fetch() {
    let dir = TempDir::new().unwrap();
    assert!(matches!(
        download_locked_asset(
            IdorisLock::embedded(),
            "x86_64-unknown-linux-gnu",
            dir.path(),
            &DownloadOptions::default(),
            &CancellationToken::new()
        )
        .await,
        Err(DownloadError::UnsupportedPlatform)
    ));
}
#[tokio::test]
#[ignore = "downloads the pinned v0.4.0 archive from GitHub"]
async fn real_github_v0_4_0_matches_embedded_sha256() {
    let dir = TempDir::new().unwrap();
    let archive = download_locked_asset(
        IdorisLock::embedded(),
        TRIPLE,
        dir.path(),
        &DownloadOptions::default(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    let bytes = fs::read(&archive.path).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(&bytes)), archive.sha256);
    assert_eq!(
        archive.sha256,
        IdorisLock::embedded().asset_for(TRIPLE).unwrap().sha256
    );
    eprintln!(
        "verified v0.4.0: {} bytes, sha256 {}",
        bytes.len(),
        archive.sha256
    );
}
