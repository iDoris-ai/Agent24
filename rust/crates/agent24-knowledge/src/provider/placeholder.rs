//! The unbound placeholder of one profile (CONTRACT-STATE-v1 S2.5): no
//! binding that is not retired, so no generation and no request. Logging in
//! (remote) or handshaking (local-managed) happens on it; only the attempt
//! in flight counts, and a failure is recorded with its reason.

use super::state::{PlaceholderFailure, Switches};

/// A login or handshake the caller is about to make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Attempt(u64);

/// How an attempt ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempted {
    /// An identity came back: the caller persists a new binding with its
    /// initial value before it is used (S2.5, I1).
    Identified,
    /// The issuer or the sidecar could not be reached.
    Unreachable,
    /// The login was refused.
    Rejected,
    /// The handshake gave no instance identifier (S2.4).
    Unidentified,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settled {
    /// Not the attempt in flight: nothing changes.
    Ignored,
    /// Create the binding; [`Placeholder::creation_failed`] with the same
    /// attempt if it cannot be persisted.
    Create,
    Failed(PlaceholderFailure),
}

#[derive(Debug, Default)]
pub struct Placeholder {
    failure: Option<PlaceholderFailure>,
    attempt: Option<Attempt>,
    /// The attempt whose binding is being persisted (S2.5, S3.5).
    creating: Option<Attempt>,
    next: u64,
    /// A `dependency` waits to take the place of the written reason.
    leaving: bool,
    /// The written reason whose clearing is being written for it.
    clearing: Option<PlaceholderFailure>,
}

impl Placeholder {
    /// `failure` as persisted: a reason recorded is a tightening (S1.10).
    pub fn new(failure: Option<PlaceholderFailure>) -> Self {
        Self {
            failure,
            ..Self::default()
        }
    }

    /// A placeholder taking over from an earlier one of the same profile:
    /// attempts go on numbering from `next`, so an answer to an attempt of
    /// the earlier one never counts.
    pub fn following(next: u64) -> Self {
        Self {
            next,
            ..Self::default()
        }
    }

    /// The first number not handed out yet.
    pub fn next(&self) -> u64 {
        self.next
    }

    /// The identity it was trying changed, or a logout: the attempt in
    /// flight, and any binding it was creating, no longer count. With
    /// `retry`, one that was dropped is tried again by backoff, as `depend`
    /// says (S3.5, S2.5).
    pub fn cancel(&mut self, retry: bool) {
        let dropped = self.attempt.take().is_some() | self.creating.take().is_some();
        if retry && dropped {
            self.depend();
        }
    }

    /// `dependency` (S2.5). A reason that waits for a manual retry stays
    /// until its clearing is written, a loosening (S1.10): `dependency` itself
    /// is never written. See [`Placeholder::take_leaving`].
    fn depend(&mut self) {
        match self.failure {
            None | Some(PlaceholderFailure::Dependency) => {
                self.failure = Some(PlaceholderFailure::Dependency);
            }
            Some(_) => self.leaving = true,
        }
    }

    /// The written reason a `dependency` waits to take the place of, once;
    /// the caller writes its clearing and reports it with
    /// [`Placeholder::left`].
    pub fn take_leaving(&mut self) -> Option<PlaceholderFailure> {
        if !std::mem::take(&mut self.leaving) {
            return None;
        }
        self.clearing = self.failure;
        self.clearing
    }

    /// The clearing of `failure` is confirmed persisted: `dependency` takes
    /// its place, unless a reason written since did.
    pub fn left(&mut self, failure: PlaceholderFailure) {
        if self.clearing == Some(failure) && self.failure == Some(failure) {
            self.clearing = None;
            self.failure = Some(PlaceholderFailure::Dependency);
        }
    }

    pub fn failure(&self) -> Option<PlaceholderFailure> {
        self.failure
    }

    /// Starts an attempt; one started earlier no longer counts. A recorded
    /// reason stays until an attempt settles otherwise.
    pub fn attempt(&mut self) -> Attempt {
        let attempt = Attempt(self.next);
        self.next += 1;
        self.attempt = Some(attempt);
        self.creating = None;
        attempt
    }

    /// A binding created without an attempt on the placeholder, after an
    /// identity change (S3.5), numbered like one so a later attempt
    /// supersedes it.
    pub fn creating(&mut self) -> Attempt {
        let attempt = self.attempt();
        self.attempt = None;
        self.creating = Some(attempt);
        attempt
    }

    pub fn settle(&mut self, attempt: Attempt, attempted: Attempted) -> Settled {
        if self.attempt != Some(attempt) {
            return Settled::Ignored;
        }
        self.attempt = None;
        let failure = match attempted {
            Attempted::Identified => {
                self.creating = Some(attempt);
                return Settled::Create;
            }
            Attempted::Unreachable => {
                self.depend();
                return Settled::Failed(PlaceholderFailure::Dependency);
            }
            Attempted::Rejected => PlaceholderFailure::CredentialRejected,
            Attempted::Unidentified => PlaceholderFailure::InstanceUnidentified,
        };
        self.failure = Some(failure);
        self.leaving = false;
        self.clearing = None;
        Settled::Failed(failure)
    }

    /// The new binding of `attempt`, or the retirement it waits for (S3.5),
    /// could not be persisted: the binding is dropped and `dependency`
    /// recorded as `depend` says. Ignored once a later attempt started or the
    /// identity changed: a late failure never overwrites a newer reason.
    pub fn creation_failed(&mut self, attempt: Attempt) {
        if self.creating == Some(attempt) {
            self.creating = None;
            self.depend();
        }
    }

    /// Whether `attempt`'s binding may still be created: no later attempt,
    /// no cancel, no failed creation since it returned
    /// [`Settled::Create`].
    pub fn creates(&self, attempt: Attempt) -> bool {
        self.creating == Some(attempt)
    }

    /// S2.5: only `dependency` retries by backoff, and only while selected,
    /// neither switch on and no `unsaved_settings`; the other reasons wait
    /// for a manual retry, a new handshake or a new login.
    pub fn retries_by_itself(&self, switches: Switches, selected: bool, unsaved: bool) -> bool {
        let blocked = switches.off || switches.profile_disabled || unsaved;
        self.failure == Some(PlaceholderFailure::Dependency) && selected && !blocked
    }
}

#[cfg(test)]
mod tests;
