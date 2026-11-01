#![allow(clippy::unwrap_used, clippy::expect_used)]
use super::*;
use sha2::{Digest, Sha256};
use std::{fs, path::PathBuf};

const URL: &str = "https://github.com/example/component/releases/download/v1/component.bin";
const NAME: &str = "component.bin";
const PAYLOAD: &[u8] = b"locked component";

fn cache(root: &Path) -> PathBuf {
    let path = root.join("idoris/component-cache/hyphae");
    fs::create_dir_all(&path).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for dir in [
            root.join("idoris"),
            root.join("idoris/component-cache"),
            path.clone(),
        ] {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700)).unwrap();
        }
    }
    fs::write(path.join(format!("{NAME}.verified")), PAYLOAD).unwrap();
    path
}

async fn fetch(
    url: &str,
    name: &str,
    cache: &Path,
    options: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    fetch_verified(
        url,
        &format!("{:x}", Sha256::digest(PAYLOAD)),
        name,
        cache,
        options,
        cancel,
    )
    .await
}

#[tokio::test]
async fn reuses_verified_cache_with_original_progress_types() {
    let root = tempfile::tempdir().unwrap();
    let cache = cache(root.path());
    let stale = cache.join(format!("{NAME}.part"));
    fs::write(&stale, b"stale").unwrap();
    let (sender, _receiver) = tokio::sync::watch::channel(DownloadProgress::default());
    let options = DownloadOptions {
        progress: Some(sender),
        ..Default::default()
    };
    let result = fetch(URL, NAME, &cache, &options, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(fs::read(&result.path).unwrap(), PAYLOAD);
    assert_eq!(result.sha256, format!("{:x}", Sha256::digest(PAYLOAD)));
    assert!(!stale.exists());
}

#[tokio::test]
async fn forwarding_cannot_widen_main_host_policy() {
    let root = tempfile::tempdir().unwrap();
    let cache = cache(root.path());
    for url in [
        URL.replace("github.com", "evil.invalid"),
        URL.replace("https:", "http:"),
        URL.replace("github.com", "github.com:8443"),
        URL.replace("github.com", "user:password@github.com"),
    ] {
        assert!(matches!(
            fetch(
                &url,
                NAME,
                &cache,
                &DownloadOptions::default(),
                &CancellationToken::new()
            )
            .await,
            Err(DownloadError::Redirect(_))
        ));
    }
}

#[tokio::test]
async fn forwarding_rejects_mismatched_url_and_cache_name() {
    let root = tempfile::tempdir().unwrap();
    assert!(matches!(
        fetch(
            URL,
            "different.bin",
            root.path(),
            &DownloadOptions::default(),
            &CancellationToken::new()
        )
        .await,
        Err(DownloadError::Network(_))
    ));
    assert!(!root.path().join("different.bin.verified").exists());
}

#[tokio::test]
async fn forwarding_preserves_precancellation() {
    let root = tempfile::tempdir().unwrap();
    let cache = cache(root.path());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        fetch(URL, NAME, &cache, &DownloadOptions::default(), &cancel).await,
        Err(DownloadError::Cancelled)
    ));
}

#[cfg(unix)]
#[tokio::test]
async fn forwarding_rejects_symlinked_managed_cache() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let cache = cache(root.path());
    fs::remove_dir_all(&cache).unwrap();
    std::os::unix::fs::symlink(outside.path(), &cache).unwrap();
    assert!(matches!(
        fetch(
            URL,
            NAME,
            &cache,
            &DownloadOptions::default(),
            &CancellationToken::new()
        )
        .await,
        Err(DownloadError::Io(_))
    ));
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
}
