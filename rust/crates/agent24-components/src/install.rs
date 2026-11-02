//! Install immutable component releases using a dedicated writer process.

use crate::{
    download::{MAX_ARCHIVE_BYTES, VerifiedArchive, hash_matches},
    lock::{Asset, ComponentLock, clean_relative},
    sign::{SignatureVerifier, SigningRequirement},
    storage::{open_regular, private_directory},
};
use flate2::read::MultiGzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const MAX_UNPACKED_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("component installation is already in progress")]
    Busy,
    #[error("component installation cancelled")]
    Cancelled,
    #[error("insufficient installation space: need {required}, available {available}")]
    InsufficientSpace { required: u64, available: u64 },
    #[error("installation space is unknown: {0}")]
    SpaceUnknown(io::Error),
    #[error("invalid release manifest: {0}")]
    ManifestViolation(String),
    #[error("installation child failed: {0}")]
    ChildFailed(String),
    #[error("component hash mismatch: {0}")]
    HashMismatch(String),
    #[error("signature requirement or TeamIdentifier does not match the lock")]
    SignatureMismatch,
    #[error("signature check could not run: {0}")]
    SignatureCheckFailed(String),
    #[error("executable lacks the Hardened Runtime flag")]
    HardenedRuntimeMissing,
    #[error("Gatekeeper does not accept the executable as notarized")]
    NotNotarized,
    #[error("component has no asset or verifier for this platform")]
    UnsupportedPlatform,
    #[error("component installation I/O failed: {0}")]
    Io(#[from] io::Error),
}

#[derive(Clone)]
pub struct InstallOptions {
    pub root: PathBuf,
    pub child_program: PathBuf,
    pub child_args: Vec<String>,
    pub max_unpacked_bytes: u64,
    pub available_space: fn(&Path) -> io::Result<u64>,
}
impl InstallOptions {
    pub fn new(root: impl Into<PathBuf>, child_program: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            child_program: child_program.into(),
            child_args: Vec::new(),
            max_unpacked_bytes: MAX_UNPACKED_BYTES,
            available_space: |path| fs2::available_space(path),
        }
    }
}

