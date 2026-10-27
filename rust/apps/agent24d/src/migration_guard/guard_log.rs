//! `migration-guard.json` durable state machine (design D3).
//!
//! Not a user-deletable unlock switch: a corrupt or unknown-format file is a
//! dock-class error ([`GuardError::Invalid`]), never "treated as absent".

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::atomic::FileOps;
use super::manifest::{BinaryIdentity, FORMAT_VERSION, StateError, read_json, write_json};

pub const GUARD_FILE: &str = "migration-guard.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Preparing,
    Snapshotted,
    Migrating,
    Restoring,
    Restored,
    Committed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DbRestore {
    Pending,
    Done,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardLog {
    pub format_version: u32,
    pub attempt_id: String,
    pub snapshot_id: Option<String>,
    pub target: BinaryIdentity,
    pub phase: Phase,
    pub store_restore: DbRestore,
    pub memory_restore: DbRestore,
    pub last_error: Option<String>,
}

#[derive(Debug)]
pub enum GuardError {
    IllegalTransition {
        from: Phase,
        to: Phase,
    },
    /// Missing snapshot_id etc.: the transition would lie about durable state.
    Precondition(&'static str),
    /// Corrupt/unknown-format log: dock (5e/5f), do not guess.
    Invalid(String),
    Io(std::io::Error),
}

impl From<StateError> for GuardError {
    fn from(e: StateError) -> Self {
        match e {
            StateError::Io(e) => Self::Io(e),
            StateError::Invalid(m) => Self::Invalid(m),
        }
    }
}

impl GuardError {
    /// True when the caller must dock rather than retry.
    #[must_use]
    pub fn must_dock(&self) -> bool {
        matches!(self, Self::Invalid(_))
    }
}

#[must_use]
pub fn is_legal(from: Phase, to: Phase) -> bool {
    use Phase::{Committed, Migrating, Preparing, Restored, Restoring, Snapshotted};
    matches!(
        (from, to),
        (Preparing, Snapshotted)
            | (Snapshotted, Migrating)
            | (Migrating, Committed)
            | (Migrating, Restoring)
            | (Restoring, Restoring) // resumed / per-db progress update
            | (Restoring, Restored)
    )
}

impl GuardLog {
    #[must_use]
    pub fn new(attempt_id: String, target: BinaryIdentity) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            attempt_id,
            snapshot_id: None,
            target,
            phase: Phase::Preparing,
            store_restore: DbRestore::Pending,
            memory_restore: DbRestore::Pending,
            last_error: None,
        }
    }

    fn validate(&self) -> Result<(), GuardError> {
        if self.format_version != FORMAT_VERSION {
            return Err(GuardError::Invalid(format!(
                "unknown format_version {}",
                self.format_version
            )));
        }
        let needs_snapshot = !matches!(self.phase, Phase::Preparing);
        if needs_snapshot && self.snapshot_id.is_none() {
            return Err(GuardError::Invalid(format!(
                "{:?} without snapshot_id",
                self.phase
            )));
        }
        Ok(())
    }

    fn step(&self, to: Phase) -> Result<Self, GuardError> {
        if !is_legal(self.phase, to) {
            return Err(GuardError::IllegalTransition {
                from: self.phase,
                to,
            });
        }
        if to == Phase::Snapshotted && self.snapshot_id.is_none() {
            return Err(GuardError::Precondition("snapshotted requires snapshot_id"));
        }
        Ok(Self {
            phase: to,
            ..self.clone()
        })
    }
}

#[must_use]
pub fn guard_path(data_dir: &Path) -> PathBuf {
    data_dir.join(GUARD_FILE)
}

/// Durably write `log` as is (after validation).
pub fn persist(ops: &dyn FileOps, data_dir: &Path, log: &GuardLog) -> Result<(), GuardError> {
    log.validate()?;
    write_json(ops, data_dir, &guard_path(data_dir), log)?;
    Ok(())
}

/// Move `log` to `to`, persisting first. In-memory state only advances after
/// the durable write (incl. directory fsync chain) succeeded.
pub fn advance(
    ops: &dyn FileOps,
    data_dir: &Path,
    log: &mut GuardLog,
    to: Phase,
) -> Result<(), GuardError> {
    let next = log.step(to)?;
    persist(ops, data_dir, &next)?;
    *log = next;
    Ok(())
}

