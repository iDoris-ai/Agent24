#![allow(clippy::unwrap_used, clippy::expect_used)]

use super::*;
use std::borrow::Cow;
use std::fs::File;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::Ordering;

use sqlx::Connection;

/// A SQLite error with a chosen extended result code, as sqlx reports it.
#[derive(Debug)]
struct Code(i32);

impl std::fmt::Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sqlite code {}", self.0)
    }
}

impl std::error::Error for Code {}

impl sqlx::error::DatabaseError for Code {
    fn message(&self) -> &str {
        "fake"
    }
    fn code(&self) -> Option<Cow<'_, str>> {
        Some(self.0.to_string().into())
    }
    fn as_error(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        self
    }
    fn as_error_mut(&mut self) -> &mut (dyn std::error::Error + Send + Sync + 'static) {
        self
    }
    fn into_error(self: Box<Self>) -> Box<dyn std::error::Error + Send + Sync + 'static> {
        self
    }
    fn kind(&self) -> sqlx::error::ErrorKind {
        sqlx::error::ErrorKind::Other
    }
}

fn sqlite(code: i32) -> sqlx::Error {
    sqlx::Error::Database(Box::new(Code(code)))
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Root can write under a 0555 directory; the permission tests then prove
/// nothing and are skipped.
fn privileged(read_only_dir: &Path) -> bool {
    let probe = read_only_dir.join(".privilege-check");
    let wrote = File::create(&probe).is_ok();
    let _ = fs::remove_file(&probe);
    if wrote {
        eprintln!("skipped: this user can write to a read-only directory");
    }
    wrote
}

async fn connect(dir: &Path) -> sqlx::SqliteConnection {
    let opts = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(dir.join(crate::db::DB_FILE))
        .create_if_missing(true);
    sqlx::SqliteConnection::connect_with(&opts).await.unwrap()
}

/// Waits, without touching the reopen lock, until a reopen has started.
async fn reopen_started(state: &AppState) {
    let waiting = Instant::now();
    while state.0.reopens.load(Ordering::SeqCst) == 0 {
        assert!(
            waiting.elapsed() < Duration::from_secs(4),
            "no reopen started"
        );
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

fn cause(state: &AppState) -> Option<StorageCause> {
    state.current().err()
}

#[tokio::test]
async fn a_fresh_data_dir_opens_ready() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(cause(&AppState::open(dir.path()).await), None);
}

#[tokio::test]
async fn a_second_instance_is_locked_until_the_first_exits() {
    let dir = tempfile::tempdir().unwrap();
    let first = AppState::open(dir.path()).await;
    let patient = AppState::open_with(dir.path(), Duration::ZERO).await;
    let waiting = AppState::open_with(dir.path(), Duration::from_secs(3600)).await;
    assert_eq!(cause(&patient), Some(StorageCause::Locked));
    assert_eq!(patient.storage().await.err(), Some(StorageCause::Locked));
    drop(first);
    assert!(
        patient.storage().await.is_ok(),
        "reopens once the lock is free"
    );
    assert_eq!(cause(&patient), None, "and keeps what it reopened");
    drop(patient);
    // Within the interval the last cause is reported without a reopen.
    assert_eq!(waiting.storage().await.err(), Some(StorageCause::Locked));
}

#[tokio::test]
async fn a_second_instance_never_recovers_the_first_ones_jobs() {
    let dir = tempfile::tempdir().unwrap();
    let first = AppState::open(dir.path()).await;
    let storage = first.storage().await.unwrap();
    sqlx::query(
        "INSERT INTO jobs (id, kind, status, origin)
         VALUES ('job_01K75A0B1C2D3E4F5G6H7J8K9M', 'import', 'running', '{\"kind\":\"page\"}')",
    )
    .execute(storage.db.pool())
    .await
    .unwrap();
    let second = AppState::open(dir.path()).await;
    assert_eq!(cause(&second), Some(StorageCause::Locked));
    let status: String = sqlx::query_scalar("SELECT status FROM jobs")
        .fetch_one(storage.db.pool())
        .await
        .unwrap();
    assert_eq!(
        status, "running",
        "still the first instance's job (#831 review)"
    );
}

#[tokio::test]
async fn a_database_that_is_not_sqlite_is_corrupt_and_releases_the_blob_lock() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join(crate::db::DB_FILE);
    fs::write(&db, vec![0x5a; 8192]).unwrap();
    let state = AppState::open_with(dir.path(), Duration::ZERO).await;
    assert_eq!(cause(&state), Some(StorageCause::Corrupt));
    // The user moves the bad file away; the reopen must get the blob lock back.
    fs::remove_file(&db).unwrap();
    assert!(state.storage().await.is_ok());
}

#[tokio::test]
async fn a_read_only_data_dir_is_not_writable() {
    let dir = tempfile::tempdir().unwrap();
    chmod(dir.path(), 0o555);
    let privileged = privileged(dir.path());
    let got = cause(&AppState::open(dir.path()).await);
    chmod(dir.path(), 0o755);
    if !privileged {
        assert_eq!(got, Some(StorageCause::NotWritable));
    }
}

#[tokio::test]
async fn a_read_only_database_file_is_not_writable() {
    let made = tempfile::tempdir().unwrap();
    Db::open(made.path()).await.unwrap().pool().close().await;
    // Within one process SQLite reuses a still-open read-write descriptor of
    // the same inode, which would make the next open writable whatever the
    // mode. Copies are new inodes.
    let dir = tempfile::tempdir().unwrap();
    for suffix in ["", "-wal"] {
        let name = format!("{}{suffix}", crate::db::DB_FILE);
        if made.path().join(&name).exists() {
            fs::copy(made.path().join(&name), dir.path().join(&name)).unwrap();
        }
    }
    let file = dir.path().join(crate::db::DB_FILE);
    chmod(&file, 0o444);
    let privileged = {
        let other = tempfile::tempdir().unwrap();
        chmod(other.path(), 0o555);
        let p = privileged(other.path());
        chmod(other.path(), 0o755);
        p
    };
    let got = cause(&AppState::open(dir.path()).await);
    chmod(&file, 0o644);
    if !privileged {
        assert_eq!(got, Some(StorageCause::NotWritable));
    }
}

#[tokio::test]
async fn a_database_held_by_another_writer_is_busy() {
    let dir = tempfile::tempdir().unwrap();
    let mut holder = connect(dir.path()).await;
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut holder)
        .await
        .unwrap();
    // Waits out `busy_timeout` (5 s), then reports the contention.
    let state = AppState::open(dir.path()).await;
    assert_eq!(cause(&state), Some(StorageCause::Busy));
}

