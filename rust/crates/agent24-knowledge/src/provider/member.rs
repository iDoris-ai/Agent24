//! One profile as the host holds it (KB-C1-04 6c): the profile's state with
//! its binding's id and its disabled flag, what a user or configuration event
//! on it persists and aborts (CONTRACT-STATE-v1 S3.3, S4.5, S5.6), and its
//! binding connecting again. Pure: the caller writes what it is given and
//! reports a generation back once it is confirmed persisted.

use std::time::Instant;

use super::binding::{Leave, WithRule};
use super::connection::{Connection, Round};
use super::credential::{Credentials, Effects, Issuance, Issued, Outcome};
use super::evidence::Rule;
use super::gate::{CallId, Gate};
use super::placeholder::{Attempt, Attempted, Placeholder, Settled};
use super::profile::{IdentityChange, Profile, Slot};
use super::record::{Change, Ids, Of, StuckCause};
use super::state::{ConnectionPhase, PlaceholderFailure, Switches};

#[derive(Debug)]
pub struct Member<I: Ids> {
    id: I::Profile,
    profile: Profile<I::Kb>,
    /// The id of the profile's binding, while it has one.
    binding: Option<I::Binding>,
    disabled: bool,
    /// The principal last confirmed (S1.13).
    confirmed: Option<I::Principal>,
}

/// What events on members persist and abort; one write's worth.
#[derive(Debug, PartialEq, Eq)]
pub struct Left<I: Ids> {
    pub changes: Vec<Change<I>>,
    /// The generation values among `changes`: each takes effect once they
    /// are confirmed persisted (S4.4).
    pub generations: Vec<(Of<I>, u64)>,
    /// Abort these calls and answer them `cancelled` (S4.5). A call's id is
    /// its binding's own, so it goes with the binding.
    pub aborted: Vec<(Of<I>, CallId)>,
}

impl<I: Ids> Left<I> {
    /// Starting with the event's own change.
    pub fn new(change: Change<I>) -> Self {
        Self {
            changes: vec![change],
            generations: Vec::new(),
            aborted: Vec::new(),
        }
    }
}

impl<I: Ids> Member<I> {
    /// As restored or configured; `binding` names the bound slot's binding.
    pub fn new(
        id: I::Profile,
        profile: Profile<I::Kb>,
        binding: Option<I::Binding>,
        disabled: bool,
    ) -> Self {
        debug_assert_eq!(binding.is_some(), matches!(profile.slot(), Slot::Bound(_)));
        Self {
            id,
            profile,
            binding,
            disabled,
            confirmed: None,
        }
    }

    /// With the principal last confirmed, as persisted.
    pub fn with_confirmed(self, confirmed: Option<I::Principal>) -> Self {
        Self { confirmed, ..self }
    }

    pub fn confirmed(&self) -> Option<&I::Principal> {
        self.confirmed.as_ref()
    }

    pub fn id(&self) -> &I::Profile {
        &self.id
    }

    pub fn profile(&self) -> &Profile<I::Kb> {
        &self.profile
    }

    pub fn profile_mut(&mut self) -> &mut Profile<I::Kb> {
        &mut self.profile
    }

    pub fn disabled(&self) -> bool {
        self.disabled
    }

    pub fn set_disabled(&mut self, disabled: bool) {
        self.disabled = disabled;
    }

    /// S3.3: the event on the profile; its binding's placeholder rule and the
    /// generation it invalidated to go into `left` (S5.6, S4.5).
    pub fn leave(&mut self, event: Leave, left: &mut Left<I>) {
        let WithRule {
            value: effects,
            rule,
        } = self.profile.leave(event);
        let Some(binding) = &self.binding else {
            return;
        };
        let of = Of {
            profile: self.id.clone(),
            binding: binding.clone(),
        };
        record(of, effects, rule, left);
    }

    /// S3.5: the binding is retired for `change`, with its placeholder rule
    /// and the generation it invalidated to (S5.6, S4.5), and the attempt
    /// that creates the new binding, if one follows. `None` without a
    /// binding, as [`Profile::change_identity`].
    pub fn change_identity(
        &mut self,
        change: IdentityChange,
    ) -> Option<(Left<I>, Option<Attempt>)> {
        let retired = self.profile.change_identity(change)?;
        let binding = self.binding.take()?;
        let of = Of {
            profile: self.id.clone(),
            binding,
        };
        let mut left = Left::new(Change::Retire(of.clone()));
        record(of, retired.effects, retired.rule, &mut left);
        Some((left, retired.create))
    }

    /// A `loading` round's issuance, as [`super::binding::BindingState::issue`].
    pub fn issue(&mut self) -> Option<Issuance> {
        match self.profile.slot_mut() {
            Slot::Bound(binding) => binding.issue(),
            Slot::Placeholder(_) => None,
        }
    }

