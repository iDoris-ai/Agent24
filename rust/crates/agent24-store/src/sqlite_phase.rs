//! Shared SQLite open / migrate / close steps for CORE-5b.
//!
//! `agent24-memory` calls this module so the two databases share one
//! close-on-failure path and one pending-migration injection.

use std::borrow::Cow;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use sqlx::SqlitePool;
use sqlx::migrate::MigrateError;
use sqlx::migrate::Migrator;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::sqlite::SqlitePoolOptions;

/// When set, dropping an unclosed [`Guard`] keeps the pool alive so a test can
/// see the leaked lock. Production leaves this false.
#[doc(hidden)]
pub static OBSERVE_UNCLOSED: AtomicBool = AtomicBool::new(false);

/// Injection points for a phase open. [`Fault::none`] is the production path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fault {
    pub fail_open: bool,
    /// 1-based index into pending up migrations. 0 runs all of them.
    pub fail_nth_pending: u32,
    /// Hold a SQLite reserved lock until [`Guard::close`].
    pub retain_exclusive_lock: bool,
}

impl Fault {
    pub const fn none() -> Self {
        Self {
            fail_open: false,
            fail_nth_pending: 0,
            retain_exclusive_lock: false,
        }
    }
}

#[derive(Debug)]
pub enum StepError {
    Sqlx {
        phase: &'static str,
        source: sqlx::Error,
    },
    Msg {
        phase: &'static str,
        migration: Option<i64>,
        message: String,
    },
    Migrate {
        migration: Option<i64>,
        source: MigrateError,
    },
}

pub struct Guard {
    pool: SqlitePool,
    armed: bool,
}

impl Drop for Guard {
    fn drop(&mut self) {
        if self.armed && OBSERVE_UNCLOSED.load(Ordering::SeqCst) && !self.pool.is_closed() {
            std::mem::forget(self.pool.clone());
        }
    }
}

impl Guard {
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    pub fn disarm(&mut self) {
        self.armed = false;
    }

    /// `sqlite3_close` every worker. Drop alone does not wait.
    pub async fn close(mut self) {
        self.pool.close().await;
        self.armed = false;
    }
}

/// Missing file, and `fail_open` is clear: no connection and no create.
pub async fn open_existing(
    path: &Path,
    opts: SqliteConnectOptions,
    fault: Fault,
) -> Result<Option<Guard>, StepError> {
    if !fault.fail_open && !path.exists() {
        return Ok(None);
    }
    open_pool(opts, 1, fault).await.map(Some)
}

pub async fn open_pool(
    opts: SqliteConnectOptions,
    max: u32,
    fault: Fault,
) -> Result<Guard, StepError> {
    let max = if fault.retain_exclusive_lock { 1 } else { max };
    let pool = SqlitePoolOptions::new()
        .max_connections(max)
        .connect_with(opts)
        .await
        .map_err(|source| StepError::Sqlx {
            phase: "open",
            source,
        })?;
    let guard = Guard { pool, armed: true };
    if fault.retain_exclusive_lock
        && let Err(source) = hold(&guard.pool).await
    {
        guard.close().await;
        return Err(StepError::Sqlx {
            phase: "open",
            source,
        });
    }
    if fault.fail_open {
        guard.close().await;
        return Err(StepError::Msg {
            phase: "open",
            migration: None,
            message: "injected open failure".into(),
        });
    }
    Ok(guard)
}

async fn hold(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let _: String = sqlx::query_scalar("PRAGMA locking_mode=EXCLUSIVE")
        .fetch_one(pool)
        .await?;
    sqlx::query("CREATE TABLE IF NOT EXISTS core5b_hold (x INTEGER)")
        .execute(pool)
        .await?;
    // A second open finds the table already there. INSERT still writes, so
    // locking_mode=EXCLUSIVE acquires the lock on that connection.
    sqlx::query("INSERT INTO core5b_hold (x) VALUES (1)")
        .execute(pool)
        .await?;
    Ok(())
}

pub async fn checkpoint(pool: &SqlitePool) -> Result<(i64, i64, i64), sqlx::Error> {
    sqlx::query_as("PRAGMA wal_checkpoint(TRUNCATE)")
        .fetch_one(pool)
        .await
}

pub fn max_version(migrator: &Migrator) -> i64 {
    let mut max = 0i64;
    for migration in migrator.migrations.iter() {
        if migration.version > max {
            max = migration.version;
        }
    }
    max
}

pub async fn migrate_up(
    pool: &SqlitePool,
    mut migrator: Migrator,
    nth: u32,
) -> Result<(), StepError> {
    let target = poison(pool, &mut migrator, nth).await?;
    match migrator.run(pool).await {
        Ok(()) => Ok(()),
        Err(source) => Err(StepError::Migrate {
            migration: script_version(&source).or(target),
            source,
        }),
    }
}

fn script_version(err: &MigrateError) -> Option<i64> {
    match err {
        MigrateError::ExecuteMigration(_, version)
        | MigrateError::VersionMismatch(version)
        | MigrateError::Dirty(version) => Some(*version),
        _ => None,
    }
}

async fn poison(
    pool: &SqlitePool,
    migrator: &mut Migrator,
    nth: u32,
) -> Result<Option<i64>, StepError> {
    if nth == 0 {
        return Ok(None);
    }
    let applied = applied(pool).await?;
    let mut owned: Vec<_> = migrator.migrations.iter().cloned().collect();
    let mut seen = 0u32;
    let mut found = None;
    for migration in &mut owned {
        if !migration.migration_type.is_up_migration() || applied.contains(&migration.version) {
            continue;
        }
        seen += 1;
        if seen == nth {
            found = Some(migration.version);
            migration.sql = Cow::Owned("SELECT RAISE(FAIL, 'core5b');".into());
            break;
        }
    }
    let Some(version) = found else {
        return Err(StepError::Msg {
            phase: "migrating",
            migration: None,
            message: format!("no pending migration {nth}"),
        });
    };
    migrator.migrations = owned.into();
    Ok(Some(version))
}

async fn applied(pool: &SqlitePool) -> Result<Vec<i64>, StepError> {
    let tables: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_one(pool)
    .await
    .map_err(|source| StepError::Sqlx {
        phase: "migrating",
        source,
    })?;
    if tables == 0 {
        return Ok(Vec::new());
    }
    sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success = 1")
        .fetch_all(pool)
        .await
        .map_err(|source| StepError::Sqlx {
            phase: "migrating",
            source,
        })
}

pub async fn watermark(pool: &SqlitePool) -> Result<i64, sqlx::Error> {
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
    match version {
        Some(version) => Ok(version),
        None => Ok(0),
    }
}
