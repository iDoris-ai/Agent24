#![allow(clippy::unwrap_used)]

use std::time::Instant;

use super::*;
use crate::provider::credential::Issued;
use crate::provider::evidence::{Kbs, Rule};
use crate::provider::gate::Gate;
use crate::provider::placeholder::{Attempted, Settled};
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

type P = Profile<&'static str>;

fn parts(generation: u64) -> (Connection, Credentials) {
    (
        Connection::new(Gate::new(generation), true),
        Credentials::new(false),
    )
}

/// A login on the placeholder that came back with an identity.
fn login(profile: &mut P) -> Attempt {
    let placeholder = placeholder(profile);
    let attempt = placeholder.attempt();
    assert_eq!(
        placeholder.settle(attempt, Attempted::Identified),
        Settled::Create
    );
    attempt
}

/// A selected profile whose binding is `ready` with a call in flight.
fn bound() -> P {
    let mut profile = P::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let (connection, credentials) = parts(1);
    let attempt = login(&mut profile);
    assert!(profile.bind(attempt, connection, credentials));
    let Slot::Bound(binding) = profile.slot_mut() else {
        unreachable!()
    };
    binding.connect(CLEAR).unwrap();
    let ticket = binding.issue().unwrap();
    let _ = binding.issued(ticket, Issued::Granted { narrowed: false }, Instant::now());
    let connection = binding.connection_mut();
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, READY);
    connection.open_call().unwrap();
    profile
}

fn state(profile: &P) -> Option<ServiceState> {
    derive(CLEAR, profile.target())
}

fn binding(profile: &mut P) -> &mut BindingState<&'static str> {
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
        let rule = Rule {
            epoch: 1,
            kbs: Kbs::All,
        };
        assert_eq!(retired.rule, Some(rule));
        assert_eq!(retired.binding.evidence().epoch(), 1);
        assert!(retired.binding.retired());
        assert_eq!(retired.create.is_some(), change != IdentityChange::Endpoint);
        assert!(matches!(profile.slot(), Slot::Placeholder(_)));
        assert_eq!(state(&profile), Some(ServiceState::Registered));
        // Without a binding there is nothing to retire.
        assert!(profile.change_identity(change).is_none());
    }
}

#[test]
fn outside_the_connection_phase_an_identity_change_does_not_increment() {
    let mut profile = bound();
    let unselect = profile.leave(Leave::Unselect).value;
    let value = unselect.invalidation.unwrap().to_persist.unwrap();
    binding(&mut profile)
        .connection_mut()
        .confirm_generation(value);
    let retired = profile.change_identity(IdentityChange::Instance).unwrap();
    assert_eq!(retired.effects.invalidation, None);
    assert!(retired.effects.placeholder);
}

#[test]
fn a_new_principal_awaits_confirmation_before_its_binding_is_activated() {
    let mut profile = bound();
    let retired = profile.change_identity(IdentityChange::Principal).unwrap();
    assert_eq!(profile.confirmation(), Some(Confirmation::PrincipalChanged));
    let (connection, credentials) = parts(3);
    assert!(profile.bind(retired.create.unwrap(), connection, credentials));
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
    let retired = profile.change_identity(IdentityChange::Instance).unwrap();
    assert_eq!(profile.confirmation(), None);
    // The retirement could not be persisted: no new binding yet.
    let Slot::Placeholder(placeholder) = profile.slot_mut() else {
        unreachable!()
    };
    placeholder.creation_failed(retired.create.unwrap());
    assert_eq!(
        profile.target(),
        Target::Placeholder {
            failure: Some(PlaceholderFailure::Dependency),
            unsaved_settings: false,
        }
    );
    // Nor can that creation still bind: a new handshake creates instead.
    let (connection, credentials) = parts(3);
    assert!(!profile.bind(retired.create.unwrap(), connection, credentials));
    let attempt = login(&mut profile);
    let (connection, credentials) = parts(3);
    assert!(profile.bind(attempt, connection, credentials));
    assert!(binding(&mut profile).connect(CLEAR).is_some());
    // A second binding is never bound over the first.
    let (connection, credentials) = parts(4);
    assert!(!profile.bind(attempt, connection, credentials));
}

#[test]
fn unsaved_settings_take_the_place_of_a_changed_principal_and_bind_nothing() {
    let placeholder = Slot::Placeholder(Placeholder::new(None));
    let mut profile = P::new(placeholder, true, Some(Confirmation::UnsavedSettings));
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
    let attempt = login(&mut profile);
    let (connection, credentials) = parts(1);
    assert!(!profile.bind(attempt, connection, credentials));
}

#[test]
fn an_unselected_profile_binds_without_activating_until_selected() {
    let mut profile = P::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let _ = profile.leave(Leave::Unselect);
    let attempt = login(&mut profile);
    let (connection, credentials) = parts(1);
    assert!(profile.bind(attempt, connection, credentials));
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
    profile.activate(Activation::Reselect);
    assert!(binding(&mut profile).connect(CLEAR).is_some());
    // Confirmed while unselected: the binding waits for the selection.
    let mut profile = bound();
    let retired = profile.change_identity(IdentityChange::Principal).unwrap();
    let _ = profile.leave(Leave::Unselect);
    let (connection, credentials) = parts(3);
    assert!(profile.bind(retired.create.unwrap(), connection, credentials));
    profile.confirm();
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
}