    /// S3.1: the issuer's answer to `ticket`, with what it persists into
    /// `left`: the placeholder rule and the generation it invalidated to, as
    /// [`Member::leave`]; a reason entered that never retries by itself
    /// (`Stuck`, S1.4); the first revocation-class event, which ends the
    /// exception for good (`Revoked`). True when granted for another
    /// principal: S3.5 for the caller.
    pub fn issued(
        &mut self,
        ticket: Issuance,
        answer: Issued,
        now: Instant,
        left: &mut Left<I>,
    ) -> bool {
        let Some(binding_id) = self.binding.clone() else {
            return false;
        };
        let Slot::Bound(state) = self.profile.slot_mut() else {
            return false;
        };
        let of = Of {
            profile: self.id.clone(),
            binding: binding_id,
        };
        let phase = state.connection().phase();
        let revoked = state.credentials().revoked_before();
        let WithRule {
            value: outcome,
            rule,
        } = state.issued(ticket, answer, now);
        match outcome {
            Outcome::OtherPrincipal => true,
            Outcome::Done(effects) => {
                record(of.clone(), effects, rule, left);
                let after = state.connection().phase();
                if let Some(ConnectionPhase::Unavailable(cause)) = after
                    && after != phase
                    && let Ok(stuck) = StuckCause::try_from(cause)
                {
                    left.changes.push(Change::Stuck {
                        of: of.clone(),
                        cause: stuck,
                    });
                }
                if !revoked && state.credentials().revoked_before() {
                    left.changes.push(Change::Revoked(of));
                }
                false
            }
            _ => false,
        }
    }

    /// Whether the placeholder still creates `attempt`'s binding.
    pub fn creates(&self, attempt: Attempt) -> bool {
        match self.profile.slot() {
            Slot::Placeholder(placeholder) => placeholder.creates(attempt),
            Slot::Bound(_) => false,
        }
    }

    /// Starts a login or handshake on the placeholder; `None` while bound.
    pub fn attempt(&mut self) -> Option<Attempt> {
        match self.profile.slot_mut() {
            Slot::Placeholder(placeholder) => Some(placeholder.attempt()),
            Slot::Bound(_) => None,
        }
    }

    /// How `attempt` ended, as [`super::placeholder::Placeholder::settle`].
    pub fn settle(&mut self, attempt: Attempt, attempted: Attempted) -> Settled {
        match self.profile.slot_mut() {
            Slot::Placeholder(placeholder) => placeholder.settle(attempt, attempted),
            Slot::Bound(_) => Settled::Ignored,
        }
    }

    /// `attempt`'s binding was not written, as
    /// [`super::placeholder::Placeholder::creation_failed`].
    pub fn creation_failed(&mut self, attempt: Attempt) {
        if let Slot::Placeholder(placeholder) = self.profile.slot_mut() {
            placeholder.creation_failed(attempt);
        }
    }

    pub fn placeholder_mut(&mut self) -> Option<&mut Placeholder> {
        match self.profile.slot_mut() {
            Slot::Placeholder(placeholder) => Some(placeholder),
            Slot::Bound(_) => None,
        }
    }

    /// The placeholder's recorded reason.
    pub fn failure(&self) -> Option<PlaceholderFailure> {
        match self.profile.slot() {
            Slot::Placeholder(placeholder) => placeholder.failure(),
            Slot::Bound(_) => None,
        }
    }

    /// `attempt`'s binding is persisted with its first `value`, and its
    /// principal if it needed no confirmation (S2.5, S4.4, S1.13). False once
    /// the attempt no longer creates, as [`Profile::bind`].
    pub fn bind(
        &mut self,
        attempt: Attempt,
        binding: I::Binding,
        value: u64,
        confirmed: Option<I::Principal>,
    ) -> bool {
        let connection = Connection::new(Gate::new(value), false);
        let credentials = Credentials::new(false);
        if !self.profile.bind(attempt, connection, credentials) {
            return false;
        }
        self.binding = Some(binding);
        self.confirm_principal(confirmed);
        true
    }

    /// A principal recorded as confirmed; `None` keeps the last.
    pub fn confirm_principal(&mut self, principal: Option<I::Principal>) {
        if principal.is_some() {
            self.confirmed = principal;
        }
    }

    /// The binding enters `loading` if the switches and its state let it.
    pub fn connect(&mut self, off: bool) -> Option<Round> {
        let switches = Switches {
            off,
            profile_disabled: self.disabled,
        };
        match self.profile.slot_mut() {
            Slot::Bound(binding) => binding.connect(switches),
            Slot::Placeholder(_) => None,
        }
    }

    /// The generation `value` written for `of` is confirmed persisted. Not
    /// this member's binding any more: nothing moves.
    pub fn confirm_generation(&mut self, of: &Of<I>, value: u64) -> Option<Round> {
        if of.profile != self.id || self.binding.as_ref() != Some(&of.binding) {
            return None;
        }
        match self.profile.slot_mut() {
            Slot::Bound(binding) => binding.connection_mut().confirm_generation(value),
            Slot::Placeholder(_) => None,
        }
    }
}

/// What leaving persists and aborts for `of`: its placeholder rule, the
/// calls it aborted and the generation it invalidated to.
fn record<I: Ids>(of: Of<I>, effects: Effects, rule: Option<Rule<I::Kb>>, left: &mut Left<I>) {
    if let Some(rule) = rule {
        let of = of.clone();
        left.changes.push(Change::Placed { of, rule });
    }
    if let Some(invalidation) = effects.invalidation {
        let calls = invalidation.aborted.into_iter();
        left.aborted.extend(calls.map(|call| (of.clone(), call)));
        if let Some(value) = invalidation.to_persist {
            let generation = Change::Generation {
                of: of.clone(),
                value,
            };
            left.changes.push(generation);
            left.generations.push((of, value));
        }
    }
}

#[cfg(test)]
mod tests;
