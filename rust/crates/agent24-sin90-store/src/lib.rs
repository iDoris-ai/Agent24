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

    /// Open `path`, first MIGRATING a database from `legacy` if one is there and
    /// `path` is not (ME-1b-b).
    ///
    /// Before ME-1b the kernel opened `~/.agent24/sin90.db` directly. Mounting
    /// Sin90 as a domain OS moves it to `~/.agent24/os/sin90/sin90.db`, and
    /// without this an upgrading user opens the new path, gets
    /// `create_if_missing`, and sees an EMPTY Sin90 — every direction, block and
    /// proposal apparently gone. That is silent data loss, so it is handled here,
    /// in the crate that owns the database, rather than in the generic mounter.
    ///
    /// The copy is one SQLite operation, not three filesystem ones. Moving
    /// `sin90.db`, `-wal` and `-shm` separately cannot be atomic, and after a
    /// crash committed data may live ONLY in the WAL — so a naive `rename` of the
    /// main file alone silently drops it. `VACUUM INTO` asks SQLite for a
    /// consistent snapshot of the whole database (WAL included) in one file.
    ///
    /// Ordering makes it idempotent and crash-safe (power-loss durable on unix,
    /// where the destination's directory is fsynced after the rename; elsewhere the
    /// rename is atomically visible but not fsynced):
    /// 1. `path` exists AND probes as an initialized Sin90 database → migration
    ///    already happened, and it wins: a legacy file is never allowed to
    ///    overwrite it (that would undo real work after a downgrade-then-upgrade).
    ///    Existence alone does NOT win — see the refusal branches below.
    /// 2. No `legacy` → nothing to migrate; open normally.
    /// 3. Otherwise: snapshot legacy → a temp file beside the destination,
    ///    `quick_check` it, then ATOMICALLY rename into place. A crash before the
    ///    rename leaves the destination absent, so the next start retries; a crash
    ///    after it leaves the destination present, so the next start skips.
    ///
    /// The legacy file's CONTENTS are never modified — it stays a rollback
    /// snapshot. (Reading a WAL-mode database may still create or touch its
    /// `-wal`/`-shm` sidecars when the directory is writable; that is SQLite's
    /// bookkeeping, not a change to the data.) Note that a
    /// downgrade after migration is not write-synchronized — new writes go only to
    /// the new path — which is a documented limitation, not something a link could
    /// fix: SQLite's WAL locking is path-sensitive and two aliases for one database
    /// can corrupt it.
    ///
    /// Any failure returns `Err` so the module DEGRADES (503). It must never fall
    /// through to `create_if_missing` and hand the user a working-looking, empty
    /// Sin90 — which is why an existing-but-uninitialized destination, and a legacy
    /// file that is not actually a Sin90 database, are both refused rather than
    /// opened: `quick_check` proves INTEGRITY, never IDENTITY.
    ///
    /// **Precondition: the caller must hold exclusive ownership of both paths.**
    /// `agent24d` does — the singleton lock is acquired before `serve()` and held
    /// for its lifetime — and this method relies on it: the temp file has a fixed
    /// name, and the destination/legacy checks are not atomic with the actions that
    /// follow them. Two concurrent migrators could delete each other's temp file,
    /// race the destination check, or drop writes committed to legacy after the
    /// snapshot. Do not call this from a context that cannot guarantee exclusivity.
    pub async fn open_migrating_from(path: &Path, legacy: &Path) -> Result<Self> {
        // `try_exists`, not `exists`: the latter reports a permission error as
        // `false`, which here would mean "no destination, migrate over it" — the
        // one answer a metadata failure must not produce.
        let dest_there = path
            .try_exists()
            .map_err(|e| StoreError::Internal(format!("cannot stat {}: {e}", path.display())))?;
        let legacy_there = legacy
            .try_exists()
            .map_err(|e| StoreError::Internal(format!("cannot stat {}: {e}", legacy.display())))?;

        if !legacy_there {
            return Self::open(path).await;
        }
        if dest_there {
            // EXISTENCE is not proof of a completed migration. A zero-byte or
            // half-created destination — an interrupted first start, a stray
            // `touch` — would otherwise "win", `create_if_missing` would fill it
            // with empty tables, and the user's real Sin90 would sit one directory
            // up, invisible forever. Refusing is loud and recoverable; opening it
            // is silent and not.
            match probe_sin90_db(path).await {
                Probe::Initialized => {}
                Probe::NotSin90 => {
                    return Err(StoreError::Internal(format!(
                        "{} exists but is not an initialized sin90 database, while a legacy \
                         database is present at {}. Refusing to open it: that would hide the \
                         legacy data behind an empty store. MOVE the incomplete file aside \
                         (do not delete it until you have looked at it) and start again.",
                        path.display(),
                        legacy.display()
                    )));
                }
                Probe::Unreadable(why) => {
                    // A lock, a permission problem, or corruption — and this file
                    // may hold NEWER data than the legacy one. Never suggest
                    // deleting it.
                    return Err(StoreError::Internal(format!(
                        "{} could not be inspected ({why}), and a legacy database is present \
                         at {}. Refusing to guess which one is current: fix access to the \
                         destination, or move it aside after inspecting it.",
                        path.display(),
                        legacy.display()
                    )));
                }
            }
            tracing::info!(
                "sin90: both {} and legacy {} exist; using the former and leaving \
                 the legacy file as a rollback snapshot",
                path.display(),
                legacy.display()
            );
            return Self::open(path).await;
        }

        // A legacy file that is not a Sin90 database must not be copied over as
        // one: `VACUUM INTO` + `quick_check` prove SQLite INTEGRITY, never
        // IDENTITY, so an unrelated (or empty) database would pass both and then
        // be filled with empty Sin90 tables by migrations — the same silent-empty
        // outcome by a different road.
        match probe_sin90_db(legacy).await {
            Probe::Initialized => {}
            Probe::NotSin90 => {
                return Err(StoreError::Internal(format!(
                    "{} is not an initialized sin90 database; refusing to migrate from it, \
                     because copying it would produce a valid-looking but EMPTY Sin90. Move \
                     it aside if it is not Sin90 data.",
                    legacy.display()
                )));
            }
            Probe::Unreadable(why) => {
                return Err(StoreError::Internal(format!(
                    "the legacy database at {} could not be inspected ({why}); refusing to \
                     migrate from a file we cannot read.",
                    legacy.display()
                )));
            }
        }
        migrate_legacy_db(legacy, path).await?;
        Self::open(path).await
    }

    pub(crate) fn pool(&self) -> &SqlitePool {
        &self.pool
    }
}