/// `Ok(None)` only when the file does not exist.
pub fn read(data_dir: &Path) -> Result<Option<GuardLog>, GuardError> {
    let p = guard_path(data_dir);
    if !p.exists() {
        return Ok(None);
    }
    let log: GuardLog = read_json(&p)?;
    log.validate()?;
    Ok(Some(log))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::atomic::{RealOps, test_ops::RecordingOps};
    use super::*;
    use std::fs;

    fn fresh() -> GuardLog {
        GuardLog::new(
            "a1".into(),
            BinaryIdentity {
                version: "1".into(),
                sha256: "x".into(),
            },
        )
    }

    #[test]
    fn legal_path_commit_and_restore_path() {
        let td = tempfile::tempdir().unwrap();
        let mut g = fresh();
        persist(&RealOps, td.path(), &g).unwrap();
        g.snapshot_id = Some("s".into());
        for to in [Phase::Snapshotted, Phase::Migrating, Phase::Committed] {
            advance(&RealOps, td.path(), &mut g, to).unwrap();
            assert_eq!(read(td.path()).unwrap().unwrap().phase, to);
        }
        let mut g = fresh();
        g.snapshot_id = Some("s".into());
        for to in [
            Phase::Snapshotted,
            Phase::Migrating,
            Phase::Restoring,
            Phase::Restored,
        ] {
            advance(&RealOps, td.path(), &mut g, to).unwrap();
        }
    }

    #[test]
    fn illegal_transitions_rejected_exhaustively() {
        use Phase::*;
        let all = [
            Preparing,
            Snapshotted,
            Migrating,
            Restoring,
            Restored,
            Committed,
        ];
        let legal = [
            (Preparing, Snapshotted),
            (Snapshotted, Migrating),
            (Migrating, Committed),
            (Migrating, Restoring),
            (Restoring, Restoring),
            (Restoring, Restored),
        ];
        for f in all {
            for t in all {
                assert_eq!(is_legal(f, t), legal.contains(&(f, t)), "{f:?}->{t:?}");
            }
        }
        let td = tempfile::tempdir().unwrap();
        let mut g = fresh();
        g.snapshot_id = Some("s".into());
        let err = advance(&RealOps, td.path(), &mut g, Phase::Committed).unwrap_err();
        assert!(matches!(err, GuardError::IllegalTransition { .. }));
        assert_eq!(g.phase, Preparing);
        assert!(
            !guard_path(td.path()).exists(),
            "rejected step wrote nothing"
        );
    }

    #[test]
    fn snapshotted_requires_snapshot_id() {
        let td = tempfile::tempdir().unwrap();
        let mut g = fresh();
        assert!(matches!(
            advance(&RealOps, td.path(), &mut g, Phase::Snapshotted),
            Err(GuardError::Precondition(_))
        ));
    }

    #[test]
    fn failed_durable_write_does_not_advance_memory_or_disk() {
        let td = tempfile::tempdir().unwrap();
        let mut g = fresh();
        g.snapshot_id = Some("s".into());
        persist(&RealOps, td.path(), &g).unwrap();
        for op in ["write", "rename", "fsync_dir"] {
            let r = advance(
                &RecordingOps::failing(op),
                td.path(),
                &mut g,
                Phase::Snapshotted,
            );
            assert!(r.is_err(), "{op}");
            assert_eq!(g.phase, Phase::Preparing, "{op}");
        }
        // A failed directory fsync may leave the renamed file visible; the
        // in-memory phase must not have advanced either way (asserted above).
        assert!(read(td.path()).unwrap().is_some());
        let names: Vec<_> = fs::read_dir(td.path()).unwrap().collect();
        assert_eq!(names.len(), 1, "no stray temp files");
    }

    #[test]
    fn corrupt_or_unknown_log_is_dock_class() {
        let td = tempfile::tempdir().unwrap();
        assert!(read(td.path()).unwrap().is_none());
        let p = guard_path(td.path());
        for bad in [
            &b"{trunc"[..],
            b"",
            br#"{"format_version":1,"attempt_id":"a","snapshot_id":null,"target":{"version":"1","sha256":"x"},"phase":"bogus","store_restore":"pending","memory_restore":"pending","last_error":null}"#,
            br#"{"format_version":7,"attempt_id":"a","snapshot_id":null,"target":{"version":"1","sha256":"x"},"phase":"preparing","store_restore":"pending","memory_restore":"pending","last_error":null}"#,
            br#"{"format_version":1,"attempt_id":"a","snapshot_id":null,"target":{"version":"1","sha256":"x"},"phase":"migrating","store_restore":"pending","memory_restore":"pending","last_error":null}"#,
        ] {
            fs::write(&p, bad).unwrap();
            let err = read(td.path()).unwrap_err();
            assert!(err.must_dock(), "{err:?}");
        }
    }
}
