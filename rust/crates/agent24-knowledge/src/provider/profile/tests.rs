#![allow(clippy::unwrap_used)]

use std::time::Instant;

use super::*;
use crate::provider::credential::Issued;
use crate::provider::gate::Gate;
use crate::provider::state::{
    ConnectionPhase, PlaceholderFailure, Probe, ServiceState, Switches, derive,
};

const CLEAR: Switches = Switches {
    off: false,
    profile_disabled: false,
};
const READY: Probe = Probe::Answered {
    live: true,
    search_ready: true,
    read_ready: true,
};

fn parts(generation: u64) -> (Connection, Credentials) {
    (
        Connection::new(Gate::new(generation), true),
        Credentials::new(false),
    )
}

/// A selected profile whose binding is `ready` with a call in flight.
fn bound() -> Profile {
    let mut profile = Profile::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let (connection, credentials) = parts(1);
    assert!(profile.bind(connection, credentials));
    let Slot::Bound(binding) = profile.slot_mut() else {
        unreachable!()
    };
    binding.connect(CLEAR).unwrap();
    let (connection, credentials) = binding.parts_mut();
    let ticket = credentials.issue(connection).unwrap();
    credentials.issued(
        connection,
        ticket,
        Issued::Granted { narrowed: false },
        Instant::now(),
    );
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, READY);
    connection.open_call().unwrap();
    profile
}

fn state(profile: &Profile) -> Option<ServiceState> {
    derive(CLEAR, profile.target())
}

fn binding(profile: &mut Profile) -> &mut BindingState {
    match profile.slot_mut() {
        Slot::Bound(binding) => binding,
        Slot::Placeholder(_) => panic!("no binding"),
    }
}

#[test]
fn an_identity_change_retires_the_binding_and_leaves_a_placeholder() {
    for change in [
        IdentityChange::Instance,
        IdentityChange::Principal,
        IdentityChange::Endpoint,
    ] {
        let mut profile = bound();
        let retired = profile.change_identity(change).unwrap();
        // In the connection phase: invalidated, the call aborted (S4.2).
        let invalidation = retired.effects.invalidation.as_ref().unwrap();
        assert_eq!(invalidation.aborted.len(), 1, "{change:?}");
        assert!(retired.effects.placeholder);
        assert!(retired.binding.retired());
        assert_eq!(retired.create, change != IdentityChange::Endpoint);
        assert!(matches!(profile.slot(), Slot::Placeholder(_)));
        assert_eq!(state(&profile), Some(ServiceState::Registered));
        // Without a binding there is nothing to retire.
        assert!(profile.change_identity(change).is_none());
    }
}

#[test]
fn outside_the_connection_phase_an_identity_change_does_not_increment() {
    let mut profile = bound();
    let unselect = profile.leave(Leave::Unselect);
    let value = unselect.invalidation.unwrap().to_persist.unwrap();
    binding(&mut profile)
        .parts_mut()
        .0
        .confirm_generation(value);
    let retired = profile.change_identity(IdentityChange::Instance).unwrap();
    assert_eq!(retired.effects.invalidation, None);
    assert!(retired.effects.placeholder);
}

#[test]
fn a_new_principal_awaits_confirmation_before_its_binding_is_activated() {
    let mut profile = bound();
    profile.change_identity(IdentityChange::Principal).unwrap();
    assert_eq!(profile.confirmation(), Some(Confirmation::PrincipalChanged));
    let (connection, credentials) = parts(3);
    assert!(profile.bind(connection, credentials));
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
    assert_eq!(state(&profile), Some(ServiceState::Registered));
    // Logging in again does not get past the confirmation either.
    profile.activate(Activation::Relogin);
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
    profile.confirm();
    assert_eq!(profile.confirmation(), None);
    assert!(binding(&mut profile).connect(CLEAR).is_some());
    assert_eq!(
        state(&profile),
        Some(ServiceState::Connected(ConnectionPhase::Loading))
    );
}

#[test]
fn a_new_instance_is_activated_at_once_and_a_failed_creation_records_dependency() {
    let mut profile = bound();
    profile.change_identity(IdentityChange::Instance).unwrap();
    assert_eq!(profile.confirmation(), None);
    // The retirement could not be persisted: no new binding yet.
    let Slot::Placeholder(placeholder) = profile.slot_mut() else {
        unreachable!()
    };
    placeholder.creation_failed();
    assert_eq!(
        profile.target(),
        Target::Placeholder {
            failure: Some(PlaceholderFailure::Dependency),
            unsaved_settings: false,
        }
    );
    let (connection, credentials) = parts(3);
    assert!(profile.bind(connection, credentials));
    assert!(binding(&mut profile).connect(CLEAR).is_some());
    // A second binding is never bound over the first.
    let (connection, credentials) = parts(4);
    assert!(!profile.bind(connection, credentials));
}

#[test]
fn unsaved_settings_take_the_place_of_a_changed_principal_and_bind_nothing() {
    let placeholder = Slot::Placeholder(Placeholder::new(None));
    let mut profile = Profile::new(placeholder, true, Some(Confirmation::UnsavedSettings));
    profile.principal_differs();
    assert_eq!(profile.confirmation(), Some(Confirmation::UnsavedSettings));
    assert_eq!(
        profile.target(),
        Target::Placeholder {
            failure: None,
            unsaved_settings: true,
        }
    );
    // A user's confirmation of a principal does not clear it.
    profile.confirm();
    assert_eq!(profile.confirmation(), Some(Confirmation::UnsavedSettings));
    let (connection, credentials) = parts(1);
    assert!(!profile.bind(connection, credentials));
}