/// What an identity probe found at a path.
///
/// Three outcomes rather than a bool, because the ADVICE differs. "Not Sin90"
/// means the file is safe to move aside. "Could not be read" might mean a lock, a
/// permission problem, or corruption on a database holding the user's real data —
/// telling them to remove that would be the worst thing this code could say.
enum Probe {
    /// Openable, with a COMPLETED migration and Sin90 tables.
    Initialized,
    /// Opened fine, but it is not (or not yet) a Sin90 database.
    NotSin90,
    /// Could not be inspected at all; carries why.
    Unreadable(String),
}

/// Has `path` actually been through Sin90's migrations?
///
/// Identity, not integrity. `PRAGMA quick_check` says a file is a structurally
/// sound SQLite database; it says nothing about whether it is OURS. Two markers,
/// both required:
///
/// - a SUCCESSFUL row in sqlx's `_sqlx_migrations`. The table merely existing is
///   not enough: a half-applied migration can leave the table, and some DDL,
///   behind without any migration having completed.
/// - at least one `sin90_*` table.
///
/// The second is a PREFIX on purpose, not a specific table name. A false positive
/// costs a copy AND leaves that copy committed at the destination before
/// `Self::open` rejects it; a false NEGATIVE refuses to migrate a user who has real
/// data — worse than the silent-empty bug this exists to prevent — so it has to
/// survive the schema evolving. Renaming or dropping any single table keeps it
/// true.
///
/// (`PRAGMA application_id` would be the durable long-term marker. It cannot be
/// used alone yet: databases written before ME-1b-b carry no stamp, so adopting it
/// needs a one-time compatibility probe — exactly this one — before stamping.)
async fn probe_sin90_db(path: &Path) -> Probe {
    let opts = match SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display())) {
        Ok(o) => o.create_if_missing(false).read_only(true),
        Err(e) => return Probe::Unreadable(e.to_string()),
    };
    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await
    {
        Ok(p) => p,
        Err(e) => return Probe::Unreadable(e.to_string()),
    };

    // `ESCAPE` because `_` is a LIKE wildcard: without it `sin90_%` would also
    // match `sin90X...`, exactly the near-miss this probe exists to reject.
    let tables: std::result::Result<(i64, i64), sqlx::Error> = sqlx::query_as(
        "SELECT
             (SELECT count(*) FROM sqlite_master
                WHERE type = 'table' AND name = '_sqlx_migrations'),
             (SELECT count(*) FROM sqlite_master
                WHERE type = 'table' AND name LIKE 'sin90\\_%' ESCAPE '\\')",
    )
    .fetch_one(&pool)
    .await;

    let outcome = match tables {
        Err(e) => Probe::Unreadable(e.to_string()),
        Ok((0, _)) | Ok((_, 0)) => Probe::NotSin90,
        Ok(_) => {
            // The table is there; require a migration that actually COMPLETED.
            let applied: std::result::Result<(i64,), sqlx::Error> =
                sqlx::query_as("SELECT count(*) FROM _sqlx_migrations WHERE success <> 0")
                    .fetch_one(&pool)
                    .await;
            match applied {
                Ok((n,)) if n >= 1 => Probe::Initialized,
                Ok(_) => Probe::NotSin90,
                Err(e) => Probe::Unreadable(e.to_string()),
            }
        }
    };
    pool.close().await;
    outcome
}

