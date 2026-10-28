//! Phased memory connection for CORE-5b.
//!
//! [`MemoryConn::open_maintenance`] does not migrate and does not create a
//! missing file. [`MemoryConn::migrate`] migrates and rebuilds FTS.
//! [`MemoryConn::close`] awaits every connection, then waits until the file
//! can be locked exclusively.

use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;

use agent24_store::sqlite_phase::Fault;
use agent24_store::sqlite_phase::Guard;
use agent24_store::sqlite_phase::StepError;
use agent24_store::sqlite_phase::checkpoint;
use agent24_store::sqlite_phase::migrate_up;
use agent24_store::sqlite_phase::open_existing;
use agent24_store::sqlite_phase::open_pool;
use agent24_store::sqlite_phase::watermark;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnectOptions;

use crate::KVSTORE_MAX_CONNECTIONS;
use crate::KvStore;
use crate::MEMORY_SQLITE_ROW_BUFFER_SIZE;
use crate::MemoryError;

pub use agent24_store::sqlite_phase::OBSERVE_UNCLOSED;

/// Injection points for the memory phase API. [`PhaseFault::none`] is production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhaseFault {
    pub fail_open: bool,
    /// 1-based index into pending up migrations. 0 runs all of them.
    pub fail_nth_pending: u32,
    pub fail_fts_rebuild: bool,
    /// Hold a SQLite reserved lock until [`MemoryConn::close`].
    pub retain_exclusive_lock: bool,
    /// Same connect-PRAGMA failure as [`agent24_store::sqlite_phase::Fault`].
    pub fail_connect_pragma: bool,
}

impl PhaseFault {
    pub const fn none() -> Self {
        Self {
            fail_open: false,
            fail_nth_pending: 0,
            fail_fts_rebuild: false,
            retain_exclusive_lock: false,
            fail_connect_pragma: false,
        }
    }

    fn core(self) -> Fault {
        Fault {
            fail_open: self.fail_open,
            fail_nth_pending: self.fail_nth_pending,
            retain_exclusive_lock: self.retain_exclusive_lock,
            fail_connect_pragma: self.fail_connect_pragma,
        }
    }
}

/// A phased open stopped. `migration` is the script version when one is known.
#[derive(Debug)]
pub struct PhaseError {
    pub phase: &'static str,
    pub migration: Option<i64>,
    pub source: MemoryError,
}

/// One memory pool. Close it with [`MemoryConn::close`].
pub struct MemoryConn {
    inner: Guard,
    kv: Option<KvStore>,
}

impl std::fmt::Debug for MemoryConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryConn")
            .field("closed", &self.inner.pool().is_closed())
            .finish_non_exhaustive()
    }
}

impl MemoryConn {
    /// Open an existing database without migrating or rebuilding FTS.
    ///
    /// `Ok(None)` means the file is absent: no connection was opened and the
    /// file was not created.
    pub async fn open_maintenance(
        path: &Path,
        fault: PhaseFault,
    ) -> Result<Option<Self>, PhaseError> {
        let opts = file_options(path, false).map_err(open_err)?;
        Ok(open_existing(path, opts, fault.core())
            .await
            .map_err(map_err)?
            .map(|inner| Self { inner, kv: None }))
    }