#[tokio::test]
async fn a_database_from_a_newer_build_is_refused_and_left_untouched() {
    let dir = tempfile::tempdir().unwrap();
    Db::open(dir.path()).await.unwrap().pool().close().await;
    let mut conn = connect(dir.path()).await;
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (9999, 'from a newer build', 1, x'00', 0)",
    )
    .execute(&mut conn)
    .await
    .unwrap();
    let before = snapshot(&mut conn).await;
    conn.close().await.unwrap();

    let state = AppState::open(dir.path()).await;
    assert_eq!(cause(&state), Some(StorageCause::Corrupt));
    drop(state);
    let mut conn = connect(dir.path()).await;
    assert_eq!(snapshot(&mut conn).await, before);
}

/// Every migration record, the schema, and the header's user_version.
async fn snapshot(conn: &mut sqlx::SqliteConnection) -> (Vec<String>, Vec<String>, i64) {
    let migrations = sqlx::query_scalar(
        "SELECT version || '|' || description || '|' || installed_on || '|' || success
                || '|' || hex(checksum) || '|' || execution_time
         FROM _sqlx_migrations ORDER BY version",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    let schema = sqlx::query_scalar(
        "SELECT type || '|' || name || '|' || coalesce(sql, '') FROM sqlite_master ORDER BY name",
    )
    .fetch_all(&mut *conn)
    .await
    .unwrap();
    let user_version = sqlx::query_scalar("PRAGMA user_version")
        .fetch_one(&mut *conn)
        .await
        .unwrap();
    (migrations, schema, user_version)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn while_one_request_reopens_the_others_answer_at_once() {
    let dir = tempfile::tempdir().unwrap();
    let first = BlobStore::open(dir.path()).unwrap();
    let state = AppState::open_with(dir.path(), Duration::ZERO).await;
    assert_eq!(cause(&state), Some(StorageCause::Locked));
    // Free the blob lock but hold the database, so the reopen waits out the
    // 5 s busy timeout.
    drop(first);
    let mut holder = connect(dir.path()).await;
    sqlx::query("BEGIN EXCLUSIVE")
        .execute(&mut holder)
        .await
        .unwrap();
    let reopening = tokio::spawn({
        let state = state.clone();
        async move { state.storage().await.err() }
    });
    reopen_started(&state).await;
    assert!(
        state.0.last_attempt.try_lock().is_err(),
        "no reopen in progress"
    );
    let asked = Instant::now();
    assert_eq!(state.storage().await.err(), Some(StorageCause::Locked));
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "waited for the reopen"
    );
    assert_eq!(reopening.await.unwrap(), Some(StorageCause::Busy));
    assert_eq!(cause(&state), Some(StorageCause::Busy));
}

