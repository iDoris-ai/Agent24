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
use crate::error::{ApiError, StorageCause};

/// How often a request may retry opening storage that failed to open.
pub const REOPEN_INTERVAL: Duration = Duration::from_secs(2);

pub struct Storage {
    pub blobs: BlobStore,
    pub db: Db,
}

type Opened = Result<Arc<Storage>, StorageCause>;

#[derive(Clone)]
pub struct AppState(Arc<Inner>);

struct Inner {
    data_dir: PathBuf,
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

    pub(crate) async fn open_with(data_dir: &Path, reopen_interval: Duration) -> Self {
        let current = open_storage(data_dir).await;
        Self(Arc::new(Inner {
            data_dir: data_dir.to_owned(),
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
            reopen_interval,
            current: Mutex::new(Err(cause)),
            last_attempt: tokio::sync::Mutex::new(Instant::now()),
            #[cfg(test)]
            reopens: std::sync::atomic::AtomicUsize::new(0),
        }))
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
        *last = Instant::now();
        #[cfg(test)]
        self.0.reopens.fetch_add(1, Ordering::SeqCst);
        let reopened = open_storage(&self.0.data_dir).await;
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
async fn open_storage(data_dir: &Path) -> Opened {
    let dir = data_dir.to_owned();
    let blobs = blocking(move || BlobStore::open(&dir)).await.map_err(|e| {
        tracing::error!(error = %e, "documents: blob store unavailable");
        blob_cause(&e)
    })?;
    let db = match Db::open(data_dir).await {
        Ok(db) => db,
        Err(e) => {
            tracing::error!(error = %e, "documents: database unavailable");
            let dir = data_dir.to_owned();
            return Err(blocking(move || db_cause(&e, &mut || probe_write(&dir))).await);
        }
    };
    Ok(Arc::new(Storage { blobs, db }))
}

/// Runs filesystem work (directory fsyncs, the probe) off the async workers.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
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

fn io_cause(e: &io::Error) -> StorageCause {
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
/// succeeds the device failed some other way, which the user also has to fix.
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

fn blob_cause(e: &BlobError) -> StorageCause {
    match e {
        BlobError::Locked => StorageCause::Locked,
        BlobError::Io(e) => io_cause(e),
        BlobError::InvalidAddress(_) | BlobError::NotFound(_) | BlobError::Corrupt { .. } => {
            StorageCause::Corrupt
        }
    }
}

/// `probe` is asked only for an I/O error, the one code that needs it.
fn sqlx_cause(e: &sqlx::Error, probe: &mut dyn FnMut() -> StorageCause) -> StorageCause {
    match e {
        sqlx::Error::Database(d) => {
            // sqlx reports the extended result code; the primary code is its low byte.
            let primary = d
                .code()
                .and_then(|c| c.parse::<i32>().ok())
                .map(|c| c & 0xff);
            match primary {
                // BUSY (incl. _RECOVERY, _SNAPSHOT, _TIMEOUT); PROTOCOL is a
                // lost WAL locking race, also contention.
                Some(5 | 15) => StorageCause::Busy,
                Some(6) => StorageCause::Locked,           // LOCKED
                Some(13) => StorageCause::DiskFull,        // FULL
                Some(8 | 14) => StorageCause::NotWritable, // READONLY, CANTOPEN
                Some(10) => probe(),                       // IOERR
                // CORRUPT, NOTADB, and anything unknown: refuse writes and tell
                // the user to check the file rather than retry.
                _ => StorageCause::Corrupt,
            }
        }
        sqlx::Error::Io(e) => io_cause(e),
        sqlx::Error::PoolTimedOut => StorageCause::Busy,
        _ => StorageCause::Corrupt,
    }
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

#[cfg(test)]
mod tests;
