#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, fs, future::ready};
use tempfile::TempDir;

const ASSET_NAME: &str = "component.bin";
const ASSET_URL: &str = "https://github.com/example/component/releases/download/v1/component.bin";
const PAYLOAD: &[u8] = b"locked archive bytes";

struct Body {
    chunks: VecDeque<Vec<u8>>,
    length: Option<u64>,
    cancel: Option<CancellationToken>,
    stall: bool,
    fail: bool,
    inspect_cache: Option<PathBuf>,
}

impl Body {
    fn new(bytes: &[u8]) -> Self {
        Self {
            chunks: VecDeque::from([bytes.to_vec()]),
            length: None,
            cancel: None,
            stall: false,
            fail: false,
            inspect_cache: None,
        }
    }
}

impl Source for Body {
    fn content_length(&self) -> Option<u64> {
        self.length
    }

    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, DownloadError> {
        if let Some(cache) = &self.inspect_cache {
            assert!(path(cache, "part").is_file());
            assert!(!path(cache, "verified").exists());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    fs::metadata(path(cache, "part"))
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o600
                );
                assert_eq!(
                    fs::metadata(cache).unwrap().permissions().mode() & 0o777,
                    0o700
                );
            }
        }
        if let Some(chunk) = self.chunks.pop_front() {
            if let Some(cancel) = self.cancel.take() {
                cancel.cancel();
            }
            return Ok(Some(chunk));
        }
        if self.stall {
            std::future::pending::<()>().await;
        }
        if self.fail {
            return Err(DownloadError::Network("body interrupted"));
        }
        Ok(None)
    }
}

fn expected(payload: &[u8]) -> String {
    format!("{:x}", Sha256::digest(payload))
}
fn path(dir: &Path, suffix: &str) -> PathBuf {
    dir.join(format!("{ASSET_NAME}.{suffix}"))
}

fn assert_clean(dir: &Path) {
    assert!(!path(dir, "part").exists());
    assert!(!path(dir, "verified").exists());
}

async fn run(
    dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
    body: Body,
) -> Result<VerifiedArchive, DownloadError> {
    fetch_with(
        ASSET_URL,
        &expected(PAYLOAD),
        ASSET_NAME,
        dir,
        RELEASE_HOSTS,
        opts,
        cancel,
        |url| {
            assert_eq!(url.as_str(), ASSET_URL);
            ready(Ok(body))
        },
    )
    .await
}

#[tokio::test]
async fn hash_mismatch_deletes_partial_and_never_creates_verified() {
    let dir = TempDir::new().unwrap();
    let result = run(
        dir.path(),
        &DownloadOptions::default(),
        &CancellationToken::new(),
        Body::new(b"corrupt"),
    )
    .await;
    assert!(
        matches!(result, Err(DownloadError::HashMismatch)),
        "{result:?}"
    );
    assert_clean(dir.path());
}

#[tokio::test]
async fn size_limit_applies_to_headers_and_streamed_bytes() {
    let dir = TempDir::new().unwrap();
    let opts = DownloadOptions {
        max_bytes: 4,
        ..Default::default()
    };
    for length in [None, Some(5), Some(1)] {
        let mut body = Body::new(b"123");
        body.chunks.push_back(b"45".to_vec());
        body.length = length;
        let result = run(dir.path(), &opts, &CancellationToken::new(), body).await;
        assert!(matches!(result, Err(DownloadError::TooLarge)), "{result:?}");
        assert_clean(dir.path());
    }
}

#[tokio::test]
async fn cancellation_during_stream_cleans_partial() {
    let dir = TempDir::new().unwrap();
    let cancel = CancellationToken::new();
    let mut body = Body::new(PAYLOAD);
    body.cancel = Some(cancel.clone());
    body.stall = true;
    let result = run(dir.path(), &DownloadOptions::default(), &cancel, body).await;
    assert!(
        matches!(result, Err(DownloadError::Cancelled)),
        "{result:?}"
    );
    assert_clean(dir.path());
}

#[tokio::test]
async fn total_timeout_cleans_partial() {
    let dir = TempDir::new().unwrap();
    let opts = DownloadOptions {
        total_timeout: Duration::from_millis(10),
        ..Default::default()
    };
    let mut body = Body::new(PAYLOAD);
    body.stall = true;
    let result = run(dir.path(), &opts, &CancellationToken::new(), body).await;
    assert!(
        matches!(result, Err(DownloadError::Network("total timeout"))),
        "{result:?}"
    );
    assert_clean(dir.path());
}

#[tokio::test]
async fn interrupted_body_cleans_partial() {
    let dir = TempDir::new().unwrap();
    let mut body = Body::new(PAYLOAD);
    body.fail = true;
    let result = run(
        dir.path(),
        &DownloadOptions::default(),
        &CancellationToken::new(),
        body,
    )
    .await;
    assert!(matches!(
        result,
        Err(DownloadError::Network("body interrupted"))
    ));
    assert_clean(dir.path());
}

#[tokio::test]
async fn cancellation_while_waiting_for_response_cleans_stale_partial() {
    let dir = TempDir::new().unwrap();
    fs::write(path(dir.path(), "part"), b"abandoned").unwrap();
    let cancel = CancellationToken::new();
    let result = fetch_with(
        ASSET_URL,
        &expected(PAYLOAD),
        ASSET_NAME,
        dir.path(),
        RELEASE_HOSTS,
        &DownloadOptions::default(),
        &cancel,
        |_| async {
            cancel.cancel();
            std::future::pending::<Result<Body, DownloadError>>().await
        },
    )
    .await;
    assert!(matches!(result, Err(DownloadError::Cancelled)));
    assert_clean(dir.path());
}

