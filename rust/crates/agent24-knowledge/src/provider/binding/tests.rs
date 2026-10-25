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
        binding.activate(Activation::Reselect);
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
    binding.activate(Activation::Reselect);
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

/// A `ready` binding that a probe then found incompatible, or that a
/// revocation made `access_revoked`.
fn stuck(cause: state::UnavailableCause) -> BindingState {
    let mut binding = ready();
    let (connection, credentials) = binding.parts_mut();
    match cause {
        state::UnavailableCause::Incompatible => {
            let probe = connection.probe().unwrap();
            connection.apply_probe(probe, Probe::Incompatible);
        }
        _ => {
            let effects = credentials.revoke(connection, Instant::now());
            let effects = match effects {
                Outcome::Done(effects) => effects,
                other => panic!("{other:?}"),
            };
            let value = effects.invalidation.unwrap().to_persist.unwrap();
            connection.confirm_generation(value);
            let ticket = credentials.issue(connection).unwrap();
            let refused = credentials.issued(connection, ticket, Issued::Refused, Instant::now());
            let Outcome::Done(refused) = refused else {
                panic!("{refused:?}")
            };
            let value = refused.invalidation.unwrap().to_persist.unwrap();
            connection.confirm_generation(value);
        }
    }
    assert_eq!(binding.connection().phase(), Some(Unavailable(cause)));
    binding
}

/// Leaves with `event`, persists, activates again as `how`, connects.
fn back(binding: &mut BindingState, event: Leave, how: Option<Activation>) {
    let effects = binding.leave(event);
    if effects.invalidation.is_some() {
        persisted(binding, &effects);
    }
    if let Some(how) = how {
        binding.activate(how);
    }
    binding.connect(CLEAR);
}

#[test]
fn coming_back_does_not_retry_a_reason_that_waits_for_the_user() {
    use state::UnavailableCause::{AccessRevoked, Incompatible};
    for cause in [Incompatible, AccessRevoked] {
        let mut binding = stuck(cause);
        back(&mut binding, Leave::Off, None);
        assert_eq!(binding.connection().phase(), Some(Unavailable(cause)));
        back(&mut binding, Leave::Unselect, Some(Activation::Reselect));
        assert_eq!(binding.connection().phase(), Some(Unavailable(cause)));
        assert_eq!(binding.parts_mut().0.probe(), None);
    }
}

#[test]
fn logging_in_again_retries_a_rejected_or_revoked_credential_only() {
    use state::UnavailableCause::{AccessRevoked, Incompatible};
    let mut binding = stuck(AccessRevoked);
    back(&mut binding, Leave::Logout, Some(Activation::Relogin));
    assert_eq!(binding.connection().phase(), Some(Loading));
    let mut binding = stuck(Incompatible);
    back(&mut binding, Leave::Logout, Some(Activation::Relogin));
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(Incompatible))
    );
}

#[test]
fn leaving_while_waiting_for_persistence_keeps_nothing() {
    let mut binding = ready();
    let (connection, credentials) = binding.parts_mut();
    credentials.revoke(connection, Instant::now());
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(state::UnavailableCause::LocalStorage))
    );
    let off = binding.leave(Leave::Off);
    persisted(&mut binding, &off);
    assert!(binding.connect(CLEAR).is_some());
    assert_eq!(binding.connection().phase(), Some(Loading));
}

#[test]
fn coming_back_retries_a_reason_that_retries_by_itself() {
    let mut binding = ready();
    let (connection, _) = binding.parts_mut();
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, Probe::NoAnswer);
    back(&mut binding, Leave::Off, None);
    assert_eq!(binding.connection().phase(), Some(Loading));
}

#[test]
fn logging_in_again_retries_a_rejected_credential() {
    // Never servable: a refused issuance is credential_rejected (S3.1).
    let mut binding = BindingState::new(
        Connection::new(Gate::new(1), false),
        Credentials::new(false),
        true,
    );
    binding.connect(CLEAR).unwrap();
    let (connection, credentials) = binding.parts_mut();
    let ticket = credentials.issue(connection).unwrap();
    credentials.issued(connection, ticket, Issued::Refused, Instant::now());
    let rejected = Some(Unavailable(state::UnavailableCause::CredentialRejected));
    assert_eq!(binding.connection().phase(), rejected);
    back(&mut binding, Leave::Logout, Some(Activation::Reselect));
    assert_eq!(binding.connection().phase(), rejected);
    back(&mut binding, Leave::Logout, Some(Activation::Relogin));
    assert_eq!(binding.connection().phase(), Some(Loading));
}

#[test]
fn logging_in_again_without_logging_out_retries_too() {
    use state::UnavailableCause::{AccessRevoked, Incompatible};
    // Still in the reason, after Off and On or never left.
    let mut binding = stuck(AccessRevoked);
    back(&mut binding, Leave::Off, None);
    binding.activate(Activation::Relogin);
    assert!(binding.connect(CLEAR).is_some());
    assert_eq!(binding.connection().phase(), Some(Loading));
    let mut binding = stuck(AccessRevoked);
    binding.activate(Activation::Relogin);
    assert!(binding.connect(CLEAR).is_some());
    // Another reason stays where it is.
    let mut binding = stuck(Incompatible);
    let round = binding.connection().round();
    binding.activate(Activation::Relogin);
    assert_eq!(binding.connect(CLEAR), None);
    assert!(binding.connection().is_current(round));
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(Incompatible))
    );
}

