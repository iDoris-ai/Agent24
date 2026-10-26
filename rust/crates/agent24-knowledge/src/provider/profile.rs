//! One profile (CONTRACT-STATE-v1 S2.5, S3.5, S1.13): its placeholder or its
//! binding, whether it is selected, the confirmation it awaits, and the
//! identity changes that retire its binding. Pure state, like the binding;
//! the caller persists the retirement and the new binding (I1, I6).

use super::binding::{Activation, BindingState, Leave, Sidecar, WithRule};
use super::connection::{Connection, Round};
use super::credential::{Credentials, Effects};
use super::evidence::Rule;
use super::placeholder::{Attempt, Placeholder};
use super::state::{Switches, Target};

/// What the profile awaits before its binding is activated (S1.13).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmation {
    PrincipalChanged,
    /// Set by a global tightening; it takes the place of `PrincipalChanged`.
    UnsavedSettings,
}

/// A change to the binding's identity (S2.4, S3.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentityChange {
    /// A local-managed handshake gave another instance identifier: a new
    /// binding follows.
    Instance,
    /// A renewal or a login gave another principal: a new binding follows,
    /// awaiting the user's confirmation.
    Principal,
    /// The remote endpoint or tenant changed: the placeholder, until logged
    /// in again.
    Endpoint,
}

#[derive(Debug)]
pub enum Slot<K> {
    Placeholder(Placeholder),
    Bound(Box<BindingState<K>>),
}

/// The binding an identity change retired.
#[derive(Debug)]
pub struct Retired<K> {
    /// Its evidence shows as placeholders from now on (S5.6).
    pub binding: BindingState<K>,
    /// Invalidated if it was in the connection phase; placeholders always.
    pub effects: Effects,
    /// The placeholder rule, persisted with the retirement.
    pub rule: Option<Rule<K>>,
    /// Create the new binding now, persisted together with or after the
    /// retirement (S3.5); if that fails, [`Placeholder::creation_failed`]
    /// with this attempt.
    pub create: Option<Attempt>,
}

/// What a sidecar `starting` retried (S2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Retried {
    /// The binding entered `loading`.
    Loading(Round),
    /// The placeholder makes a handshake attempt.
    Attempt(Attempt),
}

#[derive(Debug)]
pub struct Profile<K> {
    slot: Slot<K>,
    selected: bool,
    confirmation: Option<Confirmation>,
    /// Where the placeholder after the binding numbers its attempts from, so
    /// the earlier placeholder's never count there (S2.5).
    next_attempt: u64,
}

impl<K: Ord + Clone> Profile<K> {
    pub fn new(slot: Slot<K>, selected: bool, confirmation: Option<Confirmation>) -> Self {
        Self {
            slot,
            selected,
            confirmation,
            next_attempt: 0,
        }
    }

    pub fn slot(&self) -> &Slot<K> {
        &self.slot
    }

    pub fn slot_mut(&mut self) -> &mut Slot<K> {
        &mut self.slot
    }

    pub fn selected(&self) -> bool {
        self.selected
    }

    pub fn confirmation(&self) -> Option<Confirmation> {
        self.confirmation
    }

    /// What [`super::state::derive`] and [`super::state::admit`] take.
    pub fn target(&self) -> Target {
        match &self.slot {
            Slot::Placeholder(placeholder) => Target::Placeholder {
                failure: placeholder.failure(),
                unsaved_settings: self.confirmation == Some(Confirmation::UnsavedSettings),
            },
            Slot::Bound(binding) => Target::Binding(binding.facts()),
        }
    }

    /// S3.3 on the profile: a binding takes it as in [`BindingState::leave`];
    /// unselecting is remembered either way. On the placeholder, logging out
    /// ends the login in flight and any binding it was creating.
    pub fn leave(&mut self, event: Leave) -> WithRule<Effects, K> {
        if event == Leave::Unselect {
            self.selected = false;
        }
        match &mut self.slot {
            Slot::Bound(binding) => binding.leave(event),
            Slot::Placeholder(placeholder) => {
                if event == Leave::Logout {
                    placeholder.cancel(false);
                }
                WithRule {
                    value: Effects::default(),
                    rule: None,
                }
            }
        }
    }

