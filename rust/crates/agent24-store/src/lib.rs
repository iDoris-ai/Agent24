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
mod allocation_retention_evidence;
mod allocation_retention_expectation;
mod allocation_retention_plan;
mod allocation_retention_writer;
mod allocation_types;
mod audit;
mod conn_phase;
mod decision_log;
mod legacy_recovery;
mod model_call_timings;
mod module_approvals;
mod module_audit_event;
mod module_consents;
mod module_model_usage;
mod module_schedules;
mod repo;
mod run_workspace_admission;
mod run_workspace_authority;
mod run_workspace_orphan;
mod run_workspace_terminal;
pub mod source_policy;
#[doc(hidden)]
pub mod sqlite_phase;
mod terminal_core;
mod terminal_helpers;
pub(crate) mod terminal_mutations;
pub(crate) mod terminal_plan;
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
    WorkspaceProvenanceInput, WorkspaceResult, WorkspaceRootSnapshot, WorkspaceState,
    WorkspaceStoreError, WorkspaceTtl,
};

pub use allocation_intent::AllocationIntent;
pub use allocation_record::AllocationRecord;
pub use allocation_types::{
    AllocationFailureReason, AllocationId, AllocationPhase, AllocationValueError,
};
pub use audit::AuditEntry;
pub use decision_log::{
    DecisionLogExportRow, DecisionLogRow, DecisionOutcomeRow, NewDecisionLogEntry,
    NewDecisionOutcome,
};
pub use legacy_recovery::{
    LegacyRecoveryHold, RecoveryDecisionEffect, RecoveryState, recovery_decision_effect,
};
pub use model_call_timings::{CallTimingRow, CallTimingSummaryRow, NewCallTiming};
pub use module_audit_event::{
    ActorRef, AttemptId, AuditRef, AuditTimestamp, AuthorizationRef, DurationMs,
    InterruptedTerminalOutcome, MODULE_TOOL_AUDIT_RETENTION_DAYS, ModuleId, ModuleToolAuditEvent,
    ModuleToolAuditRelation, ModuleToolResultCode, OperationId, ResourceRef, RunId, SessionRef,
    SizeBytes, ToolCallId,
};
pub use module_consents::{
    ConsentDecision, ConsentLookup, ConsentSource, HostRiskLevel, ModuleConsentRecord,
    ModuleConsentRevocation, ToolPermissionSummary,
};
pub use module_model_usage::{ModelUsageDelta, ModelUsageRow, ServedBy, saturating_add_capped};
pub use module_schedules::*;
pub use repo::*;
pub use run_workspace_admission::{RunAdmission, RunAdmissionDenial};
pub use run_workspace_authority::RunWorkspaceAuthoritySnapshot;
pub use run_workspace_orphan::WorkspaceOrphanSweep;
pub use run_workspace_terminal::RunTerminalTransition;
pub use source_policy::{
    PolicySnapshot, SOURCE_TAG_SCHEMA_VERSION, SourceKind, SourceMode, SourceRef, StoredSourceTag,
};

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
    /// ME4-1.2.1 / design §6.2, §6.4: a module's `_a24/scheduler/upsert` for
    /// a brand-new key when it already owns 256 rows. Counted inside the
    /// `BEGIN IMMEDIATE` transaction that would have inserted the row, so
    /// nothing is written and two concurrent upserts cannot jointly cross
    /// the line.
    #[error("module schedule quota reached")]
    QuotaExceeded,
    /// K1-7.1 (ADR-K1-04 §2.1): a typed K1 module-tool audit field failed
    /// its closed-set/bounded-identifier validation.
    #[error("invalid audit metadata: {0}")]
    InvalidAuditMetadata(String),
    /// Offline open refused to migrate. `current` is 0 when the file or the
    /// `_sqlx_migrations` table is absent.
    #[error("请先启动 daemon 完成升级 (applied {current}, binary {latest})")]
    SchemaBehind { current: i64, latest: i64 },
    #[error("database schema {current} is newer than this binary ({latest})")]
    SchemaAhead { current: i64, latest: i64 },
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub use conn_phase::{OBSERVE_UNCLOSED, PhaseError, PhaseFault, StoreConn, embedded_version};

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
    module_consent_revocations: tokio::sync::broadcast::Sender<ModuleConsentRevocation>,
    terminal_commit_ack_gate:
        std::sync::Arc<std::sync::Mutex<Option<std::sync::Arc<tokio::sync::Notify>>>>,
}