#[test]
fn without_a_credential_dependency_retries_through_loading_never_a_probe() {
    // #1228 note 1: a renewal whose issuer is unreachable drops the
    // credential in `dependency`.
    let mut binding = ready();
    let old = binding.credentials().current().unwrap();
    let (connection, credentials) = binding.parts_mut();
    let Outcome::Done(expired) =
        credentials.response(connection, old, Signal::Expired, Instant::now())
    else {
        unreachable!()
    };
    let renew = expired.renew.unwrap();
    credentials.issued(connection, renew, Issued::Unreachable, Instant::now());
    let dependency = Some(Unavailable(state::UnavailableCause::Dependency));
    assert_eq!(binding.connection().phase(), dependency);
    assert_eq!(binding.credentials().current(), None);
    assert_eq!(binding.probe(), None);
    let Some(Retry::Issue(ticket)) = binding.backoff() else {
        panic!("no issuance")
    };
    assert_eq!(binding.connection().phase(), Some(Loading));
    assert_eq!(binding.probe(), None);
    let (connection, credentials) = binding.parts_mut();
    let granted = Issued::Granted { narrowed: false };
    credentials.issued(connection, ticket, granted, Instant::now());
    assert_eq!(binding.backoff(), None);
    let probe = binding.probe().unwrap();
    binding.parts_mut().0.apply_probe(probe, READY);
    assert_eq!(binding.connection().phase(), Some(Ready));
    assert!(binding.credentials().current().is_some());
}

#[test]
fn with_a_credential_backoff_probes_and_only_in_a_reason_that_retries() {
    let mut binding = ready();
    assert_eq!(binding.backoff(), None);
    let probe = binding.probe().unwrap();
    binding.parts_mut().0.apply_probe(probe, Probe::NoAnswer);
    let Some(Retry::Probe(probe)) = binding.backoff() else {
        panic!("no probe")
    };
    binding.parts_mut().0.apply_probe(probe, READY);
    assert_eq!(binding.connection().phase(), Some(Ready));
    // Not in a reason that waits for the user, with or without a credential.
    let mut binding = stuck(state::UnavailableCause::Incompatible);
    assert_eq!(binding.backoff(), None);
    let mut binding = stuck(state::UnavailableCause::AccessRevoked);
    assert_eq!(binding.credentials().current(), None);
    assert_eq!(binding.backoff(), None);
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(state::UnavailableCause::AccessRevoked))
    );
}

#[test]
fn a_retry_lets_a_reason_that_waits_for_the_user_go() {
    use state::UnavailableCause::{AccessRevoked, Incompatible, LocalStorage};
    for cause in [Incompatible, AccessRevoked] {
        let mut binding = stuck(cause);
        assert!(binding.retry().is_some(), "{cause:?}");
        assert_eq!(binding.connection().phase(), Some(Loading));
    }
    // Revoked: a new issuance follows.
    let mut binding = stuck(AccessRevoked);
    binding.retry().unwrap();
    let (connection, credentials) = binding.parts_mut();
    assert!(credentials.issue(connection).is_some());
    // Kept while out of the connection phase: let go there too.
    let mut binding = stuck(Incompatible);
    let off = binding.leave(Leave::Off);
    persisted(&mut binding, &off);
    assert_eq!(binding.retry(), None);
    assert!(binding.connect(CLEAR).is_some());
    assert_eq!(binding.connection().phase(), Some(Loading));
    // Nothing to retry in `ready`, in `local_storage`, or once retired.
    let mut binding = ready();
    let round = binding.connection().round();
    assert_eq!(binding.retry(), None);
    assert!(binding.connection().is_current(round));
    let (connection, credentials) = binding.parts_mut();
    credentials.revoke(connection, Instant::now());
    let round = binding.connection().round();
    assert_eq!(binding.retry(), None);
    assert!(binding.connection().is_current(round));
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(LocalStorage))
    );
    let mut binding = stuck(Incompatible);
    let removed = binding.leave(Leave::Remove);
    persisted(&mut binding, &removed);
    assert_eq!(binding.retry(), None);
}

#[test]
fn a_sidecar_going_down_is_dependency_without_increment() {
    use state::UnavailableCause::{Dependency, Incompatible};
    for down in [
        Sidecar::Degraded,
        Sidecar::Failed,
        Sidecar::Stopping,
        Sidecar::Stopped,
    ] {
        let mut binding = ready();
        assert_eq!(binding.sidecar(down), None);
        assert_eq!(binding.connection().phase(), Some(Unavailable(Dependency)));
        assert_eq!(binding.connection().gate().reported(), 1);
        assert!(binding.credentials().current().is_some());
        // A reason that waits for the user stays.
        let mut binding = stuck(Incompatible);
        binding.sidecar(down);
        assert_eq!(
            binding.connection().phase(),
            Some(Unavailable(Incompatible))
        );
        // Out of the connection phase nothing moves.
        let mut binding = ready();
        let off = binding.leave(Leave::Off);
        persisted(&mut binding, &off);
        binding.sidecar(down);
        assert_eq!(binding.connection().phase(), None);
    }
    for up in [Sidecar::Ready, Sidecar::Healthy] {
        let mut binding = ready();
        assert_eq!(binding.sidecar(up), None);
        assert_eq!(binding.connection().phase(), Some(Ready));
    }
}

#[test]
fn a_sidecar_starting_retries_an_unavailable_binding_only() {
    use state::UnavailableCause::Incompatible;
    let mut binding = stuck(Incompatible);
    assert!(binding.sidecar(Sidecar::Starting).is_some());
    assert_eq!(binding.connection().phase(), Some(Loading));
    let mut binding = ready();
    binding.sidecar(Sidecar::Stopped);
    assert!(binding.sidecar(Sidecar::Starting).is_some());
    let mut binding = ready();
    let round = binding.connection().round();
    assert_eq!(binding.sidecar(Sidecar::Starting), None);
    assert!(binding.connection().is_current(round));
}