    /// Selected again or logged in again; the binding is activated only when
    /// selected with nothing awaiting confirmation (S1.2, S1.13).
    pub fn activate(&mut self, how: Activation) {
        if how == Activation::Reselect {
            self.selected = true;
        }
        let ready = self.selected && self.confirmation.is_none();
        match &mut self.slot {
            Slot::Bound(binding) if ready => binding.activate(how),
            Slot::Bound(binding) if how == Activation::Relogin => binding.relogin(),
            Slot::Bound(_) | Slot::Placeholder(_) => {}
        }
    }

    /// A login on the placeholder gave a principal other than the last
    /// confirmed one (S1.13); `UnsavedSettings` stays in its place.
    pub fn principal_differs(&mut self) {
        if self.confirmation.is_none() {
            self.confirmation = Some(Confirmation::PrincipalChanged);
        }
    }

    /// S3.5: the binding is retired (invalidated if in the connection phase,
    /// placeholders in any state) and the profile is a placeholder again.
    /// `None` without a binding: the placeholder's attempt in flight, or the
    /// binding it was creating, is dropped and, but for another endpoint,
    /// retried by backoff (S3.5).
    pub fn change_identity(&mut self, change: IdentityChange) -> Option<Retired<K>> {
        match &mut self.slot {
            Slot::Placeholder(placeholder) => {
                placeholder.cancel(change != IdentityChange::Endpoint);
                return None;
            }
            // Removed: it takes no event, and nothing follows it.
            Slot::Bound(binding) if binding.retired() => return None,
            Slot::Bound(_) => {}
        }
        let mut fresh = Placeholder::following(self.next_attempt);
        let create = (change != IdentityChange::Endpoint).then(|| fresh.creating());
        let Slot::Bound(mut binding) = std::mem::replace(&mut self.slot, Slot::Placeholder(fresh))
        else {
            return None;
        };
        let WithRule {
            value: effects,
            rule,
        } = binding.leave(Leave::Retire);
        if change == IdentityChange::Principal {
            self.principal_differs();
        }
        Some(Retired {
            binding: *binding,
            effects,
            rule,
            create,
        })
    }

    /// The new binding of `attempt` (and any retirement before it) is
    /// persisted. It is activated when selected with nothing awaiting
    /// confirmation. Refused under `UnsavedSettings`, which only the host's
    /// confirmation clears, and once `attempt` no longer creates: a later
    /// attempt, a logout or an identity change came first.
    pub fn bind(
        &mut self,
        attempt: Attempt,
        connection: Connection,
        credentials: Credentials,
    ) -> bool {
        let unsaved = self.confirmation == Some(Confirmation::UnsavedSettings);
        let Slot::Placeholder(placeholder) = &self.slot else {
            return false;
        };
        if unsaved || !placeholder.creates(attempt) {
            return false;
        }
        self.next_attempt = placeholder.next();
        let activated = self.selected && self.confirmation.is_none();
        self.slot = Slot::Bound(Box::new(BindingState::new(
            connection,
            credentials,
            activated,
        )));
        true
    }

    /// S2.2 for a local-managed profile: the binding takes the sidecar's
    /// state as in [`BindingState::sidecar`]; on `starting` a placeholder
    /// holding `dependency` tries again when it would retry by backoff (S2.5).
    pub fn sidecar(&mut self, sidecar: Sidecar, switches: Switches) -> Option<Retried> {
        let unsaved = self.confirmation == Some(Confirmation::UnsavedSettings);
        match &mut self.slot {
            Slot::Bound(binding) => binding.sidecar(sidecar).map(Retried::Loading),
            Slot::Placeholder(placeholder)
                if sidecar == Sidecar::Starting
                    && placeholder.retries_by_itself(switches, self.selected, unsaved) =>
            {
                Some(Retried::Attempt(placeholder.attempt()))
            }
            Slot::Placeholder(_) => None,
        }
    }

    /// The user's confirmation of the new principal is persisted.
    pub fn confirm(&mut self) {
        if self.confirmation != Some(Confirmation::PrincipalChanged) {
            return;
        }
        self.confirmation = None;
        if let (Slot::Bound(binding), true) = (&mut self.slot, self.selected) {
            binding.activate(Activation::Reselect);
        }
    }
}

#[cfg(test)]
mod tests;