impl Store {
    /// Open (creating if needed) a database file and run migrations.
    pub async fn open(path: &Path) -> Result<Self> {
        StoreConn::migrate(path, PhaseFault::none())
            .await
            .map(StoreConn::into_store)
            .map_err(|err| err.source)
    }

    /// Open an existing file and do not migrate it.
    ///
    /// A separate function, not a flag on [`Store::open`]: the daemon keeps
    /// migrating, and an offline caller cannot forget the flag. A missing
    /// file is schema 0 and is not created. The applied version is read on a
    /// read-only connection that does not set `journal_mode` and does not
    /// create the file. Only a matching version opens the read-write pool.
    /// `journal_mode=WAL` rewrites the database header and creates `-wal` /
    /// `-shm`, so a mismatched DELETE file must be refused before that.
    pub async fn open_without_migration(path: &Path) -> Result<Self> {
        let latest = embedded_schema_version();
        if !path.exists() {
            return Err(StoreError::SchemaBehind { current: 0, latest });
        }
        let current = schema_version_readonly(path).await?;
        if current != latest {
            return Err(if current < latest {
                StoreError::SchemaBehind { current, latest }
            } else {
                StoreError::SchemaAhead { current, latest }
            });
        }
        let pool = SqlitePoolOptions::new()
            .max_connections(5)
            .connect_with(sqlite_file_options(path, false)?)
            .await?;
        let (module_consent_revocations, _) = tokio::sync::broadcast::channel(128);
        Ok(Self {
            pool,
            module_consent_revocations,
            terminal_commit_ack_gate: std::sync::Arc::default(),
        })
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
        let (module_consent_revocations, _) = tokio::sync::broadcast::channel(128);
        Ok(Self {
            pool,
            module_consent_revocations,
            terminal_commit_ack_gate: std::sync::Arc::default(),
        })
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// Latest successful migration in `_sqlx_migrations`, or 0 when none
    /// has been applied. This is the run-database watermark `/health` reports.
    pub async fn latest_applied_migration(&self) -> Result<i64> {
        let version: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(&self.pool)
                .await?;
        Ok(version.unwrap_or(0))
    }
}

fn sqlite_file_options(path: &Path, create_if_missing: bool) -> Result<SqliteConnectOptions> {
    Ok(
        SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))?
            .create_if_missing(create_if_missing)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true),
    )
}

/// Read `_sqlx_migrations` without changing the file.
///
/// `mode=ro` / [`SqliteConnectOptions::read_only`] and no `journal_mode`.
/// `create_if_missing` stays false.
fn sqlite_readonly_options(path: &Path) -> Result<SqliteConnectOptions> {
    Ok(
        SqliteConnectOptions::from_str(&format!("sqlite://{}?mode=ro", path.display()))?
            .read_only(true)
            .create_if_missing(false),
    )
}

async fn schema_version_readonly(path: &Path) -> Result<i64> {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(sqlite_readonly_options(path)?)
        .await?;
    let current = applied_schema_version(&pool).await;
    pool.close().await;
    current
}

fn embedded_schema_version() -> i64 {
    sqlx::migrate!("./migrations")
        .migrations
        .iter()
        .map(|migration| migration.version)
        .max()
        .unwrap_or(0)
}

async fn applied_schema_version(pool: &SqlitePool) -> Result<i64> {
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await?;
    if tables == 0 {
        return Ok(0);
    }
    let version: Option<i64> =
        sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
            .fetch_one(pool)
            .await?;
    Ok(version.unwrap_or(0))
}

/// Test-only escape hatch (integration tests need raw SQL to simulate
/// tampering). Not part of the supported API.
#[doc(hidden)]
pub mod test_hooks {
    use std::sync::Arc;
    use tokio::sync::Notify;

    pub fn pool(store: &super::Store) -> &sqlx::SqlitePool {
        store.pool()
    }

    /// Pause the next typed terminal append after SQLite has committed but
    /// before the append future acknowledges completion to its caller.
    pub fn pause_next_terminal_commit_ack(store: &super::Store) -> Arc<Notify> {
        let gate = Arc::new(Notify::new());
        let mut configured_gate = store
            .terminal_commit_ack_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *configured_gate = Some(Arc::clone(&gate));
        gate
    }

    pub(crate) async fn terminal_commit_ack_boundary(store: &super::Store) {
        let gate = store
            .terminal_commit_ack_gate
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(gate) = gate {
            gate.notify_one();
            gate.notified().await;
        }
    }

