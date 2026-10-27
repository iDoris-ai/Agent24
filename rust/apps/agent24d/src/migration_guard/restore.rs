//! Whole-pair restorer (design D4, 5e-1). Not wired (`ENABLED=false`).
//!
//! Per database: rebuild `<db>.restore.<attempt_id>` from the verified
//! snapshot (hash-checked, fsynced), move the failed main file and every
//! `-wal`/`-shm`/`-journal` into `<data_dir>/migration-quarantine/<attempt>/`
//! (never deleted, never overwritten), fsync, rename staging into place,
//! fsync the directory chain, persist that database's progress. Restore is
//! idempotent: every step is re-derived from disk, nothing trusts memory.
//!
//! There is no `rollback_failed` phase in `GuardLog`: it is `Restoring` plus
//! `last_error` and a returned [`RestoreError::RollbackFailed`]; the marker
//! itself is 5f. Never an `Ok` after a partial restore.
//!
//! Caller contract (5g wiring): the function does blocking IO (hashing, copy
//! of whole databases) so run it inside `spawn_blocking`; and every pool and
//! connection to both databases must already be closed and awaited (5b).
//! The snapshot is only read: it is never consumed by a restore.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use super::atomic::{FileOps, fsync_chain};
use super::guard_log::{DbRestore, GuardError, GuardLog, Phase, advance, persist};
use super::manifest::{
    DbRecord, MEMORY_FILE, STORE_FILE, sha256_file, snapshot_dir, verify_complete,
};
use super::snapshot::exists_strict;

pub const QUARANTINE_DIR: &str = "migration-quarantine";
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

#[derive(Debug)]
pub struct RestoreRequest<'a> {
    pub data_dir: &'a Path,
    pub store_db: &'a Path,
    pub memory_db: &'a Path,
}

#[derive(Debug)]
pub enum RestoreError {
    /// Log phase cannot be restored (Preparing/Snapshotted/Committed): untouched.
    NotRestorable(Phase),
    /// Could not persist `Restoring`: no file was touched.
    Guard(GuardError),
    /// Restore did not complete. Snapshot, quarantine and progress are kept;
    /// both databases stay disabled. `persisted` = error reached the log.
    RollbackFailed { reason: String, persisted: bool },
}

type R<T> = Result<T, String>;

fn e<E: std::fmt::Debug>(what: &str, err: E) -> String {
    format!("{what}: {err:?}")
}

/// Restore both databases from the snapshot named by `log`. Returns the
/// number of quarantined sidecars that could not be deleted (evidence stays;
/// harmless, startup recovery retries).
pub fn restore(
    ops: &dyn FileOps,
    req: &RestoreRequest<'_>,
    log: &mut GuardLog,
) -> Result<usize, RestoreError> {
    match log.phase {
        Phase::Migrating => {
            advance(ops, req.data_dir, log, Phase::Restoring).map_err(RestoreError::Guard)?
        }
        Phase::Restoring | Phase::Restored => {} // resume / idempotent re-run
        p => return Err(RestoreError::NotRestorable(p)),
    }
    match run(ops, req, log) {
        Ok(()) => {
            let q = req.data_dir.join(QUARANTINE_DIR).join(&log.attempt_id);
            Ok(drop_quarantined_sidecars(ops, &q))
        }
        Err(reason) => {
            let mut next = log.clone();
            next.last_error = Some(reason.clone());
            // A Restored log must not be rewritten with an error.
            let persisted =
                log.phase == Phase::Restoring && persist(ops, req.data_dir, &next).is_ok();
            if persisted {
                *log = next;
            }
            Err(RestoreError::RollbackFailed { reason, persisted })
        }
    }
}

