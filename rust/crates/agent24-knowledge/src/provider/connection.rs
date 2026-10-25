//! The connection part of one binding: its [`Gate`], its connection phase and
//! the rounds probes belong to (CONTRACT-STATE-v1 S3.4, S1.12, S1.3). Pure
//! state, like the gate; the credential events are in `credential`, the
//! events that move a binding in and out of the connection phase come next.

use super::gate::{CallId, Closed, Gate, Invalidation};
use super::state::{ConnectionPhase, Probe, UnavailableCause, judge_connection};
use crate::common::Generation;

/// A `loading` round: what is sent in it (probes, the credential issuance)
/// counts only while it is the current one (S3.4; S3.1, credential ownership).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Round(u64);

/// A probe the caller is about to send. Its result is applied only within the
/// round it was sent in, and only if no later probe's result was applied
/// (S3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeTicket {
    round: Round,
    seq: u64,
}

/// What applying a probe's result did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeApplied {
    /// The phase it leads to is now in effect.
    Moved(ConnectionPhase),
    /// Servable for the first time: persist "once servable" (a loosening)
    /// and confirm it with [`Connection::confirm_once_servable`]; until then
    /// the binding is `unavailable(local_storage)` (S1.12).
    PersistOnceServable,
    /// From an ended round, overtaken by a later probe, or for a phase a
    /// probe does not move (S1.4).
    Ignored,
}

#[derive(Debug)]
pub struct Connection {
    gate: Gate,
    /// `None` outside the connection phase (`registered`, `disabled`).
    phase: Option<ConnectionPhase>,
    round: Round,
    next_probe: u64,
    applied_probe: Option<u64>,
    /// "Once servable" is persisted (S1.12); it is never taken back.
    once_servable: bool,
    once_servable_pending: bool,
    /// A reason that does not retry by itself, kept across leaving the
    /// connection phase: coming back does not retry it (S1.4).
    held: Option<UnavailableCause>,
}

impl Connection {
    /// A binding outside the connection phase. `once_servable` as persisted;
    /// when it cannot be told, the caller passes `true` (S1.12).
    pub fn new(gate: Gate, once_servable: bool) -> Self {
        Self {
            gate,
            phase: None,
            round: Round(0),
            next_probe: 0,
            applied_probe: None,
            once_servable,
            once_servable_pending: false,
            held: None,
        }
    }

    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    // The gate is changed only through the methods below, so a generation's
    // persistence is confirmed through `confirm_generation`, which also leaves
    // `local_storage`.

    pub fn open_call(&mut self) -> Result<(CallId, Generation), Closed> {
        self.gate.open_call()
    }

    pub fn send(&mut self, call: CallId, request_id: &str) -> Result<Generation, Closed> {
        self.gate.send(call, request_id)
    }

    pub fn finish_request(&mut self, call: CallId, request_id: &str) {
        self.gate.finish_request(call, request_id);
    }

    pub fn close_call(&mut self, call: CallId) {
        self.gate.close_call(call);
    }

    pub fn invalidate(&mut self) -> Invalidation {
        self.gate.invalidate()
    }

    pub fn phase(&self) -> Option<ConnectionPhase> {
        self.phase
    }

    /// S1.7: a generation change or "once servable" not yet confirmed persisted.
    pub fn persist_pending(&self) -> bool {
        self.gate.generation_pending() || self.once_servable_pending
    }

    /// Enters `loading` in a new round: every probe and issuance of an earlier
    /// round no longer counts. With a change not yet persisted it is
    /// `unavailable(local_storage)` instead, and `None` (S1.4); `loading`
    /// follows once that is confirmed. A reason kept by leaving comes back
    /// instead, also `None`.
    pub fn start_loading(&mut self) -> Option<Round> {
        if let Some(cause) = self.held {
            self.fail(cause);
            return None;
        }
        if self.persist_pending() {
            self.fail(UnavailableCause::LocalStorage);
            return None;
        }
        self.new_round();
        self.phase = Some(ConnectionPhase::Loading);
        Some(self.round)
    }

    /// Lets `loading` try again a reason kept by [`Connection::leave`]: a
    /// loosening (S1.10) the caller has persisted.
    pub fn release(&mut self, cause: UnavailableCause) {
        if self.held == Some(cause) {
            self.held = None;
        }
    }

