#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::time::Instant;

use super::*;
use crate::provider::credential::{Issued, Outcome, Signal};
use crate::provider::evidence::Passed;
use crate::provider::gate::Gate;
use crate::provider::state::{Probe, ServiceState, Target, derive};
use ConnectionPhase::{Loading, Ready, Unavailable};

type B = BindingState<&'static str>;

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
fn ready() -> B {
    let mut binding = B::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    binding.connect(CLEAR).unwrap();
    let ticket = binding.issue().unwrap();
    let granted = Issued::Granted { narrowed: false };
    let _ = binding.issued(ticket, granted, Instant::now());
    let connection = binding.connection_mut();
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, READY);
    connection.open_call().unwrap();
    binding
}

fn state(binding: &B, switches: Switches) -> Option<ServiceState> {
    derive(switches, Target::Binding(binding.facts()))
}

/// Confirms the generation the event asked for.
fn persisted(binding: &mut B, effects: &Effects) {
    let value = effects.invalidation.as_ref().unwrap().to_persist.unwrap();
    binding.connection_mut().confirm_generation(value);
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
        let effects = binding.leave(event).value;
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
    let off = binding.leave(Leave::Off).value;
    persisted(&mut binding, &off);
    for event in [Leave::Disable, Leave::Unselect, Leave::Logout] {
        let effects = binding.leave(event).value;
        assert_eq!(effects.invalidation, None, "{event:?}");
        assert!(effects.placeholder);
    }
    assert_eq!(binding.connection().gate().reported(), 2);
}

#[test]
fn off_then_on_resumes_the_same_binding_once_the_generation_is_persisted() {
    let mut binding = ready();
    let effects = binding.leave(Leave::Off).value;
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
        let effects = binding.leave(event).value;
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
    let effects = binding.leave(Leave::Remove).value;
    assert!(binding.retired());
    persisted(&mut binding, &effects);
    binding.activate(Activation::Reselect);
    assert_eq!(binding.connect(CLEAR), None);
    assert_eq!(state(&binding, CLEAR), None);
    assert_eq!(binding.leave(Leave::Disable).value, Effects::default());
}

#[test]
fn out_of_the_connection_phase_it_derives_as_registered_or_disabled() {
    let mut binding = B::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    assert_eq!(state(&binding, CLEAR), Some(ServiceState::Registered));
    assert_eq!(state(&binding, OFF), Some(ServiceState::Disabled));
    binding.connect(CLEAR).unwrap();
    let connection = binding.connection_mut();
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
    let effects = binding.leave(Leave::Disable).value;
    persisted(&mut binding, &effects);
    assert_eq!(binding.connect(disabled), None);
    assert!(binding.connect(CLEAR).is_some());
    // A late revocation on the credential from before is a discarded one's.
    let outcome = binding.response(old, Signal::Revoked, ["a"], Instant::now());
    assert_eq!(outcome.value, Outcome::Recheck);
    assert_eq!(binding.connection().phase(), Some(Loading));
}

/// A `ready` binding that a probe then found incompatible, or that a
/// revocation made `access_revoked`.
fn stuck(cause: state::UnavailableCause) -> B {
    let mut binding = ready();
    match cause {
        state::UnavailableCause::Incompatible => {
            let connection = binding.connection_mut();
            let probe = connection.probe().unwrap();
            connection.apply_probe(probe, Probe::Incompatible);
        }
        _ => {
            let effects = binding.revoke(Instant::now()).value;
            let effects = match effects {
                Outcome::Done(effects) => effects,
                other => panic!("{other:?}"),
            };
            let value = effects.invalidation.unwrap().to_persist.unwrap();
            binding.connection_mut().confirm_generation(value);
            let ticket = binding.issue().unwrap();
            let refused = binding
                .issued(ticket, Issued::Refused, Instant::now())
                .value;
            let Outcome::Done(refused) = refused else {
                panic!("{refused:?}")
            };
            let value = refused.invalidation.unwrap().to_persist.unwrap();
            binding.connection_mut().confirm_generation(value);
        }
    }
    assert_eq!(binding.connection().phase(), Some(Unavailable(cause)));
    binding
}

/// Leaves with `event`, persists, activates again as `how`, connects.
fn back(binding: &mut B, event: Leave, how: Option<Activation>) {
    let effects = binding.leave(event).value;
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
        assert_eq!(binding.connection_mut().probe(), None);
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
    let _ = binding.revoke(Instant::now());
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(state::UnavailableCause::LocalStorage))
    );
    let off = binding.leave(Leave::Off).value;
    persisted(&mut binding, &off);
    assert!(binding.connect(CLEAR).is_some());
    assert_eq!(binding.connection().phase(), Some(Loading));
}

