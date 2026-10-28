//! Download pinned archives without making them executable.

use sha2::{Digest, Sha256};
use std::{
    fs,
    future::Future,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio_util::sync::CancellationToken;
use url::Url;

// The compressed launcher is much smaller; 256 MiB leaves release headroom
// while bounding disk consumption even when Content-Length is absent or false.
pub const MAX_ARCHIVE_BYTES: u64 = 256 * 1024 * 1024;
// GitHub release asset hosts: https://docs.github.com/en/rest/releases/assets
// and the v0.4.0 asset's observed 302 on 2026-10-28:
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
}

impl Default for DownloadOptions {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(15),
            total_timeout: Duration::from_secs(300),
            max_bytes: MAX_ARCHIVE_BYTES,
        }
    }
}

#[derive(Debug)]
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
}

/// Fetch or reverify a locked archive. Callers must serialize operations using
/// the same cache directory, as required by the launcher's installation lock.
#[allow(clippy::too_many_arguments)]
pub async fn fetch_verified(
    url: &str,
    expected_sha256: &str,
    file_name: &str,
    cache_dir: &Path,
    host_allowlist: &[&str],
    limits: &DownloadOptions,
    cancel: &CancellationToken,
) -> Result<VerifiedArchive, DownloadError> {
    fetch_with(
        url,
        expected_sha256,
        file_name,
        cache_dir,
        host_allowlist,
        limits,
        cancel,
        |url| async move {
            let client = reqwest::Client::builder()
                .https_only(true)
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(limits.connect_timeout)
                .build()
                .map_err(|_| DownloadError::Network("client initialization"))?;
            fetch_response(&client, url, host_allowlist).await
        },
    )
    .await
}

async fn fetch_response(
    client: &reqwest::Client,
    mut url: Url,
    host_allowlist: &[&str],
) -> Result<reqwest::Response, DownloadError> {
    for _ in 0..=10 {
        validate_url(&url, host_allowlist)?;
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
            validate_url(&url, host_allowlist)?;
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

#[allow(clippy::too_many_arguments)]
async fn fetch_with<S: Source, F: Future<Output = Result<S, DownloadError>>>(
    url: &str,
    expected_sha256: &str,
    file_name: &str,
    cache_dir: &Path,
    host_allowlist: &[&str],
    opts: &DownloadOptions,
    cancel: &CancellationToken,
    fetch: impl FnOnce(Url) -> F,
) -> Result<VerifiedArchive, DownloadError> {
    if Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(file_name)
        || file_name.contains(['\\', '\0'])
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "invalid asset name").into());
    }
    let url = Url::parse(url).map_err(|_| DownloadError::Network("invalid locked URL"))?;
    validate_url(&url, host_allowlist)?;
    prepare_cache(cache_dir)?;
    let part = PartialArchive(cache_dir.join(format!("{}.part", file_name)));
    // No resume: resumed bytes would also require a complete hash pass. Discard
    // partial files to keep this first implementation simple and correct.
    remove_if_exists(&part.0)?;
    let verified = cache_dir.join(format!("{}.verified", file_name));
    let limit = opts.max_bytes.min(MAX_ARCHIVE_BYTES);
    let operation = async {
        if let Some(sha256) = cached_hash(&verified, limit).await? {
            if hash_matches(&sha256, expected_sha256) {
                return Ok(VerifiedArchive {
                    path: verified,
                    sha256,
                });
            }
            remove_if_exists(&verified)?;
        }
        let mut source = fetch(url).await?;
        if source.content_length().is_some_and(|length| length > limit) {
            return Err(DownloadError::TooLarge);
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&part.0)?;
        private_file(&file)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = source.next_chunk().await? {
            if chunk.len() as u64 > limit - size {
                return Err(DownloadError::TooLarge);
            }
            size += chunk.len() as u64;
            file.write_all(&chunk)?;
            hash.update(&chunk);
            // Yield between bounded file operations so cache hashing and busy
            // streams remain cancellable on a single-threaded runtime too.
            tokio::task::yield_now().await;
        }
        let sha256 = format!("{:x}", hash.finalize());
        if !hash_matches(&sha256, expected_sha256) {
            return Err(DownloadError::HashMismatch);
        }
        if cancel.is_cancelled() {
            return Err(DownloadError::Cancelled);
        }
        file.sync_all()?;
        drop(file);
        fs::rename(&part.0, &verified)?;
        Ok(VerifiedArchive {
            path: verified,
            sha256,
        })
    };
    let result = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(DownloadError::Cancelled),
        result = tokio::time::timeout(opts.total_timeout, operation) => {
            result.unwrap_or(Err(DownloadError::Network("total timeout")))
        }
    };
    remove_if_exists(&part.0)?;
    result
}

fn validate_url(url: &Url, host_allowlist: &[&str]) -> Result<(), DownloadError> {
    let host = url.host_str().unwrap_or("<missing>");
    if url.scheme() == "https"
        && host_allowlist.contains(&host)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
    {
        Ok(())
    } else {
        Err(DownloadError::Redirect(host.to_owned()))
    }
}

fn hash_matches(actual: &str, expected: &str) -> bool {
    bool::from(actual.as_bytes().ct_eq(expected.as_bytes()))
}

fn prepare_cache(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "cache must be a directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn private_file(file: &fs::File) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

async fn cached_hash(path: &Path, limit: u64) -> Result<Option<String>, DownloadError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > limit {
        remove_if_exists(path)?;
        return Ok(None);
    }
    let mut file = fs::File::open(path)?;
    private_file(&file)?;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
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
        tokio::task::yield_now().await;
    }
    Ok(Some(format!("{:x}", hash.finalize())))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(()),
    }
}

struct PartialArchive(PathBuf);

impl Drop for PartialArchive {
    fn drop(&mut self) {
        // Also clean up when the caller drops/aborts the download future.
        let _ = remove_if_exists(&self.0);
    }
}

#[cfg(test)]
mod tests;
