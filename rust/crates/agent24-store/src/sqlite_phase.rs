//! Shared SQLite open / migrate / close steps for CORE-5b.
//!
//! `agent24-memory` calls this module so the two databases share one
//! close-on-failure path and one pending-migration injection.

use std::borrow::Cow;
use std::path::Path;
use std::str::FromStr;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;

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
    /// Run a locking statement inside SQLx's connect PRAGMAs, then fail.
    /// The worker is still inside `sqlite3_step` when connect returns.
    pub fail_connect_pragma: bool,
}

impl Fault {
    pub const fn none() -> Self {
        Self {
            fail_open: false,
            fail_nth_pending: 0,
            retain_exclusive_lock: false,
            fail_connect_pragma: false,
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
    let path = opts.get_filename().to_owned();
    let (opts, acquire_timeout) = connect_plan(opts, fault);
    let mut options = SqlitePoolOptions::new()
        .max_connections(max)
        .min_connections(0);
    if let Some(timeout) = acquire_timeout {
        options = options.acquire_timeout(timeout);
    }
    // Lazy open: the guard exists before the first connection and its PRAGMAs.
    // `connect_with` would open that connection before the guard exists.
    let guard = Guard {
        pool: options.connect_lazy_with(opts),
        armed: true,
    };
    // Dropping `PoolConnection` only spawns its return. Wait until the
    // connection is idle, or a second connection can open while
    // `journal_mode=WAL` still holds the file.
    match guard.pool().acquire().await {
        Ok(mut conn) => conn.return_to_pool().await,
        Err(source) => return Err(close_open_failure(guard, &path, source).await),
    }
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

/// `locking_mode` is emitted before `journal_mode`. The write takes an
/// exclusive lock; the recursive read holds that lock inside `sqlite3_step`
/// for longer than the acquire timeout. The missing table makes a fast
/// machine still fail the open when the timeout does not fire.
const CONNECT_PRAGMA_FAIL: &str = "\
EXCLUSIVE; \
CREATE TABLE IF NOT EXISTS core5b_hold (x INTEGER); \
INSERT INTO core5b_hold (x) VALUES (1); \
WITH RECURSIVE c(x) AS ( \
SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 3000000 \
) SELECT sum(x) FROM c; \
SELECT * FROM core5b_pragma_fail";

fn connect_plan(
    opts: SqliteConnectOptions,
    fault: Fault,
) -> (SqliteConnectOptions, Option<Duration>) {
    if !fault.fail_connect_pragma {
        return (opts, None);
    }
    (
        opts.pragma("locking_mode", CONNECT_PRAGMA_FAIL),
        Some(Duration::from_millis(400)),
    )
}

/// How long an open failure waits for a worker SQLx dropped inside connect.
const RELEASE_WAIT: Duration = Duration::from_secs(15);

/// [`Pool::close`] joins workers the pool owns. A PRAGMA error drops the
/// `SqliteConnection` inside `ConnectOptions::connect` before the pool takes
/// it, so close returns while that thread can still hold the file. Poll until
/// another connection can take `BEGIN EXCLUSIVE`, or until [`RELEASE_WAIT`].
async fn close_open_failure(guard: Guard, path: &Path, source: sqlx::Error) -> StepError {
    guard.close().await;
    let deadline = Instant::now() + RELEASE_WAIT;
    loop {
        if file_is_free(path).await || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    StepError::Sqlx {
        phase: "open",
        source,
    }
}

async fn file_is_free(path: &Path) -> bool {
    if !path.is_file() {
        return true;
    }
    let Ok(options) = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
    else {
        return true;
    };
    let options = options
        .create_if_missing(false)
        .busy_timeout(Duration::from_millis(0));
    let pool = match SqlitePoolOptions::new()
        .max_connections(1)
        .acquire_timeout(Duration::from_millis(200))
        .connect_with(options)
        .await
    {
        Ok(pool) => pool,
        Err(err) if still_held(&err) => return false,
        Err(_) => return true,
    };
    let free = match pool.begin_with("BEGIN EXCLUSIVE").await {
        Ok(_) => true,
        Err(err) if still_held(&err) => false,
        Err(_) => true,
    };
    pool.close().await;
    free
}

fn still_held(err: &sqlx::Error) -> bool {
    if matches!(err, sqlx::Error::PoolTimedOut) {
        return true;
    }
    let text = err.to_string().to_ascii_lowercase();
    text.contains("locked") || text.contains("busy")
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
