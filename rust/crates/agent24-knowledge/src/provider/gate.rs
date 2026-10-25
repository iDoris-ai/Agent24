//! One binding's generation, admission and calls in flight (CONTRACT-STATE-v1
//! S4.1–S4.6, S4.8). Pure state: the caller persists the value [`Gate`] asks
//! for and reports the outcome back, holds the binding's lock across a check
//! and the action it guards, and handles one event at a time (S3.2).

use std::collections::{HashMap, HashSet};

use crate::common::{ConstBool, Generation};
use crate::local_cancelled::{CancelledCode, CancelledDetails, CancelledError, LocalCancelled};

/// A call admitted on this binding: one Tool call's part for it, with every
/// sub-request it sends (S4.8).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CallId(u64);

/// Admission is closed: an invalidation is in effect, or a generation is not
/// yet persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Closed;

/// Why a response or a result is not taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dropped {
    /// The call was invalidated, cancelled or is unknown, or the generation
    /// moved on: the caller answers with [`Gate::cancelled`].
    Cancelled,
}

/// A call not invalidated was admitted under the value in effect and nothing
/// is pending: calls open only while nothing is pending, an invalidation
/// marks every call, and a confirmation forgets the marked ones.
#[derive(Debug)]
struct Call {
    generation: Generation,
    requests: HashSet<String>,
    invalidated: bool,
}

#[derive(Debug)]
pub struct Gate {
    /// The value in effect: confirmed persisted.
    generation: Generation,
    /// The value waiting for its persistence to be confirmed.
    pending: Option<Generation>,
    calls: HashMap<CallId, Call>,
    next_call: u64,
}

impl Gate {
    /// A binding whose generation `value` is in effect.
    pub fn new(value: Generation) -> Self {
        Self {
            generation: value,
            pending: None,
            calls: HashMap::new(),
            next_call: 0,
        }
    }

    /// At Provider start: `last` is the persisted value, and the start's +1
    /// (S4.2) must be persisted before anything is admitted. Persist
    /// [`Gate::reported`] and [`Gate::confirm`] it.
    pub fn restored(last: Generation) -> Self {
        Self {
            pending: Some(last.saturating_add(1)),
            ..Self::new(last)
        }
    }

    /// The value in effect.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// The value reported as current: the one waiting to be persisted, if any
    /// (S4.5, `current_generation`).
    pub fn reported(&self) -> Generation {
        self.pending.unwrap_or(self.generation)
    }

    /// A generation change is not yet confirmed persisted (part of S1.7's
    /// `persist_pending`).
    pub fn generation_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// No new call or sub-request is admitted while a change is pending.
    pub fn admission_open(&self) -> bool {
        self.pending.is_none()
    }

    /// S4.5: in one step, close admission and mark every call in flight
    /// invalidated, so nothing goes out under the old value; the next value is
    /// the one already pending plus one (S4.4). Returns the value to persist
    /// and the calls to abort and answer with `cancelled`.
    pub fn invalidate(&mut self) -> (Generation, Vec<CallId>) {
        let next = self.reported().saturating_add(1);
        self.pending = Some(next);
        let mut aborted = Vec::new();
        for (id, call) in &mut self.calls {
            if !call.invalidated {
                call.invalidated = true;
                aborted.push(*id);
            }
        }
        (next, aborted)
    }

    /// Persistence of `value` is confirmed. Only the newest pending value takes
    /// effect; a confirmation of one overtaken by a later increment is ignored
    /// (S4.4). A failed or unknown outcome is not reported: the value stays
    /// pending and admission closed.
    pub fn confirm(&mut self, value: Generation) -> bool {
        if self.pending != Some(value) {
            return false;
        }
        self.generation = value;
        self.pending = None;
        self.calls.retain(|_, call| !call.invalidated);
        true
    }

    /// Admits a new call under the value in effect. The binding's state is
    /// checked separately (`state::admit`).
    pub fn open_call(&mut self) -> Result<(CallId, Generation), Closed> {
        if !self.admission_open() {
            return Err(Closed);
        }
        let id = CallId(self.next_call);
        self.next_call += 1;
        self.calls.insert(
            id,
            Call {
                generation: self.generation,
                requests: HashSet::new(),
                invalidated: false,
            },
        );
        Ok((id, self.generation))
    }

    /// Registers a sub-request of `call` about to be sent with `request_id`;
    /// it carries the call's generation. Refused once the call is invalidated
    /// or the generation has moved (S4.8: a fill-up goes out only then).
    pub fn send(&mut self, call: CallId, request_id: &str) -> Result<Generation, Closed> {
        match self.calls.get_mut(&call) {
            Some(c) if !c.invalidated => {
                c.requests.insert(request_id.to_owned());
                Ok(c.generation)
            }
            _ => Err(Closed),
        }
    }

    /// S4.6: a response is taken only for a request in flight, of a call not
    /// invalidated, whose generation is the current one. `echoed` is the
    /// response's generation; `None` is an error envelope R6.1 lets echo
    /// `null`, paired by the exchange it came back on.
    pub fn accept(
        &self,
        call: CallId,
        request_id: &str,
        echoed: Option<Generation>,
    ) -> Result<(), Dropped> {
        let live = self
            .calls
            .get(&call)
            .is_some_and(|c| !c.invalidated && c.requests.contains(request_id));
        if live && echoed.is_none_or(|g| g == self.generation) {
            Ok(())
        } else {
            Err(Dropped::Cancelled)
        }
    }

    /// The request is answered (taken or not); the call stays open for its
    /// other sub-requests.
    pub fn finish_request(&mut self, call: CallId, request_id: &str) {
        if let Some(c) = self.calls.get_mut(&call) {
            c.requests.remove(request_id);
        }
    }

    /// S4.6's last check, just before the call's result joins the model's
    /// context: the generation it was admitted under is still the one in
    /// effect, it was not invalidated, and nothing is pending. The binding's
    /// state (`disabled`, `registered`, `retired`) is the caller's to check
    /// under the same lock.
    pub fn may_append(&self, call: CallId) -> Result<(), Dropped> {
        match self.calls.get(&call) {
            Some(c) if !c.invalidated => Ok(()),
            _ => Err(Dropped::Cancelled),
        }
    }

    /// The call is over: delivered, dropped or cancelled with its run (S3.3,
    /// no increment). Its late responses are then not in flight.
    pub fn close_call(&mut self, call: CallId) {
        self.calls.remove(&call);
    }

    /// The local `cancelled` for a request sent under `generation` (S4.5,
    /// S6.1): it echoes the request, and `current_generation` is the value
    /// reported now.
    pub fn cancelled(&self, request_id: &str, generation: Generation) -> LocalCancelled {
        LocalCancelled {
            error: CancelledError {
                code: CancelledCode::Cancelled,
                message: "The knowledge request was cancelled".to_owned(),
                hint: None,
                details: CancelledDetails {
                    retryable: ConstBool,
                    request_id: request_id.to_owned(),
                    generation,
                    current_generation: self.reported(),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests;
