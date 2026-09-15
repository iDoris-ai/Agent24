//! What happened when a supervised module was stopped (SHUT-1a) — facts, each
//! from one place, rather than one overloaded outcome. The semantics are in
//! `docs/design/SHUT-shutdown-observability.md`; the terms here are its terms.
//!
//! Every field is written **once**: the first writer wins and later writes are
//! ignored, so two parties racing to describe the same end (a stop confirming
//! the group empty, and a cancellation dropping the process) cannot both
//! claim it. The facts of a confirmed stop are written together, under the
//! record's lock, in one step (see [`StopRecordHandle::gone`]).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// Whether there was a process to stop when the stop was requested.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessAtStop {
    /// Not spawned yet, between runs, or given up: nothing to signal.
    None,
    /// A run's process group existed.
    Running,
}

/// Why the drain before the stop ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DrainEndedBy {
    /// Nothing was in flight any more.
    Idle,
    /// Its time ran out with requests still in flight.
    Deadline,
    /// The module's process exited during the drain.
    ProcessExited,
    /// The callback connection ended during the drain.
    CallbackEnded,
    /// No drain: its budget was zero, or the generation was not serving.
    Skipped,
    /// The stop was cut short (its owner gave up on it) during the drain.
    Cut,
}

/// The drain before the stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainEnd {
    pub ended_by: DrainEndedBy,
    /// How long it was allowed.
    pub budget: Duration,
    /// How long it took.
    pub elapsed: Duration,
}

/// How the run's leader process ended during the stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leader {
    /// It had already exited when the stop began.
    GoneBeforeTerm,
    /// It exited on SIGTERM within the stop grace.
    ExitedInGrace,
    /// The grace ran out and it was sent SIGKILL — the sign that the grace
    /// may be too short for this module.
    KilledAfterGrace,
}

/// How the run's process group ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupEnd {
    /// Confirmed empty.
    Gone,
    /// Could not be confirmed empty, even after SIGKILL.
    Failed,
    /// Dropped without a confirmed stop, and SIGKILL was sent; not confirmed.
    KillAttempted,
    /// Dropped without a confirmed stop, and no SIGKILL could be sent safely
    /// (or sending it failed).
    KillUnavailable,
}

/// How the supervisor loop itself ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisorEnd {
    /// It returned by itself — whatever the group's end.
    Stopped,
    /// Its owner gave up on the stop (`drain_and_stop_unless`) and aborted it.
    CutOff,
    /// It was cancelled otherwise (its runtime shut down, its handle dropped).
    Killed,
    /// It panicked.
    Panicked,
}

/// The facts of a stop confirmed empty, handed over in one piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GoneFacts {
    /// `None` when it cannot be told — a retry of a stop that had already
    /// reaped the leader.
    pub leader: Option<Leader>,
    /// From the first signal to the group confirmed empty.
    pub stop_elapsed: Duration,
    /// Requests in flight and already sent when the generation was revoked:
    /// their outcome is unknown.
    pub abandoned: usize,
    /// Requests in flight but never sent.
    pub never_sent: usize,
}

/// What is known about a module's stop. `None` is "not known (yet)".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StopRecord {
    pub process: Option<ProcessAtStop>,
    pub drain: Option<DrainEnd>,
    pub leader: Option<Leader>,
    pub stop_elapsed: Option<Duration>,
    pub abandoned: Option<usize>,
    pub never_sent: Option<usize>,
    pub group: Option<GroupEnd>,
    pub supervisor: Option<SupervisorEnd>,
    /// When the drain began, so a cut can say how long it had run.
    drain_began: Option<(Instant, Duration)>,
}

impl StopRecord {
    /// The stop is complete as far as the process group goes: there was none,
    /// or its end is known.
    #[must_use]
    pub fn group_settled(&self) -> bool {
        self.process == Some(ProcessAtStop::None) || self.group.is_some()
    }
}

/// A shared, cheaply cloned handle on one supervisor's [`StopRecord`]: the
/// supervisor writes it as the stop goes, whoever asked for the stop reads it.
#[derive(Debug, Clone, Default)]
pub struct StopRecordHandle(Arc<Mutex<StopRecord>>);

fn once<T>(slot: &mut Option<T>, value: T) {
    if slot.is_none() {
        *slot = Some(value);
    }
}

impl StopRecordHandle {
    fn lock(&self) -> MutexGuard<'_, StopRecord> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// A copy of what is known now, taken under the lock: never half of a
    /// step written together.
    #[must_use]
    pub fn snapshot(&self) -> StopRecord {
        self.lock().clone()
    }

    pub(crate) fn process(&self, process: ProcessAtStop) {
        once(&mut self.lock().process, process);
    }

