//! Two-database snapshot (design D2, 5d): checkpoint, SQLite backup API to
//! `.partial`, verify, close, fsync, rename, fsync chain, then manifest.
//!
//! Not wired into startup. A snapshot is the precondition of `migrating`
//! (`guard_log::persist` re-verifies the manifest). Writers are already
//! stopped (5a); a busy checkpoint means we cannot prove that, so no manifest.

use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use agent24_os_sqlite::{BackupError, backup_main};
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{Connection, Row, SqliteConnection};

use super::atomic::{FileOps, fsync_chain};
use super::manifest::{
    BinaryIdentity, DbRecord, FORMAT_VERSION, MEMORY_FILE, Manifest, STORE_FILE, StateError,
    Watermark, sha256_file, snapshot_dir, snapshot_dir_name, write_manifest,
};

#[derive(Debug)]
pub enum SnapshotError {
    /// `wal_checkpoint(TRUNCATE)` was busy or left frames: not quiescent.
    Busy(&'static str),
    Backup(BackupError),
    /// Integrity check, migration table or leftover sidecar check failed.
    Verify(String),
    Sql(String),
    State(StateError),
    Io(io::Error),
}

impl From<io::Error> for SnapshotError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<sqlx::Error> for SnapshotError {
    fn from(e: sqlx::Error) -> Self {
        Self::Sql(e.to_string())
    }
}
impl From<StateError> for SnapshotError {
    fn from(e: StateError) -> Self {
        Self::State(e)
    }
}

pub struct SnapshotRequest<'a> {
    pub data_dir: &'a Path,
    pub store_db: &'a Path,
    pub memory_db: &'a Path,
    pub attempt_id: &'a str,
    pub compact_utc: &'a str,
    pub created_at: &'a str,
    pub source: Option<BinaryIdentity>,
    pub target: BinaryIdentity,
    pub migrations_digest: &'a str,
}

struct Src {
    conn: SqliteConnection,
    watermark: i64,
}

async fn open(path: &Path, create: bool) -> Result<SqliteConnection, sqlx::Error> {
    let o = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(create)
        .busy_timeout(std::time::Duration::ZERO);
    SqliteConnection::connect_with(&o).await
}

/// Highest successful migration, 0 if the table is absent.
async fn watermark(c: &mut SqliteConnection) -> Result<i64, sqlx::Error> {
    let has: i64 = sqlx::query("SELECT count(*) FROM sqlite_master WHERE name='_sqlx_migrations'")
        .fetch_one(&mut *c)
        .await?
        .get(0);
    if has == 0 {
        return Ok(0);
    }
    Ok(
        sqlx::query("SELECT COALESCE(MAX(version),0) FROM _sqlx_migrations WHERE success=1")
            .fetch_one(c)
            .await?
            .get(0),
    )
}

/// Open the source (never creating it) and require a complete TRUNCATE checkpoint.
async fn checkpoint(label: &'static str, path: &Path) -> Result<Option<Src>, SnapshotError> {
    if !path.exists() {
        return Ok(None); // recorded as missing; never create an empty database
    }
    let mut conn = open(path, false).await?;
    let ck = sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(&mut conn)
        .await;
    let quiet = match ck {
        // (busy, log frames, checkpointed frames); log = -1 outside WAL mode.
        Ok(r) => {
            r.get::<i64, _>(0) == 0
                && (r.get::<i64, _>(1) <= 0 || r.get::<i64, _>(1) == r.get::<i64, _>(2))
        }
        Err(_) => false,
    };
    if !quiet {
        let _ = conn.close().await;
        return Err(SnapshotError::Busy(label));
    }
    let watermark = watermark(&mut conn).await?;
    Ok(Some(Src { conn, watermark }))
}