#[tokio::test]
async fn dropped_download_future_cleans_partial() {
    use std::task::Poll;
    let dir = TempDir::new().unwrap();
    let opts = DownloadOptions::default();
    let cancel = CancellationToken::new();
    let mut body = Body::new(PAYLOAD);
    body.stall = true;
    let mut download = Box::pin(run(dir.path(), &opts, &cancel, body));
    std::future::poll_fn(|cx| {
        assert!(download.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert!(path(dir.path(), "part").exists());
    drop(download);
    assert_clean(dir.path());
}

#[test]
fn redirects_require_https_and_exact_allowlisted_hosts() {
    for host in RELEASE_HOSTS {
        validate_url(
            &Url::parse(&format!("https://{host}/asset?signature=secret")).unwrap(),
            RELEASE_HOSTS,
        )
        .unwrap();
    }
    for url in [
        "https://evil.test/asset?secret=do-not-log",
        "https://github.com.evil.test/asset",
        "http://github.com/asset",
        "https://user:secret@github.com/asset",
        "https://github.com:444/asset",
    ] {
        let error = validate_url(&Url::parse(url).unwrap(), RELEASE_HOSTS).unwrap_err();
        assert!(matches!(error, DownloadError::Redirect(_)));
        assert!(!error.to_string().contains("secret"));
        assert!(!format!("{error:?}").contains("secret"));
    }
}

#[tokio::test]
async fn tampered_verified_is_redownloaded_and_stale_partial_is_discarded() {
    let dir = TempDir::new().unwrap();
    let payload = vec![42; 128 * 1024 + 1];
    let mut tampered = payload.clone();
    *tampered.last_mut().unwrap() ^= 1;
    fs::write(path(dir.path(), "verified"), tampered).unwrap();
    fs::write(path(dir.path(), "part"), b"abandoned").unwrap();
    let archive = fetch_with(
        ASSET_URL,
        &expected(&payload),
        ASSET_NAME,
        dir.path(),
        RELEASE_HOSTS,
        &DownloadOptions::default(),
        &CancellationToken::new(),
        |_| {
            assert_clean(dir.path());
            ready(Ok(Body::new(&payload)))
        },
    )
    .await
    .unwrap();
    assert_eq!(fs::read(&archive.path).unwrap(), payload);
    assert_eq!(archive.sha256, expected(&payload));
    assert_eq!(archive.path, path(dir.path(), "verified"));
    assert!(!path(dir.path(), "part").exists());
}

#[tokio::test]
async fn valid_cache_is_rehashed_without_fetching() {
    let dir = TempDir::new().unwrap();
    fs::write(path(dir.path(), "verified"), PAYLOAD).unwrap();
    let archive = fetch_with(
        ASSET_URL,
        &expected(PAYLOAD),
        ASSET_NAME,
        dir.path(),
        RELEASE_HOSTS,
        &DownloadOptions::default(),
        &CancellationToken::new(),
        |_| {
            ready(Err::<Body, _>(DownloadError::Network(
                "cache should avoid network",
            )))
        },
    )
    .await
    .unwrap();
    assert_eq!(fs::read(archive.path).unwrap(), PAYLOAD);
}

#[tokio::test]
async fn precancel_does_not_fetch() {
    let dir = TempDir::new().unwrap();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        run(
            dir.path(),
            &DownloadOptions::default(),
            &cancel,
            Body::new(PAYLOAD)
        )
        .await,
        Err(DownloadError::Cancelled)
    ));
    assert_clean(dir.path());
}

#[cfg(unix)]
#[tokio::test]
async fn cache_and_archives_have_private_nonexecutable_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = TempDir::new().unwrap();
    let cache = dir.path().join("cache");
    let mut body = Body::new(PAYLOAD);
    body.inspect_cache = Some(cache.clone());
    let archive = run(
        &cache,
        &DownloadOptions::default(),
        &CancellationToken::new(),
        body,
    )
    .await
    .unwrap();
    assert_eq!(
        fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&archive.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::set_permissions(&archive.path, fs::Permissions::from_mode(0o777)).unwrap();
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o755)).unwrap();
    run(
        &cache,
        &DownloadOptions::default(),
        &CancellationToken::new(),
        Body::new(PAYLOAD),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(&archive.path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[tokio::test]
async fn caller_policy_and_asset_names_are_checked_before_fetch() {
    let dir = TempDir::new().unwrap();
    for name in ["../escape", "/absolute", "nested/asset", "a\\b", ".", ".."] {
        let result = fetch_with(
            ASSET_URL,
            &expected(PAYLOAD),
            name,
            dir.path(),
            RELEASE_HOSTS,
            &DownloadOptions::default(),
            &CancellationToken::new(),
            |_| ready(Err::<Body, _>(DownloadError::Network("fetch must not run"))),
        )
        .await;
        assert!(
            matches!(result, Err(DownloadError::Io(_))),
            "{name}: {result:?}"
        );
    }
    assert!(validate_url(&Url::parse(ASSET_URL).unwrap(), &["example.com"]).is_err());
    let archive = fetch_with(
        "https://example.com/asset",
        &expected(PAYLOAD),
        ASSET_NAME,
        dir.path(),
        &["example.com"],
        &DownloadOptions::default(),
        &CancellationToken::new(),
        |_| ready(Ok(Body::new(PAYLOAD))),
    )
    .await
    .unwrap();
    assert_eq!(fs::read(archive.path).unwrap(), PAYLOAD);
}
