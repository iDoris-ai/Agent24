//! Download pinned archives without making them executable.

use crate::{
    install::{OperationLock, operation_lock},
    lock::ComponentLock,
    storage::{open_regular, prepare_cache},
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
use url::Url;

// The compressed launcher is much smaller; 256 MiB leaves release headroom
// while bounding disk consumption even when Content-Length is absent or false.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
// GitHub release asset hosts: https://docs.github.com/en/rest/releases/assets
// and the iDoris v0.4.0 asset's observed 302 on 2026-10-28:
// github.com/component-ai/component/releases/download/v0.4.0/
// idoris-v0.4.0-aarch64-apple-darwin.tar.gz -> release-assets.githubusercontent.com.
// objects.githubusercontent.com is GitHub's other release object host.
pub const RELEASE_HOSTS: &[&str] = &[
    "github.com",
    "objects.githubusercontent.com",
    "release-assets.githubusercontent.com",
];

#[derive(Debug, Clone)]
pub struct DownloadOptions {
    pub connect_timeout: Duration,
    pub total_timeout: Duration,
    /// May lower the limit, but cannot exceed MAX_ARCHIVE_BYTES.
    pub max_bytes: u64,
    pub progress: Option<tokio::sync::watch::Sender<DownloadProgress>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DownloadProgress {
    pub bytes_done: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_total: Option<u64>,
}

struct ProgressReporter<'a> {
    sender: Option<&'a tokio::sync::watch::Sender<DownloadProgress>>,
    last: Option<tokio::time::Instant>,
}
impl ProgressReporter<'_> {
    fn report(&mut self, progress: DownloadProgress) {
        if let Some(sender) = self.sender
            && self
                .last
                .is_none_or(|last| last.elapsed() >= Duration::from_millis(250))
        {
            sender.send_replace(progress);
            self.last = Some(tokio::time::Instant::now());
        }
    }
    async fn finish(&mut self, progress: DownloadProgress) {
        if let Some(sender) = self.sender
            && *sender.borrow() != progress
        {
            if let Some(last) = self.last {
                tokio::time::sleep_until(last + Duration::from_millis(250)).await;
            }
            self.report(progress);
        }
    }
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(300),
            max_bytes: MAX_ARCHIVE_BYTES,
            progress: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct VerifiedArchive {
    pub path: PathBuf,
    pub sha256: String,
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("component download failed: {0}")]
    Network(&'static str),
    #[error("component download destination rejected: {0}")]
    Redirect(String),
    #[error("component archive exceeds the size limit")]
    TooLarge,
    #[error("component archive does not match the locked SHA-256")]
    HashMismatch,
    #[error("component download cancelled")]
    Cancelled,
    #[error("component cache I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("component is unavailable for this platform")]
    UnsupportedPlatform,
    #[error("component installation is already in progress")]
    Busy,
}

/// One pinned release file: where it lives, its file name and the digest it must have.
#[derive(Debug, Clone)]
pub struct PinnedAsset {
    pub url: String,
    pub name: String,
    pub sha256: String,
}

impl PinnedAsset {
    pub fn from_lock(lock: &ComponentLock, platform: &str) -> Result<Self, DownloadError> {
        let asset = lock
            .asset_for(platform)
            .ok_or(DownloadError::UnsupportedPlatform)?;
        Ok(Self {
            url: lock.asset_url(asset),
            name: asset.name.clone(),
            sha256: asset.sha256.clone(),
        })
    }
}

/// Fetch or reverify an archive while holding the component's operation lock.
pub async fn download_locked_asset(
    lock: &ComponentLock,
    platform: &str,
    cache_dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    download_pinned(
        &PinnedAsset::from_lock(lock, platform)?,
        cache_dir,
        opts,
        cancel,
    )
    .await
}

/// The generic downloader: GitHub host allow-list, size cap, SHA-256, cancel,
/// progress and one operation lock per component. Callers without a
/// `ComponentLock` (Hyphae, schema 1) come in through `fetch_verified`.
pub async fn download_pinned(
    asset: &PinnedAsset,
    cache_dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    download_with(asset, cache_dir, opts, cancel, |url| async move {
        let policy = FetchPolicy::default();
        let client = fetch_client(opts, &policy)?;
        fetch_response(&client, url, &policy).await
    })
    .await
}

#[derive(Default)]
struct FetchPolicy {
    // The HTTP exception exists only in unit tests and is restricted to their
    // exact local origin. Redirects to any other HTTP origin remain forbidden.
    #[cfg(test)]
    http_origin: Option<Url>,
}
impl FetchPolicy {
    fn validate(&self, url: &Url) -> Result<(), DownloadError> {
        #[cfg(test)]
        if self
            .http_origin
            .as_ref()
            .is_some_and(|origin| origin.origin() == url.origin())
            && url.username().is_empty()
            && url.password().is_none()
        {
            return Ok(());
        }
        validate_url(url)
    }
}
fn fetch_client(
    opts: &DownloadOptions,
    policy: &FetchPolicy,
) -> Result<reqwest::Client, DownloadError> {
    let https_only = true;
    #[cfg(test)]
    let https_only = https_only && policy.http_origin.is_none();
    let _ = policy;
    let builder = reqwest::Client::builder();
    #[cfg(test)]
    let builder = if let Some(origin) = &policy.http_origin {
        let address = std::net::SocketAddr::from((
            [127, 0, 0, 1],
            origin
                .port()
                .ok_or(DownloadError::Network("missing local test port"))?,
        ));
        builder
            .no_proxy()
            .resolve("github.com", address)
            .resolve("localhost", address)
    } else {
        builder
    };
    builder
        .https_only(https_only)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(opts.connect_timeout)
        .build()
        .map_err(|_| DownloadError::Network("client initialization"))
}

async fn fetch_response(
    client: &reqwest::Client,
    mut url: Url,
    policy: &FetchPolicy,
) -> Result<reqwest::Response, DownloadError> {
    for _ in 0..=10 {
        policy.validate(&url)?;
        // Never retain reqwest errors: their Display and Debug may contain
        // signed redirect queries. Only static network diagnostics escape.
        let response = client
            .get(url.clone())
            .send()
            .await
            .map_err(|_| DownloadError::Network("request failed"))?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(DownloadError::Network("missing redirect location"))?;
            url = url
                .join(location)
                .map_err(|_| DownloadError::Network("invalid redirect location"))?;
            policy.validate(&url)?;
        } else if response.status().is_success() {
            return Ok(response);
        } else {
            return Err(DownloadError::Network("unsuccessful HTTP status"));
        }
    }
    Err(DownloadError::Network("redirect limit exceeded"))
}