/// Backup `src` into `<final>.partial`, verify, close, fsync, rename.
async fn copy_one(
    ops: &dyn FileOps,
    src: &mut Src,
    final_path: &Path,
) -> Result<(), SnapshotError> {
    let partial = PathBuf::from(format!("{}.partial", final_path.display()));
    let mut dst = open(&partial, true).await?;
    let verified = async {
        backup_main(&mut src.conn, &mut dst)
            .await
            .map_err(SnapshotError::Backup)?;
        // A plain single-file database: no -wal/-shm may ride along.
        sqlx::query("PRAGMA journal_mode=DELETE")
            .execute(&mut dst)
            .await?;
        let ok: String = sqlx::query("PRAGMA integrity_check")
            .fetch_one(&mut dst)
            .await?
            .get(0);
        if ok != "ok" {
            return Err(SnapshotError::Verify(format!("integrity_check: {ok}")));
        }
        if watermark(&mut dst).await? != src.watermark {
            return Err(SnapshotError::Verify(
                "migration table differs from source".into(),
            ));
        }
        Ok(())
    }
    .await;
    let closed = dst.close().await; // close the target before touching the file
    verified?;
    closed?;
    for sidecar in ["-wal", "-shm"] {
        if PathBuf::from(format!("{}{sidecar}", partial.display())).exists() {
            return Err(SnapshotError::Verify(format!("{sidecar} left after close")));
        }
    }
    fs::set_permissions(&partial, fs::Permissions::from_mode(0o600))?;
    ops.fsync_file(&partial)?;
    ops.rename(&partial, final_path)?;
    Ok(())
}

fn record(path: &Path, existed: bool) -> Result<DbRecord, SnapshotError> {
    if !existed {
        return Ok(DbRecord {
            existed: false,
            len: None,
            sha256: None,
        });
    }
    let (len, sha) = sha256_file(path)?;
    Ok(DbRecord {
        existed: true,
        len: Some(len),
        sha256: Some(sha),
    })
}

