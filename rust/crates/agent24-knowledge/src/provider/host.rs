//! The Provider's orchestration (KB-C1-04 6c), pure like its parts: the
//! profiles with their bindings, Knowledge Off and the ledger, driven by the
//! user's switches: Off and On, a profile's disabling and its selection
//! (CONTRACT-STATE-v1 S1.10, S3.3). A tightening takes effect at once and is
//! written; a loosening is written first and takes effect once confirmed
//! persisted (I1, I2). The opposite action while a loosening is pending gives
//! it up, and its offset is written instead.
//!
//! The caller runs the writes as [`super::journal`] orders them and reports
//! each back. New bindings are in `create`; identity changes, calls and the
//! start follow in later slices.

use std::collections::HashMap;

use super::binding::{Activation, Leave};
use super::connection::Round;
use super::gate::CallId;
use super::journal::{self, Attempt, Journal, Report, SealWrite, Written};
use super::ledger::{Confirmed, Ledger, Loosening, Refused, Scope, Ticket, Tightening};
use super::member::{Left, Member};
use super::profile::Profile;
use super::record::{Change, Ids, Of};
use super::replay::Record;

mod create;

pub use create::Created;

/// What an event or a report asks of the caller.
#[derive(Debug, PartialEq, Eq)]
pub struct Output<I: Ids> {
    /// Abort these calls and answer them `cancelled` (S4.5).
    pub aborted: Vec<(Of<I>, CallId)>,
    /// Bindings that entered `loading`, in these rounds.
    pub loading: Vec<(I::Profile, Round)>,
    /// The seal write to start.
    pub seal: Option<SealWrite>,
}

impl<I: Ids> Default for Output<I> {
    fn default() -> Self {
        Self {
            aborted: Vec::new(),
            loading: Vec::new(),
            seal: None,
        }
    }
}

/// A loosening by what it switches on.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Loose<P> {
    On,
    Enable(P),
    Select(P),
}

#[derive(Debug)]
enum Then<I: Ids> {
    /// The generation values written with it take effect too (S4.4).
    Tight {
        ticket: Ticket,
        values: Vec<(Of<I>, u64)>,
    },
    Loose {
        ticket: Loosening,
        switch: Loose<I::Profile>,
    },
    /// A new binding, in [`Host::creations`].
    Create { ticket: Loosening },
}

#[derive(Debug)]
pub struct Host<I: Ids> {
    off: bool,
    /// In the order they were added, so a write lists them the same way.
    members: Vec<Member<I>>,
    ledger: Ledger<I::Profile>,
    /// Loosenings written and not yet confirmed.
    pending: HashMap<Loose<I::Profile>, Loosening>,
    journal: Journal<I, Then<I>>,
    /// The highest generation value used or written, for any binding: a new
    /// binding's first value goes past it (S4.4, A.4).
    highest: u64,
    creations: Vec<create::Creation<I>>,
}

impl<I: Ids> Host<I> {
    /// `next_seq` is the number the log's next record takes; `highest` the
    /// highest generation value it holds, or more (A.4).
    pub fn new(off: bool, seal_present: bool, next_seq: u64, highest: u64) -> Self {
        Self {
            off,
            members: Vec::new(),
            ledger: Ledger::new(seal_present),
            pending: HashMap::new(),
            journal: Journal::new(next_seq),
            highest,
            creations: Vec::new(),
        }
    }

    pub fn insert(&mut self, member: Member<I>) {
        self.members.push(member);
    }

    pub fn is_off(&self) -> bool {
        self.off
    }

    pub fn profile(&self, id: &I::Profile) -> Option<&Profile<I::Kb>> {
        let member = self.members.iter().find(|m| m.id() == id);
        member.map(Member::profile)
    }

    /// "Some settings not saved" (I2).
    pub fn unsaved(&self) -> bool {
        self.ledger.unsaved()
    }

    fn member(&mut self, id: &I::Profile) -> Option<&mut Member<I>> {
        self.members.iter_mut().find(|m| m.id() == id)
    }

    pub fn turn_off(&mut self) -> Output<I> {
        let mut out = Output::default();
        if self.give_up(&Loose::On, Change::Off, &mut out) || self.off {
            return out;
        }
        self.off = true;
        let mut left = Left::new(Change::Off);
        for m in &mut self.members {
            m.leave(Leave::Off, &mut left);
        }
        let tightening = self.ledger.tighten(Scope::Host);
        self.tightened(tightening, left, &mut out);
        out
    }

    pub fn turn_on(&mut self) -> Result<(), Refused> {
        if !self.off {
            return Ok(());
        }
        self.loosen(Loose::On, Scope::Host, Change::On)
    }

    /// Unknown profiles are ignored, as by every per-profile event.
    pub fn disable(&mut self, id: I::Profile) -> Output<I> {
        let change = Change::Disable(id.clone());
        self.tighten(id, Loose::Enable, change, Leave::Disable, |m| {
            let takes = !m.disabled();
            m.set_disabled(true);
            takes
        })
    }

    pub fn enable(&mut self, id: I::Profile) -> Result<(), Refused> {
        match self.member(&id) {
            Some(m) if m.disabled() => {}
            _ => return Ok(()),
        }
        let change = Change::Enable(id.clone());
        self.loosen(Loose::Enable(id.clone()), Scope::profile(id), change)
    }

    pub fn unselect(&mut self, id: I::Profile) -> Output<I> {
        let change = Change::Unselect(id.clone());
        // `Profile::leave` remembers the unselection.
        self.tighten(id, Loose::Select, change, Leave::Unselect, |m| {
            m.profile().selected()
        })
    }

