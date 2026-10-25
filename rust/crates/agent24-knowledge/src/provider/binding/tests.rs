#![allow(clippy::unwrap_used)]

use std::time::Instant;

use super::*;
use crate::provider::credential::{Issued, Outcome, Signal};
use crate::provider::gate::Gate;
use crate::provider::state::{Probe, ServiceState, Target, derive};
use ConnectionPhase::{Loading, Ready, Unavailable};

const READY: Probe = Probe::Answered {
    live: true,
    search_ready: true,
    read_ready: true,
};
const CLEAR: Switches = Switches {
    off: false,
    profile_disabled: false,
};
const OFF: Switches = Switches {
    off: true,
    profile_disabled: false,
};

/// An activated binding, servable before, `ready` with a credential and a
/// call in flight.
fn ready() -> BindingState {
    let mut binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    binding.connect(CLEAR).unwrap();
    let (connection, credentials) = binding.parts_mut();
    let ticket = credentials.issue(connection).unwrap();
    let granted = Issued::Granted { narrowed: false };
    credentials.issued(connection, ticket, granted, Instant::now());
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, READY);
    connection.open_call().unwrap();
    binding
}

fn state(binding: &BindingState, switches: Switches) -> Option<ServiceState> {
    derive(switches, Target::Binding(binding.facts()))
}

/// Confirms the generation the event asked for.
fn persisted(binding: &mut BindingState, effects: &Effects) {
    let value = effects.invalidation.as_ref().unwrap().to_persist.unwrap();
    binding.parts_mut().0.confirm_generation(value);
}

#[test]
fn every_event_in_the_connection_phase_invalidates_and_drops_the_credential() {
    for event in [
        Leave::Off,
        Leave::Disable,
        Leave::Unselect,
        Leave::Logout,
        Leave::Remove,
    ] {
        let mut binding = ready();
        let effects = binding.leave(event);
        let invalidation = effects.invalidation.as_ref().unwrap();
        assert_eq!(invalidation.aborted.len(), 1, "{event:?}");
        assert_eq!(binding.connection().gate().reported(), 2);
        assert_eq!(binding.connection().phase(), None);
        assert_eq!(binding.credentials().current(), None);
        // Only Off leaves the evidence alone (S5.6).
        assert_eq!(effects.placeholder, event != Leave::Off, "{event:?}");
    }
}

#[test]
fn outside_the_connection_phase_an_event_only_changes_flags() {
    let mut binding = ready();
    let off = binding.leave(Leave::Off);
    persisted(&mut binding, &off);
    for event in [Leave::Disable, Leave::Unselect, Leave::Logout] {
        let effects = binding.leave(event);
        assert_eq!(effects.invalidation, None, "{event:?}");
        assert!(effects.placeholder);
    }
    assert_eq!(binding.connection().gate().reported(), 2);
}

#[test]
fn off_then_on_resumes_the_same_binding_once_the_generation_is_persisted() {
    let mut binding = ready();
    let effects = binding.leave(Leave::Off);
    assert_eq!(state(&binding, OFF), Some(ServiceState::Disabled));
    // Still Off: nothing to enter.
    assert_eq!(binding.connect(OFF), None);
    assert_eq!(binding.connection().phase(), None);
    // On before the increment is persisted: local_storage, then loading.
    assert_eq!(binding.connect(CLEAR), None);
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(state::UnavailableCause::LocalStorage))
    );
    assert!(binding.facts().persist_pending);
    persisted(&mut binding, &effects);
    assert!(!binding.facts().persist_pending);
    assert_eq!(binding.connection().phase(), Some(Loading));
    assert_eq!(
        state(&binding, CLEAR),
        Some(ServiceState::Connected(Loading))
    );
    // Already in the connection phase: no second round.
    assert_eq!(binding.connect(CLEAR), None);
}

#[test]
fn an_unselected_or_logged_out_binding_stays_registered_until_activated_again() {
    for event in [Leave::Unselect, Leave::Logout] {
        let mut binding = ready();
        let effects = binding.leave(event);
        persisted(&mut binding, &effects);
        assert_eq!(binding.connect(CLEAR), None, "{event:?}");
        assert_eq!(state(&binding, CLEAR), Some(ServiceState::Registered));
        binding.activate();
        assert!(binding.connect(CLEAR).is_some());
        assert_eq!(
            state(&binding, CLEAR),
            Some(ServiceState::Connected(Loading))
        );
        // Same binding: the generation moved only for the event (S4.3).
        assert_eq!(binding.connection().gate().reported(), 2);
    }
}

#[test]
fn a_removed_binding_is_retired_for_good() {
    let mut binding = ready();
    let effects = binding.leave(Leave::Remove);
    assert!(binding.retired());
    persisted(&mut binding, &effects);
    binding.activate();
    assert_eq!(binding.connect(CLEAR), None);
    assert_eq!(state(&binding, CLEAR), None);
    assert_eq!(binding.leave(Leave::Disable), Effects::default());
}

#[test]
fn out_of_the_connection_phase_it_derives_as_registered_or_disabled() {
    let mut binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    assert_eq!(state(&binding, CLEAR), Some(ServiceState::Registered));
    assert_eq!(state(&binding, OFF), Some(ServiceState::Disabled));
    binding.connect(CLEAR).unwrap();
    let (connection, _) = binding.parts_mut();
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, READY);
    assert_eq!(state(&binding, CLEAR), Some(ServiceState::Connected(Ready)));
}

#[test]
fn a_disabled_profile_keeps_the_binding_out_until_enabled() {
    let disabled = Switches {
        off: false,
        profile_disabled: true,
    };
    let mut binding = ready();
    let old = binding.credentials().current().unwrap();
    let effects = binding.leave(Leave::Disable);
    persisted(&mut binding, &effects);
    assert_eq!(binding.connect(disabled), None);
    assert!(binding.connect(CLEAR).is_some());
    // A late revocation on the credential from before is a discarded one's.
    let (connection, credentials) = binding.parts_mut();
    let outcome = credentials.response(connection, old, Signal::Revoked, Instant::now());
    assert_eq!(outcome, Outcome::Recheck);
    assert_eq!(binding.connection().phase(), Some(Loading));
}
