//! One profile (CONTRACT-STATE-v1 S2.5, S3.5, S1.13): its placeholder or its
//! binding, whether it is selected, the confirmation it awaits, and the
//! identity changes that retire its binding. Pure state, like the binding;
//! the caller persists the retirement and the new binding (I1, I6).

use super::binding::{Activation, BindingState, Leave};
use super::connection::Connection;
use super::credential::{Credentials, Effects};
use super::placeholder::Placeholder;
use super::state::Target;

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
pub enum Slot {
    Placeholder(Placeholder),
    Bound(Box<BindingState>),
}

/// The binding an identity change retired.
#[derive(Debug)]
pub struct Retired {
    pub binding: BindingState,
    /// Invalidated if it was in the connection phase; placeholders always.
    pub effects: Effects,
    /// Create the new binding now, persisted together with or after the
    /// retirement (S3.5).
    pub create: bool,
}

#[derive(Debug)]
pub struct Profile {
    slot: Slot,
    selected: bool,
    confirmation: Option<Confirmation>,
    /// Where the placeholder after the binding numbers its attempts from, so
    /// the earlier placeholder's never count there (S2.5).
    next_attempt: u64,
}

impl Profile {
    pub fn new(slot: Slot, selected: bool, confirmation: Option<Confirmation>) -> Self {
        Self {
            slot,
            selected,
            confirmation,
            next_attempt: 0,
        }
    }

    pub fn slot(&self) -> &Slot {
        &self.slot
    }

    pub fn slot_mut(&mut self) -> &mut Slot {
        &mut self.slot
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
    /// unselecting is remembered either way.
    pub fn leave(&mut self, event: Leave) -> Effects {
        if event == Leave::Unselect {
            self.selected = false;
        }
        match &mut self.slot {
            Slot::Bound(binding) => binding.leave(event),
            Slot::Placeholder(_) => Effects::default(),
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
    /// `None` without a binding; a placeholder's attempt in flight no longer
    /// counts.
    pub fn change_identity(&mut self, change: IdentityChange) -> Option<Retired> {
        let fresh = Slot::Placeholder(Placeholder::following(self.next_attempt));
        let mut binding = match std::mem::replace(&mut self.slot, fresh) {
            Slot::Bound(binding) => binding,
            Slot::Placeholder(mut placeholder) => {
                placeholder.cancel();
                self.slot = Slot::Placeholder(placeholder);
                return None;
            }
        };
        let effects = binding.leave(Leave::Retire);
        if change == IdentityChange::Principal {
            self.principal_differs();
        }
        Some(Retired {
            binding: *binding,
            effects,
            create: change != IdentityChange::Endpoint,
        })
    }

    /// The new binding (and any retirement before it) is persisted. It is
    /// activated when selected with nothing awaiting confirmation. Refused
    /// under `UnsavedSettings`, which only the host's confirmation clears.
    pub fn bind(&mut self, connection: Connection, credentials: Credentials) -> bool {
        let unsaved = self.confirmation == Some(Confirmation::UnsavedSettings);
        let Slot::Placeholder(placeholder) = &self.slot else {
            return false;
        };
        if unsaved {
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
