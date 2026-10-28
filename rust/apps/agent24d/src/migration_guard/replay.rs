//! Startup replay (design D3/D4, 5e-2). Not wired (`ENABLED=false`).
//!
//! Reads `migration-guard.json` and decides, purely from disk, what an
//! interrupted upgrade left behind. Nothing here trusts memory from the
//! crashed process; the restorer it drives re-derives every step.
//!
//! | durable log          | action                                   | outcome            |
//! |----------------------|------------------------------------------|--------------------|
//! | absent / `Committed` | none                                     | `Proceed`          |
//! | `Preparing`/`Snapshotted` | none (databases were never migrated) | `Docked(Interrupted)` |
//! | `Migrating`          | restore the whole pair from the manifest | `Restored`         |
//! | `Restoring`          | resume the restore                       | `Restored`         |
//! | `Restored`           | final check only (no rebuild)            | `Restored`         |
//! | `RollbackFailed`     | nothing (evidence is kept, never retried) | `Docked(RollbackFailed)` |
//! | corrupt / unreadable | nothing is touched                       | `Docked(GuardUnusable)` |
//!
//! `Restored` is NOT "go": the upgrade failed and the pair is back at the
//! snapshot. 5f must still publish the `migration_failed` marker and dock;
//! only `Proceed` may continue startup. A failed restore is the
//! `RollbackFailed` phase (a legacy `Restoring` + `last_error` log reads as
//! the same); it is surfaced as [`Dock::RollbackFailed`] without touching
//! any file. [`ReplayOutcome::verdict`] maps every outcome to go / dock.
//!
//! Caller contract (5g wiring):
//! - run it inside `spawn_blocking` (hashing and copying whole databases) and
//!   make the `FileOps` `Sync`;
//! - hold the daemon singleton lock for the whole call;
//! - call it before any pool/connection to either database exists; no business
//!   task, sweep or listener may start until it returned `Proceed`;
//! - treat every `Docked` and `Restored` as a dock, never as a retry loop.

use super::atomic::FileOps;
use super::guard_log::{self, GuardError, Phase};
use super::restore::{RestoreError, RestoreRequest, restore};

#[derive(Debug)]
pub enum Dock {
    /// Guard log corrupt, unknown format or unreadable. Nothing was touched.
    GuardUnusable(String),
    /// Stopped before the first migration: databases are unmigrated, but the
    /// upgrade did not finish, so the daemon docks (D3) instead of guessing.
    Interrupted(Phase),
    /// `Restoring` could not be persisted: no file was touched.
    Persist(GuardError),
    /// The restore did not complete (snapshot/quarantine/progress are kept,
    /// both databases stay disabled). `persisted` = the error reached the log.
    RollbackFailed { reason: String, persisted: bool },
}

#[derive(Debug)]
pub enum ReplayOutcome {
    /// No log or `Committed`: nothing to recover.
    Proceed,
    /// The pair is byte-identical to the snapshot with no sidecars, and the
    /// log says `Restored`. `from` is the phase found on disk.
    Restored {
        from: Phase,
        leftover_cleanup: usize,
    },
    Docked(Dock),
}

impl ReplayOutcome {
    #[must_use]
    pub fn may_proceed(&self) -> bool {
        matches!(self, Self::Proceed)
    }
}

/// Decide and, if needed, finish the interrupted upgrade. See module docs.
pub fn startup_replay(ops: &dyn FileOps, req: &RestoreRequest<'_>) -> ReplayOutcome {
    let mut log = match guard_log::read(req.data_dir) {
        Ok(None) => return ReplayOutcome::Proceed,
        Ok(Some(log)) => log,
        Err(e) => return ReplayOutcome::Docked(Dock::GuardUnusable(format!("{e:?}"))),
    };
    let from = log.phase;
    match from {
        Phase::Committed => ReplayOutcome::Proceed,
        Phase::Preparing | Phase::Snapshotted => ReplayOutcome::Docked(Dock::Interrupted(from)),
        Phase::RollbackFailed => ReplayOutcome::Docked(Dock::RollbackFailed {
            reason: log.last_error.unwrap_or_default(),
            persisted: true,
        }),
        Phase::Migrating | Phase::Restoring | Phase::Restored => {
            match restore(ops, req, &mut log) {
                Ok(leftover_cleanup) => ReplayOutcome::Restored {
                    from,
                    leftover_cleanup,
                },
                Err(RestoreError::Guard(g)) => ReplayOutcome::Docked(Dock::Persist(g)),
                Err(RestoreError::RollbackFailed { reason, persisted }) => {
                    ReplayOutcome::Docked(Dock::RollbackFailed { reason, persisted })
                }
                // Unreachable for these phases; never read it as success.
                Err(RestoreError::NotRestorable(p)) => {
                    ReplayOutcome::Docked(Dock::GuardUnusable(format!("not restorable: {p:?}")))
                }
            }
        }
    }
}
