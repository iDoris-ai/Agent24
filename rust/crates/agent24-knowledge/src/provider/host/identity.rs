//! Identity changes (KB-C1-04 6c-2b; CONTRACT-STATE-v1 S3.5, S2.4): another
//! instance, principal, endpoint or tenant retires the binding, a tightening
//! that takes effect at once. The new binding, for another instance or
//! principal, is written once the retirement is confirmed persisted (I1);
//! unconfirmed, it is dropped and the placeholder retries by backoff with
//! `dependency` (S2.5).

use super::{Created, Host, Output};
use crate::provider::journal::Written;
use crate::provider::ledger::{Scope, Ticket};
use crate::provider::placeholder::Attempt;
use crate::provider::profile::{Confirmation, IdentityChange};
use crate::provider::record::{Change, Ids};

/// A new binding waiting for its profile's retirement, `ticket`.
#[derive(Debug)]
pub(super) struct Deferred<I: Ids> {
    ticket: Ticket,
    id: I::Profile,
    attempt: Attempt,
    created: Created<I>,
}

impl<I: Ids> Host<I> {
    /// S3.5: `change` retires `id`'s binding: invalidated in the connection
    /// phase, its evidence placeholders in any state (S5.6). Another
    /// principal also awaits the user's confirmation (S1.13), and its new
    /// binding waits unconfirmed. `created` is the new binding for another
    /// instance or principal; without it the placeholder retries with
    /// `dependency`. Without a binding, an attempt in flight no longer
    /// counts and nothing is written.
    pub fn identity_changed(
        &mut self,
        id: I::Profile,
        change: IdentityChange,
        created: Option<Created<I>>,
    ) -> Output<I> {
        let mut out = Output::default();
        let Some(m) = self.member(&id) else {
            return out;
        };
        let awaited_before = m.profile().confirmation();
        let Some((mut left, create)) = m.change_identity(change) else {
            return out;
        };
        if awaited_before.is_none()
            && m.profile().confirmation() == Some(Confirmation::PrincipalChanged)
        {
            left.changes.push(Change::Await {
                profile: id.clone(),
                awaited: Confirmation::PrincipalChanged,
            });
        }
        let tightening = self.ledger.tighten(Scope::profile(id.clone()));
        let ticket = tightening.ticket;
        self.tightened(tightening, left, &mut out);
        match (create, created) {
            (Some(attempt), Some(created)) => self.deferred.push(Deferred {
                ticket,
                id,
                attempt,
                created,
            }),
            (Some(attempt), None) => {
                if let Some(m) = self.member(&id) {
                    m.creation_failed(attempt);
                }
            }
            (None, _) => {}
        }
        out
    }

    /// The retirement `ticket` was written. Confirmed, its new binding is
    /// written as a login's would be, unless the attempt no longer creates or
    /// `unsaved_settings` awaits; failed, the new binding is dropped (S3.5).
    pub(super) fn retired(&mut self, ticket: Ticket, written: Written) {
        let Some(i) = self.deferred.iter().position(|d| d.ticket == ticket) else {
            return;
        };
        let d = self.deferred.remove(i);
        let Some(m) = self.member(&d.id) else {
            return;
        };
        let unsaved = m.profile().confirmation() == Some(Confirmation::UnsavedSettings);
        if written == Written::Failed || unsaved || !m.creates(d.attempt) {
            m.creation_failed(d.attempt);
            return;
        }
        self.write_creation(d.id, d.attempt, d.created);
    }
}

#[cfg(test)]
mod tests;
