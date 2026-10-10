//! Storage state shared by every route.
//!
//! Storage (the blob store and `documents.db`) is opened at startup. If that
//! fails the OS still serves: `/capabilities` reports storage as unavailable
//! and every route that needs storage answers 503 `storage_unavailable` with
//! the cause (ADR-DOC-02 §6, §8), so the user sees why instead of a module
//! that never comes up. Each later request that needs storage tries to open
//! it again, at most once per [`REOPEN_INTERVAL`], so storage comes back once
//! the other instance exits, the writer commits, or the user frees space.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::{FromRef, FromRequestParts};
use axum::http::request::Parts;
use sqlx::migrate::MigrateError;

use crate::blob::{BlobError, BlobStore};
use crate::db::{Db, DbError};
use crate::engine::Layers;
use crate::error::{ApiError, StorageCause};
use crate::events::Events;

/// How often a request may retry opening storage that failed to open.
pub const REOPEN_INTERVAL: Duration = Duration::from_secs(2);

/// How long a request waits for the write probe after an I/O error.
const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub struct Storage {
    pub blobs: BlobStore,
    pub db: Db,
    pub events: Events,
    data_dir: PathBuf,
    /// One request-time probe at a time.
    probing: Arc<tokio::sync::Semaphore>,
}

impl Storage {
    /// The OS's data directory; the blob store and `documents.db` live in it.
    #[must_use]
    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// A database error during a request. Storage that stops working after
    /// it opened (busy, full, read-only, corrupt) is 503 with the cause; any
    /// other failure is unexpected, 500.
    pub async fn db_failure(&self, e: sqlx::Error) -> ApiError {
        tracing::error!(error = %e, "documents: database error");
        let mut io_error = false;
        let cause = availability_cause(&e, &mut || {
            io_error = true;
            StorageCause::NotWritable
        });
        let cause = match cause {
            Some(_) if io_error => Some(self.probe().await),
            cause => cause,
        };
        match cause {
            Some(cause) => ApiError::storage_unavailable(cause),
            None => ApiError::internal("the document database failed"),
        }
    }

    async fn probe(&self) -> StorageCause {
        let dir = self.data_dir.clone();
        bounded_probe(&self.probing, PROBE_TIMEOUT, move || probe_write(&dir)).await
    }
}

/// Runs `probe` for a request without letting a stalled filesystem hold the
/// response: a request that finds a probe already running does not start
/// another, and none waits longer than `timeout`. The stalled probe keeps its
/// permit until it returns, so stalls do not pile up. Either way the answer
/// is `not_writable`, the probe's own answer for a device that fails in a way
/// it cannot name.
async fn bounded_probe(
    probing: &Arc<tokio::sync::Semaphore>,
    timeout: Duration,
    probe: impl FnOnce() -> StorageCause + Send + 'static,
) -> StorageCause {
    let Ok(permit) = Arc::clone(probing).try_acquire_owned() else {
        return StorageCause::NotWritable;
    };
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        probe()
    });
    match tokio::time::timeout(timeout, task).await {
        Ok(Ok(cause)) => cause,
        Ok(Err(e)) => match e.try_into_panic() {
            Ok(panic) => std::panic::resume_unwind(panic),
            // Cancelled: the runtime is shutting down.
            Err(_) => StorageCause::NotWritable,
        },
        Err(_) => StorageCause::NotWritable,
    }
}

type Opened = Result<Arc<Storage>, StorageCause>;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    data_dir: PathBuf,
    events: Events,
    /// Text layers, and the engine that reads them (none on Linux).
    layers: Arc<Layers>,
    reopen_interval: Duration,
    current: Mutex<Opened>,
    /// When the last reopen started. Held during a reopen, so one request
    /// reopens while the others answer with the last cause.
    last_attempt: tokio::sync::Mutex<Instant>,
    /// Reopens started, so tests can wait for one without touching the lock.
    #[cfg(test)]
    reopens: std::sync::atomic::AtomicUsize,
}

impl AppState {
    /// Open storage under `data_dir`; a failure is kept, not returned.
    pub async fn open(data_dir: &Path) -> Self {
        Self::open_with(data_dir, REOPEN_INTERVAL).await
    }

    /// [`AppState::open`], telling WS clients about jobs through `events`.
    pub async fn open_with_events(data_dir: &Path, events: Events) -> Self {
        Self::opened(data_dir, REOPEN_INTERVAL, events, Layers::new(None)).await
    }

    /// [`AppState::open_with_events`], reading documents with `layers`.
    pub async fn open_serving(data_dir: &Path, events: Events, layers: Arc<Layers>) -> Self {
        Self::opened(data_dir, REOPEN_INTERVAL, events, layers).await
    }

    pub(crate) async fn open_with(data_dir: &Path, reopen_interval: Duration) -> Self {
        Self::opened(
            data_dir,
            reopen_interval,
            Events::default(),
            Layers::new(None),
        )
        .await
    }