trait Source {
    fn content_length(&self) -> Option<u64>;
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, DownloadError>;
}

impl Source for reqwest::Response {
    fn content_length(&self) -> Option<u64> {
        self.content_length()
    }
    async fn next_chunk(&mut self) -> Result<Option<Vec<u8>>, DownloadError> {
        self.chunk()
            .await
            .map(|chunk| chunk.map(|bytes| bytes.to_vec()))
            .map_err(|_| DownloadError::Network("body interrupted"))
    }
}

async fn download_with<S: Source, F: Future<Output = Result<S, DownloadError>>>(
    asset: &PinnedAsset,
    cache_dir: &Path,
    opts: &DownloadOptions,
    cancel: &CancellationToken,
    fetch: impl FnOnce(Url) -> F,
) -> Result<VerifiedArchive, DownloadError> {
    // Lock fields are public; never let a malformed asset name escape the cache.
    if Path::new(&asset.name)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(&asset.name)
        || asset.name.contains(['\\', '\0'])
    {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "invalid locked asset name").into(),
        );
    }
    let url = Url::parse(&asset.url).map_err(|_| DownloadError::Network("invalid locked URL"))?;
    validate_url(&url)?;
    let cache_dir = cache_dir.to_path_buf();
    let name = asset.name.clone();
    let state = blocking(move || {
        let root = prepare_cache(&cache_dir)?;
        let lock = operation_lock(&root).map_err(|e| match e {
            crate::install::InstallError::Busy => DownloadError::Busy,
            _ => io::Error::other(e).into(),
        })?;
        let part = cache_dir.join(format!("{name}.part"));
        remove_if_exists(&part)?;
        Ok(Arc::new(PartialArchive {
            path: part,
            _lock: lock,
        }))
    })
    .await?;
    let verified = state.path.with_extension("verified");
    let limit = opts.max_bytes.min(MAX_ARCHIVE_BYTES);
    let worker_cancel = cancel.child_token();
    let _cancel_on_drop = worker_cancel.clone().drop_guard();
    let operation = async {
        let cached_path = verified.clone();
        let token = worker_cancel.clone();
        if let Some(sha256) = disk(state.clone(), move |_| {
            cached_hash(&cached_path, limit, &token)
        })
        .await?
        {
            if hash_matches(&sha256, &asset.sha256) {
                return Ok(VerifiedArchive {
                    path: verified,
                    sha256,
                });
            }
            let path = verified.clone();
            disk(state.clone(), move |_| Ok(remove_if_exists(&path)?)).await?;
        }
        let mut source = fetch(url).await?;
        if source.content_length().is_some_and(|length| length > limit) {
            return Err(DownloadError::TooLarge);
        }
        let mut file = disk(state.clone(), |state| {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let file = options.open(&state.path)?;
            private_file(&file)?;
            Ok(file)
        })
        .await?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let total = source.content_length();
        let mut progress = ProgressReporter {
            sender: opts.progress.as_ref(),
            last: None,
        };
        while let Some(chunk) = source.next_chunk().await? {
            if chunk.len() as u64 > limit - size {
                return Err(DownloadError::TooLarge);
            }
            size += chunk.len() as u64;
            let token = worker_cancel.clone();
            (file, hash) = disk(state.clone(), move |_| {
                check_cancel(&token)?;
                file.write_all(&chunk)?;
                hash.update(&chunk);
                Ok((file, hash))
            })
            .await?;
            progress.report(DownloadProgress {
                bytes_done: size,
                bytes_total: total.filter(|t| *t >= size),
            });
        }
        progress
            .finish(DownloadProgress {
                bytes_done: size,
                bytes_total: total.filter(|t| *t >= size),
            })
            .await;
        let sha256 = format!("{:x}", hash.finalize());
        if !hash_matches(&sha256, &asset.sha256) {
            return Err(DownloadError::HashMismatch);
        }
        let token = worker_cancel.clone();
        disk(state.clone(), move |state| {
            check_cancel(&token)?;
            file.sync_all()?;
            drop(file);
            check_cancel(&token)?;
            fs::rename(&state.path, &verified)?;
            Ok(VerifiedArchive {
                path: verified,
                sha256,
            })
        })
        .await
    };
    tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(DownloadError::Cancelled),
        result = tokio::time::timeout(opts.total_timeout, operation) => {
            result.unwrap_or(Err(DownloadError::Network("total timeout")))
        }
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), DownloadError> {
    if cancel.is_cancelled() {
        Err(DownloadError::Cancelled)
    } else {
        Ok(())
    }
}
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DownloadError> + Send + 'static,
) -> Result<T, DownloadError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| io::Error::other(e.to_string()))?
}
async fn disk<T: Send + 'static>(
    state: Arc<PartialArchive>,
    work: impl FnOnce(&PartialArchive) -> Result<T, DownloadError> + Send + 'static,
) -> Result<T, DownloadError> {
    // An aborted future cannot release the lock or remove the partial file
    // until its in-flight disk operation has finished.
    blocking(move || work(&state)).await
}