#[derive(Debug)]
pub struct InstalledVersion {
    directory: PathBuf,
    /// First listed executable; what `binary()` resolves.
    primary: String,
    // Derived from the pinned archive at install time, never a disk receipt.
    manifest: Manifest,
}
impl InstalledVersion {
    /// Test fixture: digest the named files of an already laid-out directory.
    /// The first name is the primary executable.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fixture(directory: PathBuf, names: &[&str]) -> Result<Self, InstallError> {
        let manifest = names
            .iter()
            .map(|name| {
                let mut file = open_regular(&directory.join(name))?;
                Ok((
                    (*name).to_owned(),
                    digest(&mut file, &CancellationToken::new())?,
                ))
            })
            .collect::<Result<_, InstallError>>()?;
        Ok(Self {
            directory,
            primary: names[0].to_owned(),
            manifest,
        })
    }

    pub fn verify_for_launch(&self) -> Result<(), InstallError> {
        for (name, expected) in &self.manifest {
            // O_NOFOLLOW and fstat in open_regular, then hash this same fd.
            let file = open_regular(&self.directory.join(name))?;
            if file.metadata()?.len() != expected.size
                || digest(&mut file.take(expected.size + 1), &CancellationToken::new())?
                    != *expected
            {
                return Err(InstallError::HashMismatch(name.clone()));
            }
        }
        Ok(())
    }

    /// Setup tests only run the binary; they never go through launch verification.
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_test_directory(directory: PathBuf, primary: &str) -> Self {
        Self {
            directory,
            primary: primary.to_owned(),
            manifest: Manifest::new(),
        }
    }

    /// Revalidate a saved installation against the pinned archive before execution.
    pub fn verify_existing(
        directory: &Path,
        lock: &ComponentLock,
        platform: &str,
        archive: &VerifiedArchive,
        signature: &dyn SignatureVerifier,
    ) -> Result<Self, InstallError> {
        signature.check_platform()?;
        let asset = lock
            .asset_for(platform)
            .ok_or(InstallError::UnsupportedPlatform)?;
        validate_files(asset)?;
        let cancel = CancellationToken::new();
        let bytes = read_archive_bytes(open_regular(&archive.path)?, &asset.sha256, &cancel)?;
        let request = ChildRequest {
            sha256: asset.sha256.clone(),
            staging: PathBuf::new(),
            files: asset.files.clone(),
            limit: MAX_UNPACKED_BYTES,
        };
        let expected = scan_archive(&request, &bytes, false, &cancel)?;
        verify_tree(directory, &expected, &asset.executables, true, &cancel)?;
        verify_signatures(directory, lock, platform, asset, signature)?;
        Ok(Self {
            directory: fs::canonicalize(directory)?,
            primary: asset.executables[0].clone(),
            manifest: expected,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.directory
    }
    pub fn binary(&self) -> PathBuf {
        self.directory.join(&self.primary)
    }
}

/// D2a: every listed executable, on platforms that have a system signature.
/// C3 plugs the component state machine in here (`installing -> installed`
/// only after this returns Ok); nothing outside the installer may skip it.
fn verify_signatures(
    directory: &Path,
    lock: &ComponentLock,
    platform: &str,
    asset: &Asset,
    signature: &dyn SignatureVerifier,
) -> Result<(), InstallError> {
    let Some(signing) = lock
        .signing
        .as_ref()
        .filter(|_| platform.starts_with("darwin-"))
    else {
        // Linux and others: sha256 plus reproducible-build data only (D2a).
        return Ok(());
    };
    let required = SigningRequirement {
        team_id: &signing.macos_team_id,
        notarized: signing.notarized,
    };
    for executable in &asset.executables {
        signature.verify(&directory.join(executable), &required)?;
    }
    Ok(())
}

type Manifest = BTreeMap<String, FileDigest>;
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FileDigest {
    size: u64,
    sha256: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChildRequest {
    sha256: String,
    staging: PathBuf,
    files: Vec<String>,
    limit: u64,
}
#[derive(Serialize, Deserialize)]
enum ChildReply {
    Ok(Manifest),
    ManifestViolation(String),
    HashMismatch(String),
    Io(String),
}

/// The host supplies its own executable and hidden-subcommand arguments.
/// The host binary (agent24d) dispatches that subcommand directly to
/// `run_install_child`, before starting threads or loading credentials; the
/// tiny binary in agent24-idoris is the integration-test adapter, not a launcher.
pub async fn install_locked_asset(
    lock: &ComponentLock,
    platform: &str,
    archive: &VerifiedArchive,
    opts: &InstallOptions,
    signature: impl SignatureVerifier + Send + 'static,
    cancel: &CancellationToken,
) -> Result<InstalledVersion, InstallError> {
    let (lock, platform, archive, opts) = (
        lock.clone(),
        platform.to_owned(),
        archive.clone(),
        opts.clone(),
    );
    let token = cancel.child_token();
    let _cancel_on_drop = token.clone().drop_guard();
    // The worker owns the operation lock and staging guard. Dropping this
    // future cancels its work without unlocking while disk I/O is in flight.
    tokio::task::spawn_blocking(move || {
        install_blocking(&lock, &platform, &archive, &opts, &signature, &token)
    })
    .await
    .map_err(|e| io::Error::other(e.to_string()))?
}

fn install_blocking(
    lock: &ComponentLock,
    platform: &str,
    archive: &VerifiedArchive,
    opts: &InstallOptions,
    signature: &dyn SignatureVerifier,
    cancel: &CancellationToken,
) -> Result<InstalledVersion, InstallError> {
    signature.check_platform()?;
    check_cancel(cancel)?;
    let asset = lock
        .asset_for(platform)
        .ok_or(InstallError::UnsupportedPlatform)?;
    // Public lock fields may be mutated after parsing; validate path and
    // requirement inputs again at the installation boundary.
    let version_name = lock
        .version
        .strip_prefix('v')
        .filter(|tag| {
            semver::Version::parse(tag).is_ok_and(|v| v.pre.is_empty() && v.build.is_empty())
        })
        .ok_or_else(|| violation("invalid version"))?;
    if platform.starts_with("darwin-") && lock.signing.is_none() {
        return Err(violation("unsigned macOS asset"));
    }
    if let Some(signing) = &lock.signing
        && (signing.macos_team_id.len() != 10
            || !signing
                .macos_team_id
                .bytes()
                .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit()))
    {
        return Err(violation("invalid signing team"));
    }
    validate_files(asset)?;
    if let Some(parent) = opts.root.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    private_directory(&opts.root)?;
    let root = fs::canonicalize(&opts.root)?;
    let _operation_lock = operation_lock(&root)?;
    let request = ChildRequest {
        sha256: asset.sha256.clone(),
        staging: PathBuf::new(),
        files: asset.files.clone(),
        limit: opts.max_unpacked_bytes.min(MAX_UNPACKED_BYTES),
    };
    if !hash_matches(&archive.sha256, &asset.sha256) {
        return Err(InstallError::HashMismatch("archive".into()));
    }
    // Open the untrusted path exactly once without following links, then keep
    // a bounded immutable snapshot of that fd's bytes. Hashing, parent scanning
    // and the child's pipe input all use this same snapshot, so path swaps or
    // in-place writes cannot change the bytes extracted after verification.
    let bytes = read_archive_bytes(open_regular(&archive.path)?, &asset.sha256, cancel)?;
    let version = root.join(version_name);
    if fs::symlink_metadata(&version).is_ok() {
        let expected = scan_archive(&request, &bytes, false, cancel)?;
        verify_tree(&version, &expected, &asset.executables, true, cancel)?;
        verify_signatures(&version, lock, platform, asset, signature)?;
        check_cancel(cancel)?;
        return Ok(InstalledVersion {
            directory: version,
            primary: asset.executables[0].clone(),
            manifest: expected,
        });
    }
    let available = (opts.available_space)(&root).map_err(InstallError::SpaceUnknown)?;
    check_cancel(cancel)?;
    let compressed = bytes.len() as u64;
    let required = compressed
        .saturating_add(request.limit)
        .saturating_add(1024 * 1024);
    if available < required {
        return Err(InstallError::InsufficientSpace {
            required,
            available,
        });
    }
    let staging = tempfile::Builder::new()
        .prefix(".staging-")
        .tempdir_in(&root)?;
    private_dir(staging.path())?;
    let request = ChildRequest {
        staging: staging.path().to_owned(),
        ..request
    };
    // Derive authoritative sizes and hashes from the pinned archive, never an
    // editable receipt in the installed directory or an untrusted child reply.
    let expected = scan_archive(&request, &bytes, false, cancel)?;
    let extracted = invoke_child(opts, &request, &bytes, cancel)?;
    if extracted != expected {
        return Err(InstallError::HashMismatch("child manifest".into()));
    }
    verify_tree(staging.path(), &expected, &asset.executables, false, cancel)?;
    for name in expected.keys() {
        // chmod does not open the executable inode for writing (#1429).
        let path = staging.path().join(name);
        set_mode(&path, file_mode(&asset.executables, name))?;
        File::open(&path)?.sync_all()?;
    }
    sync_dirs(staging.path())?;
    check_cancel(cancel)?;
    fs::rename(staging.path(), &version)?;
    // Keep rollback under the same operation lock, including signature failure.
    let result = (|| {
        File::open(&root)?.sync_all()?;
        verify_signatures(&version, lock, platform, asset, signature)?;
        check_cancel(cancel)?;
        Ok(InstalledVersion {
            directory: version.clone(),
            primary: asset.executables[0].clone(),
            manifest: expected,
        })
    })();
    if result.is_err() {
        fs::remove_dir_all(&version)?;
        File::open(&root)?.sync_all()?;
    }
    result
}

pub struct OperationLock(File);
impl Drop for OperationLock {
    fn drop(&mut self) {
        // Concurrent forks may retain a duplicate until exec. Explicit unlock
        // releases the operation without waiting for those copies to close.
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

pub fn operation_lock(root: &Path) -> Result<OperationLock, InstallError> {
    let path = root.join(".operation.lock");
    let lock = File::from(
        rustix::fs::open(
            &path,
            rustix::fs::OFlags::WRONLY
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(io::Error::from)?,
    );
    regular_file(&lock.metadata()?)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if lock.metadata()?.uid() != rustix::process::geteuid().as_raw() {
            return Err(violation("operation lock owner mismatch"));
        }
    }
    match fs2::FileExt::try_lock_exclusive(&lock) {
        Ok(()) => Ok(OperationLock(lock)),
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => Err(InstallError::Busy),
        Err(e) => Err(e.into()),
    }
}

fn invoke_child(
    opts: &InstallOptions,
    request: &ChildRequest,
    bytes: &[u8],
    cancel: &CancellationToken,
) -> Result<Manifest, InstallError> {
    let mut reply_file = tempfile::tempfile()?;
    let mut child = Command::new(fs::canonicalize(&opts.child_program)?)
        .args(&opts.child_args)
        .env_clear()
        .current_dir(&request.staging)
        .stdin(Stdio::piped())
        .stdout(Stdio::from(reply_file.try_clone()?))
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| InstallError::ChildFailed(e.to_string()))?;
    let written = (|| {
        let input = serde_json::to_vec(request).map_err(io::Error::other)?;
        if input.len() > 64 * 1024 {
            return Err(violation("child request too large"));
        }
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| io::Error::other("missing child stdin"))?;
        rustix::fs::fcntl_setfl(&stdin, rustix::fs::OFlags::NONBLOCK).map_err(io::Error::from)?;
        let length = (input.len() as u32).to_be_bytes();
        for mut data in [length.as_slice(), input.as_slice(), bytes] {
            while !data.is_empty() {
                check_cancel(cancel)?;
                match stdin.write(data) {
                    Ok(0) => return Err(io::Error::from(io::ErrorKind::WriteZero).into()),
                    Ok(n) => data = &data[n..],
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => (),
                    Err(e) => return Err(e.into()),
                }
            }
        }
        Ok(())
    })();
    // Reap before removing staging, even when cancellation interrupted a pipe
    // write. Nonblocking stdin keeps an unresponsive child cancellable too.
    if written.is_err() {
        let _ = child.kill();
    }
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if cancel.is_cancelled() {
            let _ = child.kill();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    check_cancel(cancel)?;
    reply_file.rewind()?;
    let mut output = Vec::new();
    reply_file.take(128 * 1024).read_to_end(&mut output)?;
    let reply = serde_json::from_slice::<ChildReply>(&output);
    match (status.success(), reply) {
        (true, Ok(ChildReply::Ok(manifest))) if written.is_ok() => Ok(manifest),
        (false, Ok(ChildReply::ManifestViolation(message))) => Err(violation(message)),
        (false, Ok(ChildReply::HashMismatch(message))) => Err(InstallError::HashMismatch(message)),
        (false, Ok(ChildReply::Io(message))) => Err(InstallError::Io(io::Error::other(message))),
        _ => Err(InstallError::ChildFailed(status.to_string())),
    }
}

fn check_cancel(cancel: &CancellationToken) -> Result<(), InstallError> {
    if cancel.is_cancelled() {
        Err(InstallError::Cancelled)
    } else {
        Ok(())
    }
}

fn read_archive_bytes(
    mut source: impl Read,
    expected: &str,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, InstallError> {
    let mut bytes = Vec::new();
    let mut buffer = [0; 64 * 1024];
    let mut hash = Sha256::new();
    loop {
        check_cancel(cancel)?;
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        if bytes.len() as u64 + n as u64 > MAX_ARCHIVE_BYTES {
            return Err(violation("compressed archive too large"));
        }
        bytes.extend_from_slice(&buffer[..n]);
        hash.update(&buffer[..n]);
    }
    if !hash_matches(&format!("{:x}", hash.finalize()), expected) {
        return Err(InstallError::HashMismatch("archive".into()));
    }
    Ok(bytes)
}

/// Hidden-subcommand entry point. Reads length-prefixed JSON and archive bytes from stdin and
/// writes a JSON result to stdout. All file writes happen after exec here; this
/// path never spawns another process and inherits no environment credentials.
pub fn run_install_child() -> ExitCode {
    let result = (|| {
        let mut input = io::stdin().lock();
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_be_bytes(length) as usize;
        if length > 64 * 1024 {
            return Err(violation("child request too large"));
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
        let request: ChildRequest =
            serde_json::from_slice(&bytes).map_err(|e| violation(e.to_string()))?;
        check_file_list(&request.files)?;
        if request.limit > MAX_UNPACKED_BYTES || !request.staging.is_absolute() {
            return Err(violation("invalid child request"));
        }
        private_dir(&request.staging)?;
        if fs::read_dir(&request.staging)?.next().is_some() {
            return Err(violation("staging is not empty"));
        }
        let cancel = CancellationToken::new();
        let bytes = read_archive_bytes(input, &request.sha256, &cancel)?;
        let manifest = scan_archive(&request, &bytes, true, &cancel)?;
        sync_dirs(&request.staging)?;
        Ok(manifest)
    })();
    let success = result.is_ok();
    let reply = match result {
        Ok(manifest) => ChildReply::Ok(manifest),
        Err(InstallError::ManifestViolation(s)) => ChildReply::ManifestViolation(s),
        Err(InstallError::HashMismatch(s)) => ChildReply::HashMismatch(s),
        Err(e) => ChildReply::Io(e.to_string()),
    };
    if serde_json::to_writer(io::stdout().lock(), &reply).is_err() {
        return ExitCode::FAILURE;
    }
    if success {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn validate_files(asset: &Asset) -> Result<(), InstallError> {
    check_file_list(&asset.files)?;
    let files: BTreeSet<_> = asset.files.iter().collect();
    if asset.executables.is_empty() || !asset.executables.iter().all(|e| files.contains(e)) {
        return Err(violation("executables must be listed files"));
    }
    Ok(())
}

fn check_file_list(files: &[String]) -> Result<(), InstallError> {
    let mut seen = BTreeSet::new();
    if files.is_empty() {
        return Err(violation("no release files"));
    }
    for name in files {
        clean_path(name, false)?;
        if !clean_relative(name) || !seen.insert(name) {
            return Err(violation("invalid locked files"));
        }
    }
    Ok(())
}

/// Executables are read-and-execute only; everything else read-only.
fn file_mode(executables: &[String], name: &str) -> u32 {
    if executables.iter().any(|e| e == name) {
        0o500
    } else {
        0o400
    }
}

fn clean_path(name: &str, directory: bool) -> Result<&str, InstallError> {
    let name = if directory {
        name.strip_suffix('/').unwrap_or(name)
    } else {
        name
    };
    if name.is_empty()
        || name.contains(['\\', '\0'])
        || name.split('/').any(|s| matches!(s, "" | "." | ".."))
    {
        return Err(violation(format!("unsafe path: {name}")));
    }
    Ok(name)
}

fn parent_dirs(files: &[String]) -> BTreeSet<String> {
    files
        .iter()
        .flat_map(|name| {
            Path::new(name)
                .ancestors()
                .skip(1)
                .filter(|p| !p.as_os_str().is_empty())
                .map(|p| p.to_string_lossy().into_owned())
        })
        .collect()
}

fn scan_archive(
    request: &ChildRequest,
    bytes: &[u8],
    extract: bool,
    cancel: &CancellationToken,
) -> Result<Manifest, InstallError> {
    // Bound headers, padding, gzip members and trailing data as well as file
    // payloads. raw(true) lets us validate PAX and reject GNU indirection
    // without interpreting path, size, link or sparse overrides.
    let budget = request.limit.saturating_add(64 * 1024);
    let decoded = MultiGzDecoder::new(bytes).take(budget + 1);
    let mut tar = tar::Archive::new(decoded);
    let parents = parent_dirs(&request.files);
    let allowed: BTreeSet<_> = request.files.iter().cloned().collect();
    let mut seen = BTreeSet::new();
    let mut manifest = Manifest::new();
    let mut total = 0u64;
    let mut pax_count = 0;
    let mut pending_pax = false;
    for entry in tar
        .entries()
        .map_err(|e| violation(e.to_string()))?
        .raw(true)
    {
        check_cancel(cancel)?;
        let mut entry = entry.map_err(|e| violation(e.to_string()))?;
        let kind = entry.header().entry_type();
        if kind.is_pax_local_extensions() {
            pax_count += 1;
            if pending_pax || pax_count > request.files.len() || entry.size() > 4096 {
                return Err(violation("excessive PAX metadata"));
            }
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| violation(e.to_string()))?;
            for extension in tar::PaxExtensions::new(&bytes) {
                let extension = extension.map_err(|e| violation(e.to_string()))?;
                let key = extension.key().map_err(|_| violation("invalid PAX key"))?;
                // The pinned BSD tar release includes time/provenance metadata.
                // Discard it, including xattrs; never apply metadata to disk.
                if !matches!(
                    key,
                    "mtime"
                        | "atime"
                        | "ctime"
                        | "LIBARCHIVE.xattr.com.apple.provenance"
                        | "SCHILY.xattr.com.apple.provenance"
                ) {
                    return Err(violation(format!("unsupported PAX key: {key}")));
                }
            }
            pending_pax = true;
            continue;
        }
        pending_pax = false;
        if !kind.is_file() && !kind.is_dir() {
            return Err(violation(
                "only ordinary files and parent directories are allowed",
            ));
        }
        let raw = entry.path_bytes();
        let raw = std::str::from_utf8(&raw).map_err(|_| violation("non-UTF-8 tar path"))?;
        let name = clean_path(raw, kind.is_dir())?.to_owned();
        if !seen.insert(name.clone()) {
            return Err(violation(format!("duplicate path: {name}")));
        }
        if kind.is_dir() {
            if !parents.contains(&name) || entry.size() != 0 {
                return Err(violation(format!("unexpected directory: {name}")));
            }
            continue;
        }
        if !allowed.contains(&name) {
            return Err(violation(format!("unexpected file: {name}")));
        }
        total = total
            .checked_add(entry.size())
            .ok_or_else(|| violation("unpacked size overflow"))?;
        if total > request.limit {
            return Err(violation("unpacked size limit exceeded"));
        }
        let declared = entry.size();
        let mut output = if extract {
            let path = request.staging.join(&name);
            let parent = path
                .parent()
                .ok_or_else(|| violation("missing file parent"))?;
            private_dir(parent)?;
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            Some(options.open(&path)?)
        } else {
            None
        };
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 64 * 1024];
        loop {
            check_cancel(cancel)?;
            let n = entry
                .read(&mut buffer)
                .map_err(|e| violation(e.to_string()))?;
            if n == 0 {
                break;
            }
            size += n as u64;
            hash.update(&buffer[..n]);
            if let Some(file) = &mut output {
                file.write_all(&buffer[..n])?;
            }
        }
        if size != declared {
            return Err(violation("truncated tar entry"));
        }
        if let Some(file) = output {
            file.sync_all()?;
        } // Closed before reply or exit.
        manifest.insert(
            name,
            FileDigest {
                size,
                sha256: format!("{:x}", hash.finalize()),
            },
        );
    }
    if pending_pax {
        return Err(violation("orphaned PAX metadata"));
    }
    // Force gzip CRC validation and reject data hidden after tar's end marker.
    let mut tail = tar.into_inner();
    let mut buffer = [0u8; 8192];
    loop {
        check_cancel(cancel)?;
        let n = tail
            .read(&mut buffer)
            .map_err(|e| violation(e.to_string()))?;
        if n == 0 {
            break;
        }
        if tail.limit() == 0 || buffer[..n].iter().any(|b| *b != 0) {
            return Err(violation("oversized or nonzero tar trailer"));
        }
    }
    if manifest.keys().cloned().collect::<BTreeSet<_>>() != allowed {
        return Err(violation("missing release files"));
    }
    Ok(manifest)
}

fn verify_tree(
    root: &Path,
    expected: &Manifest,
    executables: &[String],
    immutable: bool,
    cancel: &CancellationToken,
) -> Result<(), InstallError> {
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(violation("version is not a directory"));
    }
    let parents = parent_dirs(&expected.keys().cloned().collect::<Vec<_>>());
    let mut pending = vec![root.to_path_buf()];
    let mut seen = BTreeSet::new();
    while let Some(dir) = pending.pop() {
        if immutable {
            check_mode(&dir, 0o700)?;
        }
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            let name = path
                .strip_prefix(root)
                .map_err(io::Error::other)?
                .to_string_lossy()
                .into_owned();
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_dir() && parents.contains(&name) {
                pending.push(path);
                continue;
            }
            regular_file(&metadata)?;
            let wanted = expected
                .get(&name)
                .ok_or_else(|| violation(format!("unexpected installed file: {name}")))?;
            if metadata.len() != wanted.size || digest(&mut File::open(&path)?, cancel)? != *wanted
            {
                return Err(InstallError::HashMismatch(name));
            }
            if immutable {
                check_mode(&path, file_mode(executables, &name))?;
            }
            seen.insert(name);
        }
    }
    if seen.len() != expected.len() {
        return Err(violation("missing installed file"));
    }
    Ok(())
}

fn digest(reader: &mut impl Read, cancel: &CancellationToken) -> Result<FileDigest, InstallError> {
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hash.update(&buffer[..n]);
    }
    Ok(FileDigest {
        size,
        sha256: format!("{:x}", hash.finalize()),
    })
}
fn regular_file(metadata: &fs::Metadata) -> Result<(), InstallError> {
    if !metadata.is_file() {
        return Err(violation("not an ordinary file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(violation("hard-linked file"));
        }
    }
    Ok(())
}
pub fn private_dir(path: &Path) -> Result<(), InstallError> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)?;
    if !fs::symlink_metadata(path)?.is_dir() {
        return Err(violation("directory is a symlink or non-directory"));
    }
    set_mode(path, 0o700)?;
    Ok(())
}
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
fn check_mode(path: &Path, mode: u32) -> Result<(), InstallError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::symlink_metadata(path)?.permissions().mode() & 0o7777 != mode {
            return Err(violation("installed permissions changed"));
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (path, mode);
    }
    Ok(())
}
fn sync_dirs(root: &Path) -> io::Result<()> {
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if fs::symlink_metadata(&path)?.is_dir() {
            sync_dirs(&path)?;
        }
    }
    File::open(root)?.sync_all()
}
fn violation(message: impl Into<String>) -> InstallError {
    InstallError::ManifestViolation(message.into())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::lock::test_lock;

    struct AcceptSignature;
    impl SignatureVerifier for AcceptSignature {
        fn verify(&self, _: &Path, _: &SigningRequirement) -> Result<(), InstallError> {
            Ok(())
        }
    }

    #[test]
    fn restored_installation_rechecks_every_pinned_file_before_launch() {
        use flate2::{Compression, write::GzEncoder};

        let dir = tempfile::tempdir().unwrap();
        let release = dir.path().join("release");
        let mut lock = test_lock();
        let asset = lock.assets.get_mut("darwin-arm64").unwrap();
        let mut archive = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        let original = b"fixture\n";
        for name in &asset.files {
            let path = release.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, original).unwrap();
            set_mode(&path, file_mode(&asset.executables, name)).unwrap();
            let mut header = tar::Header::new_gnu();
            header.set_size(original.len() as u64);
            header.set_mode(0o600);
            header.set_cksum();
            archive
                .append_data(&mut header, name, &original[..])
                .unwrap();
        }
        for name in ["", "config"] {
            set_mode(&release.join(name), 0o700).unwrap();
        }
        let bytes = archive.into_inner().unwrap().finish().unwrap();
        asset.sha256 = format!("{:x}", Sha256::digest(&bytes));
        let archive = VerifiedArchive {
            path: dir.path().join("archive.tar.gz"),
            sha256: asset.sha256.clone(),
        };
        fs::write(&archive.path, bytes).unwrap();
        let installed = InstalledVersion::verify_existing(
            &release,
            &lock,
            "darwin-arm64",
            &archive,
            &AcceptSignature,
        )
        .unwrap();
        installed.verify_for_launch().unwrap();

        for name in &lock.assets["darwin-arm64"].files {
            let path = release.join(name);
            set_mode(&path, 0o600).unwrap();
            fs::write(&path, b"changed\n").unwrap();
            assert!(
                matches!(installed.verify_for_launch(), Err(InstallError::HashMismatch(file)) if file == *name),
                "restored installation accepted changes to {name}"
            );
            fs::write(&path, original).unwrap();
            set_mode(
                &path,
                file_mode(&lock.assets["darwin-arm64"].executables, name),
            )
            .unwrap();
        }
    }

    #[test]
    fn operation_lock_release_does_not_wait_for_inherited_descriptors() {
        let dir = tempfile::tempdir().unwrap();
        let first = operation_lock(dir.path()).unwrap();
        // dup shares the open file description, just like a concurrent fork.
        let inherited = first.0.try_clone().unwrap();
        drop(first);
        assert!(operation_lock(dir.path()).is_ok());
        drop(inherited);
    }

    #[test]
    fn executables_must_be_listed_files() {
        let mut lock = test_lock();
        let asset = lock.assets.get_mut("darwin-arm64").unwrap();
        asset.executables = vec!["not-in-files".into()];
        assert!(matches!(
            validate_files(asset),
            Err(InstallError::ManifestViolation(_))
        ));
        asset.executables.clear();
        assert!(validate_files(asset).is_err());
    }

    /// Records which executables were checked; fails on the named one.
    struct Recording {
        seen: std::sync::Mutex<Vec<PathBuf>>,
        fail_on: Option<&'static str>,
    }
    impl SignatureVerifier for Recording {
        fn verify(&self, path: &Path, required: &SigningRequirement) -> Result<(), InstallError> {
            assert_eq!(required.team_id, "HYAP444FN9");
            self.seen.lock().unwrap().push(path.to_owned());
            if self.fail_on.is_some_and(|name| path.ends_with(name)) {
                return Err(InstallError::SignatureMismatch);
            }
            Ok(())
        }
    }

    fn two_executables() -> ComponentLock {
        let mut lock = test_lock();
        let asset = lock.assets.get_mut("darwin-arm64").unwrap();
        asset.files = vec!["demo".into(), "helper".into(), "config/a.yaml".into()];
        asset.executables = vec!["demo".into(), "helper".into()];
        lock
    }

    #[test]
    fn every_listed_executable_is_verified_and_any_failure_rejects_all() {
        let lock = two_executables();
        let asset = &lock.assets["darwin-arm64"];
        let dir = Path::new("/v");
        let ok = Recording {
            seen: Default::default(),
            fail_on: None,
        };
        verify_signatures(dir, &lock, "darwin-arm64", asset, &ok).unwrap();
        assert_eq!(
            *ok.seen.lock().unwrap(),
            [dir.join("demo"), dir.join("helper")]
        );
        // Non-executable files are never handed to the verifier.
        let bad = Recording {
            seen: Default::default(),
            fail_on: Some("helper"),
        };
        assert!(matches!(
            verify_signatures(dir, &lock, "darwin-arm64", asset, &bad),
            Err(InstallError::SignatureMismatch)
        ));
    }

    #[test]
    fn linux_assets_skip_system_signatures() {
        let lock = two_executables();
        let asset = &lock.assets["darwin-arm64"];
        let never = Recording {
            seen: Default::default(),
            fail_on: Some("demo"),
        };
        verify_signatures(Path::new("/v"), &lock, "linux-x64", asset, &never).unwrap();
        assert!(never.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn only_executables_get_the_executable_mode() {
        let exes = vec!["bin/tool".to_owned()];
        assert_eq!(file_mode(&exes, "bin/tool"), 0o500);
        assert_eq!(file_mode(&exes, "config/a.yaml"), 0o400);
    }
}