    /// Create the database if needed, migrate, rebuild FTS, and return the handle.
    /// [`KvStore::open`] is this followed by [`MemoryConn::into_kv`].
    pub async fn migrate(path: &Path, fault: PhaseFault) -> Result<Self, PhaseError> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            return Err(open_err(MemoryError::Io(format!("mkdir: {err}"))));
        }
        let opts = file_options(path, true).map_err(open_err)?;
        let mut conn = Self {
            inner: open_pool(opts, KVSTORE_MAX_CONNECTIONS, fault.core())
                .await
                .map_err(map_err)?,
            kv: None,
        };
        if let Err(err) = migrate_up(
            conn.inner.pool(),
            sqlx::migrate!("./migrations"),
            fault.fail_nth_pending,
        )
        .await
        {
            return Err(conn.fail(map_err(err)).await);
        }
        if fault.fail_fts_rebuild {
            return Err(conn
                .fail(PhaseError {
                    phase: "fts_rebuild",
                    migration: None,
                    source: MemoryError::Conflict("injected fts rebuild failure".into()),
                })
                .await);
        }
        if let Err(source) = KvStore::rebuild_fts_if_needed(conn.inner.pool()).await {
            return Err(conn
                .fail(PhaseError {
                    phase: "fts_rebuild",
                    migration: None,
                    source,
                })
                .await);
        }
        conn.kv = Some(kv_from_pool(
            conn.inner.pool().clone(),
            crate::disk_gate::DiskGate::for_path(path),
        ));
        Ok(conn)
    }

    pub async fn applied_version(&self) -> Result<i64, MemoryError> {
        watermark(self.inner.pool())
            .await
            .map_err(MemoryError::from)
    }

    /// `PRAGMA wal_checkpoint(TRUNCATE)` → `(busy, log, checkpointed)`.
    pub async fn checkpoint_truncate(&self) -> Result<(i64, i64, i64), MemoryError> {
        checkpoint(self.inner.pool())
            .await
            .map_err(MemoryError::from)
    }

    #[must_use]
    pub fn kv(&self) -> Option<&KvStore> {
        self.kv.as_ref()
    }

    /// Hand the pool to a [`KvStore`] without closing it.
    #[must_use]
    pub fn into_kv(mut self) -> KvStore {
        self.inner.disarm();
        self.kv.take().unwrap_or_else(|| {
            kv_from_pool(
                self.inner.pool().clone(),
                crate::disk_gate::DiskGate::disabled(),
            )
        })
    }

    pub async fn close(mut self) -> Result<(), MemoryError> {
        // `KvStore` keeps a pool clone. Drop it first so `Guard::close` drops
        // the last `Arc` and can see the worker release the file.
        drop(self.kv.take());
        self.inner.close().await.map_err(close_error)
    }

    #[cfg(test)]
    fn pool(&self) -> &SqlitePool {
        self.inner.pool()
    }

    async fn fail(self, err: PhaseError) -> PhaseError {
        let _ = self.close().await;
        err
    }
}

fn close_error(err: StepError) -> MemoryError {
    match err {
        StepError::Sqlx { source, .. } => source.into(),
        StepError::Migrate { source, .. } => source.into(),
        StepError::Msg { message, .. } => MemoryError::Conflict(message),
    }
}

/// Latest migration embedded in this binary.
#[must_use]
pub fn embedded_version() -> i64 {
    agent24_store::sqlite_phase::max_version(&sqlx::migrate!("./migrations"))
}

fn file_options(path: &Path, create: bool) -> Result<SqliteConnectOptions, MemoryError> {
    Ok(
        SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(create)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .foreign_keys(true)
            .busy_timeout(std::time::Duration::from_secs(5))
            .row_buffer_size(MEMORY_SQLITE_ROW_BUFFER_SIZE),
    )
}

fn open_err(source: MemoryError) -> PhaseError {
    PhaseError {
        phase: "open",
        migration: None,
        source,
    }
}

fn map_err(err: StepError) -> PhaseError {
    match err {
        StepError::Sqlx { phase, source } => PhaseError {
            phase,
            migration: None,
            source: source.into(),
        },
        StepError::Msg {
            phase,
            migration,
            message,
        } => PhaseError {
            phase,
            migration,
            source: MemoryError::Conflict(message),
        },
        StepError::Migrate { migration, source } => PhaseError {
            phase: "migrating",
            migration,
            source: MemoryError::Migrate(source),
        },
    }
}

