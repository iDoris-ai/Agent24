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
use crate::provider::record::Ids;

impl<I: Ids> Host<I> {
    /// The issuance `id`'s binding's `loading` round awaits, one at a time;
    /// `None` without one due.
    pub fn issue(&mut self, id: &I::Profile) -> Option<Issuance> {
        self.member(id)?.issue()
    }

    /// S3.1: the issuer's answer to `ticket`. The binding moves at once; the
    /// placeholder rule, the generation it invalidated to, a reason that
    /// never retries by itself and the first revocation are written in one
    /// record, a tightening (S1.10). Granted for another principal, the
    /// binding is retired as S3.5 says, and the placeholder logs in again by
    /// backoff to find it.
    pub fn issued(
        &mut self,
        id: I::Profile,
        ticket: Issuance,
        answer: Issued,
        now: Instant,
    ) -> Output<I> {
        let mut out = Output::default();
        let Some(m) = self.member(&id) else {
            return out;
        };
        let mut left = Left {
            changes: Vec::new(),
            generations: Vec::new(),
            aborted: Vec::new(),
        };
        if m.issued(ticket, answer, now, &mut left) {
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
