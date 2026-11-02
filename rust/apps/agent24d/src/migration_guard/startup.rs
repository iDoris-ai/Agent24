//! Startup decision (design D5/D8, 5f-3): go on, or dock. Only reachable from
//! the coordinator path (`ENABLED=false`).
//!
//! Order: read the marker (bounded) -> replay the guard log (finishes an
//! interrupted restore, never migrates) -> fold into one answer. An existing
//! marker always docks, whatever the binary: a changed hash or semver is not
//! consent to retry (D8, T6), and nothing here checkpoints, snapshots or
//! migrates. A new marker is built only from stable codes
//! ([`DockCode::summary`]); no error text from replay reaches it.

use std::path::Path;

use super::atomic::FileOps;
use super::dock::{
    DbMark, DockCode, DockPhase, DockReason, MigrationMarker, MigrationsState, PairMark,
    RollbackState, Verdict, read_marker, write_marker,
};
use super::guard_log::{self, GuardLog};
use super::manifest::{
    BinaryIdentity, FORMAT_VERSION, MEMORY_FILE, Manifest, STORE_FILE, read_manifest, snapshot_dir,
};
use super::replay::startup_replay;
use super::restore::RestoreRequest;

/// A startup that must dock: the marker (real or in-memory) and its manifest.
#[derive(Clone, Debug)]
pub struct Docking {
    pub marker: MigrationMarker,
    pub manifest: Option<Manifest>,
}

/// `None` = no marker and the guard log allows startup to continue.
pub fn decide(
    ops: &dyn FileOps,
    data_dir: &Path,
    current: &dyn Fn() -> BinaryIdentity,
) -> Option<Docking> {
    let existing = read_marker(data_dir);
    let (store_db, memory_db) = (data_dir.join(STORE_FILE), data_dir.join(MEMORY_FILE));
    let req = RestoreRequest {
        data_dir,
        store_db: &store_db,
        memory_db: &memory_db,
    };
    // Replay first even with a marker: an unfinished restore is finished
    // (D8); a finished one is a read-only check.
    let verdict = startup_replay(ops, &req).verdict();
    let (marker, manifest) = match (existing, verdict) {
        (Ok(Some(m)), _) => {
            let manifest = manifest_of(data_dir, m.snapshot_id.as_deref());
            (m, manifest)
        }
        (Ok(None), Verdict::Proceed) => return None,
        (Ok(None), Verdict::Dock(reason)) => {
            let log = guard_log::read(data_dir).ok().flatten();
            let manifest = manifest_of(
                data_dir,
                log.as_ref().and_then(|l| l.snapshot_id.as_deref()),
            );
            let mut m = build_marker(reason, log.as_ref(), manifest.as_ref(), current());
            if let Err(err) = write_marker(ops, data_dir, &m) {
                // D4: report the persistence fault; the guard log still
                // blocks the next migration.
                tracing::error!("cannot persist migration marker: {err}");
                m.error_code = DockCode::PersistFailed;
                m.error_summary = DockCode::PersistFailed.summary().to_owned();
            }
            (m, manifest)
        }
        // Unreadable, oversize or unknown-format marker: evidence stays as is.
        (Err(_), _) => {
            let reason = DockReason {
                code: DockCode::MarkerInvalid,
                phase: DockPhase::Restoring,
                rollback: RollbackState::Failed,
                retry_allowed: false,
                migrations: MigrationsState::Unknown,
            };
            (build_marker(reason, None, None, current()), None)
        }
    };
    Some(Docking { marker, manifest })
}

fn manifest_of(data_dir: &Path, id: Option<&str>) -> Option<Manifest> {
    let id = id.filter(|id| id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))?;
    read_manifest(&snapshot_dir(data_dir, id).join("manifest.json")).ok()
}