#[tokio::test]
async fn a_reopen_waits_for_the_interval() {
    let interval = Duration::from_secs(1);
    let dir = tempfile::tempdir().unwrap();
    let first = AppState::open(dir.path()).await;
    let state = AppState::open_with(dir.path(), interval).await;
    let attempted = *state.0.last_attempt.try_lock().unwrap();
    drop(first);
    let got = state.storage().await.err();
    // Only meaningful if the scheduler let us ask within the interval.
    if attempted.elapsed() < interval {
        assert_eq!(
            got,
            Some(StorageCause::Locked),
            "reopened within the interval"
        );
    }
    tokio::time::sleep(interval + Duration::from_millis(100)).await;
    assert!(state.storage().await.is_ok());
}

/// On a single-thread runtime, a reopen whose filesystem work blocks must not
/// block other requests: the work has to run off the async worker.
#[tokio::test(flavor = "current_thread")]
async fn a_blocked_reopen_does_not_stall_the_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_owned();
    fs::create_dir_all(path.join("blobs")).unwrap();
    // Opening a FIFO for writing blocks until a reader opens it.
    let fifo = path.join("blobs/.lock");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .unwrap();
    assert!(made.success());
    let state = AppState::failed(&path, StorageCause::Locked, Duration::ZERO);

    let started = Instant::now();
    // Unblocks the reopen after 2 s, so an inline reopen fails this test
    // instead of hanging it. It owns the directory, so a failed assertion
    // cannot delete the FIFO before the release.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        let reader = File::open(&fifo);
        // Later opens then create a plain file instead of blocking again.
        fs::remove_file(&fifo).unwrap();
        drop(reader);
        drop(dir);
    });
    let reopening = tokio::spawn({
        let state = state.clone();
        async move { state.storage().await }
    });
    // Inline, the reopen would hold this only thread until the release at 2 s.
    reopen_started(&state).await;
    assert!(
        state.0.last_attempt.try_lock().is_err(),
        "no reopen in progress"
    );
    assert_eq!(state.storage().await.err(), Some(StorageCause::Locked));
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "the runtime was blocked"
    );
    let _ = reopening.await.unwrap();
    release.join().unwrap();
}

