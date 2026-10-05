//! Hash-verified Hyphae binaries.
//!
//! `hyphae.lock.json` (embedded via `include_str!`) records, per platform,
//! the sha256 of a Hyphae binary built with a fixed, reproducible recipe
//! (COMM-HYPHAE.md §8.1). [`VerifiedBinary::install`] is the *only* way to
//! obtain a [`VerifiedBinary`]: it reads the candidate binary into memory
//! once, hashes those bytes, and — only on a match — writes an O_EXCL,
//! mode-0500 copy into `install_dir`. A copy that already exists there is
//! re-hashed on every call; a mismatch there is a hard error (a tampered
//! copy), never a silent overwrite (COMM-0 r2 decision M9: no TOCTOU window
//! between verifying and executing).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions, Permissions};
use std::io;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The lock file this crate ships, embedded at compile time.
const LOCK_JSON: &str = include_str!("../hyphae.lock.json");

/// A sha256 digest, compared by raw bytes (not string case).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Sha256Digest(pub [u8; 32]);

impl Sha256Digest {
    /// Parses a 64-character hex string (case-insensitive).
    pub fn from_hex(s: &str) -> Result<Self, BinaryError> {
        let bytes = hex::decode(s.trim()).map_err(|_| BinaryError::BadLockHash)?;
        let array: [u8; 32] = bytes.try_into().map_err(|_| BinaryError::BadLockHash)?;
        Ok(Self(array))
    }

    /// Lowercase hex encoding of the full digest.
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// First 16 hex characters (8 bytes) — used for the installed copy's
    /// filename (`hyphae-<short>`), not for verification.
    pub fn short(&self) -> String {
        hex::encode(&self.0[..8])
    }
}

impl std::fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

/// Hashes a byte slice with sha256. Exposed so callers (and tests) that need
/// to compute an expected digest — e.g. for a freshly-built fixture binary —
/// don't have to pull in `sha2` themselves.
pub fn sha256_of(bytes: &[u8]) -> Sha256Digest {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = [0u8; 32];
    out.copy_from_slice(&digest);
    Sha256Digest(out)
}

/// Returns the current platform in the `hyphae.lock.json` key format
/// (`darwin-arm64`, `linux-x64`, ...).
pub fn current_platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{os}-{arch}")
}

/// The parsed contents of `hyphae.lock.json`.
#[derive(serde::Deserialize)]
pub struct HyphaeLock {
    pub schema: u32,
    pub source_sha: String,
    pub go: String,
    pub recipe: String,
    pub binaries: BTreeMap<String, Option<String>>,
}

impl HyphaeLock {
    /// Parses the lock file embedded in this binary at compile time.
    pub fn embedded() -> Result<Self, BinaryError> {
        serde_json::from_str(LOCK_JSON).map_err(|_| BinaryError::BadLockHash)
    }

    /// The expected digest for `platform`, or an error if the platform is
    /// absent or recorded as `null` (not yet verified for that platform).
    pub fn expected_for(&self, platform: &str) -> Result<Sha256Digest, BinaryError> {
        match self.binaries.get(platform) {
            Some(Some(hex)) => Sha256Digest::from_hex(hex),
            _ => Err(BinaryError::NoLockForPlatform(platform.to_string())),
        }
    }

    /// Test-only: builds a lock whose only entry is `platform -> sha`,
    /// standing in for the embedded lock. This lets tests exercise the real
    /// `install` → `expected_for` flow against a locally-built fake binary,
    /// or against a legitimate-but-unreproducible reference binary, without
    /// the production lock ever being overridable (there is no non-test path
    /// to this constructor).
    #[cfg(any(test, feature = "test-lock-override"))]
    pub fn override_for_test(platform: &str, sha: Sha256Digest) -> Self {
        let mut binaries = BTreeMap::new();
        binaries.insert(platform.to_string(), Some(sha.to_hex()));
        Self {
            schema: 1,
            source_sha: "test-override".to_string(),
            go: "test-override".to_string(),
            recipe: "test-override".to_string(),
            binaries,
        }
    }
}

/// A Hyphae binary whose bytes have been verified against an expected
/// sha256. The only way to construct one is [`VerifiedBinary::install`].
/// `Clone` is cheap (a `PathBuf` plus a 32-byte digest) and is used by
/// COMM-2b's `import` flow (`runner::HyphaeRunner::with_home`) to point a
/// second, throwaway [`crate::runner::HyphaeRunner`] at the same verified
/// binary but a different `HOME` (the import staging/verify directories)
/// without re-verifying or re-copying anything.
#[derive(Debug, Clone)]
pub struct VerifiedBinary {
    path: PathBuf,
    sha256: Sha256Digest,
}