fn kv_from_pool(pool: SqlitePool, disk: crate::disk_gate::DiskGate) -> KvStore {
    KvStore {
        pool,
        oop_admission: Some(Arc::new(tokio::sync::Semaphore::new(
            (KVSTORE_MAX_CONNECTIONS - 1) as usize,
        ))),
        rejection_queue: Default::default(),
        disk,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::str::FromStr;
    use std::sync::atomic::Ordering;

    use super::MemoryConn;
    use super::OBSERVE_UNCLOSED;
    use super::PhaseFault;
    use super::embedded_version;

    fn guard() {
        OBSERVE_UNCLOSED.store(true, Ordering::SeqCst);
    }

    fn fault(nth: u32, fail_open: bool, fail_fts: bool) -> PhaseFault {
        PhaseFault {
            fail_open,
            fail_nth_pending: nth,
            fail_fts_rebuild: fail_fts,
            retain_exclusive_lock: true,
            fail_connect_pragma: false,
        }
    }

    async fn can_take_exclusive(path: &std::path::Path) -> bool {
        let options =
            sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(false)
                .busy_timeout(std::time::Duration::from_millis(0));
        let Ok(pool) = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
        else {
            return false;
        };
        let got = pool.begin_with("BEGIN EXCLUSIVE").await.is_ok();
        pool.close().await;
        got
    }

    async fn fts_marker(path: &std::path::Path) -> Option<String> {
        let conn = MemoryConn::open_maintenance(path, PhaseFault::none())
            .await
            .unwrap()
            .unwrap();
        let marker: Option<String> =
            sqlx::query_scalar("SELECT v FROM mem_fts_state WHERE k = 'needs_rebuild'")
                .fetch_optional(conn.pool())
                .await
                .unwrap();
        conn.close().await.unwrap();
        marker
    }

    #[tokio::test]
    async fn maintenance_does_not_create_or_migrate() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.db");
        assert!(
            MemoryConn::open_maintenance(&missing, PhaseFault::none())
                .await
                .unwrap()
                .is_none()
        );
        assert!(!missing.exists());

        let path = dir.path().join("memory.db");
        let migrated = MemoryConn::migrate(&path, PhaseFault::none())
            .await
            .unwrap();
        let version = migrated.applied_version().await.unwrap();
        migrated.close().await.unwrap();
        let maint = MemoryConn::open_maintenance(&path, PhaseFault::none())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(maint.applied_version().await.unwrap(), version);
        let (busy, _, _) = maint.checkpoint_truncate().await.unwrap();
        assert_eq!(busy, 0);
        maint.close().await.unwrap();
        assert_eq!(version, embedded_version());
        assert!(can_take_exclusive(&path).await);
    }

    #[tokio::test]
    async fn connect_pragma_failure_is_open_and_releases_the_file() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pragma.db");
        let err = MemoryConn::migrate(
            &path,
            PhaseFault {
                fail_open: false,
                fail_nth_pending: 0,
                fail_fts_rebuild: false,
                retain_exclusive_lock: false,
                fail_connect_pragma: true,
            },
        )
        .await
        .unwrap_err();
        assert_eq!(err.phase, "open", "{err:?}");
        assert_eq!(err.migration, None, "{err:?}");
        assert!(
            can_take_exclusive(&path).await,
            "open failure returned while the file was still locked: {err:?}"
        );
    }

    #[tokio::test]
    async fn open_and_nth_migration_failures_release_the_connection() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        MemoryConn::migrate(&path, PhaseFault::none())
            .await
            .unwrap()
            .close()
            .await
            .unwrap();
        let err = MemoryConn::open_maintenance(&path, fault(0, true, false))
            .await
            .unwrap_err();
        assert_eq!(err.phase, "open");
        assert_eq!(err.migration, None);
        assert!(can_take_exclusive(&path).await, "{err:?}");

        for nth in [1_u32, 2] {
            let path = dir.path().join(format!("{nth}.db"));
            let err = MemoryConn::migrate(&path, fault(nth, false, false))
                .await
                .unwrap_err();
            assert_eq!(err.phase, "migrating", "{err:?}");
            assert_eq!(err.migration, Some(i64::from(nth)), "{err:?}");
            assert!(can_take_exclusive(&path).await, "{err:?}");
            let conn = MemoryConn::open_maintenance(&path, PhaseFault::none())
                .await
                .unwrap()
                .unwrap();
            let version = conn.applied_version().await.unwrap();
            conn.close().await.unwrap();
            assert_eq!(version, i64::from(nth - 1));
        }
    }

    #[tokio::test]
    async fn fts_rebuild_failure_is_a_gate_and_releases_the_connection() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let err = MemoryConn::migrate(&path, fault(0, false, true))
            .await
            .unwrap_err();
        assert_eq!(err.phase, "fts_rebuild", "{err:?}");
        assert_eq!(err.migration, None);
        assert!(can_take_exclusive(&path).await, "{err:?}");
        let conn = MemoryConn::open_maintenance(&path, PhaseFault::none())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conn.applied_version().await.unwrap(), embedded_version());
        conn.close().await.unwrap();
        assert_eq!(fts_marker(&path).await.as_deref(), Some("1"));
    }

    #[tokio::test]
    async fn kv_open_still_reaches_the_embedded_version_and_clears_fts() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.db");
        let kv = crate::KvStore::open(&path).await.unwrap();
        assert_eq!(
            kv.latest_applied_migration().await.unwrap(),
            embedded_version()
        );
        drop(kv);
        assert_eq!(fts_marker(&path).await, None);
    }
}
