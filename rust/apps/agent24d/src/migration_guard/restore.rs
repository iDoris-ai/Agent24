//! Whole-pair restorer (design D4, 5e-1). Not wired (`ENABLED=false`).
//!
//! Per database: rebuild `<db>.restore.<attempt_id>` from the verified
//! snapshot (hash-checked, fsynced), move the failed main file and every
//! `-wal`/`-shm`/`-journal` into `<data_dir>/migration-quarantine/<attempt>/`
//! (never deleted, never overwritten), fsync, rename staging into place,
//! fsync the directory chain, persist that database's progress. Restore is
//! idempotent: every step is re-derived from disk, nothing trusts memory.
//!
//! A failed restore is persisted as the `RollbackFailed` phase (with
//! `last_error`) and returned as [`RestoreError::RollbackFailed`]; the marker
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
            // Only `Restoring` may fail over; a Restored log is never rewritten
            // with an error (`failed_restore` refuses every other phase).
            let persisted = match log.failed_restore(reason.clone()) {
                Ok(next) if persist(ops, req.data_dir, &next).is_ok() => {
                    *log = next;
                    true
                }
                _ => false,
            };
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
        // Restored: the pair is live again and may have been written to, so
        // it is only checked below, never rebuilt. Restoring: every database,
        // Done or not, is re-derived from disk (idempotent shortcut), so a
        // Done database that drifted heals instead of failing the gate forever.
        if log.phase != Phase::Restoring {
            continue;
        }
        let store = file == STORE_FILE;
        restore_one(
            ops,
            req.data_dir,
            &log.attempt_id,
            path,
            rec,
            &snap.join(file),
            &q,
        )?;
        if (if store {
            log.store_restore
        } else {
            log.memory_restore
        }) == DbRestore::Pending
        {
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
    // Crash after install but before progress was persisted: the formal db
    // already is the snapshot. Do not copy again (the recovery budget was
    // spent once; a second copy can ENOSPC) nor quarantine the good file.
    // Any read error here just falls through to the strict full path.
    // `check_final` also requires no sidecar, so a stray -wal/-shm/-journal
    // beside the good file sends us down the full path (quarantine) instead.
    // The accepted file itself is fsynced too: it may match the snapshot
    // bytes while still being an unflushed in-place rewrite.
    if check_final(path, rec).is_ok() {
        if rec.existed {
            ops.fsync_file(path)
                .map_err(|x| e("fsync accepted db", x))?;
        }
        return fsync_chain(ops, data_dir, parent).map_err(|x| e("fsync db dir", x));
    }
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
            let dest = evidence_name(q, &format!("{name}{sfx}"), &src)?;
            if same_inode(&src, &dest) {
                // rename(2) of one inode onto its own other name is a silent
                // no-op that keeps `src`; the evidence name already holds the
                // inode (crash after the move, before the db dir fsync).
                ops.remove_file(&src).map_err(|x| e("unlink moved", x))?;
            } else {
                ops.rename(&src, &dest).map_err(|x| e("quarantine", x))?;
            }
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

fn same_inode(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (fs::symlink_metadata(a), fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

/// Where to move `src` in `q`: a name already holding the very same bytes
/// is reused (a crash between the quarantine move and the db-directory fsync
/// leaves both names; re-moving must not burn a new suffix on every restart),
/// otherwise the first unused name.
fn evidence_name(q: &Path, base: &str, src: &Path) -> R<PathBuf> {
    let mut want = None;
    for i in 0..1000 {
        let p = q.join(if i == 0 {
            base.to_owned()
        } else {
            format!("{base}.{i}")
        });
        if !present(&p)? {
            return Ok(p);
        }
        if want.is_none() {
            want = Some(sha256_file(src).map_err(|x| e("evidence hash", x))?);
        }
        if sha256_file(&p).is_ok_and(|h| Some(h) == want) {
            return Ok(p); // identical bytes: replacing it loses nothing
        }
    }
    Err(format!("no free quarantine name for {base}"))
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

/// Exactly `<db><-wal|-shm|-journal>` with an optional `.N` suffix (the
/// `free_name` collision form) for one of the two managed databases.
fn is_quarantined_sidecar(n: &str) -> bool {
    [STORE_FILE, MEMORY_FILE].iter().any(|db| {
        SIDECARS.iter().any(|s| {
            let base = format!("{db}{s}");
            n == base
                || n.strip_prefix(&base)
                    .and_then(|r| r.strip_prefix('.'))
                    .is_some_and(|d| !d.is_empty() && d.bytes().all(|b| b.is_ascii_digit()))
        })
    })
}

/// After `Restored` is durable the failed WAL/SHM/journal copies are junk.
/// Returns how many things could not be cleaned (listing, delete, fsync
/// errors all count); a missing quarantine dir is a genuine zero.
fn drop_quarantined_sidecars(ops: &dyn FileOps, q: &Path) -> usize {
    let rd = match fs::read_dir(q) {
        Ok(rd) => rd,
        Err(x) if x.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(_) => return 1,
    };
    let mut bad = 0;
    for ent in rd {
        let Ok(ent) = ent else {
            bad += 1;
            continue;
        };
        let n = ent.file_name().to_string_lossy().into_owned();
        if is_quarantined_sidecar(&n) && remove_if_present(ops, &ent.path()).is_err() {
            bad += 1;
        }
    }
    bad + usize::from(ops.fsync_dir(q).is_err())
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
        fn fsync_file(&self, p: &Path) -> io::Result<()> {
            self.hit("fsync_file", p)?;
            RealOps.fsync_file(p)
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

        /// Fresh process after a failed restore: the log on disk is
        /// `RollbackFailed` and startup would dock. Then model the future
        /// authorized recovery (installer/manual, 5h/5i) re-opening it, so the
        /// idempotence of `restore` itself stays covered.
        fn reload_for_recovery(&mut self) {
            self.log = guard_log::read(&self.root).unwrap().unwrap();
            assert_eq!(self.log.phase, Phase::RollbackFailed);
            self.log.phase = Phase::Restoring;
            self.log.last_error = None;
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
            fx.reload_for_recovery();
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
        let on_disk = guard_log::read(&fx.root).unwrap().unwrap();
        assert_eq!(on_disk.phase, Phase::RollbackFailed);
        assert!(on_disk.last_error.is_some());
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

    /// Review High: crash after the store was installed but before its Done
    /// was persisted. The replay must neither copy it again (ENOSPC under the
    /// reserved budget) nor quarantine the already-correct file again.
    #[tokio::test]
    async fn t9_replay_after_install_does_not_recopy_or_requarantine() {
        let mut fx = fixture(true).await;
        let clean = Probe::new(None);
        fx.go(&clean).unwrap();
        let calls = clean.calls.borrow().clone();
        let install = calls
            .iter()
            .position(|x| x == &format!("rename:{}", fx.s.display()))
            .unwrap();
        let progress = install
            + calls[install..]
                .iter()
                .position(|x| x.starts_with("write:"))
                .unwrap();
        let mut fx = fixture(true).await;
        assert!(fx.go(&Probe::new(Some(progress))).is_err());
        fx.reload_for_recovery();
        assert_eq!(
            fx.log.store_restore,
            DbRestore::Pending,
            "fixture: Done not yet durable"
        );
        assert_eq!(
            fs::read(&fx.s).unwrap(),
            fs::read(fx.snap.join(STORE_FILE)).unwrap()
        );
        let retry = Probe::new(None);
        fx.go(&retry).unwrap();
        let rc = retry.calls.borrow();
        assert!(
            !rc.iter()
                .any(|x| x.starts_with("copy:") && x.contains("agent24.db.restore")),
            "{rc:?}"
        );
        let q = fx.root.join(QUARANTINE_DIR).join("a1");
        assert!(
            q.join(STORE_FILE).exists() && !q.join("agent24.db.1").exists(),
            "no extra quarantine copy"
        );
        assert_restored(&fx);
    }

    /// State after "installed, Done not yet durable": the store is already
    /// the snapshot, its progress flag is still Pending (fresh log from disk).
    async fn crash_after_install() -> Fx {
        let mut fx = fixture(true).await;
        let clean = Probe::new(None);
        fx.go(&clean).unwrap();
        let calls = clean.calls.borrow().clone();
        let install = calls
            .iter()
            .position(|x| x == &format!("rename:{}", fx.s.display()))
            .unwrap();
        let progress = install
            + calls[install..]
                .iter()
                .position(|x| x.starts_with("write:"))
                .unwrap();
        let mut fx = fixture(true).await;
        assert!(fx.go(&Probe::new(Some(progress))).is_err());
        fx.reload_for_recovery();
        assert_eq!(fx.log.store_restore, DbRestore::Pending);
        fx
    }

    /// Review 1 / M14: a stray WAL next to the already-installed db must not
    /// survive the replay shortcut.
    #[tokio::test]
    async fn m14_replay_after_install_removes_stray_wal() {
        let mut fx = crash_after_install().await;
        fs::write(with_suffix(&fx.s, "-wal"), b"stray").unwrap();
        fx.go(&RealOps).unwrap();
        assert_restored(&fx);
    }

    /// Review 1 / M11: re-quarantining must never overwrite earlier evidence.
    #[tokio::test]
    async fn m11_requarantine_does_not_overwrite_evidence() {
        let mut fx = fixture(true).await;
        let q = fx.root.join(QUARANTINE_DIR).join("a1");
        fs::create_dir_all(&q).unwrap();
        fs::write(q.join(STORE_FILE), b"old evidence").unwrap();
        let failed = fs::read(&fx.s).unwrap();
        fx.go(&RealOps).unwrap();
        assert_eq!(fs::read(q.join(STORE_FILE)).unwrap(), b"old evidence");
        assert_eq!(fs::read(q.join("agent24.db.1")).unwrap(), failed);
        assert_restored(&fx);
    }

    /// Review (Codex, 5e-2): repeated crashes after the quarantine move must
    /// not burn a new suffix each time; different bytes still get a new name.
    #[tokio::test]
    async fn identical_evidence_name_is_reused_not_suffixed() {
        let mut fx = fixture(true).await;
        let q = fx.root.join(QUARANTINE_DIR).join("a1");
        fs::create_dir_all(&q).unwrap();
        fs::copy(&fx.s, q.join(STORE_FILE)).unwrap(); // the same failed bytes
        fx.go(&RealOps).unwrap();
        assert!(q.join(STORE_FILE).exists() && !q.join("agent24.db.1").exists());
        assert_restored(&fx);
    }

    /// Review (Codex, 5e-2; #1427 F1): after a crash a failed file can be a
    /// hard link of its own quarantined name; the move must still take it out
    /// of the db directory. The main file would be overwritten by the install
    /// rename anyway, so the branch is only observable on a sidecar: without
    /// the `same_inode` branch the `-wal` survives (`assert_restored`, T3).
    #[tokio::test]
    async fn aliased_quarantine_name_still_leaves_the_db_directory() {
        let mut fx = fixture(true).await;
        let q = fx.root.join(QUARANTINE_DIR).join("a1");
        fs::create_dir_all(&q).unwrap();
        let wal = with_suffix(&fx.s, "-wal");
        fs::write(&wal, b"failed wal").unwrap();
        let alias = q.join(format!("{STORE_FILE}-wal"));
        fs::hard_link(&wal, &alias).unwrap();
        fx.go(&RealOps).unwrap();
        assert_restored(&fx);
    }

    #[test]
    fn evidence_name_cap_holds_only_for_distinct_bytes() {
        let td = tempfile::tempdir().unwrap();
        let q = td.path().join("q");
        fs::create_dir(&q).unwrap();
        let src = td.path().join("src");
        fs::write(&src, b"same").unwrap();
        for i in 0..1000 {
            let n = if i == 0 {
                "x".to_owned()
            } else {
                format!("x.{i}")
            };
            fs::write(q.join(n), format!("other{i}")).unwrap();
        }
        assert!(
            evidence_name(&q, "x", &src).is_err(),
            "all taken, all different"
        );
        fs::write(q.join("x.999"), b"same").unwrap();
        assert_eq!(evidence_name(&q, "x", &src).unwrap(), q.join("x.999"));
    }

    #[test]
    fn m11_free_name_skips_taken_names() {
        let td = tempfile::tempdir().unwrap();
        let q = td.path();
        assert_eq!(free_name(q, "x.db").unwrap(), q.join("x.db"));
        fs::write(q.join("x.db"), b"a").unwrap();
        assert_eq!(free_name(q, "x.db").unwrap(), q.join("x.db.1"));
        fs::write(q.join("x.db.1"), b"b").unwrap();
        assert_eq!(free_name(q, "x.db").unwrap(), q.join("x.db.2"));
    }

    /// Review 1 / M13 guard: once Restored, a sidecar that drifts in later is
    /// refused by the final check (never `Ok`), and left untouched.
    #[tokio::test]
    async fn m13_restored_log_with_drifted_sidecar_is_not_ok() {
        let mut fx = fixture(true).await;
        fx.go(&RealOps).unwrap();
        assert_eq!(fx.log.phase, Phase::Restored);
        let shm = with_suffix(&fx.s, "-shm");
        fs::write(&shm, b"drift").unwrap();
        // the live db also took a legitimate post-restore write
        fs::write(&fx.s, b"post-restore write").unwrap();
        let q = fx.root.join(QUARANTINE_DIR).join("a1");
        let quarantined = fs::read_dir(&q).unwrap().count();
        let r = fx.go(&RealOps);
        assert!(
            matches!(
                r,
                Err(RestoreError::RollbackFailed {
                    persisted: false,
                    ..
                })
            ),
            "{r:?}"
        );
        assert!(shm.exists(), "Restored state: nothing is touched");
        assert_eq!(fs::read(&fx.s).unwrap(), b"post-restore write");
        assert_eq!(fs::read_dir(&q).unwrap().count(), quarantined);
        assert_eq!(fx.log.phase, Phase::Restored);
    }

    /// Review 1 (G2) + Low 6: store is Done, the crash hits the memory stage,
    /// then the store drifts (bytes + -shm). The retry must re-derive the Done
    /// store from disk and self-heal, not fail the final gate forever.
    #[tokio::test]
    async fn low6_done_database_that_drifted_is_rechecked_and_healed() {
        let mut fx = fixture(true).await;
        let clean = Probe::new(None);
        fx.go(&clean).unwrap();
        let mem_stage = clean
            .calls
            .borrow()
            .iter()
            .position(|x| x.contains("memory.db.restore"))
            .unwrap();
        let mut fx = fixture(true).await;
        assert!(fx.go(&Probe::new(Some(mem_stage))).is_err());
        fx.reload_for_recovery();
        assert_eq!(
            (fx.log.store_restore, fx.log.memory_restore),
            (DbRestore::Done, DbRestore::Pending)
        );
        fs::write(&fx.s, b"drifted bytes").unwrap();
        fs::write(with_suffix(&fx.s, "-shm"), b"drift").unwrap();
        fx.go(&RealOps).unwrap();
        assert_restored(&fx);
        assert_eq!(logical(&fx.s).await, fx.before[0]);
        assert_eq!(logical(&fx.m).await, fx.before[1]);
        assert_eq!(fx.log.phase, Phase::Restored);
    }

    /// Review Low 2: the accepted main file itself is fsynced by the shortcut.
    #[tokio::test]
    async fn low2_shortcut_fsyncs_the_accepted_main_file() {
        let mut fx = crash_after_install().await;
        let retry = Probe::new(None);
        fx.go(&retry).unwrap();
        let want = format!("fsync_file:{}", fx.s.display());
        assert!(retry.calls.borrow().contains(&want), "{:?}", retry.calls);
    }

    /// Review Low 3: exact `<db>-wal|-shm|-journal[.N]` only.
    #[test]
    fn low3_cleanup_matches_sidecar_names_exactly() {
        let td = tempfile::tempdir().unwrap();
        let q = td.path();
        let junk = [
            "agent24.db-wal",
            "agent24.db-shm.1",
            "agent24.db-journal.12",
            "memory.db-wal",
            "memory.db-shm.2",
        ];
        let keep = [
            "agent24.db",
            "memory.db",
            "agent24.db.1",
            "xxx.db-wal.backup",
            "agent24.db-wal.backup",
            "agent24.db-wal.",
            "agent24.db-wal.1x",
            "other.db-wal",
        ];
        for n in junk.iter().chain(keep.iter()) {
            fs::write(q.join(n), b"x").unwrap();
        }
        assert_eq!(drop_quarantined_sidecars(&RealOps, q), 0);
        for n in junk {
            assert!(!q.join(n).exists(), "{n} should be removed");
        }
        for n in keep {
            assert!(q.join(n).exists(), "{n} must be kept");
        }
    }

    /// Review Low: an unreadable quarantine dir is reported, not "0 left".
    #[test]
    fn cleanup_reports_unreadable_quarantine() {
        let td = tempfile::tempdir().unwrap();
        let f = td.path().join("not-a-dir");
        fs::write(&f, b"x").unwrap();
        assert_eq!(drop_quarantined_sidecars(&RealOps, &f), 1);
        assert_eq!(
            drop_quarantined_sidecars(&RealOps, &td.path().join("absent")),
            0
        );
    }
}