#[tokio::test]
async fn a_route_that_needs_storage_answers_503_with_the_cause() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;
    async fn probe(_: Ready) -> &'static str {
        "ok"
    }
    let app = |state| {
        axum::Router::new()
            .route("/probe", axum::routing::get(probe))
            .with_state(state)
    };
    let get = || Request::get("/probe").body(Body::empty()).unwrap();

    let res = app(AppState::unavailable(StorageCause::Locked))
        .oneshot(get())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    let bytes = axum::body::to_bytes(res.into_body(), 4096).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(v["error"]["code"], "storage_unavailable");
    assert_eq!(
        v["error"]["details"],
        serde_json::json!({ "retryable": true, "cause": "locked" })
    );

    let dir = tempfile::tempdir().unwrap();
    let res = app(AppState::open(dir.path()).await)
        .oneshot(get())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[test]
fn sqlite_result_codes_map_by_their_primary_code() {
    for (code, want) in [
        (5, StorageCause::Busy),
        (261, StorageCause::Busy), // BUSY_RECOVERY
        (517, StorageCause::Busy), // BUSY_SNAPSHOT
        (15, StorageCause::Busy),  // PROTOCOL
        (6, StorageCause::Locked),
        (262, StorageCause::Locked), // LOCKED_SHAREDCACHE
        (13, StorageCause::DiskFull),
        (8, StorageCause::NotWritable),
        (1032, StorageCause::NotWritable), // READONLY_DBMOVED
        (14, StorageCause::NotWritable),
        (11, StorageCause::Corrupt),
        (26, StorageCause::Corrupt), // NOTADB
        (1, StorageCause::Corrupt),
    ] {
        let mut probe = || panic!("code {code} needs no probe");
        assert_eq!(sqlx_cause(&sqlite(code), &mut probe), want, "code {code}");
    }
}

#[test]
fn an_io_error_takes_its_cause_from_the_probe() {
    for code in [10, 778, 4874] {
        // IOERR, IOERR_WRITE, IOERR_SHMSIZE
        for cause in [StorageCause::DiskFull, StorageCause::NotWritable] {
            assert_eq!(
                sqlx_cause(&sqlite(code), &mut || cause),
                cause,
                "code {code}"
            );
        }
    }
}

#[test]
fn the_probe_writes_only_a_file_of_its_own() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(probe_write(dir.path()), StorageCause::NotWritable);
    assert_eq!(
        fs::read_dir(dir.path()).unwrap().count(),
        0,
        "probe left behind"
    );

    // A link at the next probe name must not be followed, truncated or removed.
    let target = dir.path().join(crate::db::DB_FILE);
    fs::write(&target, b"precious").unwrap();
    let next = PROBE_SEQ.load(Ordering::Relaxed);
    for n in next..next + 64 {
        let name = format!(".write-probe-{}-{n}", std::process::id());
        std::os::unix::fs::symlink(&target, dir.path().join(name)).unwrap();
    }
    let _ = probe_write(dir.path());
    assert_eq!(fs::read(&target).unwrap(), b"precious");
    let links = fs::read_dir(dir.path()).unwrap().count() - 1;
    assert_eq!(links, 64, "a link was removed");
}

#[test]
fn during_a_request_only_availability_errors_are_storage_unavailable() {
    let mut probe = || StorageCause::DiskFull;
    for (code, want) in [
        (517, Some(StorageCause::Busy)),
        (13, Some(StorageCause::DiskFull)),
        (4874, Some(StorageCause::DiskFull)), // via the probe
        (11, Some(StorageCause::Corrupt)),
        (26, Some(StorageCause::Corrupt)),
        (1, None),    // a generic SQL error
        (19, None),   // a constraint: a bug, not storage
        (2067, None), // CONSTRAINT_UNIQUE
        (3082, None), // IOERR_NOMEM: out of memory, not storage (#824 review)
    ] {
        assert_eq!(
            availability_cause(&sqlite(code), &mut probe),
            want,
            "code {code}"
        );
    }
    assert_eq!(
        availability_cause(&sqlx::Error::RowNotFound, &mut probe),
        None
    );
    assert_eq!(
        availability_cause(&sqlx::Error::PoolTimedOut, &mut probe),
        Some(StorageCause::Busy)
    );
    // Running out of memory or threads is the process, not storage: 500.
    let io = |k| sqlx::Error::Io(io::Error::from(k));
    for kind in [
        io::ErrorKind::OutOfMemory,
        io::ErrorKind::WouldBlock,
        io::ErrorKind::Interrupted,
    ] {
        assert_eq!(availability_cause(&io(kind), &mut probe), None, "{kind:?}");
    }
    assert_eq!(
        availability_cause(&io(io::ErrorKind::PermissionDenied), &mut probe),
        Some(StorageCause::NotWritable)
    );
    assert_eq!(
        availability_cause(&io(io::ErrorKind::StorageFull), &mut probe),
        Some(StorageCause::DiskFull)
    );
}

