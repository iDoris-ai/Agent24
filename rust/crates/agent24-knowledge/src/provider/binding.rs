//! One binding as a whole (CONTRACT-STATE-v1 S3.3): its connection and
//! credential parts, whether it is activated, whether it is retired, and the
//! user and configuration events that take it out of the connection phase or
//! back in. Pure state, like its parts. Persisting what an event asks for,
//! and refusing a loosening while a tightening is not yet persisted (I1), are
//! the persistence slice's; identity changes and the placeholder come next.

use super::connection::{Connection, Round};
use super::credential::{Credentials, Effects};
use super::state::{self, ConnectionPhase, Switches, UnavailableCause};

/// An event that takes the binding out of the connection phase (S3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leave {
    /// Knowledge Off, for every binding.
    Off,
    /// The binding's profile disabled.
    Disable,
    /// The profile no longer selected.
    Unselect,
    /// Logged out (remote only).
    Logout,
    /// The profile removed: disabled first, then `retired`.
    Remove,
}

/// How the same binding is activated again (S3.3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Activation {
    /// The profile selected again.
    Reselect,
    /// The same principal logged in again.
    Relogin,
}

#[derive(Debug)]
pub struct BindingState {
    connection: Connection,
    credentials: Credentials,
    /// Selected, principal bound, no confirmation pending; Off and the
    /// disabled flag are [`Switches`], not part of this.
    activated: bool,
    retired: bool,
}

impl BindingState {
    pub fn new(connection: Connection, credentials: Credentials, activated: bool) -> Self {
        Self {
            connection,
            credentials,
            activated,
            retired: false,
        }
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    pub fn credentials(&self) -> &Credentials {
        &self.credentials
    }

    /// Both parts, for the probe and credential events.
    pub fn parts_mut(&mut self) -> (&mut Connection, &mut Credentials) {
        (&mut self.connection, &mut self.credentials)
    }

    pub fn retired(&self) -> bool {
        self.retired
    }

    /// The facts [`state::derive`] and [`state::admit`] take. Out of the
    /// connection phase it reports not activated, so it derives as
    /// `registered` or `disabled` until [`BindingState::connect`] runs.
    pub fn facts(&self) -> state::Binding {
        let phase = self.connection.phase();
        state::Binding {
            retired: self.retired,
            activated: self.activated && phase.is_some(),
            phase: phase.unwrap_or(ConnectionPhase::Loading),
            persist_pending: self.connection.persist_pending(),
            reverify_pending: false,
        }
    }

    /// S3.3: in the connection phase it invalidates; in any state the short
    /// lived credential goes, and every event but Off advances the evidence
    /// epoch (S5.6, decision 3). A retired binding takes no event.
    pub fn leave(&mut self, event: Leave) -> Effects {
        if self.retired {
            return Effects::default();
        }
        let invalidation = self
            .connection
            .phase()
            .map(|_| self.connection.invalidate());
        self.connection.leave();
        self.credentials.discard();
        match event {
            Leave::Unselect | Leave::Logout => self.activated = false,
            Leave::Remove => self.retired = true,
            Leave::Off | Leave::Disable => {}
        }
        Effects {
            invalidation,
            placeholder: event != Leave::Off,
            ..Effects::default()
        }
    }

    /// The same binding again, no increment (S3.3, S4.3); the caller has
    /// persisted the loosening first (I1). Logging in again also retries
    /// `credential_rejected` and `access_revoked` (S1.4). A retired binding
    /// still never connects.
    pub fn activate(&mut self, how: Activation) {
        self.activated = true;
        if how == Activation::Relogin {
            self.connection
                .release(UnavailableCause::CredentialRejected);
            self.connection.release(UnavailableCause::AccessRevoked);
        }
    }

    /// Enters `loading` in a new round once activated with neither switch on
    /// (S1.2); `local_storage` instead while a change is pending (S1.4).
    /// `None` when it does not enter `loading` now.
    pub fn connect(&mut self, switches: Switches) -> Option<Round> {
        let blocked = switches.off || switches.profile_disabled;
        if self.retired || !self.activated || blocked || self.connection.phase().is_some() {
            return None;
        }
        self.connection.start_loading()
    }
}

#[cfg(test)]
mod tests;