    /// Whether `round` is the current one.
    pub fn is_current(&self, round: Round) -> bool {
        round == self.round
    }

    pub fn round(&self) -> Round {
        self.round
    }

    /// S1.12: servable at least once, persisted or not yet.
    pub fn ever_servable(&self) -> bool {
        self.once_servable || self.once_servable_pending
    }

    /// Leaves the connection phase, ending the round: nothing sent in it
    /// counts any more. A reason that does not retry by itself is kept.
    pub fn leave(&mut self) {
        if let Some(ConnectionPhase::Unavailable(cause)) = self.phase
            && !cause.retries_automatically()
            && cause != UnavailableCause::LocalStorage
        {
            self.held = Some(cause);
        }
        self.new_round();
        self.phase = None;
    }

    /// Moves to `unavailable` with `cause` in a new round, so no result of the
    /// round it ends moves it back.
    pub fn fail(&mut self, cause: UnavailableCause) {
        self.new_round();
        self.phase = Some(ConnectionPhase::Unavailable(cause));
    }

    /// Probe numbers keep growing across rounds, so the last applied one need
    /// not be reset.
    fn new_round(&mut self) {
        self.round = Round(self.round.0 + 1);
    }

    /// A probe may go out from `loading`, a servable phase, or an
    /// `unavailable` that retries by itself (S1.5). A single request that
    /// came back `unavailable` or `timeout` asks for one, and only that
    /// probe's result moves the binding (S1.3).
    pub fn probe(&mut self) -> Option<ProbeTicket> {
        let probing = match self.phase? {
            ConnectionPhase::Unavailable(cause) => cause.retries_automatically(),
            ConnectionPhase::Loading
            | ConnectionPhase::Ready
            | ConnectionPhase::Degraded { .. } => true,
        };
        if !probing || self.persist_pending() {
            return None;
        }
        let ticket = ProbeTicket {
            round: self.round,
            seq: self.next_probe,
        };
        self.next_probe += 1;
        Some(ticket)
    }

    /// S3.4: moves between `loading`, `ready`, `degraded` and `unavailable`;
    /// never increments the generation and never aborts calls in flight.
    pub fn apply_probe(&mut self, ticket: ProbeTicket, probe: Probe) -> ProbeApplied {
        let current =
            ticket.round == self.round && self.applied_probe.is_none_or(|s| ticket.seq > s);
        let movable = match self.phase {
            Some(ConnectionPhase::Unavailable(cause)) => cause.retries_automatically(),
            Some(_) => true,
            None => false,
        };
        // A change not yet persisted holds the binding where it is.
        if !current || !movable || self.persist_pending() {
            return ProbeApplied::Ignored;
        }
        self.applied_probe = Some(ticket.seq);
        let phase = judge_connection(probe);
        if phase.is_servable() && !self.once_servable {
            self.once_servable_pending = true;
            self.fail(UnavailableCause::LocalStorage);
            return ProbeApplied::PersistOnceServable;
        }
        self.phase = Some(phase);
        ProbeApplied::Moved(phase)
    }

    /// "Once servable" is persisted. Returns the new round if the binding left
    /// `local_storage` for `loading`.
    pub fn confirm_once_servable(&mut self) -> Option<Round> {
        if !self.once_servable_pending {
            return None;
        }
        self.once_servable = true;
        self.once_servable_pending = false;
        self.leave_local_storage()
    }

    /// The generation `value` is persisted ([`Gate::confirm`]). Returns the new
    /// round if the binding left `local_storage` for `loading`.
    pub fn confirm_generation(&mut self, value: Generation) -> Option<Round> {
        self.gate.confirm(value);
        self.leave_local_storage()
    }

    /// S1.4: `local_storage` goes back to `loading` once nothing is pending;
    /// [`Connection::start_loading`] keeps it there while something still is.
    fn leave_local_storage(&mut self) -> Option<Round> {
        let stored =
            self.phase == Some(ConnectionPhase::Unavailable(UnavailableCause::LocalStorage));
        if stored { self.start_loading() } else { None }
    }
}

#[cfg(test)]
mod tests;