    /// Build a `Store` around an already-migrated pool a test built by hand
    /// (a WAL file with a chosen connection count, or a migrator truncated to
    /// an older version) — `Store::open`/`open_memory` hardcode their own
    /// pool options and always run every migration. ME4-1.2.1a note: this
    /// moved here (rather than the tick-read-model cut where it was first
    /// sketched) because C1.2's own concurrency test — in THIS cut — already
    /// needs a WAL file at `max_connections(2)`, which only this hook can
    /// build.
    #[must_use]
    pub fn from_pool(pool: sqlx::SqlitePool) -> super::Store {
        let (module_consent_revocations, _) = tokio::sync::broadcast::channel(128);
        super::Store {
            pool,
            module_consent_revocations,
            terminal_commit_ack_gate: std::sync::Arc::default(),
        }
    }

    /// Create `path` with only migrations whose version is `< stop_before`.
    pub async fn prepare_db_stopped_before(
        path: &std::path::Path,
        stop_before: i64,
    ) -> super::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| super::StoreError::Conflict(e.to_string()))?;
        }
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(super::sqlite_file_options(path, true)?)
            .await?;
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.migrations = migrator
            .migrations
            .iter()
            .filter(|migration| migration.version < stop_before)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        migrator.run(&pool).await?;
        pool.close().await;
        Ok(())
    }

    pub async fn applied_schema_version(path: &std::path::Path) -> super::Result<i64> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(super::sqlite_file_options(path, false)?)
            .await?;
        let version = super::applied_schema_version(&pool).await?;
        pool.close().await;
        Ok(version)
    }

    pub fn embedded_schema_version() -> i64 {
        super::embedded_schema_version()
    }

    /// Truncate the WAL and close this pool so a later open sees one file.
    pub async fn checkpoint(store: &super::Store) -> super::Result<()> {
        sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
            .execute(store.pool())
            .await?;
        store.pool().close().await;
        Ok(())
    }

    pub async fn revocation_count(store: &super::Store, module: &str) -> super::Result<i64> {
        let count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM module_consent_revocations WHERE module = ?")
                .bind(module)
                .fetch_one(store.pool())
                .await?;
        Ok(count)
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

#[cfg(test)]
mod open_without_migration_tests {
    #![allow(clippy::unwrap_used)]

    use std::str::FromStr;

    fn sidecar(path: &std::path::Path, suffix: &str) -> std::path::PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        std::path::PathBuf::from(name)
    }

    async fn delete_mode_database(path: &std::path::Path, ahead: bool) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let options =
            sqlx::sqlite::SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true)
                .journal_mode(sqlx::sqlite::SqliteJournalMode::Delete)
                .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        let latest = super::embedded_schema_version();
        let mut migrator = sqlx::migrate!("./migrations");
        if !ahead {
            migrator.migrations = migrator
                .migrations
                .iter()
                .filter(|migration| migration.version < latest)
                .cloned()
                .collect::<Vec<_>>()
                .into();
        }
        migrator.run(&pool).await.unwrap();
        if ahead {
            sqlx::query(
                "INSERT INTO _sqlx_migrations \
                 (version, description, success, checksum, execution_time) \
                 VALUES (?, 'test schema ahead', 1, ?, 0)",
            )
            .bind(latest + 1)
            .bind(Vec::<u8>::new())
            .execute(&pool)
            .await
            .unwrap();
        }
        let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(mode.to_ascii_lowercase(), "delete");
        pool.close().await;
        assert!(
            !sidecar(path, "-wal").exists() && !sidecar(path, "-shm").exists(),
            "DELETE fixture must not already have WAL sidecars"
        );
    }

    #[tokio::test]
    async fn open_without_migration_rejects_a_delete_mode_mismatch_without_rewriting_the_file() {
        let dir = tempfile::tempdir().unwrap();
        for ahead in [false, true] {
            let path = dir
                .path()
                .join(if ahead { "ahead.db" } else { "behind.db" });
            delete_mode_database(&path, ahead).await;
            let before = std::fs::read(&path).unwrap();
            let err = match super::Store::open_without_migration(&path).await {
                Ok(_) => panic!("mismatched schema must be refused"),
                Err(err) => err,
            };
            let message = err.to_string();
            if ahead {
                assert!(message.contains("newer"), "{message}");
            } else {
                assert!(message.contains("请先启动 daemon"), "{message}");
            }
            assert_eq!(
                before,
                std::fs::read(&path).unwrap(),
                "refusing {message} must not rewrite the database header"
            );
            assert!(
                !sidecar(&path, "-wal").exists(),
                "refusing a DELETE database must not create a -wal sidecar"
            );
            assert!(
                !sidecar(&path, "-shm").exists(),
                "refusing a DELETE database must not create a -shm sidecar"
            );
        }
    }
}