impl VerifiedBinary {
    /// Reads `source` into memory once, hashes those bytes, and — only if
    /// they match `expected` — installs a verified copy into `install_dir`
    /// as `hyphae-<sha16>` (mode 0500, written via O_EXCL so two concurrent
    /// installs can't race each other into a half-written file).
    ///
    /// If a copy already exists at that path it is re-hashed on *every*
    /// call rather than trusted; a mismatch there is reported as
    /// [`BinaryError::HashMismatch`], never silently rewritten.
    pub async fn install(
        source: &Path,
        expected: Sha256Digest,
        install_dir: &Path,
    ) -> Result<Self, BinaryError> {
        if !source.is_absolute() {
            return Err(BinaryError::NotAbsolute(source.to_path_buf()));
        }
        if !install_dir.is_absolute() {
            return Err(BinaryError::NotAbsolute(install_dir.to_path_buf()));
        }
        let source = source.to_path_buf();
        let install_dir = install_dir.to_path_buf();
        tokio::task::spawn_blocking(move || install_blocking(&source, expected, &install_dir))
            .await
            .map_err(|e| BinaryError::Io(io::Error::other(e)))?
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sha256(&self) -> Sha256Digest {
        self.sha256
    }
}

fn install_blocking(
    source: &Path,
    expected: Sha256Digest,
    install_dir: &Path,
) -> Result<VerifiedBinary, BinaryError> {
    let bytes = fs::read(source).map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            BinaryError::Missing(source.to_path_buf())
        } else {
            BinaryError::Io(e)
        }
    })?;
    let actual = sha256_of(&bytes);
    if actual != expected {
        return Err(BinaryError::HashMismatch {
            expected: expected.to_hex(),
            actual: actual.to_hex(),
        });
    }

    fs::create_dir_all(install_dir).map_err(BinaryError::Io)?;
    let dest = install_dir.join(format!("hyphae-{}", expected.short()));

    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o500)
        .open(&dest)
    {
        Ok(mut file) => {
            use std::io::Write;
            file.write_all(&bytes).map_err(BinaryError::Io)?;
            file.flush().map_err(BinaryError::Io)?;
            drop(file);
            // `create_new` + `mode` is still subject to the process umask on
            // some platforms; pin the exact mode explicitly rather than rely
            // on it.
            fs::set_permissions(&dest, Permissions::from_mode(0o500)).map_err(BinaryError::Io)?;
            Ok(VerifiedBinary {
                path: dest,
                sha256: expected,
            })
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let existing = fs::read(&dest).map_err(BinaryError::Io)?;
            let existing_hash = sha256_of(&existing);
            if existing_hash == expected {
                Ok(VerifiedBinary {
                    path: dest,
                    sha256: expected,
                })
            } else {
                Err(BinaryError::HashMismatch {
                    expected: expected.to_hex(),
                    actual: existing_hash.to_hex(),
                })
            }
        }
        Err(e) => Err(BinaryError::Io(e)),
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BinaryError {
    #[error("path is not absolute: {0:?}")]
    NotAbsolute(PathBuf),
    #[error("binary not found: {0:?}")]
    Missing(PathBuf),
    #[error("hash mismatch: expected {expected}, got {actual}")]
    HashMismatch { expected: String, actual: String },
    #[error("hyphae.lock.json entry is not a valid sha256 hex string")]
    BadLockHash,
    #[error("no verified binary for platform {0}")]
    NoLockForPlatform(String),
    #[error("io error: {0}")]
    Io(#[source] io::Error),
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const SAMPLE_SHA: &str = "bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7";

    #[test]
    fn digest_hex_roundtrip() {
        let digest = Sha256Digest::from_hex(SAMPLE_SHA).unwrap();
        assert_eq!(digest.to_hex(), SAMPLE_SHA);
        assert_eq!(digest.short(), &SAMPLE_SHA[..16]);
    }

    #[test]
    fn digest_from_hex_rejects_garbage() {
        assert!(Sha256Digest::from_hex("not-hex").is_err());
        assert!(Sha256Digest::from_hex("ab").is_err()); // too short
    }

    #[test]
    fn debug_shows_hex_not_bytes() {
        let digest = Sha256Digest::from_hex(SAMPLE_SHA).unwrap();
        assert_eq!(format!("{digest:?}"), format!("Sha256Digest({SAMPLE_SHA})"));
    }

    #[test]
    fn embedded_lock_parses_and_resolves_baseline_platform() {
        let lock = HyphaeLock::embedded().unwrap();
        assert_eq!(lock.source_sha, "671c584f9e9eb807a15968e2aa42fd7507e178b8");
        let expected = lock.expected_for("darwin-arm64").unwrap();
        assert_eq!(
            expected.to_hex(),
            "d1171421e91ae62c40374bd00049cd51dd6ac1135b6cd7b31908968b9158df60"
        );
    }

    #[test]
    fn embedded_lock_reports_missing_platform() {
        let lock = HyphaeLock::embedded().unwrap();
        assert!(matches!(
            lock.expected_for("darwin-x64"),
            Err(BinaryError::NoLockForPlatform(p)) if p == "darwin-x64"
        ));
        assert!(matches!(
            lock.expected_for("freebsd-x64"),
            Err(BinaryError::NoLockForPlatform(_))
        ));
    }

    async fn write_fixture(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        tokio::fs::write(&path, content).await.unwrap();
        path
    }

    #[tokio::test]
    async fn install_rejects_relative_source() {
        let err = VerifiedBinary::install(
            Path::new("relative/hyphae"),
            Sha256Digest::from_hex(SAMPLE_SHA).unwrap(),
            Path::new("/tmp"),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, BinaryError::NotAbsolute(_)));
    }

    #[tokio::test]
    async fn install_accepts_matching_bytes_and_rejects_one_byte_change() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("src");
        let install_dir = tmp.path().join("bin");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();

        let content = b"hyphae-binary-fixture-bytes".to_vec();
        let expected = sha256_of(&content);
        let source = write_fixture(&source_dir, "hyphae", &content).await;

        let verified = VerifiedBinary::install(&source, expected, &install_dir)
            .await
            .unwrap();
        assert_eq!(verified.sha256(), expected);
        assert!(verified.path().starts_with(&install_dir));

        // Flip one byte: must be rejected, not silently accepted.
        let mut tampered = content.clone();
        tampered[0] ^= 0x01;
        let tampered_source = write_fixture(&source_dir, "hyphae-bad", &tampered).await;
        let err = VerifiedBinary::install(&tampered_source, expected, &install_dir)
            .await
            .unwrap_err();
        assert!(matches!(err, BinaryError::HashMismatch { .. }));
    }

    #[tokio::test]
    async fn install_reverifies_existing_copy_and_reuses_it() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("src");
        let install_dir = tmp.path().join("bin");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();

        let content = b"second-install-fixture".to_vec();
        let expected = sha256_of(&content);
        let source = write_fixture(&source_dir, "hyphae", &content).await;

        let first = VerifiedBinary::install(&source, expected, &install_dir)
            .await
            .unwrap();
        let second = VerifiedBinary::install(&source, expected, &install_dir)
            .await
            .unwrap();
        assert_eq!(first.path(), second.path());
    }

    #[tokio::test]
    async fn install_rejects_tampered_existing_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let source_dir = tmp.path().join("src");
        let install_dir = tmp.path().join("bin");
        tokio::fs::create_dir_all(&source_dir).await.unwrap();
        tokio::fs::create_dir_all(&install_dir).await.unwrap();

        let content = b"tamper-after-install".to_vec();
        let expected = sha256_of(&content);
        let source = write_fixture(&source_dir, "hyphae", &content).await;
        let verified = VerifiedBinary::install(&source, expected, &install_dir)
            .await
            .unwrap();

        // Simulate tampering with the installed copy after the fact: the
        // next install() call must re-hash it and reject the stale copy
        // rather than trusting the filename.
        let mut perms = tokio::fs::metadata(verified.path())
            .await
            .unwrap()
            .permissions();
        perms.set_mode(0o600);
        tokio::fs::set_permissions(verified.path(), perms)
            .await
            .unwrap();
        tokio::fs::write(verified.path(), b"corrupted bytes, same filename")
            .await
            .unwrap();

        let err = VerifiedBinary::install(&source, expected, &install_dir)
            .await
            .unwrap_err();
        assert!(matches!(err, BinaryError::HashMismatch { .. }));
    }

    #[tokio::test]
    async fn install_reports_missing_source() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("does-not-exist").join("hyphae");
        let install_dir = tmp.path().join("bin");
        let err = VerifiedBinary::install(
            &missing,
            Sha256Digest::from_hex(SAMPLE_SHA).unwrap(),
            &install_dir,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, BinaryError::Missing(_)));
    }
}