/// The marker for a freshly computed dock. `error_summary` comes only from
/// the closed set keyed by `reason.code`.
fn build_marker(
    reason: DockReason,
    log: Option<&GuardLog>,
    manifest: Option<&Manifest>,
    current: BinaryIdentity,
) -> MigrationMarker {
    let mark = |existed: Option<bool>, version: Option<i64>| DbMark {
        existed: existed.unwrap_or(false),
        version,
    };
    let before = PairMark {
        store: mark(
            manifest.map(|m| m.store_db.existed),
            manifest.map(|m| m.watermark.store),
        ),
        memory: mark(
            manifest.map(|m| m.memory_db.existed),
            manifest.map(|m| m.watermark.memory),
        ),
    };
    // Restored / untouched = the pair equals the snapshot; anything else is unknown.
    let known = manifest.is_some()
        && matches!(
            reason.rollback,
            RollbackState::Restored | RollbackState::NotNeeded
        );
    MigrationMarker {
        format_version: FORMAT_VERSION,
        attempt_id: log.map_or_else(|| "unavailable".to_owned(), |l| l.attempt_id.clone()),
        snapshot_id: log.and_then(|l| l.snapshot_id.clone()),
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        failed_binary: log.map_or(current, |l| l.target.clone()),
        previous_binary: manifest.and_then(|m| m.source.clone()),
        database: None,
        migration_version: None,
        phase: reason.phase,
        error_code: reason.code,
        error_summary: reason.code.summary().to_owned(),
        rollback: reason.rollback,
        before_migrations: before,
        observed_migrations: known.then_some(before),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::cell::RefCell;
    use std::fs;
    use std::io;
    use std::path::Path;

    use super::super::atomic::{FileOps, RealOps};
    use super::super::dock::{MARKER_FILE, MARKER_MAX_BYTES, marker_path};
    use super::super::guard_log::{GuardLog, Phase, advance, persist};
    use super::super::manifest::test_empty_snapshot;
    use super::*;

    /// Counts every file operation: a startup that must not touch anything
    /// leaves it at zero.
    #[derive(Default)]
    struct CountingOps(RefCell<usize>);
    impl FileOps for CountingOps {
        fn write_new_synced(&self, p: &Path, d: &[u8]) -> io::Result<()> {
            *self.0.borrow_mut() += 1;
            RealOps.write_new_synced(p, d)
        }
        fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            *self.0.borrow_mut() += 1;
            RealOps.rename(a, b)
        }
        fn fsync_dir(&self, d: &Path) -> io::Result<()> {
            *self.0.borrow_mut() += 1;
            RealOps.fsync_dir(d)
        }
        fn remove_file(&self, p: &Path) -> io::Result<()> {
            *self.0.borrow_mut() += 1;
            RealOps.remove_file(p)
        }
    }

    fn id(sha: &str) -> BinaryIdentity {
        BinaryIdentity {
            version: "0.6.0".into(),
            sha256: sha.into(),
        }
    }

    /// A log that failed to restore, with hostile text in `last_error`.
    fn rollback_failed_log(dir: &Path, raw: &str) {
        let snap = test_empty_snapshot(dir, "a1");
        let mut log = GuardLog::new("a1".into(), id("old"));
        log.snapshot_id = Some(snap);
        persist(&RealOps, dir, &log).unwrap();
        for p in [Phase::Snapshotted, Phase::Migrating, Phase::Restoring] {
            advance(&RealOps, dir, &mut log, p).unwrap();
        }
        let next = log.failed_restore(raw.to_owned()).unwrap();
        persist(&RealOps, dir, &next).unwrap();
    }

    #[test]
    fn no_marker_and_no_log_proceeds_without_touching_disk() {
        let td = tempfile::tempdir().unwrap();
        let ops = CountingOps::default();
        assert!(decide(&ops, td.path(), &|| id("new")).is_none());
        assert_eq!(*ops.0.borrow(), 0);
        assert_eq!(fs::read_dir(td.path()).unwrap().count(), 0);
    }

    /// T6: the same marker docks again and again with no file operation, no
    /// database created; a different binary hash docks too.
    #[test]
    fn repeated_startup_with_a_marker_does_nothing_and_a_new_hash_still_docks() {
        let td = tempfile::tempdir().unwrap();
        rollback_failed_log(td.path(), "boom");
        let first = decide(&RealOps, td.path(), &|| id("old")).unwrap();
        assert_eq!(first.marker.error_code, DockCode::RollbackFailed);
        let ops = CountingOps::default();
        for sha in ["old", "old", "brand-new-hash"] {
            let again = decide(&ops, td.path(), &|| id(sha)).unwrap();
            assert_eq!(again.marker, first.marker, "the first failure stays");
        }
        assert_eq!(*ops.0.borrow(), 0, "no write, rename or remove");
        assert!(!td.path().join(STORE_FILE).exists());
        assert!(!td.path().join(MEMORY_FILE).exists());
    }

    /// Gate: the marker's summary is the closed-set text even when the
    /// replay error carries paths and SQL.
    #[test]
    fn summary_comes_only_from_the_closed_set() {
        let raw = "RAW-ERR /Users/jason/.agent24/agent24.db INSERT INTO t VALUES ('secret')";
        let td = tempfile::tempdir().unwrap();
        rollback_failed_log(td.path(), raw);
        let m = decide(&RealOps, td.path(), &|| id("x")).unwrap().marker;
        assert_eq!(m.error_summary, DockCode::RollbackFailed.summary());
        let on_disk = fs::read_to_string(marker_path(td.path())).unwrap();
        assert!(!on_disk.contains("RAW-ERR") && !on_disk.contains("secret"));
    }

    #[test]
    fn oversize_marker_is_marker_invalid_and_left_alone() {
        let td = tempfile::tempdir().unwrap();
        let big = vec![b' '; MARKER_MAX_BYTES as usize + 1];
        fs::write(td.path().join(MARKER_FILE), &big).unwrap();
        let ops = CountingOps::default();
        let d = decide(&ops, td.path(), &|| id("x")).unwrap();
        assert_eq!(d.marker.error_code, DockCode::MarkerInvalid);
        assert_eq!(d.marker.rollback, RollbackState::Failed);
        assert_eq!(*ops.0.borrow(), 0);
        assert_eq!(fs::read(td.path().join(MARKER_FILE)).unwrap(), big);
    }

    #[test]
    fn restored_marker_reports_the_snapshot_watermark() {
        let td = tempfile::tempdir().unwrap();
        let reason = DockReason {
            code: DockCode::MigrationFailed,
            phase: DockPhase::Migrating,
            rollback: RollbackState::Restored,
            retry_allowed: false,
            migrations: MigrationsState::Verified,
        };
        let snap = test_empty_snapshot(td.path(), "a1");
        let manifest = manifest_of(td.path(), Some(&snap)).unwrap();
        let m = build_marker(reason, None, Some(&manifest), id("x"));
        assert!(m.observed_migrations.is_some());
        let unknown = DockReason {
            rollback: RollbackState::Failed,
            ..reason
        };
        assert!(
            build_marker(unknown, None, Some(&manifest), id("x"))
                .observed_migrations
                .is_none()
        );
    }
}
