//! CORE-5b migration coordinator.
//!
//! Owns every startup connection for the two databases: open maintenance,
//! close those connections, migrate the store, then migrate memory including
//! the FTS rebuild. Any failure closes whatever is still open and returns the
//! database name, migration version, and phase.
//!
//! [`ENABLED`] is false. Production startup does not call [`run`]. The memory
//! failure here is a hard gate; `open_memory_base` is unchanged and still
//! warns.

use std::path::Path;

use agent24_memory::KvStore;
use agent24_memory::MemoryConn;
use agent24_store::Store;
use agent24_store::StoreConn;

/// Keep this false until a later slice wires the coordinator into startup.
pub const ENABLED: bool = false;

/// Structured failure for one database. `migration` is `None` when no script
/// number is known (open, or FTS rebuild).
#[derive(Debug, PartialEq, Eq)]
pub struct MigrationFailure {
    pub database: &'static str,
    pub migration: Option<i64>,
    pub phase: &'static str,
    pub summary: String,
}

/// Both databases after a successful coordinated migrate. Drop is not close.
#[derive(Debug)]
pub struct MigratedPair {
    store: StoreConn,
    memory: MemoryConn,
}

impl MigratedPair {
    #[must_use]
    pub fn store(&self) -> Option<&Store> {
        self.store.store()
    }

    #[must_use]
    pub fn memory(&self) -> Option<&KvStore> {
        self.memory.kv()
    }

    pub async fn close(self) {
        self.store.close().await;
        self.memory.close().await;
    }
}

/// Maintenance, then store migrate, then memory migrate and FTS rebuild.
///
/// A missing database is not a maintenance failure. An injected or real
/// failure closes every connection this function opened.
pub async fn run(
    store_path: &Path,
    memory_path: &Path,
    store_fault: agent24_store::PhaseFault,
    memory_fault: agent24_memory::PhaseFault,
) -> Result<MigratedPair, MigrationFailure> {
    let store_maint = match StoreConn::open_maintenance(store_path, store_fault).await {
        Ok(conn) => conn,
        Err(err) => return Err(from_store(err)),
    };
    let memory_maint = match MemoryConn::open_maintenance(memory_path, memory_fault).await {
        Ok(conn) => conn,
        Err(err) => {
            if let Some(conn) = store_maint {
                conn.close().await;
            }
            return Err(from_memory(err));
        }
    };
    if let Some(conn) = store_maint {
        conn.close().await;
    }
    if let Some(conn) = memory_maint {
        conn.close().await;
    }

    let store = match StoreConn::migrate(store_path, store_fault).await {
        Ok(conn) => conn,
        Err(err) => return Err(from_store(err)),
    };
    let memory = match MemoryConn::migrate(memory_path, memory_fault).await {
        Ok(conn) => conn,
        Err(err) => {
            store.close().await;
            return Err(from_memory(err));
        }
    };
    Ok(MigratedPair { store, memory })
}

fn from_store(err: agent24_store::PhaseError) -> MigrationFailure {
    MigrationFailure {
        database: "agent24.db",
        migration: err.migration,
        phase: err.phase,
        summary: err.source.to_string(),
    }
}

