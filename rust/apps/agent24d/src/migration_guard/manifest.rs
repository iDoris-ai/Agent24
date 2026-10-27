//! Snapshot manifest and `migration-last-success.json` (design D2).
//!
//! Layout: `<data_dir>/migration-snapshots/<UTC compact>-<attempt_id>/`
//! (0700, files 0600; directory creation is 5d). This module owns the data
//! model, naming, hashing and the durable atomic writes.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::atomic::{FileOps, atomic_write};

pub const FORMAT_VERSION: u32 = 1;
pub const SNAPSHOTS_DIR: &str = "migration-snapshots";
pub const STORE_FILE: &str = "agent24.db";
pub const MEMORY_FILE: &str = "memory.db";
pub const LAST_SUCCESS_FILE: &str = "migration-last-success.json";

#[derive(Debug)]
pub enum StateError {
    Io(io::Error),
    /// Corrupt or unknown-format file: caller must dock, never guess.
    Invalid(String),
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "io: {e}"),
            Self::Invalid(m) => write!(f, "invalid state file: {m}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryIdentity {
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DbRecord {
    pub existed: bool,
    pub len: Option<u64>,
    pub sha256: Option<String>,
}

/// Highest successful migration version per database before migrating.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Watermark {
    pub store: i64,
    pub memory: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format_version: u32,
    pub attempt_id: String,
    /// RFC 3339 UTC.
    pub created_at: String,
    /// `None` on first install or legacy install without a success record.
    pub source: Option<BinaryIdentity>,
    pub target: BinaryIdentity,
    pub store_db: DbRecord,
    pub memory_db: DbRecord,
    pub migrations_digest: String,
    pub watermark: Watermark,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LastSuccess {
    pub format_version: u32,
    pub binary: BinaryIdentity,
    pub watermark: Watermark,
}

/// `<UTC compact>-<attempt_id>`; attempt_id must be plain `[A-Za-z0-9]`.
pub fn snapshot_dir_name(compact_utc: &str, attempt_id: &str) -> Result<String, StateError> {
    let ok = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric());
    if !ok(attempt_id) || !ok(compact_utc.trim_end_matches('Z')) {
        return Err(StateError::Invalid("unsafe snapshot name component".into()));
    }
    Ok(format!("{compact_utc}-{attempt_id}"))
}

#[must_use]
pub fn snapshot_dir(data_dir: &Path, name: &str) -> PathBuf {
    data_dir.join(SNAPSHOTS_DIR).join(name)
}

pub fn sha256_file(path: &Path) -> io::Result<(u64, String)> {
    let mut f = fs::File::open(path)?;
    let (mut h, mut len, mut buf) = (Sha256::new(), 0u64, vec![0u8; 1 << 16]);
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        len += n as u64;
    }
    Ok((len, format!("{:x}", h.finalize())))
}

pub fn write_manifest(
    ops: &dyn FileOps,
    data_dir: &Path,
    snapshot_dir: &Path,
    m: &Manifest,
) -> Result<(), StateError> {
    write_json(ops, data_dir, &snapshot_dir.join("manifest.json"), m)
}

pub fn read_manifest(path: &Path) -> Result<Manifest, StateError> {
    let m: Manifest = read_json(path)?;
    check_version(m.format_version)?;
    Ok(m)
}

/// A snapshot authorises migration only when this passes: the manifest is
/// readable, belongs to `attempt_id`, and each recorded database file matches
/// its length and SHA-256 (a database recorded as missing must be absent).
pub fn verify_complete(
    data_dir: &Path,
    snapshot_id: &str,
    attempt_id: &str,
) -> Result<Manifest, StateError> {
    if !snapshot_id
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        return Err(StateError::Invalid("unsafe snapshot id".into()));
    }
    let dir = snapshot_dir(data_dir, snapshot_id);
    let m = read_manifest(&dir.join("manifest.json"))?;
    if m.attempt_id != attempt_id || !snapshot_id.ends_with(&format!("-{attempt_id}")) {
        return Err(StateError::Invalid("manifest/attempt mismatch".into()));
    }
    for (file, rec) in [(STORE_FILE, &m.store_db), (MEMORY_FILE, &m.memory_db)] {
        let p = dir.join(file);
        let ok = if rec.existed {
            sha256_file(&p).map_err(StateError::Io)?
                == (
                    rec.len.unwrap_or(u64::MAX),
                    rec.sha256.clone().unwrap_or_default(),
                )
        } else {
            !p.exists()
        };
        if !ok {
            return Err(StateError::Invalid(format!(
                "{file} does not match manifest"
            )));
        }
    }
    Ok(m)
}

pub fn write_last_success(
    ops: &dyn FileOps,
    data_dir: &Path,
    v: &LastSuccess,
) -> Result<(), StateError> {
    write_json(ops, data_dir, &data_dir.join(LAST_SUCCESS_FILE), v)
}

/// `Ok(None)` when absent (legacy install); corrupt or unknown => `Invalid`.
pub fn read_last_success(data_dir: &Path) -> Result<Option<LastSuccess>, StateError> {
    let p = data_dir.join(LAST_SUCCESS_FILE);
    if !p.exists() {
        return Ok(None);
    }
    let v: LastSuccess = read_json(&p)?;
    check_version(v.format_version)?;
    Ok(Some(v))
}