#[test]
fn coming_back_retries_a_reason_that_retries_by_itself() {
    let mut binding = ready();
    let connection = binding.connection_mut();
    let probe = connection.probe().unwrap();
    connection.apply_probe(probe, Probe::NoAnswer);
    back(&mut binding, Leave::Off, None);
    assert_eq!(binding.connection().phase(), Some(Loading));
}

#[test]
fn logging_in_again_retries_a_rejected_credential() {
    // Never servable: a refused issuance is credential_rejected (S3.1).
    let mut binding = B::new(
        Connection::new(Gate::new(1), false),
        Credentials::new(false),
        true,
    );
    binding.connect(CLEAR).unwrap();
    let ticket = binding.issue().unwrap();
    let _ = binding.issued(ticket, Issued::Refused, Instant::now());
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
    let Outcome::Done(expired) = binding
        .response(old, Signal::Expired, ["a"], Instant::now())
        .value
    else {
        unreachable!()
    };
    let renew = expired.renew.unwrap();
    let _ = binding.issued(renew, Issued::Unreachable, Instant::now());
    let dependency = Some(Unavailable(state::UnavailableCause::Dependency));
    assert_eq!(binding.connection().phase(), dependency);
    assert_eq!(binding.credentials().current(), None);
    assert_eq!(binding.probe(), None);
    let Some(Retry::Issue(ticket)) = binding.backoff() else {
        panic!("no issuance")
    };
    assert_eq!(binding.connection().phase(), Some(Loading));
    assert_eq!(binding.probe(), None);
    let granted = Issued::Granted { narrowed: false };
    let _ = binding.issued(ticket, granted, Instant::now());
    assert_eq!(binding.backoff(), None);
    let probe = binding.probe().unwrap();
    binding.connection_mut().apply_probe(probe, READY);
    assert_eq!(binding.connection().phase(), Some(Ready));
    assert!(binding.credentials().current().is_some());
}

