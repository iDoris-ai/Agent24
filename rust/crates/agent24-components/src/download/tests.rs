#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use sha2::{Digest, Sha256};
use std::{collections::VecDeque, fs, future::ready};
use tempfile::TempDir;

struct CacheDir {
    _temp: TempDir,
    path: PathBuf,
}
impl CacheDir {
    fn new() -> Self {
        use std::os::unix::fs::DirBuilderExt;
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("components").join("demo");
        fs::create_dir_all(&root).unwrap();
        fs::set_permissions(&root, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
        let path = root.join("cache");
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self { _temp: temp, path }
    }
    fn path(&self) -> &Path {
        &self.path
    }
}

const TRIPLE: &str = "darwin-arm64";
const PAYLOAD: &[u8] = b"locked archive bytes";

#[tokio::test(start_paused = true)]
async fn progress_is_monotonic_and_throttled_to_four_updates_per_second() {
    let (sender, mut receiver) = tokio::sync::watch::channel(DownloadProgress::default());
    let mut reporter = ProgressReporter {
        sender: Some(&sender),
        last: None,
    };
    let mut observed = Vec::new();
    for bytes in 1..=100 {
        reporter.report(DownloadProgress {
            bytes_done: bytes,
            bytes_total: None,
        });
        if receiver.has_changed().unwrap() {
            observed.push((tokio::time::Instant::now(), *receiver.borrow_and_update()));
        }
        tokio::time::advance(Duration::from_millis(10)).await;
    }
    assert_eq!(observed.len(), 4);
    for pair in observed.windows(2) {
        assert!(pair[1].0 - pair[0].0 >= Duration::from_millis(250));
        assert!(pair[1].1.bytes_done > pair[0].1.bytes_done);
        assert_eq!(pair[1].1.bytes_total, None);
    }
    reporter
        .finish(DownloadProgress {
            bytes_done: 100,
            bytes_total: None,
        })
        .await;
    assert_eq!(receiver.borrow().bytes_done, 100);
}

#[tokio::test]
async fn streamed_progress_preserves_unknown_and_known_totals() {
    for length in [None, Some(PAYLOAD.len() as u64)] {
        let dir = CacheDir::new();
        let (sender, receiver) = tokio::sync::watch::channel(DownloadProgress::default());
        let opts = DownloadOptions {
            progress: Some(sender),
            ..DownloadOptions::default()
        };
        let mut body = Body::new(PAYLOAD);
        body.length = length;
        body.chunks = PAYLOAD.chunks(3).map(Vec::from).collect();
        run(dir.path(), &opts, &CancellationToken::new(), body)
            .await
            .unwrap();
        assert_eq!(
            *receiver.borrow(),
            DownloadProgress {
                bytes_done: PAYLOAD.len() as u64,
                bytes_total: length
            }
        );
    }
}

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

fn lock() -> ComponentLock {
    lock_for(PAYLOAD)
}

fn lock_for(payload: &[u8]) -> ComponentLock {
    let mut value: serde_json::Value = serde_json::from_str(crate::lock::TEST_LOCK).unwrap();
    value["assets"][TRIPLE]["sha256"] = format!("{:x}", Sha256::digest(payload)).into();
    ComponentLock::parse(&value.to_string()).unwrap()
}

fn pinned(lock: &ComponentLock, platform: &str) -> PinnedAsset {
    PinnedAsset::from_lock(lock, platform).unwrap()
}

fn path(dir: &Path, suffix: &str) -> PathBuf {
    dir.join(format!(
        "{}.{}",
        lock().asset_for(TRIPLE).unwrap().name,
        suffix
    ))
}

async fn assert_clean(dir: &Path) {
    // A cancelled blocking syscall may still be finishing; its guard owns both
    // cleanup and the lock until the last disk operation has returned.
    tokio::time::timeout(Duration::from_secs(5), async {
        while path(dir, "part").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert!(!path(dir, "part").exists());
    assert!(!path(dir, "verified").exists());
}

async fn run(
    dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
    body: Body,
) -> Result<VerifiedArchive, DownloadError> {
    let lock = lock();
    download_with(&pinned(&lock, TRIPLE), dir, opts, cancel, |url| {
        assert_eq!(
            url.as_str(),
            lock.asset_url(lock.asset_for(TRIPLE).unwrap())
        );
        ready(Ok(body))
    })
    .await
}

#[tokio::test]
async fn hash_mismatch_deletes_partial_and_never_creates_verified() {
    let dir = CacheDir::new();
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
    assert_clean(dir.path()).await;
}

#[tokio::test]
async fn size_limit_applies_to_headers_and_streamed_bytes() {
    let dir = CacheDir::new();
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
        assert_clean(dir.path()).await;
    }
}

#[tokio::test]
async fn cancellation_during_stream_cleans_partial() {
    let dir = CacheDir::new();
    let cancel = CancellationToken::new();
    let mut body = Body::new(PAYLOAD);
    body.cancel = Some(cancel.clone());
    body.stall = true;
    let result = run(dir.path(), &DownloadOptions::default(), &cancel, body).await;
    assert!(
        matches!(result, Err(DownloadError::Cancelled)),
        "{result:?}"
    );
    assert_clean(dir.path()).await;
}

#[tokio::test]
async fn total_timeout_cleans_partial() {
    let dir = CacheDir::new();
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
    assert_clean(dir.path()).await;
}

#[tokio::test]
async fn interrupted_body_cleans_partial() {
    let dir = CacheDir::new();
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
    assert_clean(dir.path()).await;
}

#[tokio::test]
async fn cancellation_while_waiting_for_response_cleans_stale_partial() {
    let dir = CacheDir::new();
    fs::write(path(dir.path(), "part"), b"abandoned").unwrap();
    let cancel = CancellationToken::new();
    let result = download_with(
        &pinned(&lock(), TRIPLE),
        dir.path(),
        &DownloadOptions::default(),
        &cancel,
        |_| async {
            cancel.cancel();
            std::future::pending::<Result<Body, DownloadError>>().await
        },
    )
    .await;
    assert!(matches!(result, Err(DownloadError::Cancelled)));
    assert_clean(dir.path()).await;
}

#[tokio::test]
async fn dropped_download_future_cleans_partial() {
    use std::task::Poll;
    let dir = CacheDir::new();
    let opts = DownloadOptions::default();
    let cancel = CancellationToken::new();
    let mut body = Body::new(PAYLOAD);
    body.stall = true;
    let mut download = Box::pin(run(dir.path(), &opts, &cancel, body));
    tokio::time::timeout(
        Duration::from_secs(5),
        std::future::poll_fn(|cx| {
            assert!(download.as_mut().poll(cx).is_pending());
            if path(dir.path(), "part").exists() {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        }),
    )
    .await
    .unwrap();
    drop(download);
    tokio::time::timeout(Duration::from_secs(5), async {
        while path(dir.path(), "part").exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_clean(dir.path()).await;
}

#[test]
fn redirects_require_https_and_exact_allowlisted_hosts() {
    for host in RELEASE_HOSTS {
        validate_url(&Url::parse(&format!("https://{host}/asset?signature=secret")).unwrap())
            .unwrap();
    }
    for url in [
        "https://evil.test/asset?secret=do-not-log",
        "https://github.com.evil.test/asset",
        "http://github.com/asset",
        "https://user:secret@github.com/asset",
        "https://github.com:444/asset",
    ] {
        let error = validate_url(&Url::parse(url).unwrap()).unwrap_err();
        assert!(matches!(error, DownloadError::Redirect(_)));
        assert!(!error.to_string().contains("secret"));
        assert!(!format!("{error:?}").contains("secret"));
    }
}

#[tokio::test]
async fn tampered_verified_is_redownloaded_and_stale_partial_is_discarded() {
    let dir = CacheDir::new();
    let payload = vec![42; 128 * 1024 + 1];
    let mut tampered = payload.clone();
    *tampered.last_mut().unwrap() ^= 1;
    fs::write(path(dir.path(), "verified"), tampered).unwrap();
    fs::write(path(dir.path(), "part"), b"abandoned").unwrap();
    let lock = lock_for(&payload);
    let archive = download_with(
        &pinned(&lock, TRIPLE),
        dir.path(),
        &DownloadOptions::default(),
        &CancellationToken::new(),
        |_| {
            assert!(!path(dir.path(), "part").exists());
            assert!(!path(dir.path(), "verified").exists());
            ready(Ok(Body::new(&payload)))
        },
    )
    .await
    .unwrap();
    assert_eq!(fs::read(&archive.path).unwrap(), payload);
    assert_eq!(archive.sha256, lock.asset_for(TRIPLE).unwrap().sha256);
    assert_eq!(archive.path, path(dir.path(), "verified"));
    assert!(!path(dir.path(), "part").exists());
}

#[tokio::test]
async fn valid_cache_is_rehashed_without_fetching() {
    let dir = CacheDir::new();
    fs::write(path(dir.path(), "verified"), PAYLOAD).unwrap();
    let archive = download_with(
        &pinned(&lock(), TRIPLE),
        dir.path(),
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
async fn unsupported_platform_and_precancel_do_not_fetch() {
    let dir = CacheDir::new();
    let cancel = CancellationToken::new();
    fn assert_send<T: Send>(_: T) {}
    assert_send(download_locked_asset(
        &lock(),
        TRIPLE,
        dir.path(),
        &DownloadOptions::default(),
        &cancel,
    ));
    assert!(matches!(
        download_locked_asset(
            &lock(),
            "linux-x64",
            dir.path(),
            &DownloadOptions::default(),
            &cancel
        )
        .await,
        Err(DownloadError::UnsupportedPlatform)
    ));
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
    assert_clean(dir.path()).await;
}

#[cfg(unix)]
#[tokio::test]
async fn cache_and_archives_have_private_nonexecutable_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = CacheDir::new();
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
    assert!(
        run(
            &cache,
            &DownloadOptions::default(),
            &CancellationToken::new(),
            Body::new(PAYLOAD)
        )
        .await
        .is_err()
    );
    fs::set_permissions(&cache, fs::Permissions::from_mode(0o700)).unwrap();
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

#[cfg(unix)]
#[tokio::test]
async fn cached_symlink_never_changes_target_permissions() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = CacheDir::new();
    let target = dir.path().join("target");
    fs::write(&target, PAYLOAD).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let link = dir.path().join("cached");
    symlink(&target, &link).unwrap();
    assert!(
        cached_hash(&link, MAX_ARCHIVE_BYTES, &CancellationToken::new())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[tokio::test]
async fn cache_outside_managed_tree_is_rejected() {
    let dir = TempDir::new().unwrap();
    assert!(
        run(
            dir.path(),
            &DownloadOptions::default(),
            &CancellationToken::new(),
            Body::new(PAYLOAD)
        )
        .await
        .is_err()
    );
}

struct RedirectServer {
    origin: Url,
    requests: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl RedirectServer {
    fn new() -> Self {
        use std::{
            net::TcpListener,
            sync::{
                Arc, Mutex,
                atomic::{AtomicBool, Ordering},
            },
        };
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("offline redirect server must listen");
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let origin = Url::parse(&format!("http://{address}/")).unwrap();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let (seen, stopped) = (requests.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(e) => panic!("accept: {e}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                stream
                    .set_write_timeout(Some(Duration::from_secs(1)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0; 1024];
                while request.len() < 8192 && !request.ends_with(b"\r\n\r\n") {
                    match stream.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => request.extend_from_slice(&buffer[..n]),
                    }
                }
                let request = String::from_utf8_lossy(&request);
                let path = request.split_whitespace().nth(1).unwrap_or("");
                seen.lock().unwrap().push(path.to_owned());
                let location = match path {
                    "/off-host" => Some(format!("https://localhost:{}/final", address.port())),
                    "/http" => Some(format!("http://github.com:{}/final", address.port())),
                    "/relative" => Some("final".to_owned()),
                    _ => path
                        .strip_prefix("/hops/")
                        .and_then(|n| n.parse::<u32>().ok())
                        .filter(|n| *n < 11)
                        .map(|n| format!("/hops/{}", n + 1)),
                };
                let response = if let Some(location) = location {
                    format!(
                        "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok".to_owned()
                };
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            origin,
            requests,
            stop,
            thread: Some(thread),
        }
    }
    async fn fetch(&self, path: &str) -> Result<reqwest::Response, DownloadError> {
        let policy = FetchPolicy {
            http_origin: Some(self.origin.clone()),
        };
        let client = fetch_client(&DownloadOptions::default(), &policy).unwrap();
        tokio::time::timeout(
            Duration::from_secs(5),
            fetch_response(&client, self.origin.join(path).unwrap(), &policy),
        )
        .await
        .unwrap()
    }
}
impl Drop for RedirectServer {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        self.thread.take().unwrap().join().unwrap();
    }
}

#[tokio::test]
async fn fetch_response_rejects_redirect_to_nonallowlisted_host() {
    let server = RedirectServer::new();
    assert!(
        matches!(server.fetch("off-host").await, Err(DownloadError::Redirect(host)) if host == "localhost")
    );
    assert_eq!(*server.requests.lock().unwrap(), ["/off-host"]);
}

#[tokio::test]
async fn fetch_response_rejects_non_https_redirect_to_allowlisted_host() {
    let server = RedirectServer::new();
    assert!(
        matches!(server.fetch("http").await, Err(DownloadError::Redirect(host)) if host == "github.com")
    );
    assert_eq!(*server.requests.lock().unwrap(), ["/http"]);
}

#[tokio::test]
async fn fetch_response_follows_relative_redirect() {
    let server = RedirectServer::new();
    let response = server.fetch("relative").await.unwrap();
    assert_eq!(response.bytes().await.unwrap(), b"ok".as_slice());
    assert_eq!(*server.requests.lock().unwrap(), ["/relative", "/final"]);
}

#[tokio::test]
async fn fetch_response_rejects_more_than_ten_redirects() {
    let server = RedirectServer::new();
    assert!(matches!(
        server.fetch("hops/0").await,
        Err(DownloadError::Network("redirect limit exceeded"))
    ));
    assert_eq!(server.requests.lock().unwrap().len(), 11);
}

#[tokio::test]
async fn production_client_rejects_plain_http_before_network_io() {
    let client = fetch_client(&DownloadOptions::default(), &FetchPolicy::default()).unwrap();
    assert!(
        client
            .get("http://github.com/asset")
            .send()
            .await
            .unwrap_err()
            .is_builder()
    );
}

#[tokio::test]
async fn download_refuses_a_launcher_operation_already_in_progress() {
    let dir = CacheDir::new();
    let held = operation_lock(dir.path().parent().unwrap()).unwrap();
    let result = download_with(
        &pinned(&lock(), TRIPLE),
        dir.path(),
        &DownloadOptions::default(),
        &CancellationToken::new(),
        |_| {
            panic!("a busy download must not fetch");
            #[allow(unreachable_code)]
            ready(Ok(Body::new(PAYLOAD)))
        },
    )
    .await;
    assert!(matches!(result, Err(DownloadError::Busy)));
    drop(held);
    run(
        dir.path(),
        &DownloadOptions::default(),
        &CancellationToken::new(),
        Body::new(PAYLOAD),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn download_holds_launcher_lock_until_fetch_and_writes_finish() {
    use crate::install::{InstallError, InstallOptions, install_locked_asset};
    use crate::sign::{SignatureVerifier, SigningRequirement};
    struct NeverVerify;
    impl SignatureVerifier for NeverVerify {
        fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
            panic!("busy installer must not verify")
        }
    }
    let dir = CacheDir::new();
    let lock = lock();
    let opts = DownloadOptions::default();
    let cancel = CancellationToken::new();
    download_with(
        &pinned(&lock, TRIPLE),
        dir.path(),
        &opts,
        &cancel,
        |_| async {
            let root = dir.path().parent().unwrap();
            let archive = VerifiedArchive {
                path: path(dir.path(), "verified"),
                sha256: lock.assets[TRIPLE].sha256.clone(),
            };
            assert!(matches!(
                install_locked_asset(
                    &lock,
                    TRIPLE,
                    &archive,
                    &InstallOptions::new(root, "/usr/bin/false"),
                    NeverVerify,
                    &cancel
                )
                .await,
                Err(InstallError::Busy)
            ));
            Ok(Body::new(PAYLOAD))
        },
    )
    .await
    .unwrap();
    assert!(operation_lock(dir.path().parent().unwrap()).is_ok());
}

#[tokio::test]
async fn cache_rejects_symlinked_managed_directories() {
    use std::os::unix::fs::symlink;
    let dir = CacheDir::new();
    let outside = TempDir::new().unwrap();
    symlink(outside.path(), dir.path().join("link")).unwrap();
    assert!(
        run(
            &dir.path().join("link/cache"),
            &DownloadOptions::default(),
            &CancellationToken::new(),
            Body::new(PAYLOAD)
        )
        .await
        .is_err()
    );
    fs::create_dir(outside.path().join("components")).unwrap();
    let alias = outside.path().join("components").join("demo");
    symlink(dir.path().parent().unwrap(), &alias).unwrap();
    assert!(
        run(
            &alias.join("cache"),
            &DownloadOptions::default(),
            &CancellationToken::new(),
            Body::new(PAYLOAD)
        )
        .await
        .is_err()
    );
    assert!(!outside.path().join("cache").exists());
}

#[tokio::test]
async fn cache_rejects_nonprivate_managed_root() {
    use std::os::unix::fs::PermissionsExt;
    let dir = CacheDir::new();
    fs::set_permissions(
        dir.path().parent().unwrap(),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(
        run(
            dir.path(),
            &DownloadOptions::default(),
            &CancellationToken::new(),
            Body::new(PAYLOAD)
        )
        .await
        .is_err()
    );
}

#[test]
fn cached_hardlink_never_changes_target_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = CacheDir::new();
    let target = dir.path().join("target");
    fs::write(&target, PAYLOAD).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    let alias = dir.path().join("alias");
    fs::hard_link(&target, &alias).unwrap();
    assert!(
        cached_hash(&alias, MAX_ARCHIVE_BYTES, &CancellationToken::new())
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fs::metadata(target).unwrap().permissions().mode() & 0o777,
        0o644
    );
}

#[tokio::test]
async fn fetch_response_follows_exactly_ten_redirects() {
    let server = RedirectServer::new();
    assert_eq!(
        server.fetch("hops/1").await.unwrap().bytes().await.unwrap(),
        b"ok".as_slice()
    );
    assert_eq!(server.requests.lock().unwrap().len(), 11);
}
