//! Phased store connection for CORE-5b.
//!
//! [`StoreConn::open_maintenance`] does not migrate and does not create a
//! missing file. [`StoreConn::migrate`] is the migrate half of [`Store::open`].
//! [`StoreConn::close`] awaits every connection in the pool.

use std::path::Path;

use sqlx::SqlitePool;

use crate::Store;
use crate::StoreError;
use crate::sqlite_file_options;
use crate::sqlite_phase::Guard;
use crate::sqlite_phase::StepError;
use crate::sqlite_phase::checkpoint;
use crate::sqlite_phase::migrate_up;
use crate::sqlite_phase::open_existing;
use crate::sqlite_phase::open_pool;

pub use crate::sqlite_phase::Fault as PhaseFault;
pub use crate::sqlite_phase::OBSERVE_UNCLOSED;

/// A phased open stopped. `migration` is the script version when one is known.
#[derive(Debug)]
pub struct PhaseError {
    pub phase: &'static str,
    pub migration: Option<i64>,
    pub source: StoreError,
}

/// One store pool. Close it with [`StoreConn::close`].
pub struct StoreConn {
    inner: Guard,
    store: Option<Store>,
}

impl std::fmt::Debug for StoreConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreConn")
            .field("closed", &self.inner.pool().is_closed())
            .finish_non_exhaustive()
    }
}

impl StoreConn {
    /// Open an existing database without migrating it.
    ///
    /// `Ok(None)` means the file is absent: no connection was opened and the
    /// file was not created.
    pub async fn open_maintenance(
        path: &Path,
        fault: PhaseFault,
    ) -> Result<Option<Self>, PhaseError> {
        let opts = sqlite_file_options(path, false).map_err(open_err)?;
        Ok(open_existing(path, opts, fault)
            .await
            .map_err(map_err)?
            .map(|inner| Self { inner, store: None }))
    }

    /// Create the database if needed, migrate, and return the owning handle.
    /// [`Store::open`] is this followed by [`StoreConn::into_store`].
    pub async fn migrate(path: &Path, fault: PhaseFault) -> Result<Self, PhaseError> {
        if let Some(parent) = path.parent()
            && let Err(err) = std::fs::create_dir_all(parent)
        {
            return Err(open_err(StoreError::Conflict(err.to_string())));
        }
        let opts = sqlite_file_options(path, true).map_err(open_err)?;
        let mut conn = Self {
            inner: open_pool(opts, 5, fault).await.map_err(map_err)?,
            store: None,
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
        conn.store = Some(store_from_pool(conn.inner.pool().clone()));
        Ok(conn)
    }

    pub async fn applied_version(&self) -> Result<i64, StoreError> {
        crate::applied_schema_version(self.inner.pool()).await
    }

    /// `PRAGMA wal_checkpoint(TRUNCATE)` → `(busy, log, checkpointed)`.
    pub async fn checkpoint_truncate(&self) -> Result<(i64, i64, i64), StoreError> {
        checkpoint(self.inner.pool())
            .await
            .map_err(StoreError::from)
    }

    #[must_use]
    pub fn store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// Hand the pool to a [`Store`] without closing it.
    #[must_use]
    pub fn into_store(mut self) -> Store {
        self.inner.disarm();
        self.store
            .take()
            .unwrap_or_else(|| store_from_pool(self.inner.pool().clone()))
    }

    pub async fn close(self) {
        self.inner.close().await;
    }

    async fn fail(self, err: PhaseError) -> PhaseError {
        self.close().await;
        err
    }
}

/// Latest migration embedded in this binary.
#[must_use]
pub fn embedded_version() -> i64 {
    crate::embedded_schema_version()
}

fn open_err(source: StoreError) -> PhaseError {
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
            source: StoreError::Conflict(message),
        },
        StepError::Migrate { migration, source } => PhaseError {
            phase: "migrating",
            migration,
            source: StoreError::Migrate(source),
        },
    }
}

