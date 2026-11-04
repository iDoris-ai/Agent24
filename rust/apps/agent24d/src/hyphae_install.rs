//! Pinned Hyphae installation owned by the daemon, outside comm's no-HTTP boundary.
//!
//! The embedded `hyphae.lock.json` (Hyphae's published schema-2 `ComponentLock`, copied
//! byte for byte into agent24-comm) is the single source of truth: release location,
//! asset names, digests, signing policy, contract and provides. agent24-comm only embeds
//! it; it is parsed here with the one strict parser from agent24-components.
use agent24_comm::{BinaryError, HYPHAE_LOCK_JSON, Sha256Digest, VerifiedBinary};
use agent24_components::{
    ComponentLock, DownloadError, DownloadOptions, LockError, SignatureVerifier,
    SigningRequirement, VerifyMode, download_locked_asset, install::InstallError as SignError,
    signing_requirement,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The only file every Hyphae asset contains: the release asset IS the bare binary.
const EXECUTABLE: &str = "hyphae";

/// The D2a verifier (`Codesign` in production, a fake in tests), shared by install and start.
pub type Verifier = Arc<dyn SignatureVerifier + Send + Sync>;

#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error(transparent)]
    Download(#[from] DownloadError),
    #[error(transparent)]
    Binary(#[from] BinaryError),
    #[error("Hyphae signature verification failed: {0}")]
    Signature(#[from] SignError),
    #[error("component installation I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum HyphaeLockError {
    #[error(transparent)]
    Lock(#[from] LockError),
    #[error("hyphae.lock.json is not a Hyphae lock: {0}")]
    Shape(&'static str),
}

/// The embedded Hyphae lock, checked for the shape this installer handles.
#[derive(Debug, Clone)]
pub struct HyphaeLock(ComponentLock);

impl HyphaeLock {
    pub fn embedded() -> Result<Self, HyphaeLockError> {
        Self::new(ComponentLock::parse(HYPHAE_LOCK_JSON)?)
    }

    /// Hyphae ships bare binaries, not archives: every asset must be exactly the one
    /// `hyphae` executable, so `executables` (D2a) is that file and nothing is skipped.
    pub fn new(lock: ComponentLock) -> Result<Self, HyphaeLockError> {
        if lock.id != "hyphae" {
            return Err(HyphaeLockError::Shape("id"));
        }
        for asset in lock.assets.values() {
            if asset.files != [EXECUTABLE] || asset.executables != [EXECUTABLE] {
                return Err(HyphaeLockError::Shape(
                    "assets must be the bare hyphae binary",
                ));
            }
        }
        Ok(Self(lock))
    }

    pub fn component(&self) -> &ComponentLock {
        &self.0
    }

    /// Release tag (`v0.30.1`); also the managed-root version directory.
    pub fn version(&self) -> &str {
        &self.0.version
    }

    /// Digest of the published asset (after signing, on macOS).
    pub fn expected_for(&self, platform: &str) -> Result<Sha256Digest, BinaryError> {
        let asset = self
            .0
            .asset_for(platform)
            .ok_or_else(|| BinaryError::NoLockForPlatform(platform.into()))?;
        Sha256Digest::from_hex(&asset.sha256)
    }

    pub fn install_dir(&self, state_dir: &Path) -> PathBuf {
        state_dir.join("components/hyphae").join(self.version())
    }

    pub fn installed_path(&self, state_dir: &Path) -> PathBuf {
        self.install_dir(state_dir).join(EXECUTABLE)
    }

    /// Test-only stand-in for the embedded lock: the real lock with this platform's
    /// asset replaced by a fixture digest, so the production flow runs end to end.
    #[cfg(test)]
    #[allow(clippy::expect_used)]
    pub fn override_for_test(platform: &str, sha: Sha256Digest) -> Self {
        let mut lock = Self::embedded().expect("embedded Hyphae lock");
        let asset = lock
            .0
            .assets
            .values()
            .next()
            .cloned()
            .expect("lock has an asset");
        lock.0.assets.insert(
            platform.into(),
            agent24_components::lock::Asset {
                name: "fixture-hyphae".into(),
                sha256: sha.to_hex(),
                ..asset
            },
        );
        lock
    }

    #[cfg(test)]
    pub fn component_mut(&mut self) -> &mut ComponentLock {
        &mut self.0
    }
}

/// What D2a established for one executable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureCheck {
    /// macOS: Developer ID of the lock's team, Hardened Runtime; `notarized` when
    /// Gatekeeper was asked too (install only).
    DeveloperId { notarized: bool },
    /// This platform has no system code signature (Linux). The sha256 against the
    /// lock, which the caller has already checked, plus the reproducible-build CI is
    /// the whole verification (ADR-038 D2a). Explicit, never a skipped step.
    UnsignedPlatform,
}

/// D2a for the installed Hyphae binary. `Install` asks Gatekeeper for the notarization
/// verdict (`spctl --assess --type install`, online for a bare unstapled Mach-O);
/// `AtRest` (every start) is offline: codesign team + Hardened Runtime only.
pub async fn verify_signature(
    lock: &HyphaeLock,
    platform: &str,
    executable: &Path,
    verifier: &Verifier,
    mode: VerifyMode,
) -> Result<SignatureCheck, SignError> {
    let Some(required) = signing_requirement(lock.component(), platform, mode)? else {
        tracing::debug!(
            platform,
            ?mode,
            "Hyphae: no system code signature on this platform; verified by sha256 against \
             the lock (and the reproducible build in CI) only"
        );
        return Ok(SignatureCheck::UnsignedPlatform);
    };
    let (team, notarized) = (required.team_id.to_owned(), required.notarized);
    let (verifier, executable) = (verifier.clone(), executable.to_path_buf());
    // codesign/spctl are blocking subprocesses (spctl may wait on the network).
    tokio::task::spawn_blocking(move || {
        verifier.verify(
            &executable,
            &SigningRequirement {
                team_id: &team,
                notarized,
            },
        )
    })
    .await
    .map_err(|e| SignError::Io(std::io::Error::other(e.to_string())))??;
    Ok(SignatureCheck::DeveloperId { notarized })
}

/// Callers serialize installs and supply a lock that cannot be overridden in production.
pub async fn install(
    lock: &HyphaeLock,
    platform: &str,
    state_dir: &Path,
    verifier: &Verifier,
    cancel: &CancellationToken,
) -> Result<PathBuf, InstallError> {
    let expected = lock
        .expected_for(platform)
        .map_err(|_| DownloadError::UnsupportedPlatform)?;
    let installed = lock.installed_path(state_dir);
    let install_dir = lock.install_dir(state_dir);
    // Reinstall is idempotent, but an existing binary is never trusted by name: it is
    // re-hashed and goes through the full install-time signature check again. As on a
    // fresh install, a hashed copy that fails D2a is removed (the stable path is left
    // for the next start's at-rest check to refuse and report).
    if tokio::fs::try_exists(&installed).await? {
        let verified = VerifiedBinary::install(&installed, expected, &install_dir).await?;
        if let Err(e) =
            verify_signature(lock, platform, &installed, verifier, VerifyMode::Install).await
        {
            let _ = tokio::fs::remove_file(verified.path()).await;
            return Err(e.into());
        }
        return Ok(installed);
    }
    let archive = download_locked_asset(
        lock.component(),
        platform,
        // A sibling of `components/hyphae`: that directory is created 0755 by the
        // binary installer, while a download root must be an owned 0700 directory.
        &state_dir.join("components/hyphae-cache"),
        &DownloadOptions::default(),
        cancel,
    )
    .await?;
    let verified = VerifiedBinary::install(&archive.path, expected, &install_dir).await?;
    // Signature before publication: a binary that fails D2a never gets the stable path,
    // and its hashed copy is removed so no half-installed component is left behind.
    if let Err(e) = verify_signature(
        lock,
        platform,
        verified.path(),
        verifier,
        VerifyMode::Install,
    )
    .await
    {
        let _ = tokio::fs::remove_file(verified.path()).await;
        return Err(e.into());
    }
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
    use std::sync::Mutex;

    /// Records what it was asked; accepts or refuses everything.
    #[derive(Default)]
    pub(crate) struct Recorder {
        pub(crate) calls: Mutex<Vec<(PathBuf, String, bool)>>,
        pub(crate) refuse: bool,
    }
    impl SignatureVerifier for Recorder {
        fn verify(&self, path: &Path, required: &SigningRequirement) -> Result<(), SignError> {
            self.calls.lock().unwrap().push((
                path.to_path_buf(),
                required.team_id.to_owned(),
                required.notarized,
            ));
            if self.refuse {
                Err(SignError::SignatureMismatch)
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn embedded_lock_is_the_signed_v0_30_1_release() {
        // Byte-for-byte the published release file (ADR-038 §7.4).
        assert_eq!(
            agent24_comm::binary::sha256_of(HYPHAE_LOCK_JSON.as_bytes()).to_hex(),
            "0a3d77d3f50582b4690946000c5e4f7da185403f14e3f2da282a6abeb990bf6f"
        );
        let lock = HyphaeLock::embedded().unwrap();
        let c = lock.component();
        assert_eq!(c.schema, 2);
        assert_eq!(lock.version(), "v0.30.1");
        assert_eq!(c.source.commit, "068e2d2e5a657bbf87204b3892451e8a280ac3a7");
        assert_eq!(
            lock.expected_for("darwin-arm64").unwrap().to_hex(),
            "7a079c5d43d6179dda0e46f05d0c31d991c0b7ada42a41b862d508a09e67b172"
        );
        assert_eq!(
            lock.expected_for("linux-x64").unwrap().to_hex(),
            "096d9664fb4ab7ebf2bb11204bb78e076cf45b58c140e8a753e5d247f008b838"
        );
        assert_eq!(c.assets["linux-x64"].name, "hyphae-linux-amd64");
        assert_eq!(
            c.asset_url(&c.assets["darwin-arm64"]),
            "https://github.com/iDoris-ai/Hyphae/releases/download/v0.30.1/hyphae-darwin-arm64"
        );
        let signing = c.signing.as_ref().unwrap();
        assert_eq!(signing.macos_team_id, "HYAP444FN9");
        assert!(signing.notarized);
        let repro = c.reproducible.as_ref().unwrap();
        // Signing changes the bytes: the reproduction target differs from the asset on
        // macOS and equals it on (unsigned) Linux.
        assert_ne!(
            repro.unsigned_sha256["darwin-arm64"],
            c.assets["darwin-arm64"].sha256
        );
        assert_eq!(
            repro.unsigned_sha256["linux-x64"],
            c.assets["linux-x64"].sha256
        );
        assert!(repro.recipe.contains("-X main.version=v0.30.1"));
        assert_eq!(c.stop_grace_ms, Some(15000));
        assert!(c.accepts_contract(&c.contract.tested));
        assert!(matches!(
            lock.expected_for("darwin-x64"),
            Err(BinaryError::NoLockForPlatform(p)) if p == "darwin-x64"
        ));
    }

    #[test]
    fn lock_shape_is_enforced_and_unknown_schemas_refused() {
        let mut lock = HyphaeLock::embedded().unwrap().0;
        lock.id = "other".into();
        assert!(matches!(
            HyphaeLock::new(lock.clone()),
            Err(HyphaeLockError::Shape("id"))
        ));
        lock.id = "hyphae".into();
        lock.assets
            .get_mut("linux-x64")
            .unwrap()
            .files
            .push("extra".into());
        assert!(HyphaeLock::new(lock).is_err());
        // Nothing weaker than schema 2 is read, including the previous schema-1 lock.
        let v1 = r#"{"schema":1,"source_sha":"x","go":"go1","recipe":"r","binaries":{}}"#;
        assert!(matches!(
            ComponentLock::parse(v1),
            Err(LockError::Schema(1))
        ));
        let v3 = HYPHAE_LOCK_JSON.replace("\"schema\": 2", "\"schema\": 3");
        assert!(matches!(
            ComponentLock::parse(&v3),
            Err(LockError::Schema(3))
        ));
    }

    #[tokio::test]
    async fn signature_policy_per_platform_and_mode() {
        let lock = HyphaeLock::embedded().unwrap();
        let recorder = Arc::new(Recorder::default());
        let verifier: Verifier = recorder.clone();
        let path = Path::new("/nonexistent/hyphae");
        assert_eq!(
            verify_signature(&lock, "darwin-arm64", path, &verifier, VerifyMode::Install)
                .await
                .unwrap(),
            SignatureCheck::DeveloperId { notarized: true }
        );
        assert_eq!(
            verify_signature(&lock, "darwin-arm64", path, &verifier, VerifyMode::AtRest)
                .await
                .unwrap(),
            SignatureCheck::DeveloperId { notarized: false }
        );
        for mode in [VerifyMode::Install, VerifyMode::AtRest] {
            assert_eq!(
                verify_signature(&lock, "linux-x64", path, &verifier, mode)
                    .await
                    .unwrap(),
                SignatureCheck::UnsignedPlatform
            );
        }
        assert_eq!(
            *recorder.calls.lock().unwrap(),
            [
                (path.to_path_buf(), "HYAP444FN9".to_owned(), true),
                (path.to_path_buf(), "HYAP444FN9".to_owned(), false),
            ],
            "Linux never reaches the verifier; macOS asks Gatekeeper only at install"
        );
        // A lock cannot switch notarization off for macOS.
        let mut lax = lock.clone();
        lax.component_mut().signing.as_mut().unwrap().notarized = false;
        assert!(matches!(
            verify_signature(&lax, "darwin-arm64", path, &verifier, VerifyMode::Install).await,
            Err(SignError::ManifestViolation(_))
        ));
        // A macOS asset whose lock lost `signing` is refused, not treated as unsigned.
        let mut unsigned = lock.clone();
        unsigned.component_mut().signing = None;
        assert!(matches!(
            verify_signature(
                &unsigned,
                "darwin-arm64",
                path,
                &verifier,
                VerifyMode::AtRest
            )
            .await,
            Err(SignError::ManifestViolation(_))
        ));
    }

    #[tokio::test]
    async fn failed_signature_publishes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        const FIXTURE: &[u8] = b"#!/bin/sh\nexit 0\n";
        let lock =
            HyphaeLock::override_for_test("darwin-arm64", agent24_comm::binary::sha256_of(FIXTURE));
        let cache = dir.path().join("components/hyphae-cache");
        std::fs::create_dir_all(&cache).unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&cache, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(cache.join("fixture-hyphae.verified"), FIXTURE).unwrap();
        let refuse: Verifier = Arc::new(Recorder {
            refuse: true,
            ..Default::default()
        });
        let err = install(
            &lock,
            "darwin-arm64",
            dir.path(),
            &refuse,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            InstallError::Signature(SignError::SignatureMismatch)
        ));
        let install_dir = lock.install_dir(dir.path());
        assert!(!lock.installed_path(dir.path()).exists());
        let left: Vec<_> = std::fs::read_dir(&install_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(left, [".install.lock"], "no executable copy is left behind");
        // #1551 L1: the reinstall branch (a stable path already exists) cleans up too.
        std::fs::write(lock.installed_path(dir.path()), FIXTURE).unwrap();
        let err = install(
            &lock,
            "darwin-arm64",
            dir.path(),
            &refuse,
            &CancellationToken::new(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err,
            InstallError::Signature(SignError::SignatureMismatch)
        ));
        let mut left: Vec<_> = std::fs::read_dir(&install_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [".install.lock", "hyphae"],
            "reinstall leaves no hashed copy behind either"
        );
        std::fs::remove_file(lock.installed_path(dir.path())).unwrap();
        // Linux: same fixture installs with sha256 only, no verifier call at all.
        let lock =
            HyphaeLock::override_for_test("linux-x64", agent24_comm::binary::sha256_of(FIXTURE));
        let path = install(
            &lock,
            "linux-x64",
            dir.path(),
            &refuse,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), FIXTURE);
    }

    #[tokio::test]
    #[ignore = "downloads the pinned v0.30.1 asset from GitHub; on macOS also runs codesign + spctl"]
    async fn real_github_v0_30_1_matches_lock_and_signature() {
        let dir = tempfile::tempdir().unwrap();
        let lock = HyphaeLock::embedded().unwrap();
        let platform = agent24_comm::current_platform();
        let verifier: Verifier = Arc::new(agent24_components::sign::Codesign);
        let path = install(
            &lock,
            &platform,
            dir.path(),
            &verifier,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let bytes = tokio::fs::read(&path).await.unwrap();
        let digest = agent24_comm::binary::sha256_of(&bytes);
        assert_eq!(digest, lock.expected_for(&platform).unwrap());
        // Start-time (offline) check on the same file.
        let at_rest = verify_signature(&lock, &platform, &path, &verifier, VerifyMode::AtRest)
            .await
            .unwrap();
        if platform.starts_with("darwin-") {
            assert_eq!(at_rest, SignatureCheck::DeveloperId { notarized: false });
        } else {
            assert_eq!(at_rest, SignatureCheck::UnsignedPlatform);
        }
        eprintln!(
            "verified Hyphae v0.30.1 {platform}: {} bytes, sha256 {}, install-time signature ok, at rest {at_rest:?}",
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
