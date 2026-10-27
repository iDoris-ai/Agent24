//! New bindings (KB-C1-04 6c-2a; CONTRACT-STATE-v1 S2.5, S4.4, S1.13): a
//! login or handshake on a placeholder that comes back with an identity is
//! written as a binding with its first generation value, a loosening, and
//! binds once that is confirmed persisted (I1). The caller mints a fresh
//! id for the binding (design F5).
//!
//! A failed attempt's reason is persisted, a tightening, except
//! `dependency`: it retries by backoff whatever is persisted, so it stays in
//! memory (best effort, S1.10).

use super::{Host, Output, Then};
use crate::provider::ledger::{Confirmed, Loosening, Scope};
use crate::provider::member::Left;
use crate::provider::placeholder::{Attempt, Attempted, Settled};
use crate::provider::profile::Confirmation;
use crate::provider::record::{Change, Ids, Of};
use crate::provider::state::PlaceholderFailure;

/// What a login or handshake that came back with an identity creates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Created<I: Ids> {
    /// Fresh, minted by the caller.
    pub binding: I::Binding,
    pub identity: I::Identity,
    pub principal: I::Principal,
}

/// A new binding written and not yet confirmed.
#[derive(Debug)]
pub(super) struct Creation<I: Ids> {
    ticket: Loosening,
    attempt: Attempt,
    of: Of<I>,
    value: u64,
    confirmed: Option<I::Principal>,
    /// The placeholder's reason the write clears.
    cleared: Option<PlaceholderFailure>,
}

impl<I: Ids> Host<I> {
    /// Starts a login or handshake on `id`'s placeholder (S2.5); an earlier
    /// one no longer counts. `None` while the profile is bound, or unknown.
    pub fn attempt(&mut self, id: &I::Profile) -> Option<Attempt> {
        self.member(id)?.attempt()
    }

    /// `attempt` came back with an identity. A principal other than the one
    /// last confirmed awaits the user's confirmation, a tightening (S1.13).
    /// The binding is written with a first value past any used (S4.4); while
    /// a tightening of the profile is unconfirmed it is refused (I1), and
    /// the creation fails with `dependency`. Under `unsaved_settings` nothing
    /// binds until the host's confirmation, so nothing is written.
    pub fn identified(
        &mut self,
        id: I::Profile,
        attempt: Attempt,
        created: Created<I>,
    ) -> Output<I> {
        let mut out = Output::default();
        let Some(m) = self.member(&id) else {
            return out;
        };
        if m.settle(attempt, Attempted::Identified) != Settled::Create {
            return out;
        }
        let awaited = m.profile().confirmation();
        if awaited == Some(Confirmation::UnsavedSettings) {
            m.creation_failed(attempt);
            return out;
        }
        if awaited.is_none() && m.confirmed().is_some_and(|c| *c != created.principal) {
            m.profile_mut().principal_differs();
            let change = Change::Await {
                profile: id.clone(),
                awaited: Confirmation::PrincipalChanged,
            };
            let tightening = self.ledger.tighten(Scope::profile(id.clone()));
            self.tightened(tightening, Left::new(change), &mut out);
        }
        self.write_creation(id, attempt, created);
        out
    }

    pub(super) fn write_creation(&mut self, id: I::Profile, attempt: Attempt, created: Created<I>) {
        let loosened = self.highest.checked_add(1).and_then(|value| {
            let scope = Scope::profile(id.clone());
            Some((value, self.ledger.loosen(scope, None, self.off).ok()?))
        });
        let Some(m) = self.member(&id) else {
            return;
        };
        let Some((value, ticket)) = loosened else {
            m.creation_failed(attempt);
            return;
        };
        let awaited = m.profile().confirmation();
        let confirmed = awaited.is_none().then_some(created.principal);
        let of = Of {
            profile: id.clone(),
            binding: created.binding,
        };
        let mut changes = vec![Change::Bind {
            of: of.clone(),
            identity: created.identity,
            generation: value,
            confirmed: confirmed.clone(),
        }];
        // A reason persisted for the placeholder no longer holds.
        let cleared = m.failure().filter(|f| *f != PlaceholderFailure::Dependency);
        if let Some(failure) = cleared {
            changes.push(Change::PlaceholderCleared {
                profile: id,
                failure,
            });
        }
        self.highest = value;
        self.creations.push(Creation {
            ticket,
            attempt,
            of,
            value,
            confirmed,
            cleared,
        });
        self.journal.push(changes, Then::Create { ticket });
    }

    /// `attempt` failed: `attempted` is not `Identified`. The reason is
    /// recorded; any but `dependency` is written (S2.5, S1.10).
    pub fn attempt_failed(
        &mut self,
        id: I::Profile,
        attempt: Attempt,
        attempted: Attempted,
    ) -> Output<I> {
        debug_assert_ne!(attempted, Attempted::Identified);
        let mut out = Output::default();
        if let Some(m) = self.member(&id)
            && let Settled::Failed(failure) = m.settle(attempt, attempted)
            && failure != PlaceholderFailure::Dependency
        {
            let change = Change::PlaceholderFailed {
                profile: id.clone(),
                failure,
            };
            let tightening = self.ledger.tighten(Scope::profile(id));
            self.tightened(tightening, Left::new(change), &mut out);
        }
        out
    }

    /// A new binding's write is confirmed: it binds and connects, or, its
    /// attempt superseded or its profile removed, it is retired (A.5). The
    /// rest stands as a restart reads it: its principal is confirmed (S1.13),
    /// and the reason it cleared is written back unless a newer one replaced
    /// it or a later creation, written after it, clears it again.
    pub(super) fn created(&mut self, ticket: Loosening, out: &mut Output<I>) {
        let Some(i) = self.creations.iter().position(|c| c.ticket == ticket) else {
            return;
        };
        let c = self.creations.remove(i);
        if self.ledger.loosening_durable(ticket) != Confirmed::Applied {
            return;
        }
        let off = self.off;
        let binding = c.of.binding.clone();
        if let Some(m) = self.member(&c.of.profile)
            && m.bind(c.attempt, binding, c.value, c.confirmed.clone())
        {
            if let Some(round) = m.connect(off) {
                out.loading.push((c.of.profile, round));
            }
            return;
        }
        let profile = c.of.profile.clone();
        // Confirmed in the order written: any left are later.
        let later = self.creations.iter().any(|l| l.of.profile == profile);
        let mut left = Left::new(Change::Retire(c.of));
        if let Some(m) = self.member(&profile) {
            m.confirm_principal(c.confirmed);
            let held = |f: &PlaceholderFailure| !later && m.failure() == Some(*f);
            if let Some(failure) = c.cleared.filter(held) {
                let profile = profile.clone();
                left.changes
                    .push(Change::PlaceholderFailed { profile, failure });
            }
        }
        let tightening = self.ledger.tighten(Scope::profile(profile));
        self.tightened(tightening, left, out);
    }
}

#[cfg(test)]
mod tests;
