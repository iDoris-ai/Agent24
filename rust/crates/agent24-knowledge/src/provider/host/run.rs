//! Runs' binding sets (KB-C1-04 6c-3a; CONTRACT-STATE-v1 S4.7). A run binds
//! the bindings active at its start and, Off or not, the identity each
//! selected profile with a binding points at. Its calls use those bindings,
//! whatever their generation, and never others. The set is a loosening (I1):
//! until it is confirmed persisted the run's calls are `local_storage` (§6),
//! and a set the ledger refused is written again at the run's next call. A
//! set recorded empty is backfilled once, by a call admitted on an active
//! binding that matches a candidate exactly; a call refused locally does not
//! use it up.

use super::{Host, Then};
use crate::provider::ledger::{Confirmed, Loosening};
use crate::provider::record::{Change, Ids, Of};
use crate::provider::replay::RunSet;
use crate::provider::state::{self, ServiceState, Switches};

/// A run's binding set as the host holds it.
#[derive(Debug)]
pub(super) struct Run<I: Ids> {
    id: I::Run,
    bindings: Vec<Of<I>>,
    candidates: Vec<(I::Profile, I::Identity)>,
    persisted: Persisted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Persisted {
    /// The set is written and not yet confirmed.
    Pending(Loosening),
    /// The ledger refused the set: it is written again at the next call.
    Refused,
    Durable,
    /// Durable, and its backfill is written and not yet confirmed.
    Backfilling(Loosening),
}

/// What a run's call may use (S4.7).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunCall<I: Ids> {
    /// The set, or its backfill, is not confirmed persisted:
    /// `local_storage` (§6).
    Unpersisted,
    /// These bindings and no others, active or not.
    Bound(Vec<Of<I>>),
    /// The set is empty, and this active binding matches a candidate exactly.
    /// [`Host::backfill`] records it once a call on it is admitted.
    Candidate(Of<I>),
    /// The set is empty, and no active binding matches a candidate.
    Empty,
}

impl<I: Ids> Host<I> {
    /// S4.7: `run` starts. The bindings active now and the identity of each
    /// selected profile's binding, in the members' order, are its set, written
    /// as a loosening. A run already known keeps its own.
    pub fn run_started(&mut self, run: I::Run) {
        todo!()
    }

    /// A run as the log has it: its set confirmed persisted; one unbound
    /// (`None`) binds nothing and has no candidate.
    pub fn restore_run(&mut self, run: I::Run, set: RunSet<I>) {
        todo!()
    }

    /// What a call of `run` may use; `None` for a run never started or
    /// restored. A set the ledger refused is written again first.
    pub fn run_call(&mut self, run: &I::Run) -> Option<RunCall<I>> {
        todo!()
    }

    /// A call of `run` on `of`, its [`RunCall::Candidate`], was admitted: the
    /// set is backfilled with it, a loosening. Ignored unless the set is
    /// still empty and durable, with no backfill written, and `of` is still
    /// active and matches a candidate exactly.
    pub fn backfill(&mut self, run: &I::Run, of: Of<I>) {
        todo!()
    }

    /// The set of `run`, written under `ticket`, is confirmed persisted.
    pub(super) fn run_written(&mut self, run: I::Run, ticket: Loosening) {
        todo!()
    }

    /// The backfill of `run` with `of`, written under `ticket`, is confirmed
    /// persisted.
    pub(super) fn backfilled(&mut self, run: I::Run, ticket: Loosening, of: Of<I>) {
        todo!()
    }

    /// The bindings active now (the term in CONTRACT-STATE-v1): derived as
    /// in the connection phase, Off and the disabled flag included.
    fn active(&self) -> Vec<Of<I>> {
        todo!()
    }
}

#[cfg(test)]
mod tests;