#[test]
fn an_unselected_profile_binds_without_activating_until_selected() {
    let mut profile = Profile::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    profile.leave(Leave::Unselect);
    let (connection, credentials) = parts(1);
    profile.bind(connection, credentials);
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
    profile.activate(Activation::Reselect);
    assert!(binding(&mut profile).connect(CLEAR).is_some());
    // Confirmed while unselected: the binding waits for the selection.
    let mut profile = bound();
    profile.change_identity(IdentityChange::Principal).unwrap();
    profile.leave(Leave::Unselect);
    let (connection, credentials) = parts(3);
    profile.bind(connection, credentials);
    profile.confirm();
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
}

#[test]
fn a_selection_on_the_placeholder_carries_over_to_the_binding() {
    let mut profile = Profile::new(Slot::Placeholder(Placeholder::new(None)), false, None);
    profile.activate(Activation::Reselect);
    let (connection, credentials) = parts(1);
    profile.bind(connection, credentials);
    assert!(binding(&mut profile).connect(CLEAR).is_some());
}

#[test]
fn logging_in_again_while_unselected_activates_nothing_but_still_lets_go() {
    use crate::provider::credential::Outcome;
    let mut profile = bound();
    // Revoked, then refused: access_revoked, held once the binding leaves.
    let (connection, credentials) = binding(&mut profile).parts_mut();
    let Outcome::Done(revoked) = credentials.revoke(connection, Instant::now()) else {
        unreachable!()
    };
    connection.confirm_generation(revoked.invalidation.unwrap().to_persist.unwrap());
    let ticket = credentials.issue(connection).unwrap();
    let refused = credentials.issued(connection, ticket, Issued::Refused, Instant::now());
    let Outcome::Done(refused) = refused else {
        unreachable!()
    };
    connection.confirm_generation(refused.invalidation.unwrap().to_persist.unwrap());
    let unselect = profile.leave(Leave::Unselect);
    let value = unselect.invalidation.unwrap().to_persist.unwrap();
    binding(&mut profile)
        .parts_mut()
        .0
        .confirm_generation(value);
    profile.activate(Activation::Relogin);
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
    assert_eq!(state(&profile), Some(ServiceState::Registered));
    // Selected again: the login let access_revoked go.
    profile.activate(Activation::Reselect);
    assert!(binding(&mut profile).connect(CLEAR).is_some());
    let phase = binding(&mut profile).connection().phase();
    assert_eq!(phase, Some(ConnectionPhase::Loading));
}

fn placeholder(profile: &mut Profile) -> &mut Placeholder {
    match profile.slot_mut() {
        Slot::Placeholder(placeholder) => placeholder,
        Slot::Bound(_) => panic!("bound"),
    }
}

#[test]
fn an_attempt_of_an_earlier_placeholder_never_counts_on_a_later_one() {
    use crate::provider::placeholder::{Attempted, Settled};
    let mut profile = Profile::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let first = placeholder(&mut profile).attempt();
    let stale = placeholder(&mut profile).attempt();
    assert_eq!(
        placeholder(&mut profile).settle(stale, Attempted::Identified),
        Settled::Create
    );
    let (connection, credentials) = parts(1);
    assert!(profile.bind(connection, credentials));
    profile.change_identity(IdentityChange::Endpoint).unwrap();
    let fresh = placeholder(&mut profile).attempt();
    assert_ne!(fresh, first);
    assert_ne!(fresh, stale);
    // Answers to the earlier placeholder's attempts arrive late.
    for late in [first, stale] {
        let settled = placeholder(&mut profile).settle(late, Attempted::Rejected);
        assert_eq!(settled, Settled::Ignored);
    }
    assert_eq!(placeholder(&mut profile).failure(), None);
    let settled = placeholder(&mut profile).settle(fresh, Attempted::Rejected);
    assert_eq!(
        settled,
        Settled::Failed(PlaceholderFailure::CredentialRejected)
    );
}

#[test]
fn an_identity_change_on_the_placeholder_drops_the_attempt_in_flight() {
    use crate::provider::placeholder::{Attempted, Settled};
    let mut profile = Profile::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let attempt = placeholder(&mut profile).attempt();
    assert!(profile.change_identity(IdentityChange::Endpoint).is_none());
    let settled = placeholder(&mut profile).settle(attempt, Attempted::Identified);
    assert_eq!(settled, Settled::Ignored);
}

#[test]
fn a_sidecar_starting_retries_a_placeholder_holding_dependency_when_it_would_back_off() {
    use crate::provider::binding::Sidecar;
    let holding = |failure, selected| {
        Profile::new(
            Slot::Placeholder(Placeholder::new(Some(failure))),
            selected,
            None,
        )
    };
    let mut profile = holding(PlaceholderFailure::Dependency, true);
    assert_eq!(profile.sidecar(Sidecar::Healthy, CLEAR), None);
    assert!(matches!(
        profile.sidecar(Sidecar::Starting, CLEAR),
        Some(Retried::Attempt(_))
    ));
    let off = Switches {
        off: true,
        profile_disabled: false,
    };
    assert_eq!(profile.sidecar(Sidecar::Starting, off), None);
    let mut profile = holding(PlaceholderFailure::Dependency, false);
    assert_eq!(profile.sidecar(Sidecar::Starting, CLEAR), None);
    let mut profile = holding(PlaceholderFailure::InstanceUnidentified, true);
    assert_eq!(profile.sidecar(Sidecar::Starting, CLEAR), None);
    // A binding takes it as its own.
    let mut profile = bound();
    assert_eq!(profile.sidecar(Sidecar::Stopped, CLEAR), None);
    assert!(matches!(
        profile.sidecar(Sidecar::Starting, CLEAR),
        Some(Retried::Loading(_))
    ));
}
