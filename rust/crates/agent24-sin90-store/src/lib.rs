//! Sin90 persistence — sqlx over its OWN `sin90.db` (SIN90-domain.md §3).
//!
//! Mirrors `agent24-store`'s discipline: every status change runs inside a
//! `BEGIN IMMEDIATE` transaction — current state is read under the write lock,
//! checked against `agent24-sin90`'s transition matrix, then updated, and an
//! event is appended in the SAME transaction. Proposal apply is CAS-idempotent
//! (pending → applying → applied) so a re-tried accept never applies twice.
//! This crate depends on the kernel (agent24-sin90, agent24-core util) but the
//! kernel never depends on it — the dependency arrow points one way.

mod attention;
mod repo;

pub use attention::AttentionRow;
pub use repo::{AppliedProposal, ApplyOutcome, StoredProposal};

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
    Transition(#[from] agent24_sin90::TransitionError),
    #[error(transparent)]
    Proposal(#[from] agent24_sin90::ProposalError),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    /// Relational invariants the pure validator cannot see (ValidationCtx is
    /// per-entity), enforced here under the write lock (SIN90-domain.md §2.3):
    /// mutating a task whose week is not open (planning|active) ...
    #[error("task {0}'s week is not open; cannot mutate")]
    WeekNotOpen(String),
    /// ... or carrying a task over into the very week it already lives in
    /// (which would strand the closed task and spawn an endlessly re-carryable
    /// duplicate in the same week).
    #[error("cannot carry task {0} into its own week")]
    SameWeekCarry(String),
    /// A broken internal invariant (not the client's fault) — maps to 500.
    #[error("internal: {0}")]
    Internal(String),
}

impl StoreError {
    /// True if this is a FOREIGN KEY violation (SQLite extended code 787) —
    /// i.e. the client referenced an entity that doesn't exist, a 4xx not a 5xx.
    /// Encapsulated here so callers need not depend on sqlx to classify it.
    pub fn is_fk_violation(&self) -> bool {
        matches!(
            self,
            StoreError::Sqlx(sqlx::Error::Database(db)) if db.code().as_deref() == Some("787")
        )
    }
}

pub type Result<T> = std::result::Result<T, StoreError>;

/// Opaque wire status returned to callers is defined in `agent24-sin90`; this
/// struct only owns the connection pool.
#[derive(Clone)]
pub struct Sin90Store {
    pool: SqlitePool,
}

impl Sin90Store {
    /// Open (creating if needed) `sin90.db` and run migrations.
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

    /// In-memory database for tests (single connection — each `:memory:` handle
    /// is its own database).
    pub async fn open_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
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

/// Test-only escape hatches (raw SQL peeks + a mutation to prove event replay
/// is unaffected). Feature-gated so they are NOT part of the released API —
/// `#[doc(hidden)]` alone would still export them. Integration tests get them
/// via the self dev-dependency in Cargo.toml (`features = ["test-hooks"]`).
#[cfg(any(test, feature = "test-hooks"))]
#[doc(hidden)]
pub mod test_hooks {
    use crate::{Result, Sin90Store};
    use sqlx::Row;

    /// Rename a direction via raw SQL — historical event payloads must NOT change.
    pub async fn rename_direction(store: &Sin90Store, id: &str, new_title: &str) -> Result<()> {
        sqlx::query("UPDATE sin90_directions SET title = ? WHERE id = ?")
            .bind(new_title)
            .bind(id)
            .execute(store.pool())
            .await?;
        Ok(())
    }

    pub async fn direction_count(store: &Sin90Store) -> Result<i64> {
        Ok(sqlx::query("SELECT COUNT(*) AS n FROM sin90_directions")
            .fetch_one(store.pool())
            .await?
            .get::<i64, _>("n"))
    }

    /// The lexicographically-first task id (tests seed exactly one).
    pub async fn first_task_id(store: &Sin90Store) -> Result<String> {
        Ok(
            sqlx::query("SELECT id FROM sin90_tasks ORDER BY id LIMIT 1")
                .fetch_one(store.pool())
                .await?
                .get::<String, _>("id"),
        )
    }

    /// Force a week to `closed` via raw SQL, to test the store-side relational
    /// invariant (a pure proposal can't legally close a week itself here).
    pub async fn close_week(store: &Sin90Store, id: &str) -> Result<()> {
        sqlx::query("UPDATE sin90_weeks SET status = 'closed' WHERE id = ?")
            .bind(id)
            .execute(store.pool())
            .await?;
        Ok(())
    }

    /// Count events appended for one entity — proves a submit/apply left the
    /// expected receipt in `sin90_events` (and that an idempotent replay did not).
    pub async fn event_count(store: &Sin90Store, entity: &str, entity_id: &str) -> Result<i64> {
        Ok(
            sqlx::query(
                "SELECT COUNT(*) AS n FROM sin90_events WHERE entity = ? AND entity_id = ?",
            )
            .bind(entity)
            .bind(entity_id)
            .fetch_one(store.pool())
            .await?
            .get::<i64, _>("n"),
        )
    }

    pub async fn proposal_status(store: &Sin90Store, id: &str) -> Result<Option<String>> {
        Ok(
            sqlx::query("SELECT status FROM sin90_proposals WHERE id = ?")
                .bind(id)
                .fetch_optional(store.pool())
                .await?
                .map(|r| r.get::<String, _>("status")),
        )
    }
}