fn store_from_pool(pool: SqlitePool) -> Store {
    let (module_consent_revocations, _) = tokio::sync::broadcast::channel(128);
    Store {
        pool,
        module_consent_revocations,
        terminal_commit_ack_gate: std::sync::Arc::default(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::str::FromStr;
    use std::sync::atomic::Ordering;

    use super::OBSERVE_UNCLOSED;
    use super::PhaseFault;
    use super::StoreConn;
    use super::embedded_version;

    fn guard() {
        OBSERVE_UNCLOSED.store(true, Ordering::SeqCst);
    }

    fn fault(nth: u32, fail_open: bool) -> PhaseFault {
        PhaseFault {
            fail_open,
            fail_nth_pending: nth,
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

    #[tokio::test]
    async fn maintenance_does_not_create_migrate_or_skip_checkpoint() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("missing.db");
        assert!(
            StoreConn::open_maintenance(&missing, PhaseFault::none())
                .await
                .unwrap()
                .is_none()
        );
        assert!(!missing.exists());

        let path = dir.path().join("partial.db");
        let latest = embedded_version();
        crate::test_hooks::prepare_db_stopped_before(&path, latest)
            .await
            .unwrap();
        let before = crate::test_hooks::applied_schema_version(&path)
            .await
            .unwrap();
        assert!(before < latest);
        let conn = StoreConn::open_maintenance(&path, PhaseFault::none())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(conn.applied_version().await.unwrap(), before);
        let (busy, _, _) = conn.checkpoint_truncate().await.unwrap();
        assert_eq!(busy, 0);
        conn.close().await;
        assert_eq!(
            crate::test_hooks::applied_schema_version(&path)
                .await
                .unwrap(),
            before
        );
        assert!(can_take_exclusive(&path).await);
    }

    #[tokio::test]
    async fn connect_pragma_failure_is_open_and_releases_the_file() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pragma.db");
        let err = StoreConn::migrate(
            &path,
            PhaseFault {
                fail_open: false,
                fail_nth_pending: 0,
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
    async fn open_failure_releases_the_connection() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("open.db");
        StoreConn::migrate(&path, PhaseFault::none())
            .await
            .unwrap()
            .close()
            .await;
        let err = StoreConn::open_maintenance(&path, fault(0, true))
            .await
            .unwrap_err();
        assert_eq!(err.phase, "open");
        assert_eq!(err.migration, None);
        assert!(can_take_exclusive(&path).await, "{err:?}");
    }

    #[tokio::test]
    async fn nth_pending_migration_failure_closes_and_reports_its_version() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        for nth in [1_u32, 2] {
            let path = dir.path().join(format!("{nth}.db"));
            let err = StoreConn::migrate(&path, fault(nth, false))
                .await
                .unwrap_err();
            assert_eq!(err.phase, "migrating", "{err:?}");
            assert_eq!(err.migration, Some(i64::from(nth)), "{err:?}");
            assert!(can_take_exclusive(&path).await, "{err:?}");
            let conn = StoreConn::open_maintenance(&path, PhaseFault::none())
                .await
                .unwrap()
                .unwrap();
            let version = conn.applied_version().await.unwrap();
            conn.close().await;
            assert_eq!(version, i64::from(nth - 1));
        }
    }

    #[tokio::test]
    async fn migrate_and_store_open_reach_the_embedded_version() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent24.db");
        let conn = StoreConn::migrate(&path, PhaseFault::none()).await.unwrap();
        let version = conn
            .store()
            .unwrap()
            .latest_applied_migration()
            .await
            .unwrap();
        assert_eq!(version, embedded_version());
        conn.close().await;

        let opened = crate::Store::open(&dir.path().join("via-open.db"))
            .await
            .unwrap();
        assert_eq!(
            opened.latest_applied_migration().await.unwrap(),
            embedded_version()
        );
    }
}
