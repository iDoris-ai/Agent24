//! P1-02c c4 (design D5): the pre-write disk check.
//!
//! A [`DiskGate`] looks at the volume that holds `memory.db` (NOT the process
//! cwd) and refuses a write while free space is under a threshold that depends
//! on the write's class:
//!
//! * [`WriteClass::Content`] (A class: messages, assertions, re-assertions,
//!   summaries, imports, artifacts) needs `max(512 MiB, 2 x current WAL bytes)`;
//! * [`WriteClass::Retract`] (`assertion.retracted`, one <=256 B row plus a WAL
//!   increment) needs only 16 MiB.
//!
//! The sample (free bytes + WAL bytes) is cached for [`CACHE_TTL`]. The cache
//! only saves `statvfs` calls; it is not the safety basis. The safety basis is
//! the `SQLITE_FULL` -> `memory_disk_full` mapping on the write itself, which
//! also covers a probe that failed (the gate fails OPEN: an unreadable volume
//! never blocks a write that SQLite itself would accept).
//!
//! The gate is checked once at the entry of each public write operation, BEFORE
//! its transaction opens, so a refused write leaves no row and a write that is
//! allowed is not checked again by the inner `append_tx` calls.
//! An ephemeral (`:memory:`) store has no volume and gets [`DiskGate::disabled`].

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{MemoryError, Result};

const MIB: u64 = 1024 * 1024;
/// Floor of the content-write threshold.
pub const CONTENT_MIN_FREE_BYTES: u64 = 512 * MIB;
/// Threshold of the retraction exemption.
pub const RETRACT_MIN_FREE_BYTES: u64 = 16 * MIB;
/// How long one sample may be reused.
pub const CACHE_TTL: Duration = Duration::from_secs(5);

/// Which threshold applies to a write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteClass {
    /// A class: content writes.
    Content,
    /// `assertion.retracted`, host path.
    Retract,
}

/// One reading of the volume that holds the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiskSample {
    /// Bytes available to an unprivileged writer on the database's volume.
    pub free_bytes: u64,
    /// Current size of the `-wal` file next to the database (0 if absent).
    pub wal_bytes: u64,
}

/// Injection point: tests swap in a fake to control free space.
pub trait DiskProbe: Send + Sync {
    fn sample(&self, db_path: &Path) -> io::Result<DiskSample>;
}

/// Monotonic time source (injected so tests can expire the cache).
pub trait GateClock: Send + Sync {
    fn now(&self) -> Duration;
}

struct SystemClock(Instant);

impl GateClock for SystemClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}

/// The real probe: `statvfs` on the database file itself, so it reports the
/// volume the file lives on even when that is not the cwd's volume.
pub struct StatvfsProbe;

impl DiskProbe for StatvfsProbe {
    fn sample(&self, db_path: &Path) -> io::Result<DiskSample> {
        let free_bytes = free_bytes_of(db_path)?;
        let mut wal = db_path.as_os_str().to_owned();
        wal.push("-wal");
        let wal_bytes = std::fs::metadata(PathBuf::from(wal)).map_or(0, |m| m.len());
        Ok(DiskSample {
            free_bytes,
            wal_bytes,
        })
    }
}

#[cfg(unix)]
fn free_bytes_of(path: &Path) -> io::Result<u64> {
    let s = rustix::fs::statvfs(path).map_err(io::Error::from)?;
    Ok(s.f_bavail.saturating_mul(s.f_frsize))
}

/// Windows free-space reading is PLT-WIN's (design D5); until then the gate
/// fails open and `SQLITE_FULL` stays the backstop.
#[cfg(not(unix))]
fn free_bytes_of(_path: &Path) -> io::Result<u64> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "volume free space is not implemented on this platform",
    ))
}

struct Inner {
    db_path: PathBuf,
    probe: Arc<dyn DiskProbe>,
    clock: Arc<dyn GateClock>,
    cache: Mutex<Option<(Duration, DiskSample)>>,
}

/// Cheap to clone; clones share one cache.
#[derive(Clone, Default)]
pub struct DiskGate {
    inner: Option<Arc<Inner>>,
}

impl std::fmt::Debug for DiskGate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskGate")
            .field("enabled", &self.inner.is_some())
            .finish()
    }
}

impl DiskGate {
    /// No check at all (ephemeral stores; bare pools handed to `::new`).
    pub fn disabled() -> Self {
        Self { inner: None }
    }

    /// The production gate for the database file at `db_path`.
    pub fn for_path(db_path: &Path) -> Self {
        Self::with_probe(
            db_path,
            Arc::new(StatvfsProbe),
            Arc::new(SystemClock(Instant::now())),
        )
    }

    /// A gate with an injected probe and clock (tests).
    pub fn with_probe(
        db_path: &Path,
        probe: Arc<dyn DiskProbe>,
        clock: Arc<dyn GateClock>,
    ) -> Self {
        Self {
            inner: Some(Arc::new(Inner {
                db_path: db_path.to_path_buf(),
                probe,
                clock,
                cache: Mutex::new(None),
            })),
        }
    }

    /// The free-space floor for `class` given the current WAL size.
    pub fn threshold(class: WriteClass, wal_bytes: u64) -> u64 {
        match class {
            WriteClass::Content => CONTENT_MIN_FREE_BYTES.max(wal_bytes.saturating_mul(2)),
            WriteClass::Retract => RETRACT_MIN_FREE_BYTES,
        }
    }

    /// `Err(MemoryError::DiskLow)` when the volume is under `class`'s floor.
    pub fn check(&self, class: WriteClass) -> Result<()> {
        let Some(inner) = &self.inner else {
            return Ok(());
        };
        let Some(sample) = inner.sample() else {
            return Ok(());
        };
        if sample.free_bytes < Self::threshold(class, sample.wal_bytes) {
            return Err(MemoryError::DiskLow);
        }
        Ok(())
    }
}

impl Inner {
    fn sample(&self) -> Option<DiskSample> {
        let now = self.clock.now();
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some((at, s)) = *cache
            && now.saturating_sub(at) < CACHE_TTL
        {
            return Some(s);
        }
        match self.probe.sample(&self.db_path) {
            Ok(s) => {
                *cache = Some((now, s));
                Some(s)
            }
            Err(err) => {
                tracing::warn!("memory disk probe failed; not gating this write: {err}");
                None
            }
        }
    }
}
