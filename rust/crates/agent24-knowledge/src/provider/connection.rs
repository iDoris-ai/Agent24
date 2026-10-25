//! The connection part of one binding: its [`Gate`], its connection phase and
//! the rounds probes belong to (CONTRACT-STATE-v1 S3.4, S1.12, S1.3). Pure
//! state, like the gate; the events that move a binding in and out of the
//! connection phase, and the credential events, come in the next slices.

use super::gate::Gate;
use super::state::{ConnectionPhase, Probe, UnavailableCause, judge_connection};

/// A probe the caller is about to send. Its result is applied only within the
/// round it was sent in, and only if no later probe's result was applied
/// (S3.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProbeTicket {
    round: u64,
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
    round: u64,
    next_probe: u64,
    applied_probe: Option<u64>,
    /// "Once servable" is persisted (S1.12); it is never taken back.
    once_servable: bool,
    once_servable_pending: bool,
}

impl Connection {
    /// A binding outside the connection phase. `once_servable` as persisted;
    /// when it cannot be told, the caller passes `true` (S1.12).
    pub fn new(gate: Gate, once_servable: bool) -> Self {
        Self {
            gate,
            phase: None,
            round: 0,
            next_probe: 0,
            applied_probe: None,
            once_servable,
            once_servable_pending: false,
        }
    }

    pub fn gate(&self) -> &Gate {
        &self.gate
    }

    pub fn gate_mut(&mut self) -> &mut Gate {
        &mut self.gate
    }

    pub fn phase(&self) -> Option<ConnectionPhase> {
        self.phase
    }

    /// S1.7: a generation change or "once servable" not yet confirmed persisted.
    pub fn persist_pending(&self) -> bool {
        self.gate.generation_pending() || self.once_servable_pending
    }

    /// Enters `loading` in a new round: every probe and issuance of an earlier
    /// round no longer counts.
    pub fn start_loading(&mut self) {
        self.new_round();
        self.phase = Some(ConnectionPhase::Loading);
    }

    /// Leaves the connection phase. No result moves a binding outside it, and
    /// coming back starts a new round.
    pub fn leave(&mut self) {
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
        self.round += 1;
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
        if !current || !movable {
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

    /// "Once servable" is persisted: back to `loading` for a new probe
    /// (S1.4, `local_storage`).
    pub fn confirm_once_servable(&mut self) {
        if !self.once_servable_pending {
            return;
        }
        self.once_servable = true;
        self.once_servable_pending = false;
        if self.phase == Some(ConnectionPhase::Unavailable(UnavailableCause::LocalStorage)) {
            self.start_loading();
        }
    }
}

#[cfg(test)]
mod tests;