fn run(ops: &dyn FileOps, req: &RestoreRequest<'_>, log: &mut GuardLog) -> R<()> {
    let id = log
        .snapshot_id
        .clone()
        .ok_or("restoring without snapshot_id")?;
    let m = verify_complete(req.data_dir, &id, &log.attempt_id).map_err(|x| e("snapshot", x))?;
    let snap = snapshot_dir(req.data_dir, &id);
    let q = req.data_dir.join(QUARANTINE_DIR).join(&log.attempt_id);
    let dbs = [
        (STORE_FILE, req.store_db, &m.store_db),
        (MEMORY_FILE, req.memory_db, &m.memory_db),
    ];
    for (file, path, rec) in dbs {
        let store = file == STORE_FILE;
        if (if store {
            log.store_restore
        } else {
            log.memory_restore
        }) == DbRestore::Pending
        {
            restore_one(
                ops,
                req.data_dir,
                &log.attempt_id,
                path,
                rec,
                &snap.join(file),
                &q,
            )?;
            let mut next = log.clone();
            *(if store {
                &mut next.store_restore
            } else {
                &mut next.memory_restore
            }) = DbRestore::Done;
            persist(ops, req.data_dir, &next).map_err(|x| e("progress", x))?;
            *log = next;
        }
    }
    // Only a snapshot-identical, sidecar-free formal pair may be called restored.
    for (_, path, rec) in dbs {
        check_final(path, rec)?;
    }
    if log.phase == Phase::Restoring {
        advance(ops, req.data_dir, log, Phase::Restored).map_err(|x| e("restored", x))?;
    }
    Ok(())
}

fn with_suffix(p: &Path, sfx: &str) -> PathBuf {
    PathBuf::from(format!("{}{sfx}", p.display()))
}

fn present(p: &Path) -> R<bool> {
    exists_strict(p).map_err(|x| e(&p.display().to_string(), x))
}

fn check_final(path: &Path, rec: &DbRecord) -> R<()> {
    let exists = present(path)?;
    if exists != rec.existed {
        return Err(format!(
            "{} existence differs from snapshot",
            path.display()
        ));
    }
    if exists
        && sha256_file(path).map_err(|x| e("hash", x))?
            != (
                rec.len.unwrap_or(u64::MAX),
                rec.sha256.clone().unwrap_or_default(),
            )
    {
        return Err(format!("{} differs from snapshot", path.display()));
    }
    for sfx in SIDECARS {
        if present(&with_suffix(path, sfx))? {
            return Err(format!("{}{sfx} survived restore", path.display()));
        }
    }
    Ok(())
}

fn remove_if_present(ops: &dyn FileOps, p: &Path) -> R<()> {
    match ops.remove_file(p) {
        Ok(()) => Ok(()),
        Err(x) if x.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(x) => Err(e(&format!("remove {}", p.display()), x)),
    }
}

fn restore_one(
    ops: &dyn FileOps,
    data_dir: &Path,
    attempt: &str,
    path: &Path,
    rec: &DbRecord,
    snap_file: &Path,
    q: &Path,
) -> R<()> {
    let parent = path.parent().ok_or("database path has no parent")?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or("database name not utf-8")?;
    let staging = parent.join(format!("{name}.restore.{attempt}"));
    // A leftover staging file is untrusted (crash mid-copy): always rebuild.
    remove_if_present(ops, &staging)?;
    if rec.existed {
        ops.copy_new_synced(snap_file, &staging)
            .map_err(|x| e("stage copy", x))?;
        let got = sha256_file(&staging).map_err(|x| e("stage hash", x))?;
        if got
            != (
                rec.len.unwrap_or(u64::MAX),
                rec.sha256.clone().unwrap_or_default(),
            )
        {
            return Err(format!(
                "staging {} differs from snapshot",
                staging.display()
            ));
        }
    }
    // Main first, then sidecars; every one that exists, whatever its state.
    let mut moved = false;
    for sfx in ["", "-wal", "-shm", "-journal"] {
        let src = with_suffix(path, sfx);
        if present(&src)? {
            if !moved {
                fs::DirBuilder::new()
                    .recursive(true)
                    .mode(0o700)
                    .create(q)
                    .map_err(|x| e("quarantine dir", x))?;
            }
            moved = true;
            let dest = free_name(q, &format!("{name}{sfx}"))?;
            ops.rename(&src, &dest).map_err(|x| e("quarantine", x))?;
        }
    }
    if moved {
        fsync_chain(ops, data_dir, q).map_err(|x| e("fsync quarantine", x))?;
        fsync_chain(ops, data_dir, parent).map_err(|x| e("fsync db dir", x))?;
    }
    if rec.existed {
        ops.rename(&staging, path).map_err(|x| e("install", x))?;
    }
    fsync_chain(ops, data_dir, parent).map_err(|x| e("fsync install", x))
}