#[test]
fn a_selection_on_the_placeholder_carries_over_to_the_binding() {
    let mut profile = P::new(Slot::Placeholder(Placeholder::new(None)), false, None);
    profile.activate(Activation::Reselect);
    let attempt = login(&mut profile);
    let (connection, credentials) = parts(1);
    assert!(profile.bind(attempt, connection, credentials));
    assert!(binding(&mut profile).connect(CLEAR).is_some());
}

#[test]
fn logging_in_again_while_unselected_activates_nothing_but_still_lets_go() {
    use crate::provider::credential::Outcome;
    let mut profile = bound();
    // Revoked, then refused: access_revoked, held once the binding leaves.
    let bound = binding(&mut profile);
    let Outcome::Done(revoked) = bound.revoke(Instant::now()).value else {
        unreachable!()
    };
    let value = revoked.invalidation.unwrap().to_persist.unwrap();
    bound.connection_mut().confirm_generation(value);
    let ticket = bound.issue().unwrap();
    let refused = bound.issued(ticket, Issued::Refused, Instant::now()).value;
    let Outcome::Done(refused) = refused else {
        unreachable!()
    };
    let value = refused.invalidation.unwrap().to_persist.unwrap();
    bound.connection_mut().confirm_generation(value);
    let unselect = profile.leave(Leave::Unselect).value;
    let value = unselect.invalidation.unwrap().to_persist.unwrap();
    binding(&mut profile)
        .connection_mut()
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

fn placeholder(profile: &mut P) -> &mut Placeholder {
    match profile.slot_mut() {
        Slot::Placeholder(placeholder) => placeholder,
        Slot::Bound(_) => panic!("bound"),
    }
}

#[test]
fn an_attempt_of_an_earlier_placeholder_never_counts_on_a_later_one() {
    let mut profile = P::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let first = placeholder(&mut profile).attempt();
    let stale = placeholder(&mut profile).attempt();
    assert_eq!(
        placeholder(&mut profile).settle(stale, Attempted::Identified),
        Settled::Create
    );
    let (connection, credentials) = parts(1);
    assert!(profile.bind(stale, connection, credentials));
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
    let mut profile = P::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let attempt = placeholder(&mut profile).attempt();
    assert!(profile.change_identity(IdentityChange::Endpoint).is_none());
    let settled = placeholder(&mut profile).settle(attempt, Attempted::Identified);
    assert_eq!(settled, Settled::Ignored);
}

#[test]
fn a_sidecar_starting_retries_a_placeholder_holding_dependency_when_it_would_back_off() {
    use crate::provider::binding::Sidecar;
    let holding = |failure, selected| {
        P::new(
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

#[test]
fn a_creation_binds_only_while_nothing_came_after_it() {
    // #1233 G-A: a login started before logging out creates nothing.
    let mut profile = P::new(Slot::Placeholder(Placeholder::new(None)), true, None);
    let attempt = placeholder(&mut profile).attempt();
    let _ = profile.leave(Leave::Logout);
    let settled = placeholder(&mut profile).settle(attempt, Attempted::Identified);
    assert_eq!(settled, Settled::Ignored);
    let (connection, credentials) = parts(1);
    assert!(!profile.bind(attempt, connection, credentials));
    // Answered first, then logged out: the creation is over too.
    let attempt = login(&mut profile);
    let _ = profile.leave(Leave::Logout);
    let (connection, credentials) = parts(1);
    assert!(!profile.bind(attempt, connection, credentials));
    // G-B: the identity changed before the creation was persisted.
    let attempt = login(&mut profile);
    assert!(profile.change_identity(IdentityChange::Endpoint).is_none());
    let (connection, credentials) = parts(1);
    assert!(!profile.bind(attempt, connection, credentials));
    // G-C: a later attempt, refused, is not overwritten by the earlier one.
    let earlier = login(&mut profile);
    let later = placeholder(&mut profile).attempt();
    placeholder(&mut profile).settle(later, Attempted::Rejected);
    let (connection, credentials) = parts(1);
    assert!(!profile.bind(earlier, connection, credentials));
    assert_eq!(
        placeholder(&mut profile).failure(),
        Some(PlaceholderFailure::CredentialRejected)
    );
    // Unselecting does not end it: the binding waits, unactivated.
    let attempt = login(&mut profile);
    let _ = profile.leave(Leave::Unselect);
    let (connection, credentials) = parts(1);
    assert!(profile.bind(attempt, connection, credentials));
    assert_eq!(binding(&mut profile).connect(CLEAR), None);
}

#[test]
fn a_removed_binding_takes_no_identity_change() {
    // #1233 note 2.
    let mut profile = bound();
    let removed = profile.leave(Leave::Remove).value;
    let value = removed.invalidation.unwrap().to_persist.unwrap();
    binding(&mut profile)
        .connection_mut()
        .confirm_generation(value);
    for change in [
        IdentityChange::Instance,
        IdentityChange::Principal,
        IdentityChange::Endpoint,
    ] {
        assert!(profile.change_identity(change).is_none(), "{change:?}");
        assert!(binding(&mut profile).retired());
    }
    assert_eq!(profile.confirmation(), None);
}
