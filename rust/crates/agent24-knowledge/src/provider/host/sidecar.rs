//! Local-managed sidecar events (KB-LITE-02 3b'; CONTRACT-STATE-v1 S2.1,
//! S2.2, S2.4, S5.4): the desktop's sidecar state, its handshakes, and a
//! rotation of the instance secret, on a local-managed profile.
//!
//! The managed WeKnora Lite issues a fresh instance key on every start and
//! revokes the previous one (WeKnora#180). `starting` drops the old one, and
//! the new key is the new `loading` round's credential, answered to
//! [`Host::issue`] as `Issued::Granted`; it is
//! not a rotation. A revocation in `loading` or `unavailable` is
//! `access_revoked` (credential.rs), so treating each restart's key as one
//! would leave the binding stuck after every restart. [`Host::rotated`] is
//! for a key replaced while the same instance keeps running (suspected leak,
//! S5.4).

use std::time::Instant;

use super::{Created, Host, Output};
use crate::provider::binding::Sidecar;
use crate::provider::ledger::Scope;
use crate::provider::member::Left;
use crate::provider::placeholder::Attempt;
use crate::provider::profile::{IdentityChange, Retried};
use crate::provider::record::Ids;

impl<I: Ids> Host<I> {
    /// S2.2: `id`'s sidecar is in `sidecar`. A binding takes it as
    /// [`crate::provider::binding::BindingState::sidecar`]: `starting`
    /// retries into `loading` (in `Output::loading`), a sidecar going down
    /// makes it `unavailable(dependency)`; nothing is written. `starting`
    /// drops the binding's credential (a new process has a new key), and
    /// leaves a persisted reason that does not retry by itself in place: only
    /// a written loosening lets that go (S1.10, I1). On `starting`,
    /// a placeholder that retries by backoff makes a handshake attempt, which
    /// the caller runs and reports to [`Host::identified`] or
    /// [`Host::attempt_failed`].
    pub fn sidecar(&mut self, id: &I::Profile, sidecar: Sidecar) -> (Output<I>, Option<Attempt>) {
        let off = self.off;
        let mut out = Output::default();
        let Some(m) = self.member(id) else {
            return (out, None);
        };
        match m.sidecar(sidecar, off) {
            Some(Retried::Loading(round)) => {
                out.loading.push((id.clone(), round));
                (out, None)
            }
            Some(Retried::Attempt(attempt)) => (out, Some(attempt)),
            None => (out, None),
        }
    }

    /// S2.4: a handshake of `id`'s sidecar on a bound profile gave
    /// `created.identity`. The binding's own identity: the same instance
    /// restarted, nothing changes (its new key is the round's credential).
    /// Another: another instance, S3.5 with `created` as the new binding.
    /// A placeholder's handshake goes to [`Host::identified`] instead.
    pub fn handshake(&mut self, id: I::Profile, created: Created<I>) -> Output<I> {
        let other = self
            .member(&id)
            .and_then(|m| m.identity().cloned())
            .is_some_and(|identity| identity != created.identity);
        if !other {
            return Output::default();
        }
        self.identity_changed(id, IdentityChange::Instance, Some(created))
    }

    /// S2.1, S5.4: the instance secret of `id`'s running sidecar was rotated:
    /// a revocation signal, as from a response. What it persists (the
    /// placeholder rule, the generation it invalidated to, the first
    /// revocation) is written as a tightening.
    pub fn rotated(&mut self, id: &I::Profile, now: Instant) -> Output<I> {
        let mut out = Output::default();
        let Some(m) = self.member(id) else {
            return out;
        };
        let mut left = Left {
            changes: Vec::new(),
            generations: Vec::new(),
            aborted: Vec::new(),
        };
        if !m.revoke(now, &mut left) || left.changes.is_empty() {
            out.aborted.extend(left.aborted);
            return out;
        }
        let tightening = self.ledger.tighten(Scope::profile(id.clone()));
        self.tightened(tightening, left, &mut out);
        out
    }
}

#[cfg(test)]
mod tests;