/// First unused name in `q`: earlier evidence is never overwritten.
fn free_name(q: &Path, base: &str) -> R<PathBuf> {
    for i in 0..1000 {
        let p = q.join(if i == 0 {
            base.to_owned()
        } else {
            format!("{base}.{i}")
        });
        if !present(&p)? {
            return Ok(p);
        }
    }
    Err(format!("no free quarantine name for {base}"))
}

/// After `Restored` is durable the failed WAL/SHM/journal copies are junk.
fn drop_quarantined_sidecars(ops: &dyn FileOps, q: &Path) -> usize {
    let Ok(rd) = fs::read_dir(q) else { return 0 };
    let mut bad = 0;
    for ent in rd.flatten() {
        let n = ent.file_name().to_string_lossy().into_owned();
        if SIDECARS.iter().any(|s| n.contains(&format!("db{s}")))
            && remove_if_present(ops, &ent.path()).is_err()
        {
            bad += 1;
        }
    }
    let _ = ops.fsync_dir(q);
    bad
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::atomic::{FileOps, RealOps};
    use super::super::guard_log::{self, GuardLog};
    use super::super::manifest::{BinaryIdentity, snapshot_dir};
    use super::super::snapshot::{SnapshotRequest, take_snapshot};
    use super::*;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode};
    use sqlx::{Connection, Row, SqliteConnection};
    use std::cell::{Cell, RefCell};
    use std::io;

    /// Counts every mutating call, records them, fails the `fail_at`-th.
    struct Probe {
        calls: RefCell<Vec<String>>,
        n: Cell<usize>,
        fail_at: Option<usize>,
    }
    impl Probe {
        fn new(fail_at: Option<usize>) -> Self {
            Self {
                calls: RefCell::default(),
                n: Cell::new(0),
                fail_at,
            }
        }
        fn hit(&self, op: &str, p: &Path) -> io::Result<()> {
            let i = self.n.get();
            self.n.set(i + 1);
            self.calls
                .borrow_mut()
                .push(format!("{op}:{}", p.display()));
            if self.fail_at == Some(i) {
                Err(io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    }
    impl FileOps for Probe {
        fn write_new_synced(&self, p: &Path, d: &[u8]) -> io::Result<()> {
            self.hit("write", p)?;
            RealOps.write_new_synced(p, d)
        }
        fn rename(&self, a: &Path, b: &Path) -> io::Result<()> {
            self.hit("rename", b)?;
            RealOps.rename(a, b)
        }
        fn fsync_dir(&self, d: &Path) -> io::Result<()> {
            self.hit("fsync_dir", d)?;
            RealOps.fsync_dir(d)
        }
        fn remove_file(&self, p: &Path) -> io::Result<()> {
            self.hit("remove", p)?;
            RealOps.remove_file(p)
        }
        fn copy_new_synced(&self, a: &Path, b: &Path) -> io::Result<()> {
            self.hit("copy", b)?;
            RealOps.copy_new_synced(a, b)
        }
    }

    async fn conn(p: &Path) -> SqliteConnection {
        let o = SqliteConnectOptions::new()
            .filename(p)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .pragma("wal_autocheckpoint", "0");
        SqliteConnection::connect_with(&o).await.unwrap()
    }
    async fn exec(c: &mut SqliteConnection, sql: &str) {
        sqlx::query(sql).execute(c).await.unwrap();
    }
    /// (rows, schema, watermark) of a closed database; opens a *copy* so the
    /// checked file's header is never rewritten.
    async fn logical(p: &Path) -> (Vec<i64>, Vec<String>, i64) {
        let tmp = p.with_extension("probe");
        fs::copy(p, &tmp).unwrap();
        let mut c = conn(&tmp).await;
        let r = sqlx::query("SELECT x FROM t ORDER BY x")
            .fetch_all(&mut c)
            .await
            .unwrap();
        let s = sqlx::query("SELECT sql FROM sqlite_master ORDER BY name")
            .fetch_all(&mut c)
            .await
            .unwrap();
        let w: i64 = sqlx::query("SELECT MAX(version) FROM _sqlx_migrations")
            .fetch_one(&mut c)
            .await
            .unwrap()
            .get(0);
        let out = (
            r.iter().map(|x| x.get(0)).collect(),
            s.iter().map(|x| x.get(0)).collect(),
            w,
        );
        c.close().await.unwrap();
        for x in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(with_suffix(&tmp, x));
        }
        out
    }

    struct Fx {
        _td: tempfile::TempDir,
        root: PathBuf,
        s: PathBuf,
        m: PathBuf,
        snap: PathBuf,
        log: GuardLog,
        before: [(Vec<i64>, Vec<String>, i64); 2],
    }
    impl Fx {
        fn go(&mut self, ops: &dyn FileOps) -> Result<usize, RestoreError> {
            let req = RestoreRequest {
                data_dir: &self.root,
                store_db: &self.s,
                memory_db: &self.m,
            };
            restore(ops, &req, &mut self.log)
        }
    }

    /// Pre-migration pair (memory optional), snapshot, `migrating` log, then a
    /// "failed migration": extra rows + DDL left in an un-checkpointed WAL
    /// (crash image) plus a stray -journal, or a brand-new memory.db.
    async fn fixture(memory_existed: bool) -> Fx {
        let td = tempfile::tempdir().unwrap();
        let root = td.path().to_path_buf();
        let (s, m) = (root.join(STORE_FILE), root.join(MEMORY_FILE));
        let mut before = [(vec![], vec![], 0), (vec![], vec![], 0)];
        for (i, p) in [&s, &m].into_iter().enumerate() {
            if i == 1 && !memory_existed {
                continue;
            }
            let mut c = conn(p).await;
            exec(&mut c, "CREATE TABLE t(x INTEGER)").await;
            exec(
                &mut c,
                "CREATE TABLE _sqlx_migrations(version INTEGER, success BOOLEAN)",
            )
            .await;
            exec(&mut c, "INSERT INTO _sqlx_migrations VALUES (5, 1)").await;
            exec(&mut c, "INSERT INTO t VALUES (1),(2)").await;
            c.close().await.unwrap();
            before[i] = logical(p).await;
        }
        let target = BinaryIdentity {
            version: "2".into(),
            sha256: "y".into(),
        };
        let req = SnapshotRequest {
            data_dir: &root,
            store_db: &s,
            memory_db: &m,
            attempt_id: "a1",
            compact_utc: "20261027T000000Z",
            created_at: "2026-10-27T00:00:00Z",
            source: None,
            target: target.clone(),
            migrations_digest: "dd",
        };
        let (id, _) = take_snapshot(&RealOps, &req).await.unwrap();
        let mut log = GuardLog::new("a1".into(), target);
        guard_log::persist(&RealOps, &root, &log).unwrap();
        log.snapshot_id = Some(id.clone());
        for to in [Phase::Snapshotted, Phase::Migrating] {
            advance(&RealOps, &root, &mut log, to).unwrap();
        }
        // failed migration residue
        let mut c = conn(&s).await;
        exec(&mut c, "INSERT INTO t VALUES (99)").await;
        exec(&mut c, "ALTER TABLE t ADD COLUMN junk TEXT").await;
        exec(&mut c, "UPDATE _sqlx_migrations SET version = 6").await;
        for x in ["", "-wal", "-shm"] {
            fs::copy(with_suffix(&s, x), with_suffix(&root.join("crash.db"), x)).unwrap();
        }
        c.close().await.unwrap();
        for x in ["", "-wal", "-shm"] {
            let _ = fs::remove_file(with_suffix(&s, x));
            fs::rename(with_suffix(&root.join("crash.db"), x), with_suffix(&s, x)).unwrap();
        }
        assert!(
            fs::metadata(with_suffix(&s, "-wal")).unwrap().len() > 0,
            "fixture needs WAL"
        );
        fs::write(with_suffix(&s, "-journal"), b"stale").unwrap();
        if !memory_existed {
            let mut c = conn(&m).await; // migration created a new, unwanted db
            exec(&mut c, "CREATE TABLE new(x)").await;
            c.close().await.unwrap();
            fs::write(with_suffix(&m, "-wal"), b"stale").unwrap();
        }
        let snap = snapshot_dir(&root, &id);
        Fx {
            _td: td,
            root,
            s,
            m,
            snap,
            log,
            before,
        }
    }

    fn assert_restored(fx: &Fx) {
        assert_eq!(
            fs::read(&fx.s).unwrap(),
            fs::read(fx.snap.join(STORE_FILE)).unwrap(),
            "T2 bytes"
        );
        for p in [&fx.s, &fx.m] {
            for x in SIDECARS {
                assert!(!with_suffix(p, x).exists(), "T3 {}{x}", p.display());
            }
        }
    }

    #[tokio::test]
    async fn t1_t2_t3_restore_bytes_logic_and_no_sidecars() {
        let mut fx = fixture(true).await;
        let leftovers = fx.go(&RealOps).unwrap();
        assert_eq!(leftovers, 0);
        assert_restored(&fx);
        assert_eq!(
            fs::read(&fx.m).unwrap(),
            fs::read(fx.snap.join(MEMORY_FILE)).unwrap()
        );
        assert_eq!(logical(&fx.s).await, fx.before[0], "T2 logical store");
        assert_eq!(logical(&fx.m).await, fx.before[1], "T2 logical memory");
        let g = guard_log::read(&fx.root).unwrap().unwrap();
        assert_eq!(
            (g.phase, g.store_restore, g.memory_restore),
            (Phase::Restored, DbRestore::Done, DbRestore::Done)
        );
        // evidence kept, junk sidecars deleted, snapshot intact
        let q = fx.root.join(QUARANTINE_DIR).join("a1");
        assert_eq!(
            fs::read_dir(&q).unwrap().count(),
            1 + 1,
            "failed main files only"
        );
        assert!(q.join(STORE_FILE).exists());
        assert!(verify_complete(&fx.root, fx.log.snapshot_id.as_deref().unwrap(), "a1").is_ok());
        assert!(!fx.s.with_file_name("agent24.db.restore.a1").exists());
        // idempotent re-run on a Restored log
        fx.go(&RealOps).unwrap();
        assert_restored(&fx);
    }

    #[tokio::test]
    async fn t2_missing_database_stays_missing() {
        let mut fx = fixture(false).await;
        fx.go(&RealOps).unwrap();
        assert!(
            !fx.m.exists(),
            "database absent in snapshot must stay absent"
        );
        assert_restored(&fx);
        assert_eq!(logical(&fx.s).await, fx.before[0]);
    }

    /// T3 ordering: sidecars leave the db dir and that move is fsynced before
    /// the install rename; the install is fsynced; progress follows.
    #[tokio::test]
    async fn t3_fsync_order_around_install() {
        let mut fx = fixture(true).await;
        let p = Probe::new(None);
        fx.go(&p).unwrap();
        let c = p.calls.borrow();
        let pos = |f: &dyn Fn(&String) -> bool| c.iter().position(f).unwrap();
        let root = fx.root.display().to_string();
        let install = pos(&|x| x == &format!("rename:{}", fx.s.display()));
        let last_quarantine = c
            .iter()
            .take(install)
            .rposition(|x| x.contains("migration-quarantine"))
            .unwrap();
        let dir_sync_between = c[last_quarantine..install]
            .iter()
            .any(|x| x == &format!("fsync_dir:{root}"));
        assert!(
            dir_sync_between,
            "db dir fsynced after quarantine, before install: {c:?}"
        );
        assert_eq!(
            c[install + 1],
            format!("fsync_dir:{root}"),
            "fsync right after install"
        );
    }

    /// T8 + T9: fail every single mutating call in turn. Each failure is
    /// explicit (RollbackFailed, never Restored, snapshot kept, no ready); a
    /// retry then converges to the exact snapshot (idempotent).
    #[tokio::test]
    async fn t8_t9_fail_at_every_step_then_retry_converges() {
        let total = {
            let mut fx = fixture(true).await;
            let p = Probe::new(None);
            fx.go(&p).unwrap();
            p.n.get()
        };
        assert!(total > 20, "{total}");
        for n in 0..total {
            let mut fx = fixture(true).await;
            let r = fx.go(&Probe::new(Some(n)));
            let Err(RestoreError::RollbackFailed { reason, .. }) = r else {
                match r {
                    // n hit the Migrating->Restoring persist: nothing touched
                    Err(RestoreError::Guard(_)) => assert_eq!(fx.log.phase, Phase::Migrating),
                    // n hit the post-Restored junk-sidecar cleanup: still restored
                    Ok(_) => assert_eq!(fx.log.phase, Phase::Restored, "step {n}"),
                    other => panic!("step {n}: {other:?}"),
                }
                fx.go(&RealOps).unwrap();
                assert_restored(&fx);
                continue;
            };
            assert!(!reason.is_empty());
            assert_ne!(fx.log.phase, Phase::Restored, "step {n} pretended success");
            assert!(
                verify_complete(&fx.root, fx.log.snapshot_id.as_deref().unwrap(), "a1").is_ok(),
                "step {n}"
            );
            // fresh process: reload the log from disk, not from memory
            fx.log = guard_log::read(&fx.root).unwrap().unwrap();
            assert_eq!(fx.log.phase, Phase::Restoring, "step {n}");
            fx.go(&RealOps)
                .unwrap_or_else(|x| panic!("retry after step {n}: {x:?}"));
            assert_restored(&fx);
            assert_eq!(logical(&fx.s).await, fx.before[0], "step {n}");
            assert_eq!(logical(&fx.m).await, fx.before[1], "step {n}");
        }
    }

    #[tokio::test]
    async fn t8_corrupt_snapshot_fails_and_touches_nothing() {
        let mut fx = fixture(true).await;
        let before = fs::read(&fx.s).unwrap();
        fs::write(fx.snap.join(MEMORY_FILE), b"garbage").unwrap();
        let r = fx.go(&RealOps);
        assert!(matches!(
            r,
            Err(RestoreError::RollbackFailed {
                persisted: true,
                ..
            })
        ));
        assert_eq!(fs::read(&fx.s).unwrap(), before, "formal db untouched");
        assert!(
            guard_log::read(&fx.root)
                .unwrap()
                .unwrap()
                .last_error
                .is_some()
        );
        assert!(!fx.root.join(QUARANTINE_DIR).exists());
    }

    #[tokio::test]
    async fn only_migrating_or_restoring_may_restore() {
        let mut fx = fixture(true).await;
        fx.log.phase = Phase::Committed;
        assert!(matches!(
            fx.go(&RealOps),
            Err(RestoreError::NotRestorable(Phase::Committed))
        ));
    }
}
