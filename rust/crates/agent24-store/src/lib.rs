//! Agent24 persistence (C1 scope).
//!
//! sqlx SQLite repositories over the protocol types. Status updates run
//! inside BEGIN IMMEDIATE transactions: current state is read under the write
//! lock, checked against `agent24-core`'s transition matrix, then updated —
//! illegal or raced transitions surface as precise errors, never clobber.
//!
//! Queries are runtime-bound (`sqlx::query`) for now; the switch to
//! compile-time-checked macros + committed `.sqlx` offline data is planned
//! once the query surface stabilizes at the end of C2 (recorded deviation).

mod allocation_commitment;
mod allocation_intent;
mod allocation_materialization;
mod allocation_record;
mod allocation_registration;
// The authorized service slice has not wired this internal journal primitive yet.
#[allow(dead_code)]
mod allocation_reservation;
mod allocation_types;
mod audit;
mod module_approvals;
mod repo;
pub mod workspace_api;
pub mod workspace_decode;
pub(crate) mod workspace_decode_support;
pub mod workspace_lease_decode;
mod workspace_lifecycle;
mod workspace_registry;
pub(crate) mod workspace_tx;
pub mod workspaces;
pub use workspace_api::{
    WorkspaceListCursor, WorkspaceListLimit, WorkspaceListQuery, WorkspacePage,
};
pub use workspace_decode::WorkspaceRow;
pub use workspace_lease_decode::WorkspaceLeaseRow;
pub use workspaces::{
    HostLeaseTtl, LeaseKind, LifecycleOwnerRef, NewScratchWorkspace, RootIdentity,
    TrustedRootRegistration, WorkspaceAuthority, WorkspaceCleanupRecord, WorkspaceConflict,
    WorkspaceInstant, WorkspaceKind, WorkspaceLeaseId, WorkspaceLeaseRecord,
    WorkspaceProvenanceInput, WorkspaceResult, WorkspaceState, WorkspaceStoreError, WorkspaceTtl,
};

pub use allocation_intent::AllocationIntent;
pub use allocation_record::AllocationRecord;
pub use allocation_types::{
    AllocationFailureReason, AllocationId, AllocationPhase, AllocationValueError,
};
pub use audit::AuditEntry;
pub use repo::*;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlx(#[from] sqlx::Error),
    #[error(transparent)]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("serialization: {0}")]
    Serde(#[from] serde_json::Error),
    #[error(transparent)]
    Transition(#[from] agent24_core::TransitionError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Persistence boundary for trusted, internally authorized workflow code.
///
/// `Store` validates and journals persistence data; it does not establish
/// caller authorization, filesystem authority, or lifecycle ownership. Public
/// workspace operations belong to the authorized `WorkspaceService` layer.
///
/// ```compile_fail
/// let _ = agent24_store::Store::reserve_workspace_allocation;
/// ```
#[derive(Clone)]
pub struct Store {
    pool: SqlitePool,
}

impl Store {
    /// Open (creating if needed) a database file and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| StoreError::Conflict(e.to_string()))?;
        }
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory database for tests.
    pub async fn open_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        // A single connection: every :memory: connection is a separate database
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await?;
        sqlx::migrate!("./migrations").run(&pool).await?;
        Ok(Self { pool })
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// Test-only escape hatch (integration tests need raw SQL to simulate
/// tampering). Not part of the supported API.
#[doc(hidden)]
pub mod test_hooks {
    pub fn pool(store: &super::Store) -> &sqlx::SqlitePool {
        store.pool()
    }

    /// Insert a schedule row with arbitrary (possibly invalid) JSON columns —
    /// lets tests simulate a corrupt row that would fail deserialization.
    pub async fn insert_raw_schedule(
        store: &super::Store,
        id: &str,
        spec_json: &str,
        next_run_at: &str,
    ) -> super::Result<()> {
        sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery,
                                    last_run_at, next_run_at, consecutive_failures)
             VALUES (?, 'corrupt', 1, ?, '{}', '[]', NULL, ?, 0)",
        )
        .bind(id)
        .bind(spec_json)
        .bind(next_run_at)
        .execute(store.pool())
        .await?;
        Ok(())
    }
}