    pub fn select(&mut self, id: I::Profile) -> Result<(), Refused> {
        match self.member(&id) {
            Some(m) if !m.profile().selected() => {}
            _ => return Ok(()),
        }
        let change = Change::Select(id.clone());
        self.loosen(Loose::Select(id.clone()), Scope::profile(id), change)
    }

    /// A per-profile tightening: gives up the pending opposite, or else takes
    /// effect if `takes` says it changes something.
    fn tighten(
        &mut self,
        id: I::Profile,
        opposite: fn(I::Profile) -> Loose<I::Profile>,
        change: Change<I>,
        event: Leave,
        takes: impl FnOnce(&mut Member<I>) -> bool,
    ) -> Output<I> {
        let mut out = Output::default();
        if self.give_up(&opposite(id.clone()), change.clone(), &mut out) {
            return out;
        }
        let Some(m) = self.member(&id) else {
            return out;
        };
        if !takes(m) {
            return out;
        }
        let mut left = Left::new(change);
        m.leave(event, &mut left);
        let tightening = self.ledger.tighten(Scope::profile(id));
        self.tightened(tightening, left, &mut out);
        out
    }

    /// I1: the opposite action while `switch` is pending gives it up; `offset`
    /// is written so that a restart never finds it. Nothing changed in memory.
    fn give_up(
        &mut self,
        switch: &Loose<I::Profile>,
        offset: Change<I>,
        out: &mut Output<I>,
    ) -> bool {
        let Some(ticket) = self.pending.remove(switch) else {
            return false;
        };
        // The ledger no longer holds it (confirmed meanwhile): the action is
        // an ordinary tightening.
        let Some(tightening) = self.ledger.abandon(ticket) else {
            return false;
        };
        self.tightened(tightening, Left::new(offset), out);
        true
    }

    fn loosen(
        &mut self,
        switch: Loose<I::Profile>,
        scope: Scope<I::Profile>,
        change: Change<I>,
    ) -> Result<(), Refused> {
        if self.pending.contains_key(&switch) {
            return Ok(());
        }
        let ticket = self.ledger.loosen(scope, None, self.off)?;
        self.pending.insert(switch.clone(), ticket);
        let then = Then::Loose { ticket, switch };
        self.journal.push(vec![change], then);
        Ok(())
    }

    fn tightened(
        &mut self,
        Tightening { ticket, unseal }: Tightening,
        left: Left<I>,
        out: &mut Output<I>,
    ) {
        if let Some(op) = unseal {
            out.seal = Some(SealWrite::Delete(op));
        }
        out.aborted.extend(left.aborted);
        let values = left.generations;
        let written = values.iter().map(|(_, value)| *value);
        self.highest = written.fold(self.highest, u64::max);
        self.journal
            .push(left.changes, Then::Tight { ticket, values });
    }

    /// The next write to run, unless one is running.
    pub fn take_write(&mut self) -> Option<(Attempt, Record<I>)> {
        self.journal.take()
    }

    /// The write taken in `attempt` ended; any other report is ignored.
    pub fn written(&mut self, attempt: Attempt, outcome: Written) -> Output<I> {
        let mut out = Output::default();
        match self.journal.written(attempt, outcome) {
            Report::Ignored | Report::Failed(Then::Loose { .. } | Then::Create { .. }) => {
                return out;
            }
            Report::Failed(Then::Tight { ticket, .. }) => {
                self.ledger.record_failed(*ticket);
                return out;
            }
            Report::Durable(Then::Tight { ticket, values }) => {
                self.ledger.record_durable(ticket);
                for (of, value) in values {
                    let member = self.member(&of.profile);
                    if let Some(round) = member.and_then(|m| m.confirm_generation(&of, value)) {
                        out.loading.push((of.profile, round));
                    }
                }
            }
            Report::Durable(Then::Loose { ticket, switch }) => {
                if self.pending.get(&switch) == Some(&ticket) {
                    self.pending.remove(&switch);
                }
                if self.ledger.loosening_durable(ticket) == Confirmed::Applied {
                    self.apply(switch, &mut out);
                }
            }
            Report::Durable(Then::Create { ticket }) => self.created(ticket, &mut out),
        }
        // A tightening on the way deleted the seal: no rebuild yet.
        if let Some(op) = self.ledger.reseal() {
            out.seal = Some(SealWrite::Rebuild(op));
        }
        out
    }

    /// The seal write ended.
    pub fn sealed(&mut self, write: SealWrite, outcome: Written) -> Output<I> {
        Output {
            seal: journal::sealed(&mut self.ledger, write, outcome),
            ..Output::default()
        }
    }

    /// A confirmed loosening takes effect; the bindings it frees connect.
    fn apply(&mut self, switch: Loose<I::Profile>, out: &mut Output<I>) {
        let off = self.off;
        let mut connect = |m: &mut Member<I>, off: bool| {
            if let Some(round) = m.connect(off) {
                out.loading.push((m.id().clone(), round));
            }
        };
        match switch {
            Loose::On => {
                self.off = false;
                self.members.iter_mut().for_each(|m| connect(m, false));
            }
            Loose::Enable(id) => {
                if let Some(m) = self.member(&id) {
                    m.set_disabled(false);
                    connect(m, off);
                }
            }
            Loose::Select(id) => {
                if let Some(m) = self.member(&id) {
                    m.profile_mut().activate(Activation::Reselect);
                    connect(m, off);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