    async fn opened(
        data_dir: &Path,
        reopen_interval: Duration,
        events: Events,
        layers: Arc<Layers>,
    ) -> Self {
        let current = open_storage(data_dir, &events).await;
        Self(Arc::new(Inner {
            data_dir: data_dir.to_owned(),
            events,
            layers,
            reopen_interval,
            current: Mutex::new(current),
            last_attempt: tokio::sync::Mutex::new(Instant::now()),
            #[cfg(test)]
            reopens: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    #[cfg(test)]
    pub(crate) fn unavailable(cause: StorageCause) -> Self {
        // Never reopened; a path that cannot be created, should that change.
        Self::failed(Path::new("/dev/null/documents"), cause, Duration::MAX)
    }

    #[cfg(test)]
    pub(crate) fn failed(data_dir: &Path, cause: StorageCause, reopen_interval: Duration) -> Self {
        Self(Arc::new(Inner {
            data_dir: data_dir.to_owned(),
            events: Events::default(),
            layers: Layers::new(None),
            reopen_interval,
            current: Mutex::new(Err(cause)),
            last_attempt: tokio::sync::Mutex::new(Instant::now()),
            #[cfg(test)]
            reopens: std::sync::atomic::AtomicUsize::new(0),
        }))
    }

    /// Where text layers come from.
    #[must_use]
    pub fn layers(&self) -> &Arc<Layers> {
        &self.0.layers
    }

    fn current(&self) -> Opened {
        self.0
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Ready storage, reopening it first if it failed and the interval has passed.
    pub async fn storage(&self) -> Opened {
        let current = self.current();
        if current.is_ok() {
            return current;
        }
        let Ok(mut last) = self.0.last_attempt.try_lock() else {
            return self.current(); // another request is reopening
        };
        let current = self.current();
        if current.is_ok() || last.elapsed() < self.0.reopen_interval {
            return current;
        }
        // Counted before the attempt: a request dropped mid-reopen still uses
        // up this interval, so a client that keeps cancelling cannot make the
        // service reopen on every request.
        *last = Instant::now();
        #[cfg(test)]
        self.0.reopens.fetch_add(1, Ordering::SeqCst);
        let reopened = open_storage(&self.0.data_dir, &self.0.events).await;
        *self
            .0
            .current
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = reopened.clone();
        reopened
    }
}

/// Opens both stores. On a database failure the blob store is dropped, which
/// releases its lock, so a reopen can take it again.
async fn open_storage(data_dir: &Path, events: &Events) -> Opened {
    // The blob store's lock comes first: it makes this the only instance, so
    // the `recover` below never interrupts jobs another live instance runs.
    let dir = data_dir.to_owned();
    let blobs = blocking(move || BlobStore::open(&dir)).await.map_err(|e| {
        tracing::error!(error = %e, "documents: blob store unavailable");
        blob_cause(&e)
    })?;
    let opened = match Db::open(data_dir).await {
        Ok(db) => crate::jobs::recover(&db)
            .await
            .map(|ids| (db, ids))
            .map_err(DbError::from),
        Err(e) => Err(e),
    };
    let (db, recovered) = match opened {
        Ok(opened) => opened,
        Err(e) => {
            tracing::error!(error = %e, "documents: database unavailable");
            let dir = data_dir.to_owned();
            return Err(blocking(move || db_cause(&e, &mut || probe_write(&dir))).await);
        }
    };
    for job in &recovered {
        let cancelled = job.status == "cancelled";
        events.ended(&crate::events::Ended {
            job_id: &job.id,
            kind: &job.kind,
            status: &job.status,
            attempt: job.attempt,
            error_code: cancelled.then_some("cancelled"),
            document: None,
        });
    }
    Ok(Arc::new(Storage {
        blobs,
        db,
        events: events.clone(),
        data_dir: data_dir.to_owned(),
        probing: Arc::new(tokio::sync::Semaphore::new(1)),
    }))
}

/// Runs filesystem work (directory fsyncs, the probe) off the async workers.
pub(crate) async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        // A blocking task is only cancelled when the runtime shuts down.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// Extracts ready storage, or rejects with 503 `storage_unavailable`.
pub struct Ready(pub Arc<Storage>);

impl<S> FromRequestParts<S> for Ready
where
    AppState: FromRef<S>,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(_parts: &mut Parts, state: &S) -> Result<Self, ApiError> {
        AppState::from_ref(state)
            .storage()
            .await
            .map(Ready)
            .map_err(ApiError::storage_unavailable)
    }
}

pub(crate) fn io_cause(e: &io::Error) -> StorageCause {
    match e.kind() {
        io::ErrorKind::StorageFull | io::ErrorKind::QuotaExceeded => StorageCause::DiskFull,
        // Permission, read-only volume, a file where a directory should be…:
        // all mean the user has to fix the data directory.
        _ => StorageCause::NotWritable,
    }
}

static PROBE_SEQ: AtomicU64 = AtomicU64::new(0);

/// SQLite's I/O errors do not say why (a full disk can surface as
/// `SQLITE_IOERR_SHMSIZE` or `_WRITE`). Writing a probe the size of a WAL
/// index page tells a full disk from an unwritable directory; if the probe
/// succeeds the device failed some other way, which the user also has to fix,
/// so a successful probe still answers `not_writable`: every I/O error that
/// reaches the probe is reported as storage.
fn probe_write(data_dir: &Path) -> StorageCause {
    let n = PROBE_SEQ.fetch_add(1, Ordering::Relaxed);
    let path = data_dir.join(format!(".write-probe-{}-{n}", std::process::id()));
    // `create_new` never opens an existing file or follows a symlink, so the
    // probe only ever writes to, and removes, a file it created itself.
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(e) => return io_cause(&e),
    };
    let written = file
        .write_all(&[0u8; 32 * 1024])
        .and_then(|()| file.sync_all());
    drop(file);
    // Best effort: a leftover after a crash here is 32 KiB under a unique name.
    let _ = fs::remove_file(&path);
    written
        .err()
        .map_or(StorageCause::NotWritable, |e| io_cause(&e))
}

pub(crate) fn blob_cause(e: &BlobError) -> StorageCause {
    match e {
        BlobError::Locked => StorageCause::Locked,
        BlobError::Io(e) => io_cause(e),
        BlobError::InvalidAddress(_) | BlobError::NotFound(_) | BlobError::Corrupt { .. } => {
            StorageCause::Corrupt
        }
    }
}

/// At open, any failure means storage is unavailable; one that is not about
/// availability is reported as `corrupt`, telling the user to check the file
/// rather than retry.
fn sqlx_cause(e: &sqlx::Error, probe: &mut dyn FnMut() -> StorageCause) -> StorageCause {
    match e {
        sqlx::Error::Io(e) => io_cause(e),
        e => availability_cause(e, probe).unwrap_or(StorageCause::Corrupt),
    }
}

/// The availability cause of a database error, or `None` for an error that
/// is not about storage (a constraint, a decode failure: a bug). `probe` is
/// asked only for an I/O error, the one code that needs it.
/// Why `e` means storage is unavailable, if it does; an I/O error counts as
/// not writable, without the probe a request may run.
pub(crate) fn unavailable_cause(e: &sqlx::Error) -> Option<StorageCause> {
    availability_cause(e, &mut || StorageCause::NotWritable)
}

fn availability_cause(
    e: &sqlx::Error,
    probe: &mut dyn FnMut() -> StorageCause,
) -> Option<StorageCause> {
    Some(match e {
        sqlx::Error::Database(d) => {
            // sqlx reports the extended result code; the primary code is its low byte.
            let code = d.code().and_then(|c| c.parse::<i32>().ok());
            match code.map(|c| c & 0xff) {
                // BUSY (incl. _RECOVERY, _SNAPSHOT, _TIMEOUT); PROTOCOL is a
                // lost WAL locking race, also contention.
                Some(5 | 15) => StorageCause::Busy,
                Some(6) => StorageCause::Locked,           // LOCKED
                Some(13) => StorageCause::DiskFull,        // FULL
                Some(8 | 14) => StorageCause::NotWritable, // READONLY, CANTOPEN
                // IOERR_NOMEM: the I/O layer ran out of memory, not storage.
                Some(10) if code == Some(3082) => return None,
                Some(10) => probe(),                    // IOERR
                Some(11 | 26) => StorageCause::Corrupt, // CORRUPT, NOTADB
                _ => return None,
            }
        }
        // Out of memory or threads is the process, not storage.
        sqlx::Error::Io(e)
            if matches!(
                e.kind(),
                io::ErrorKind::OutOfMemory | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) =>
        {
            return None;
        }
        sqlx::Error::Io(e) => io_cause(e),
        sqlx::Error::PoolTimedOut => StorageCause::Busy,
        _ => return None,
    })
}

fn db_cause(e: &DbError, probe: &mut dyn FnMut() -> StorageCause) -> StorageCause {
    match e {
        DbError::Sqlx(e) => sqlx_cause(e, probe),
        DbError::Migrate(MigrateError::Execute(e) | MigrateError::ExecuteMigration(e, _)) => {
            sqlx_cause(e, probe)
        }
        // Applied migrations this build does not know (a database written by a
        // newer or edited build). The closed cause set (§6) has no
        // "incompatible", and a retry never fixes it, so it is reported as
        // `corrupt`; the database is left untouched.
        DbError::Migrate(_) => StorageCause::Corrupt,
    }
}

// The tests use Unix permissions, symlinks and FIFOs.
#[cfg(all(test, unix))]
mod tests;