/// Returns the snapshot directory name (the guard log's `snapshot_id`).
pub async fn take_snapshot(
    ops: &dyn FileOps,
    req: &SnapshotRequest<'_>,
) -> Result<(String, Manifest), SnapshotError> {
    let name = snapshot_dir_name(req.compact_utc, req.attempt_id)?;
    // Checkpoint both before any file is created: a busy db leaves no trace.
    let mut store = checkpoint("agent24.db", req.store_db).await?;
    let mut memory = match checkpoint("memory.db", req.memory_db).await {
        Ok(m) => m,
        Err(e) => {
            if let Some(s) = store {
                let _ = s.conn.close().await;
            }
            return Err(e);
        }
    };
    let dir = snapshot_dir(req.data_dir, &name);
    let result = async {
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir.parent().unwrap_or(&dir))?;
        fs::DirBuilder::new().mode(0o700).create(&dir)?;
        if let Some(s) = store.as_mut() {
            copy_one(ops, s, &dir.join(STORE_FILE)).await?;
        }
        if let Some(m) = memory.as_mut() {
            copy_one(ops, m, &dir.join(MEMORY_FILE)).await?;
        }
        fsync_chain(ops, req.data_dir, &dir)?; // renames + new dirs, up to data_dir
        Ok::<_, SnapshotError>(())
    }
    .await;
    let (store_existed, memory_existed) = (store.is_some(), memory.is_some());
    let wm = Watermark {
        store: store.as_ref().map_or(0, |s| s.watermark),
        memory: memory.as_ref().map_or(0, |m| m.watermark),
    };
    // Source maintenance connections are closed before anything migrates.
    for s in [store, memory].into_iter().flatten() {
        s.conn.close().await?;
    }
    result?;
    let manifest = Manifest {
        format_version: FORMAT_VERSION,
        attempt_id: req.attempt_id.into(),
        created_at: req.created_at.into(),
        source: req.source.clone(),
        target: req.target.clone(),
        store_db: record(&dir.join(STORE_FILE), store_existed)?,
        memory_db: record(&dir.join(MEMORY_FILE), memory_existed)?,
        migrations_digest: req.migrations_digest.into(),
        watermark: wm,
    };
    write_manifest(ops, req.data_dir, &dir, &manifest)?; // only after both databases
    Ok((name, manifest))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::super::atomic::{RealOps, test_ops::RecordingOps};
    use super::super::manifest::verify_complete;
    use super::*;
    use sqlx::sqlite::SqliteJournalMode;

    async fn live(path: &Path, rows: &[i64]) -> SqliteConnection {
        let o = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal);
        let mut c = SqliteConnection::connect_with(&o).await.unwrap();
        sqlx::query("CREATE TABLE t(x INTEGER)")
            .execute(&mut c)
            .await
            .unwrap();
        sqlx::query("CREATE TABLE _sqlx_migrations(version INTEGER, success BOOLEAN)")
            .execute(&mut c)
            .await
            .unwrap();
        sqlx::query("INSERT INTO _sqlx_migrations VALUES (3, 1), (4, 0)")
            .execute(&mut c)
            .await
            .unwrap();
        for r in rows {
            sqlx::query("INSERT INTO t VALUES (?)")
                .bind(r)
                .execute(&mut c)
                .await
                .unwrap();
        }
        c
    }

    async fn rows(path: &Path) -> Vec<i64> {
        let mut c = open(path, false).await.unwrap();
        let v = sqlx::query("SELECT x FROM t ORDER BY x")
            .fetch_all(&mut c)
            .await
            .unwrap();
        let v = v.iter().map(|r| r.get(0)).collect();
        c.close().await.unwrap();
        v
    }

    fn req<'a>(d: &'a Path, s: &'a Path, m: &'a Path) -> SnapshotRequest<'a> {
        SnapshotRequest {
            data_dir: d,
            store_db: s,
            memory_db: m,
            attempt_id: "a1",
            compact_utc: "20261027T000000Z",
            created_at: "2026-10-27T00:00:00Z",
            source: None,
            target: BinaryIdentity {
                version: "2".into(),
                sha256: "y".into(),
            },
            migrations_digest: "dd",
        }
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        v.sort();
        v
    }

    /// Crash image: db + un-checkpointed -wal/-shm copied while a writer is live.
    fn crash_copy(from: &Path, to: &Path) {
        for sfx in ["", "-wal", "-shm"] {
            let (a, b) = (
                format!("{}{sfx}", from.display()),
                format!("{}{sfx}", to.display()),
            );
            fs::copy(a, b).unwrap();
        }
    }

    /// T2 + T3 + close semantics: WAL-only data is in the snapshot, the snapshot
    /// directory is exactly db+db+manifest, and both sources and both targets
    /// are really closed (SQLite removes -wal/-shm only when the last
    /// connection closes; a leaked handle would leave them behind).
    #[tokio::test]
    async fn t2_t3_snapshot_complete_wal_data_and_connections_closed() {
        let td = tempfile::tempdir().unwrap();
        let (live_dir, root) = (td.path().join("live"), td.path().join("data"));
        fs::create_dir_all(&live_dir).unwrap();
        fs::create_dir_all(&root).unwrap();
        let (ls, lm) = (live_dir.join("s.db"), live_dir.join("m.db"));
        let (c1, c2) = (live(&ls, &[1, 2, 3]).await, live(&lm, &[10, 20]).await);
        let (s, m) = (root.join(STORE_FILE), root.join(MEMORY_FILE));
        crash_copy(&ls, &s);
        crash_copy(&lm, &m);
        assert!(
            fs::metadata(format!("{}-wal", s.display())).unwrap().len() > 0,
            "fixture needs WAL data"
        );

        let (name, man) = take_snapshot(&RealOps, &req(&root, &s, &m)).await.unwrap();
        let dir = snapshot_dir(&root, &name);
        assert_eq!(names(&dir), ["agent24.db", "manifest.json", "memory.db"]);
        assert_eq!(rows(&dir.join(STORE_FILE)).await, [1, 2, 3]);
        assert_eq!(rows(&dir.join(MEMORY_FILE)).await, [10, 20]);
        assert_eq!(rows(&s).await, [1, 2, 3], "source logically unchanged");
        assert_eq!(
            man.watermark,
            Watermark {
                store: 3,
                memory: 3
            }
        );
        assert!(verify_complete(&root, &name, "a1").is_ok());
        assert_eq!(
            fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(dir.join(STORE_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        // Closed: nothing left beside any source or snapshot database.
        for p in [&s, &m, &dir.join(STORE_FILE), &dir.join(MEMORY_FILE)] {
            for sfx in ["-wal", "-shm", "-journal"] {
                assert!(
                    !PathBuf::from(format!("{}{sfx}", p.display())).exists(),
                    "{}{sfx}",
                    p.display()
                );
            }
        }
        c1.close().await.unwrap();
        c2.close().await.unwrap();
    }

    /// T10: a reader pinned on an old WAL snapshot makes TRUNCATE busy.
    #[tokio::test]
    async fn t10_busy_checkpoint_produces_no_manifest() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let (s, m) = (root.join(STORE_FILE), root.join(MEMORY_FILE));
        let mut w = live(&s, &[1]).await;
        let mut r = SqliteConnection::connect_with(&SqliteConnectOptions::new().filename(&s))
            .await
            .unwrap();
        sqlx::query("BEGIN").execute(&mut r).await.unwrap();
        sqlx::query("SELECT x FROM t")
            .fetch_all(&mut r)
            .await
            .unwrap();
        sqlx::query("INSERT INTO t VALUES (2)")
            .execute(&mut w)
            .await
            .unwrap();
        let err = take_snapshot(&RealOps, &req(root, &s, &m))
            .await
            .unwrap_err();
        assert!(matches!(err, SnapshotError::Busy("agent24.db")), "{err:?}");
        assert!(
            !root.join("migration-snapshots").exists(),
            "no trace, hence no manifest"
        );
        sqlx::query("ROLLBACK").execute(&mut r).await.unwrap();
        r.close().await.unwrap();
        w.close().await.unwrap();
    }

    #[tokio::test]
    async fn absent_database_is_recorded_missing_and_not_created() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let (s, m) = (root.join(STORE_FILE), root.join(MEMORY_FILE));
        live(&s, &[7]).await.close().await.unwrap();
        let (name, man) = take_snapshot(&RealOps, &req(root, &s, &m)).await.unwrap();
        assert!(man.store_db.existed && !man.memory_db.existed);
        assert!(!m.exists(), "no empty source database created");
        assert_eq!(
            names(&snapshot_dir(root, &name)),
            ["agent24.db", "manifest.json"]
        );
        assert!(verify_complete(root, &name, "a1").is_ok());
    }

    /// D2/R1: both renames, then the chain fsync to data_dir, then the manifest.
    #[tokio::test]
    async fn publish_order_and_directory_fsync_chain() {
        let td = tempfile::tempdir().unwrap();
        let root = td.path();
        let (s, m) = (root.join(STORE_FILE), root.join(MEMORY_FILE));
        live(&s, &[1]).await.close().await.unwrap();
        live(&m, &[2]).await.close().await.unwrap();
        let ops = RecordingOps::default();
        let (name, _) = take_snapshot(&ops, &req(root, &s, &m)).await.unwrap();
        let calls = ops.calls.borrow();
        let pos = |p: &str| calls.iter().position(|c| c.starts_with(p)).unwrap();
        let dir = snapshot_dir(root, &name);
        let fs_dirs: Vec<_> = calls
            .iter()
            .filter(|c| c.starts_with("fsync_dir"))
            .collect();
        assert_eq!(fs_dirs.len(), 6, "{calls:?}"); // chain for dbs + chain for manifest
        for d in [dir.as_path(), dir.parent().unwrap(), root] {
            assert!(
                fs_dirs
                    .iter()
                    .any(|c| **c == format!("fsync_dir:{}", d.display())),
                "{d:?}"
            );
        }
        let (mem_rename, first_dir, w) = (
            calls
                .iter()
                .position(|c| c.starts_with("rename:") && c.contains("memory.db.partial"))
                .unwrap(),
            pos("fsync_dir"),
            pos("write"),
        );
        assert!(
            pos("fsync_file") < pos("rename:") && mem_rename < first_dir && first_dir < w,
            "{calls:?}"
        );
    }

    #[tokio::test]
    async fn publish_failure_leaves_no_manifest_and_cannot_authorise() {
        for op in ["fsync_file", "rename", "fsync_dir"] {
            let td = tempfile::tempdir().unwrap();
            let root = td.path();
            let (s, m) = (root.join(STORE_FILE), root.join(MEMORY_FILE));
            live(&s, &[1]).await.close().await.unwrap();
            live(&m, &[2]).await.close().await.unwrap();
            let r = take_snapshot(&RecordingOps::failing(op), &req(root, &s, &m)).await;
            assert!(r.is_err(), "{op}");
            let dir = snapshot_dir(root, "20261027T000000Z-a1");
            assert!(!dir.join("manifest.json").exists(), "{op}");
            assert!(
                verify_complete(root, "20261027T000000Z-a1", "a1").is_err(),
                "{op}"
            );
        }
    }
}
