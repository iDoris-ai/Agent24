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
    /// Create the binding; [`Placeholder::creation_failed`] if it cannot be
    /// persisted.
    Create,
    Failed(PlaceholderFailure),
}

#[derive(Debug, Default)]
pub struct Placeholder {
    failure: Option<PlaceholderFailure>,
    attempt: Option<Attempt>,
    next: u64,
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

    /// The identity it was trying changed: the attempt in flight no longer
    /// counts.
    pub fn cancel(&mut self) {
        self.attempt = None;
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
        attempt
    }

    pub fn settle(&mut self, attempt: Attempt, attempted: Attempted) -> Settled {
        if self.attempt != Some(attempt) {
            return Settled::Ignored;
        }
        self.attempt = None;
        let failure = match attempted {
            Attempted::Identified => return Settled::Create,
            Attempted::Unreachable => PlaceholderFailure::Dependency,
            Attempted::Rejected => PlaceholderFailure::CredentialRejected,
            Attempted::Unidentified => PlaceholderFailure::InstanceUnidentified,
        };
        self.failure = Some(failure);
        Settled::Failed(failure)
    }

    /// The new binding, or the retirement it waits for (S3.5), could not be
    /// persisted: the binding is dropped and `dependency` is recorded.
    pub fn creation_failed(&mut self) {
        self.failure = Some(PlaceholderFailure::Dependency);
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