fn from_memory(err: agent24_memory::PhaseError) -> MigrationFailure {
    MigrationFailure {
        database: "memory.db",
        migration: err.migration,
        phase: err.phase,
        summary: err.source.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::path::Path;
    use std::str::FromStr;
    use std::sync::atomic::Ordering;

    use agent24_memory::PhaseFault as MemoryFault;
    use agent24_store::PhaseFault as StoreFault;

    use super::run;

    fn guard() {
        agent24_store::OBSERVE_UNCLOSED.store(true, Ordering::SeqCst);
        agent24_memory::OBSERVE_UNCLOSED.store(true, Ordering::SeqCst);
    }

    fn store_fault(nth: u32, fail_open: bool) -> StoreFault {
        StoreFault {
            fail_open,
            fail_nth_pending: nth,
            retain_exclusive_lock: true,
        }
    }

    fn memory_fault(nth: u32, fail_open: bool, fail_fts: bool) -> MemoryFault {
        MemoryFault {
            fail_open,
            fail_nth_pending: nth,
            fail_fts_rebuild: fail_fts,
            retain_exclusive_lock: true,
        }
    }

    async fn can_take_exclusive(path: &Path) -> bool {
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
    async fn success_path_lands_both_watermarks_and_close_releases_both_locks() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("agent24.db");
        let memory_path = dir.path().join("memory.db");
        let opened = run(
            &store_path,
            &memory_path,
            store_fault(0, false),
            memory_fault(0, false, false),
        )
        .await
        .unwrap();
        let store = opened.store().unwrap_or_else(|| panic!("missing store"));
        let memory = opened.memory().unwrap_or_else(|| panic!("missing memory"));
        assert_eq!(
            store.latest_applied_migration().await.unwrap(),
            agent24_store::embedded_version()
        );
        assert_eq!(
            memory.latest_applied_migration().await.unwrap(),
            agent24_memory::embedded_version()
        );
        assert!(!can_take_exclusive(&store_path).await);
        assert!(!can_take_exclusive(&memory_path).await);
        opened.close().await;
        assert!(can_take_exclusive(&store_path).await);
        assert!(can_take_exclusive(&memory_path).await);

        // Second pass: both files exist, so maintenance opens real connections
        // before the migrate phase.
        let again = run(
            &store_path,
            &memory_path,
            store_fault(0, false),
            memory_fault(0, false, false),
        )
        .await
        .unwrap();
        assert!(!can_take_exclusive(&store_path).await);
        assert!(!can_take_exclusive(&memory_path).await);
        again.close().await;
        assert!(can_take_exclusive(&store_path).await);
        assert!(can_take_exclusive(&memory_path).await);
    }

    #[tokio::test]
    async fn store_migration_failure_is_structured_and_leaves_no_connection() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("agent24.db");
        let memory_path = dir.path().join("memory.db");
        let err = run(
            &store_path,
            &memory_path,
            store_fault(2, false),
            memory_fault(0, false, false),
        )
        .await
        .unwrap_err();
        assert_eq!(err.database, "agent24.db");
        assert_eq!(err.phase, "migrating");
        assert_eq!(err.migration, Some(2));
        assert!(!memory_path.exists());
        assert!(can_take_exclusive(&store_path).await, "{err:?}");
    }

    #[tokio::test]
    async fn memory_migration_and_fts_failures_stop_the_pair() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("agent24.db");
        let memory_path = dir.path().join("memory.db");
        let err = run(
            &store_path,
            &memory_path,
            store_fault(0, false),
            memory_fault(1, false, false),
        )
        .await
        .unwrap_err();
        assert_eq!(err.database, "memory.db");
        assert_eq!(err.phase, "migrating");
        assert_eq!(err.migration, Some(1));
        assert!(can_take_exclusive(&store_path).await, "{err:?}");
        assert!(can_take_exclusive(&memory_path).await, "{err:?}");

        let fts = run(
            &dir.path().join("agent24-fts.db"),
            &dir.path().join("memory-fts.db"),
            store_fault(0, false),
            memory_fault(0, false, true),
        )
        .await
        .unwrap_err();
        assert_eq!(fts.database, "memory.db");
        assert_eq!(fts.phase, "fts_rebuild");
        assert_eq!(fts.migration, None);
        assert!(can_take_exclusive(&dir.path().join("agent24-fts.db")).await);
        assert!(can_take_exclusive(&dir.path().join("memory-fts.db")).await);
    }

    #[tokio::test]
    async fn open_failure_on_either_database_releases_every_connection() {
        guard();
        let dir = tempfile::tempdir().unwrap();
        let store_path = dir.path().join("agent24.db");
        let memory_path = dir.path().join("memory.db");
        agent24_store::StoreConn::migrate(&store_path, StoreFault::none())
            .await
            .unwrap()
            .close()
            .await;
        agent24_memory::MemoryConn::migrate(&memory_path, MemoryFault::none())
            .await
            .unwrap()
            .close()
            .await;

        let err = run(
            &store_path,
            &memory_path,
            store_fault(0, true),
            memory_fault(0, false, false),
        )
        .await
        .unwrap_err();
        assert_eq!(err.database, "agent24.db");
        assert_eq!(err.phase, "open");
        assert_eq!(err.migration, None);
        assert!(can_take_exclusive(&store_path).await, "{err:?}");
        assert!(can_take_exclusive(&memory_path).await, "{err:?}");

        let err = run(
            &store_path,
            &memory_path,
            store_fault(0, false),
            memory_fault(0, true, false),
        )
        .await
        .unwrap_err();
        assert_eq!(err.database, "memory.db");
        assert_eq!(err.phase, "open");
        assert_eq!(err.migration, None);
        assert!(can_take_exclusive(&store_path).await, "{err:?}");
        assert!(can_take_exclusive(&memory_path).await, "{err:?}");
    }
}
