//! iDoris's use of the generic installer (agent24-components). The writer-process
//! protocol, `run_install_child` and the signature verifier live there; the host
//! binary still dispatches the hidden subcommand to `run_install_child`.

use crate::{IdorisLock, platform_of};
use agent24_components::{download::VerifiedArchive, storage::private_directory};
pub use agent24_components::{
    install::{
        InstallError, InstallOptions, InstalledVersion, MAX_UNPACKED_BYTES, OperationLock,
        operation_lock, private_dir, run_install_child,
    },
    sign::{Codesign, SignatureVerifier, SigningRequirement},
};
use std::{
    io,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;

/// Where iDoris keeps its *state* (installation receipt, `data/`, `instances/`,
/// operation journal). Releases no longer live here; see [`managed_root`].
pub fn default_install_root() -> Result<PathBuf, InstallError> {
    let home = std::env::var_os("HOME").ok_or_else(|| io::Error::other("HOME is unset"))?;
    Ok(PathBuf::from(home).join("Library/Application Support/Agent24/idoris"))
}

/// ADR-038 D1 managed root: `<data dir>/components/idoris`. Versions are
/// `<root>/<version>/`, the download cache is `<root>/cache/`.
pub fn managed_root(data_dir: &Path) -> PathBuf {
    data_dir.join("components").join("idoris")
}

/// Create the state directories iDoris expects below its state root (0700).
pub fn prepare_state_dirs(root: &Path) -> Result<(), InstallError> {
    if let Some(parent) = root.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    private_directory(root)?;
    for name in ["data", "instances"] {
        private_dir(&root.join(name))?;
    }
    Ok(())
}

pub async fn install_locked_asset(
    lock: &IdorisLock,
    triple: &str,
    archive: &VerifiedArchive,
    opts: &InstallOptions,
    signature: impl SignatureVerifier + Send + 'static,
    cancel: &CancellationToken,
) -> Result<InstalledVersion, InstallError> {
    let platform = platform_of(triple).ok_or(InstallError::UnsupportedPlatform)?;
    agent24_components::install::install_locked_asset(
        &lock.to_component(),
        platform,
        archive,
        opts,
        signature,
        cancel,
    )
    .await
}

/// Revalidate a saved installation against the pinned archive before execution.
pub fn verify_existing(
    directory: &Path,
    lock: &IdorisLock,
    archive: &VerifiedArchive,
    signature: &dyn SignatureVerifier,
) -> Result<InstalledVersion, InstallError> {
    let platform = crate::host_triple()
        .and_then(platform_of)
        .ok_or(InstallError::UnsupportedPlatform)?;
    verify_existing_on(platform, directory, lock, archive, signature)
}

/// `verify_existing` for an explicit platform key (`darwin-arm64`).
pub fn verify_existing_on(
    platform: &str,
    directory: &Path,
    lock: &IdorisLock,
    archive: &VerifiedArchive,
    signature: &dyn SignatureVerifier,
) -> Result<InstalledVersion, InstallError> {
    InstalledVersion::verify_existing(
        directory,
        &lock.to_component(),
        platform,
        archive,
        signature,
    )
}