#[tokio::test]
async fn a_request_time_probe_is_bounded_and_one_at_a_time() {
    let probing = Arc::new(tokio::sync::Semaphore::new(1));
    assert_eq!(
        bounded_probe(&probing, Duration::from_secs(5), || StorageCause::DiskFull).await,
        StorageCause::DiskFull
    );
    // A stalled probe: the request answers at the timeout, and the probe
    // keeps its permit, so the next request does not start a second one.
    let (release, stalled) = std::sync::mpsc::channel::<()>();
    let started = Instant::now();
    let cause = bounded_probe(&probing, Duration::from_millis(100), move || {
        let _ = stalled.recv();
        StorageCause::DiskFull
    })
    .await;
    assert_eq!(cause, StorageCause::NotWritable);
    assert!(started.elapsed() < Duration::from_secs(2));
    let cause = bounded_probe(&probing, Duration::from_secs(5), || {
        panic!("a second probe started while one is stalled")
    })
    .await;
    assert_eq!(cause, StorageCause::NotWritable);
    release.send(()).unwrap();
    let _ = probing.acquire().await.unwrap(); // the stalled probe returned
}

#[tokio::test]
async fn a_request_failure_is_503_for_storage_and_500_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let state = AppState::open(dir.path()).await;
    let storage = state.storage().await.unwrap();
    let e = storage.db_failure(sqlite(5)).await;
    assert_eq!(
        (e.status().as_u16(), e.code()),
        (503, "storage_unavailable")
    );
    let e = storage.db_failure(sqlite(2067)).await;
    assert_eq!((e.status().as_u16(), e.code()), (500, "internal"));
}

#[test]
fn io_and_migration_failures_map_to_their_cause() {
    let mut dir = || panic!("no probe");
    let io = |k| sqlx::Error::Io(io::Error::from(k));
    assert_eq!(
        sqlx_cause(&io(io::ErrorKind::StorageFull), &mut dir),
        StorageCause::DiskFull
    );
    assert_eq!(
        sqlx_cause(&io(io::ErrorKind::QuotaExceeded), &mut dir),
        StorageCause::DiskFull
    );
    assert_eq!(
        sqlx_cause(&io(io::ErrorKind::PermissionDenied), &mut dir),
        StorageCause::NotWritable
    );
    assert_eq!(
        sqlx_cause(&sqlx::Error::PoolTimedOut, &mut dir),
        StorageCause::Busy
    );
    let migrate = |e| db_cause(&DbError::Migrate(e), &mut || panic!("no probe"));
    assert_eq!(
        migrate(MigrateError::VersionMismatch(1)),
        StorageCause::Corrupt
    );
    assert_eq!(
        migrate(MigrateError::VersionMissing(9999)),
        StorageCause::Corrupt
    );
    assert_eq!(
        migrate(MigrateError::Execute(sqlite(13))),
        StorageCause::DiskFull
    );
    assert_eq!(
        migrate(MigrateError::ExecuteMigration(sqlite(5), 3)),
        StorageCause::Busy
    );
    assert_eq!(
        db_cause(&DbError::Sqlx(sqlite(6)), &mut dir),
        StorageCause::Locked
    );
}