    pub(crate) fn drain_began(&self, budget: Duration) {
        once(&mut self.lock().drain_began, (Instant::now(), budget));
    }

    pub(crate) fn drain(&self, drain: DrainEnd) {
        once(&mut self.lock().drain, drain);
    }

    /// A stop that ran no drain: whatever was asked, nothing was drained.
    pub(crate) fn drain_skipped(&self, budget: Duration) {
        once(
            &mut self.lock().drain,
            DrainEnd {
                ended_by: DrainEndedBy::Skipped,
                budget,
                elapsed: Duration::ZERO,
            },
        );
    }

    /// The group is confirmed empty: every fact of that, in one step.
    pub(crate) fn gone(&self, facts: GoneFacts) {
        let mut r = self.lock();
        if r.group.is_some() {
            return;
        }
        r.group = Some(GroupEnd::Gone);
        r.leader = facts.leader;
        r.stop_elapsed = Some(facts.stop_elapsed);
        r.abandoned = Some(facts.abandoned);
        r.never_sent = Some(facts.never_sent);
    }

    pub(crate) fn group(&self, group: GroupEnd) {
        once(&mut self.lock().group, group);
    }

    pub(crate) fn supervisor(&self, end: SupervisorEnd) {
        once(&mut self.lock().supervisor, end);
    }

    /// The stop was cut short: its owner gave up on it (only the owner's
    /// give-up branch says so).
    pub(crate) fn cut(&self) {
        once(&mut self.lock().supervisor, SupervisorEnd::CutOff);
    }

    /// The loop is gone — returned, aborted or panicking: fill in what only
    /// its end can tell (SHUT-1a, review round 1). A drain that began and
    /// never ended was cut; a stop whose process nobody recorded had one iff
    /// a process was still unconfirmed; and the loop's own end, unless its
    /// owner already said it cut the stop short.
    pub(crate) fn finalize(&self, process_left: bool, end: SupervisorEnd) {
        let mut r = self.lock();
        if r.drain.is_none()
            && let Some((began, budget)) = r.drain_began
        {
            r.drain = Some(DrainEnd {
                ended_by: DrainEndedBy::Cut,
                budget,
                elapsed: began.elapsed(),
            });
        }
        once(
            &mut r.process,
            if process_left {
                ProcessAtStop::Running
            } else {
                ProcessAtStop::None
            },
        );
        once(&mut r.supervisor, end);
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    /// Every field is first-writer-wins; a confirmed stop's facts land
    /// together and a later drop cannot overwrite them.
    #[test]
    fn a_stop_is_described_once_and_in_one_piece() {
        let h = StopRecordHandle::default();
        h.process(ProcessAtStop::Running);
        h.process(ProcessAtStop::None);
        h.gone(GoneFacts {
            leader: Some(Leader::ExitedInGrace),
            stop_elapsed: Duration::from_millis(40),
            abandoned: 1,
            never_sent: 2,
        });
        h.group(GroupEnd::KillAttempted);
        h.supervisor(SupervisorEnd::Stopped);
        h.cut();
        let r = h.snapshot();
        assert_eq!(r.process, Some(ProcessAtStop::Running));
        assert_eq!(r.group, Some(GroupEnd::Gone));
        assert_eq!(r.leader, Some(Leader::ExitedInGrace));
        assert_eq!((r.abandoned, r.never_sent), (Some(1), Some(2)));
        assert_eq!(r.supervisor, Some(SupervisorEnd::Stopped));
        assert!(r.group_settled());
    }

    /// The loop's end fills in what only it can tell: a drain that began and
    /// never ended was cut, and the owner's cut wins over "killed". A drain
    /// that had ended stays as it ended.
    #[test]
    fn the_loops_end_fills_in_a_cut_drain_and_its_own_end() {
        let h = StopRecordHandle::default();
        h.drain_began(Duration::from_secs(30));
        h.cut();
        h.finalize(true, SupervisorEnd::Killed);
        let r = h.snapshot();
        assert_eq!(r.drain.unwrap().ended_by, DrainEndedBy::Cut);
        assert_eq!(r.supervisor, Some(SupervisorEnd::CutOff));
        assert_eq!(r.process, Some(ProcessAtStop::Running));
        assert!(!r.group_settled(), "a running group's end is still unknown");

        let h = StopRecordHandle::default();
        h.drain_began(Duration::from_secs(30));
        h.drain_skipped(Duration::ZERO);
        h.finalize(false, SupervisorEnd::Stopped);
        let r = h.snapshot();
        assert_eq!(r.drain.unwrap().ended_by, DrainEndedBy::Skipped);
        assert_eq!(r.process, Some(ProcessAtStop::None));
        assert_eq!(r.supervisor, Some(SupervisorEnd::Stopped));
    }
}
