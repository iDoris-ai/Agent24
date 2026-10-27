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

use sqlx::Connection;
use sqlx::SqliteConnection;
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

    /// `sqlite3_close` every worker. `Pool::close` can return while a
    /// connection is still idle and holding the file; dropping the pool only
    /// signals that worker. Returns an error if the file still cannot be
    /// locked exclusively after 15 seconds.
    pub async fn close(mut self) -> Result<(), StepError> {
        self.armed = false;
        let path = self.pool.connect_options().get_filename().to_owned();
        self.pool.close().await;
        // `Guard` implements `Drop`, so the pool cannot be moved out. Idle
        // workers are signaled only when the last pool handle is dropped.
        // The stand-in is never opened.
        let stand_in = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_lazy_with(SqliteConnectOptions::new());
        drop(std::mem::replace(&mut self.pool, stand_in));
        if wait_until_free(&path).await {
            Ok(())
        } else {
            Err(StepError::Msg {
                phase: "close",
                migration: None,
                message: "sqlite file stayed locked for 15s after pool close".into(),
            })
        }
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
        Err(source) => return Err(close_open_failure(guard, source).await),
    }
    if fault.retain_exclusive_lock
        && let Err(source) = hold(&guard.pool).await
    {
        let _ = guard.close().await;
        return Err(StepError::Sqlx {
            phase: "open",
            source,
        });
    }
    if fault.fail_open {
        let _ = guard.close().await;
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
SELECT 1 UNION ALL SELECT x + 1 FROM c LIMIT 1000000 \
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
        Some(Duration::from_millis(150)),
    )
}

/// How long close waits for a worker that `Pool::close` did not join.
/// Same bound on the success path and on an open failure.
const RELEASE_WAIT: Duration = Duration::from_secs(15);

/// `Pool::close` joins workers the pool owns, then can still return with one
/// of them idle. A connect PRAGMA error can also drop a `SqliteConnection`
/// before the pool takes it. [`Guard::close`] waits until `BEGIN EXCLUSIVE`
/// works. The open error stays the result, including when that wait times out.
async fn close_open_failure(guard: Guard, source: sqlx::Error) -> StepError {
    let _ = guard.close().await;
    StepError::Sqlx {
        phase: "open",
        source,
    }
}

async fn wait_until_free(path: &Path) -> bool {
    let deadline = Instant::now() + RELEASE_WAIT;
    loop {
        if file_is_free(path).await {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
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
    // A pool's `close` returns before its worker runs `sqlite3_close`, so the
    // probe would report the file free while still holding it.
    let mut conn = match SqliteConnection::connect_with(&options).await {
        Ok(conn) => conn,
        Err(err) if still_held(&err) => return false,
        Err(_) => return true,
    };
    let free = match sqlx::query("BEGIN EXCLUSIVE").execute(&mut conn).await {
        Ok(_) => sqlx::query("ROLLBACK").execute(&mut conn).await.is_ok(),
        Err(err) if still_held(&err) => false,
        Err(_) => true,
    };
    let _ = conn.close().await;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::str::FromStr;
    use std::sync::Arc;
    use std::sync::Mutex;

    use sqlx::Connection;
    use sqlx::sqlite::SqliteConnectOptions;
    use sqlx::sqlite::SqliteJournalMode;
    use tokio::sync::oneshot;

    use super::Guard;

    /// SQLx 0.8.6 `Pool::close` can return while the connection that passed
    /// the `is_closed` check is released back to the idle queue still holding
    /// `BEGIN EXCLUSIVE`. Dropping the pool only signals that worker.
    #[tokio::test]
    async fn close_waits_out_a_connection_returned_during_pool_close() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("race.db");
        let opts = |create| {
            SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(create)
                .journal_mode(SqliteJournalMode::Wal)
                .busy_timeout(std::time::Duration::from_millis(0))
        };
        let mut setup = sqlx::SqliteConnection::connect_with(&opts(true))
            .await
            .unwrap();
        sqlx::query("CREATE TABLE t (x INTEGER)")
            .execute(&mut setup)
            .await
            .unwrap();
        setup.close().await.unwrap();

        let (started_tx, started_rx) = oneshot::channel::<()>();
        let (go_tx, go_rx) = oneshot::channel::<()>();
        let started_tx = Arc::new(Mutex::new(Some(started_tx)));
        let go_rx = Arc::new(Mutex::new(Some(go_rx)));
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .min_connections(0)
            .after_release(move |conn, _| {
                let started_tx = Arc::clone(&started_tx);
                let go_rx = Arc::clone(&go_rx);
                Box::pin(async move {
                    let tx = started_tx.lock().unwrap().take();
                    let rx = go_rx.lock().unwrap().take();
                    if let Some(tx) = tx {
                        let _ = tx.send(());
                        if let Some(rx) = rx {
                            let _ = rx.await;
                        }
                        sqlx::query("BEGIN EXCLUSIVE").execute(conn).await?;
                    }
                    Ok(true)
                })
            })
            .connect_with(opts(false))
            .await
            .unwrap();

        drop(pool.acquire().await.unwrap());
        started_rx.await.unwrap();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let _ = go_tx.send(());
        });
        Guard { pool, armed: true }.close().await.unwrap();
        let probe = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(false)
            .busy_timeout(std::time::Duration::from_millis(0));
        let mut conn = sqlx::SqliteConnection::connect_with(&probe).await.unwrap();
        let began = sqlx::query("BEGIN EXCLUSIVE").execute(&mut conn).await;
        let _ = sqlx::query("ROLLBACK").execute(&mut conn).await;
        conn.close().await.unwrap();
        began.expect("close returned while the file was still locked");
    }
}