#[test]
fn with_a_credential_backoff_probes_and_only_in_a_reason_that_retries() {
    let mut binding = ready();
    assert_eq!(binding.backoff(), None);
    let probe = binding.probe().unwrap();
    binding.connection_mut().apply_probe(probe, Probe::NoAnswer);
    let Some(Retry::Probe(probe)) = binding.backoff() else {
        panic!("no probe")
    };
    binding.connection_mut().apply_probe(probe, READY);
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
    assert!(binding.issue().is_some());
    // Kept while out of the connection phase: let go there too.
    let mut binding = stuck(Incompatible);
    let off = binding.leave(Leave::Off).value;
    persisted(&mut binding, &off);
    assert_eq!(binding.retry(), None);
    assert!(binding.connect(CLEAR).is_some());
    assert_eq!(binding.connection().phase(), Some(Loading));
    // Nothing to retry in `ready`, in `local_storage`, or once retired.
    let mut binding = ready();
    let round = binding.connection().round();
    assert_eq!(binding.retry(), None);
    assert!(binding.connection().is_current(round));
    let _ = binding.revoke(Instant::now());
    let round = binding.connection().round();
    assert_eq!(binding.retry(), None);
    assert!(binding.connection().is_current(round));
    assert_eq!(
        binding.connection().phase(),
        Some(Unavailable(LocalStorage))
    );
    let mut binding = stuck(Incompatible);
    let removed = binding.leave(Leave::Remove).value;
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
        let off = binding.leave(Leave::Off).value;
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

#[test]
fn back_in_loading_no_probe_goes_out_before_the_issuance() {
    // #1229 P1: Off, persisted, On: `loading` without a credential; a probe
    // racing the issuance would reach `ready` with nothing to send.
    let mut binding = ready();
    let off = binding.leave(Leave::Off).value;
    persisted(&mut binding, &off);
    binding.connect(CLEAR).unwrap();
    assert_eq!(binding.credentials().current(), None);
    assert_eq!(binding.probe(), None);
    assert_eq!(binding.connection().phase(), Some(Loading));
    let ticket = binding.issue().unwrap();
    let granted = Issued::Granted { narrowed: false };
    let _ = binding.issued(ticket, granted, Instant::now());
    let probe = binding.probe().unwrap();
    binding.connection_mut().apply_probe(probe, READY);
    assert_eq!(binding.connection().phase(), Some(Ready));
    assert!(binding.credentials().current().is_some());
}

#[test]
fn every_event_but_off_places_a_rule_over_every_kb() {
    for event in [
        Leave::Off,
        Leave::Disable,
        Leave::Unselect,
        Leave::Logout,
        Leave::Remove,
        Leave::Retire,
    ] {
        let mut binding = ready();
        let left = binding.leave(event);
        let placed = event != Leave::Off;
        let rule = placed.then_some(Rule {
            epoch: 1,
            kbs: Kbs::All,
        });
        assert_eq!(left.rule, rule, "{event:?}");
        assert_eq!(binding.evidence().epoch(), u64::from(placed));
    }
    // Retired, it takes no event.
    let mut binding = ready();
    let _ = binding.leave(Leave::Remove);
    assert_eq!(binding.leave(Leave::Disable).rule, None);
    assert_eq!(binding.evidence().epoch(), 1);
}

#[test]
fn a_credential_event_places_a_rule_exactly_when_it_calls_for_a_placeholder() {
    let now = Instant::now();
    let mut binding = ready();
    let current = binding.credentials().current().unwrap();
    // The first invalid-class response renews: no placeholder.
    let renew = binding.response(current, Signal::Invalid, ["a"], now);
    assert_eq!(renew.value, Outcome::Done(Effects::default()));
    assert_eq!(renew.rule, None);
    let ticket = binding.issue().unwrap();
    let granted = Issued::Granted { narrowed: false };
    assert_eq!(binding.issued(ticket, granted, now).rule, None);
    // The renewal fails too: a placeholder for every KB.
    let current = binding.credentials().current().unwrap();
    let failed = binding.response(current, Signal::Invalid, ["a"], now);
    assert!(matches!(
        failed.value,
        Outcome::Done(Effects {
            placeholder: true,
            ..
        })
    ));
    assert_eq!(
        failed.rule,
        Some(Rule {
            epoch: 1,
            kbs: Kbs::All
        })
    );
    let revoked = binding.revoke(now);
    assert_eq!(
        revoked.rule,
        Some(Rule {
            epoch: 2,
            kbs: Kbs::All
        })
    );
    assert_eq!(binding.evidence().epoch(), 2);
    // A renewal granted narrowed, then an issuance refused once revoked.
    let mut binding = ready();
    let current = binding.credentials().current().unwrap();
    let expired = binding.response(current, Signal::Expired, ["a"], now).value;
    let Outcome::Done(Effects {
        renew: Some(renew), ..
    }) = expired
    else {
        panic!("{expired:?}")
    };
    let narrowed = binding.issued(renew, Issued::Granted { narrowed: true }, now);
    assert_eq!(
        narrowed.rule,
        Some(Rule {
            epoch: 1,
            kbs: Kbs::All
        })
    );
    let Outcome::Done(narrowed) = narrowed.value else {
        unreachable!()
    };
    persisted(&mut binding, &narrowed);
    let ticket = binding.issue().unwrap();
    let refused = binding.issued(ticket, Issued::Refused, now);
    assert!(matches!(
        refused.value,
        Outcome::Done(Effects {
            placeholder: true,
            ..
        })
    ));
    assert_eq!(
        refused.rule,
        Some(Rule {
            epoch: 2,
            kbs: Kbs::All
        })
    );
    assert_eq!(binding.evidence().epoch(), 2);
}

/// A `ready` binding left Off and back in `loading`, with the credential
/// it had before.
fn discarded() -> (B, CredentialId) {
    let mut binding = ready();
    let old = binding.credentials().current().unwrap();
    let off = binding.leave(Leave::Off).value;
    persisted(&mut binding, &off);
    binding.connect(CLEAR).unwrap();
    (binding, old)
}

#[test]
fn a_revocation_on_a_discarded_credential_registers_what_it_asked() {
    let now = Instant::now();
    let (mut binding, old) = discarded();
    let stale = binding.response(old, Signal::Expired, ["a"], now);
    assert_eq!(stale.value, Outcome::Stale);
    assert!(!binding.facts().reverify_pending);
    let recheck = binding.response(old, Signal::Revoked, ["a"], now);
    assert_eq!(recheck.value, Outcome::Recheck);
    assert_eq!(recheck.rule, None);
    assert!(binding.facts().reverify_pending);
    assert_eq!(binding.evidence().epoch(), 0);
    // An empty Q fails closed: every KB's evidence becomes placeholders.
    let (mut binding, old) = discarded();
    let empty = binding.response(old, Signal::Revoked, [], now);
    assert_eq!(empty.value, Outcome::Recheck);
    assert_eq!(
        empty.rule,
        Some(Rule {
            epoch: 1,
            kbs: Kbs::All
        })
    );
}

#[test]
fn a_recheck_that_came_back_revoked_places_once_and_ends_the_hold() {
    let now = Instant::now();
    let (mut binding, old) = discarded();
    let _ = binding.response(old, Signal::Revoked, ["a"], now);
    // Sent with a credential discarded since: another recheck.
    let again = binding.recheck_revoked(old, ["a"], now);
    assert_eq!(again.value, Outcome::Recheck);
    assert_eq!(again.rule, None);
    assert!(binding.facts().reverify_pending);
    let ticket = binding.issue().unwrap();
    let granted = Issued::Granted { narrowed: false };
    let _ = binding.issued(ticket, granted, now);
    let current = binding.credentials().current().unwrap();
    let revoked = binding.recheck_revoked(current, ["a"], now);
    assert!(matches!(
        revoked.value,
        Outcome::Done(Effects {
            placeholder: true,
            ..
        })
    ));
    assert_eq!(
        revoked.rule,
        Some(Rule {
            epoch: 1,
            kbs: Kbs::All
        })
    );
    assert_eq!(binding.evidence().epoch(), 1);
    assert!(!binding.facts().reverify_pending);
}

#[test]
fn a_removal_is_handled_as_a_narrowing_in_the_same_step() {
    let now = Instant::now();
    let mut binding = ready();
    let current = binding.credentials().current().unwrap();
    let asked = BTreeSet::from(["a", "b"]);
    let joined = binding.observe(&asked, &Scope::Listed(asked.clone()), 0, current, now);
    assert_eq!(joined.value.0.added, asked);
    assert_eq!((joined.value.1, joined.rule), (None, None));
    let only_a = Scope::Listed(BTreeSet::from(["a"]));
    let narrowed = binding.observe(&asked, &only_a, 0, current, now);
    assert_eq!(narrowed.value.0.removed, BTreeSet::from(["b"]));
    let outcome = narrowed.value.1.unwrap();
    assert!(matches!(
        outcome,
        Outcome::Done(Effects {
            placeholder: true,
            ..
        })
    ));
    assert_eq!(
        narrowed.rule,
        Some(Rule {
            epoch: 1,
            kbs: Kbs::All
        })
    );
    assert_eq!(binding.credentials().current(), None);
    // The next response, on the credential the narrowing discarded, tells
    // no removal and narrows nothing again.
    let late = binding.observe(&asked, &Scope::Empty, 0, current, now);
    assert!(late.value.0.removed.is_empty());
    assert_eq!((late.value.1, late.rule), (None, None));
    assert_eq!(binding.evidence().epoch(), 1);
}

/// Pending on `a`, a recheck R1 sent with c1, then a renewal that does not
/// invalidate, and a recheck R2 sent with c2: the binding, c1, R1's version
/// and R2's.
fn renewed_meanwhile() -> (B, CredentialId, u64, u64) {
    let now = Instant::now();
    let (mut binding, old) = discarded();
    let _ = binding.response(old, Signal::Revoked, ["a"], now);
    let ticket = binding.issue().unwrap();
    let _ = binding.issued(ticket, Issued::Granted { narrowed: false }, now);
    let c1 = binding.credentials().current().unwrap();
    let r1 = binding.evidence().recheck().unwrap();
    let expired = binding.response(c1, Signal::Expired, ["a"], now).value;
    let Outcome::Done(Effects {
        renew: Some(renew), ..
    }) = expired
    else {
        panic!("{expired:?}")
    };
    let _ = binding.issued(renew, Issued::Granted { narrowed: false }, now);
    assert_ne!(binding.credentials().current(), Some(c1));
    let r2 = binding.evidence().recheck().unwrap();
    (binding, c1, r1.version, r2.version)
}

#[test]
fn a_recheck_revoked_on_a_credential_renewed_since_registers_again() {
    // Codex on 5e, C1: R2 passed and was released, then R1 came back.
    let now = Instant::now();
    let (mut binding, c1, _, r2) = renewed_meanwhile();
    assert_eq!(binding.evidence().passed(r2), Passed::Release);
    assert!(binding.evidence_mut().release(r2));
    let late = binding.recheck_revoked(c1, ["a"], now);
    assert_eq!((late.value, late.rule), (Outcome::Recheck, None));
    assert!(binding.facts().reverify_pending);
    // R1 back first: R2 passing no longer releases.
    let (mut binding, c1, r1, r2) = renewed_meanwhile();
    assert_eq!(r1, r2);
    let _ = binding.recheck_revoked(c1, ["a"], now);
    assert!(matches!(binding.evidence().passed(r2), Passed::Again(_)));
    // An empty Q fails closed here too.
    let (mut binding, c1, _, _) = renewed_meanwhile();
    let empty = binding.recheck_revoked(c1, [], now);
    assert_eq!(
        empty.rule,
        Some(Rule {
            epoch: 1,
            kbs: Kbs::All
        })
    );
}