/// Snapshot `legacy` into `dest` via `VACUUM INTO` + atomic rename. See
/// [`Sin90Store::open_migrating_from`].
async fn migrate_legacy_db(legacy: &Path, dest: &Path) -> Result<()> {
    let parent = dest
        .parent()
        .ok_or_else(|| StoreError::Internal(format!("{} has no parent", dest.display())))?;
    std::fs::create_dir_all(parent).map_err(|e| StoreError::Internal(e.to_string()))?;

    // Fixed name, not a random one: the daemon holds a singleton lock, so there is
    // no concurrent migrator to collide with, and a fixed name lets a crashed
    // attempt be cleaned up deterministically. `VACUUM INTO` REQUIRES the target
    // to be absent, so a stale temp file must go first.
    let tmp = parent.join("sin90.db.migrating");
    if tmp.exists() {
        tracing::warn!(
            "sin90: removing stale migration temp {} (a previous attempt crashed)",
            tmp.display()
        );
        std::fs::remove_file(&tmp).map_err(|e| StoreError::Internal(e.to_string()))?;
    }

    tracing::info!(
        "sin90: migrating {} -> {}",
        legacy.display(),
        dest.display()
    );

    // READ-ONLY on the legacy file: a migration must not be able to modify the
    // rollback snapshot, and `create_if_missing(false)` means a legacy path that
    // is not a database errors out instead of being created as an empty one.
    let src_opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", legacy.display()))?
        .create_if_missing(false)
        .read_only(true)
        .busy_timeout(std::time::Duration::from_secs(5));
    let src = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(src_opts)
        .await?;

    // Bound, not formatted: a path containing a quote would otherwise break the
    // statement — or worse, not break it.
    let copied = sqlx::query("VACUUM INTO ?")
        .bind(tmp.to_string_lossy().as_ref())
        .execute(&src)
        .await;
    src.close().await;
    if let Err(e) = copied {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.into());
    }

    // Verify the SNAPSHOT before it becomes the live database — a corrupt copy
    // renamed into place would be worse than no migration, because the legacy file
    // would then never be consulted again.
    let check_opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", tmp.display()))?
        .create_if_missing(false)
        .read_only(true);
    let check = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(check_opts)
        .await?;
    let integrity: std::result::Result<(String,), sqlx::Error> =
        sqlx::query_as("PRAGMA quick_check").fetch_one(&check).await;
    check.close().await;
    match integrity {
        Ok((ref s,)) if s == "ok" => {}
        Ok((s,)) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(StoreError::Internal(format!(
                "migrated sin90 snapshot failed quick_check: {s}"
            )));
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
    }

    // The commit point. Everything before this is discardable; after it, the
    // destination exists and later starts skip the migration entirely.
    std::fs::rename(&tmp, dest).map_err(|e| {
        StoreError::Internal(format!(
            "cannot rename {} to {}: {e}",
            tmp.display(),
            dest.display()
        ))
    })?;
    // The rename is atomically VISIBLE, but not yet DURABLE: after a power loss the
    // directory entry can be gone while the daemon has already accepted writes into
    // the destination, and the next start would then re-migrate stale legacy data
    // over them. fsync the directory so the commit survives. (unix only — a
    // directory cannot be opened as a file on Windows, so there the guarantee is
    // atomic-visible but not power-loss durable, as the doc says.)
    //
    // Errors PROPAGATE. Reporting "migrated" while the commit is not durable is
    // the same class of lie this whole function exists to avoid, and it would
    // contradict "any failure returns Err". The rename has already happened, so an
    // error here leaves a VALID destination — the next start finds it, skips the
    // migration and opens it — while the operator is told what failed.
    #[cfg(unix)]
    {
        let dir = std::fs::File::open(parent).map_err(|e| {
            StoreError::Internal(format!(
                "migrated, but cannot open {} to fsync it: {e}",
                parent.display()
            ))
        })?;
        dir.sync_all().map_err(|e| {
            StoreError::Internal(format!(
                "migrated, but fsync of {} failed: {e}",
                parent.display()
            ))
        })?;
    }
    tracing::info!(
        "sin90: migrated to {}; legacy {} kept as a rollback snapshot",
        dest.display(),
        legacy.display()
    );
    Ok(())
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