fn validate_url(url: &Url) -> Result<(), DownloadError> {
    let host = url.host_str().unwrap_or("<missing>");
    if url.scheme() == "https"
        && RELEASE_HOSTS.contains(&host)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
    {
        Ok(())
    } else {
        Err(DownloadError::Redirect(host.to_owned()))
    }
}

pub(crate) fn hash_matches(actual: &str, expected: &str) -> bool {
    bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
}

fn private_file(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn cached_hash(
    path: &Path,
    limit: u64,
    cancel: &CancellationToken,
) -> Result<Option<String>, DownloadError> {
    let mut file = match open_regular(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e)
            if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
                || e.kind() == io::ErrorKind::InvalidInput =>
        {
            remove_if_exists(path)?;
            return Ok(None);
        }
        Err(e) => return Err(e.into()),
    };
    if file.metadata()?.len() > limit {
        remove_if_exists(path)?;
        return Ok(None);
    }
    private_file(&file)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            remove_if_exists(path)?;
            return Ok(None);
        }
        hash.update(&buffer[..count]);
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

struct PartialArchive {
    path: PathBuf,
    _lock: OperationLock,
}

impl Drop for PartialArchive {
    fn drop(&mut self) {
        // Also clean up when the caller drops/aborts the download future.
        let _ = remove_if_exists(&self.path);
    }
}

#[cfg(test)]
mod tests;