fn check_version(v: u32) -> Result<(), StateError> {
    if v == FORMAT_VERSION {
        Ok(())
    } else {
        Err(StateError::Invalid(format!("unknown format_version {v}")))
    }
}

pub(super) fn write_json<T: Serialize>(
    ops: &dyn FileOps,
    data_dir: &Path,
    path: &Path,
    v: &T,
) -> Result<(), StateError> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| StateError::Invalid(e.to_string()))?;
    atomic_write(ops, data_dir, path, &bytes).map_err(StateError::Io)
}

pub(super) fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, StateError> {
    let bytes = fs::read(path).map_err(StateError::Io)?;
    serde_json::from_slice(&bytes).map_err(|e| StateError::Invalid(e.to_string()))
}

/// Test helper: a complete snapshot of two absent databases.
#[cfg(test)]
pub(crate) fn test_empty_snapshot(root: &Path, attempt_id: &str) -> String {
    let id = format!("20261027T000000Z-{attempt_id}");
    let dir = snapshot_dir(root, &id);
    fs::create_dir_all(&dir).unwrap_or_default();
    let none = DbRecord {
        existed: false,
        len: None,
        sha256: None,
    };
    let bin = BinaryIdentity {
        version: "1".into(),
        sha256: "x".into(),
    };
    let m = Manifest {
        format_version: FORMAT_VERSION,
        attempt_id: attempt_id.into(),
        created_at: "2026-10-27T00:00:00Z".into(),
        source: None,
        target: bin,
        store_db: none.clone(),
        memory_db: none,
        migrations_digest: String::new(),
        watermark: Watermark {
            store: 0,
            memory: 0,
        },
    };
    write_manifest(&super::atomic::RealOps, root, &dir, &m).unwrap_or_default();
    id
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::atomic::{RealOps, test_ops::RecordingOps};
    use super::*;

    fn sample() -> Manifest {
        let id = BinaryIdentity {
            version: "0.5.1".into(),
            sha256: "ab".into(),
        };
        Manifest {
            format_version: FORMAT_VERSION,
            attempt_id: "a1".into(),
            created_at: "2026-10-27T00:00:00Z".into(),
            source: None,
            target: id,
            store_db: DbRecord {
                existed: true,
                len: Some(3),
                sha256: Some("cd".into()),
            },
            memory_db: DbRecord {
                existed: false,
                len: None,
                sha256: None,
            },
            migrations_digest: "dd".into(),
            watermark: Watermark {
                store: 18,
                memory: 17,
            },
        }
    }

    #[test]
    fn manifest_roundtrip_and_fsync_chain_to_data_dir() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let dir = snapshot_dir(root, &snapshot_dir_name("20261027T000000Z", "a1").unwrap());
        fs::create_dir_all(&dir).unwrap();
        let ops = RecordingOps::default();
        write_manifest(&ops, root, &dir, &sample()).unwrap();
        assert_eq!(read_manifest(&dir.join("manifest.json")).unwrap(), sample());
        let fsyncs = ops
            .calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("fsync_dir"))
            .count();
        assert_eq!(fsyncs, 3, "attempt dir, migration-snapshots, data_dir");
    }

    #[test]
    fn last_success_roundtrip_absent_and_corrupt() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        assert!(read_last_success(root).unwrap().is_none());
        let v = LastSuccess {
            format_version: FORMAT_VERSION,
            binary: sample().target,
            watermark: Watermark {
                store: 1,
                memory: 2,
            },
        };
        write_last_success(&RealOps, root, &v).unwrap();
        assert_eq!(read_last_success(root).unwrap(), Some(v));
        fs::write(root.join(LAST_SUCCESS_FILE), b"{trunc").unwrap();
        assert!(matches!(
            read_last_success(root),
            Err(StateError::Invalid(_))
        ));
    }

    #[test]
    fn unknown_version_and_unknown_fields_rejected() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("manifest.json");
        let mut m = sample();
        m.format_version = 99;
        fs::write(&p, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(matches!(read_manifest(&p), Err(StateError::Invalid(_))));
        let mut j = serde_json::to_value(sample()).unwrap();
        j["extra"] = 1.into();
        fs::write(&p, serde_json::to_vec(&j).unwrap()).unwrap();
        assert!(matches!(read_manifest(&p), Err(StateError::Invalid(_))));
    }

    #[test]
    fn manifest_write_failure_leaves_no_manifest() {
        let td = tempfile::tempdir().unwrap();
        for op in ["write", "rename"] {
            let r = write_manifest(&RecordingOps::failing(op), td.path(), td.path(), &sample());
            assert!(r.is_err());
            assert_eq!(fs::read_dir(td.path()).unwrap().count(), 0, "{op}");
        }
    }

    #[test]
    fn name_components_are_sanitised_and_hash_matches() {
        assert!(snapshot_dir_name("20261027T000000Z", "../x").is_err());
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("f");
        fs::write(&p, b"abc").unwrap();
        let (len, h) = sha256_file(&p).unwrap();
        assert_eq!(len, 3);
        assert!(h.starts_with("ba7816bf"));
    }
}
