//! Issuances (KB-C1-04 6c-2c; CONTRACT-STATE-v1 S3.1, S1.10): a `loading`
//! round's issuance goes to the caller, which sends it (design F4); the
//! issuer's answer moves the binding at once, and what of it persists is
//! written as a tightening. Another principal is S3.5's identity change.

use std::time::Instant;

use super::{Host, Output};
use crate::provider::credential::{Issuance, Issued};
use crate::provider::ledger::Scope;
use crate::provider::member::Left;
use crate::provider::profile::IdentityChange;
use crate::provider::record::{Ids, Of};

impl<I: Ids> Host<I> {
    /// The issuance `id`'s binding's `loading` round awaits, one at a time,
    /// with the binding it is for; `None` without one due.
    pub fn issue(&mut self, id: &I::Profile) -> Option<(Of<I>, Issuance)> {
        self.member(id)?.issue()
    }

    /// S3.1: the issuer's answer to `ticket` for `of`, counted only while
    /// `of` is still its profile's binding (S3.1, credential ownership: a
    /// new binding numbers its rounds afresh). The binding moves at once; the
    /// placeholder rule, the generation it invalidated to, a reason that
    /// never retries by itself and the first revocation are written in one
    /// record, a tightening (S1.10). Granted for another principal, the
    /// binding is retired as S3.5 says, and the placeholder logs in again by
    /// backoff to find it.
    pub fn issued(
        &mut self,
        of: Of<I>,
        ticket: Issuance,
        answer: Issued,
        now: Instant,
    ) -> Output<I> {
        let mut out = Output::default();
        let id = of.profile.clone();
        let Some(m) = self.member(&id) else {
            return out;
        };
        let mut left = Left {
            changes: Vec::new(),
            generations: Vec::new(),
            aborted: Vec::new(),
        };
        if m.issued(of, ticket, answer, now, &mut left) {
            return self.identity_changed(id, IdentityChange::Principal, None);
        }
        if left.changes.is_empty() {
            out.aborted.extend(left.aborted);
            return out;
        }
        let tightening = self.ledger.tighten(Scope::profile(id));
        self.tightened(tightening, left, &mut out);
        out
    }
}

#[cfg(test)]
mod tests;
