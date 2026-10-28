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
use crate::provider::member::Member;
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
        if self.runs.iter().any(|r| r.id == run) {
            return;
        }
        let candidates = self.members.iter().filter(|m| m.profile().selected());
        let candidates = candidates
            .filter_map(|m| Some((m.id().clone(), m.identity()?.clone())))
            .collect();
        self.runs.push(Run {
            id: run,
            bindings: self.active(),
            candidates,
            persisted: Persisted::Refused,
        });
        self.persist(self.runs.len() - 1);
    }

    /// A run as the log has it, once each: its set confirmed persisted; one
    /// unbound (`None`) binds nothing and has no candidate.
    pub fn restore_run(&mut self, run: I::Run, set: RunSet<I>) {
        let (bindings, candidates) = set.unwrap_or_default();
        self.runs.push(Run {
            id: run,
            bindings,
            candidates,
            persisted: Persisted::Durable,
        });
    }

    /// What a call of `run` may use; `None` for a run never started or
    /// restored. A set the ledger refused is written again first.
    pub fn run_call(&mut self, run: &I::Run) -> Option<RunCall<I>> {
        let i = self.runs.iter().position(|r| r.id == *run)?;
        if self.runs[i].persisted == Persisted::Refused {
            self.persist(i);
        }
        let r = &self.runs[i];
        Some(match r.persisted {
            Persisted::Durable if !r.bindings.is_empty() => RunCall::Bound(r.bindings.clone()),
            Persisted::Durable => match self.candidate(r) {
                Some(of) => RunCall::Candidate(of),
                None => RunCall::Empty,
            },
            _ => RunCall::Unpersisted,
        })
    }

    /// A call of `run` on `of`, its [`RunCall::Candidate`], was admitted: the
    /// set is backfilled with it, a loosening. Ignored unless the set is
    /// still empty and durable, with no backfill written, and `of` is still
    /// active and matches a candidate exactly.
    pub fn backfill(&mut self, run: &I::Run, of: Of<I>) {
        let Some(i) = self.runs.iter().position(|r| r.id == *run) else {
            return;
        };
        let r = &self.runs[i];
        let empty = r.persisted == Persisted::Durable && r.bindings.is_empty();
        if !empty || !self.matching(r).any(|m| m == of) {
            return;
        }
        let change = Change::RunBackfilled {
            run: run.clone(),
            of: of.clone(),
        };
        let Some(scope) = change.scope() else {
            return;
        };
        if let Ok(ticket) = self.ledger.loosen(scope, None, self.off) {
            let run = run.clone();
            self.journal
                .push(vec![change], Then::Backfill { ticket, run, of });
            self.runs[i].persisted = Persisted::Backfilling(ticket);
        }
    }

    /// The set of `run`, written under `ticket`, is confirmed persisted.
    pub(super) fn run_written(&mut self, run: I::Run, ticket: Loosening) {
        if self.ledger.loosening_durable(ticket) != Confirmed::Applied {
            return;
        }
        let r = self.runs.iter_mut().find(|r| r.id == run);
        if let Some(r) = r.filter(|r| r.persisted == Persisted::Pending(ticket)) {
            r.persisted = Persisted::Durable;
        }
    }

    /// The backfill of `run` with `of`, written under `ticket`, is confirmed
    /// persisted.
    pub(super) fn backfilled(&mut self, run: I::Run, ticket: Loosening, of: Of<I>) {
        if self.ledger.loosening_durable(ticket) != Confirmed::Applied {
            return;
        }
        let r = self.runs.iter_mut().find(|r| r.id == run);
        if let Some(r) = r.filter(|r| r.persisted == Persisted::Backfilling(ticket)) {
            r.bindings = vec![of];
            r.persisted = Persisted::Durable;
        }
    }

    /// Writes the set of the run at `i`, unless the ledger refuses it (I1).
    fn persist(&mut self, i: usize) {
        let r = &self.runs[i];
        let change = Change::RunBound {
            run: r.id.clone(),
            bindings: r.bindings.clone(),
            candidates: r.candidates.clone(),
        };
        let Some(scope) = change.scope() else {
            return;
        };
        if let Ok(ticket) = self.ledger.loosen(scope, None, self.off) {
            let run = r.id.clone();
            self.journal.push(vec![change], Then::Run { ticket, run });
            self.runs[i].persisted = Persisted::Pending(ticket);
        }
    }

    /// The first active binding matching one of `r`'s candidates exactly.
    fn candidate(&self, r: &Run<I>) -> Option<Of<I>> {
        self.matching(r).next()
    }

    /// The active bindings matching `r`'s candidates exactly, in the
    /// candidates' order.
    fn matching<'a>(&'a self, r: &'a Run<I>) -> impl Iterator<Item = Of<I>> + 'a {
        r.candidates.iter().filter_map(|(profile, identity)| {
            let m = self.members.iter().find(|m| m.id() == profile)?;
            let of = m.of().filter(|_| m.identity() == Some(identity))?;
            self.is_active(m).then_some(of)
        })
    }

    /// The bindings active now (the term in CONTRACT-STATE-v1).
    fn active(&self) -> Vec<Of<I>> {
        let active = self.members.iter().filter(|m| self.is_active(m));
        active.filter_map(Member::of).collect()
    }

    /// Derived as in the connection phase, Off and the disabled flag
    /// included.
    fn is_active(&self, m: &Member<I>) -> bool {
        let switches = Switches {
            off: self.off,
            profile_disabled: m.disabled(),
        };
        let state = state::derive(switches, m.profile().target());
        matches!(state, Some(ServiceState::Connected(_)))
    }
}

#[cfg(test)]
mod tests;
