#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::gate::Gate;
use crate::provider::state::Probe;
use ConnectionPhase::{Loading, Ready, Unavailable};
use UnavailableCause as U;

const READY: Probe = Probe::Answered {
    live: true,
    search_ready: true,
    read_ready: true,
};
const GRANTED: Issued = Issued::Granted { narrowed: false };

/// A binding in `loading` with no credential yet.
fn loading(once_servable: bool, revoked_before: bool) -> (Connection, Credentials) {
    let mut connection = Connection::new(Gate::new(1), once_servable);
    connection.start_loading().unwrap();
    (connection, Credentials::new(revoked_before))
}

fn granted(
    connection: &mut Connection,
    credentials: &mut Credentials,
    now: Instant,
) -> CredentialId {
    let ticket = credentials.issue(connection).unwrap();
    assert_eq!(
        credentials.issued(connection, ticket, GRANTED, now),
        Outcome::Done(Effects::default())
    );
    credentials.current().unwrap()
}

fn probed(connection: &mut Connection, probe: Probe) {
    let ticket = connection.probe().unwrap();
    connection.apply_probe(ticket, probe);
}

/// A `ready` binding that was servable before, with a current credential.
fn ready(now: Instant) -> (Connection, Credentials, CredentialId) {
    let (mut connection, mut credentials) = loading(true, false);
    let credential = granted(&mut connection, &mut credentials, now);
    probed(&mut connection, READY);
    assert_eq!(connection.phase(), Some(Ready));
    (connection, credentials, credential)
}

fn effects(outcome: Outcome) -> Effects {
    match outcome {
        Outcome::Done(effects) => effects,
        other => panic!("{other:?}"),
    }
}

/// Confirms the generation an invalidation asked for.
fn persisted(connection: &mut Connection, effects: &Effects) -> Option<Round> {
    let value = effects.invalidation.as_ref().unwrap().to_persist.unwrap();
    connection.confirm_generation(value)
}

#[test]
fn an_issuance_counts_only_in_its_round_and_one_goes_out_at_a_time() {
    let now = Instant::now();
    let (mut connection, mut credentials) = loading(true, false);
    let ticket = credentials.issue(&connection).unwrap();
    assert_eq!(credentials.issue(&connection), None);
    // The round ends before the answer: not adopted, nothing moves.
    connection.leave();
    connection.start_loading();
    assert_eq!(
        credentials.issued(&mut connection, ticket, GRANTED, now),
        Outcome::Ignored
    );
    assert_eq!(credentials.current(), None);
    assert_eq!(connection.phase(), Some(Loading));
    // The new round issues again; an answer to no issuance in flight counts
    // neither.
    let ticket = credentials.issue(&connection).unwrap();
    credentials.issued(&mut connection, ticket, GRANTED, now);
    let adopted = credentials.current().unwrap();
    assert_eq!(
        credentials.issued(&mut connection, ticket, Issued::Revoked, now),
        Outcome::Ignored
    );
    assert_eq!(credentials.current(), Some(adopted));
    // With a change pending persistence, nothing is issued.
    let (mut pending, mut fresh) = loading(true, false);
    pending.invalidate();
    assert_eq!(fresh.issue(&pending), None);
    // With a credential, or outside `loading`, nothing is issued.
    assert_eq!(credentials.issue(&connection), None);
    connection.leave();
    assert_eq!(credentials.issue(&connection), None);
}

#[test]
fn a_revocation_signal_in_a_servable_phase_invalidates_and_issues_again_once_persisted() {
    let now = Instant::now();
    let (mut connection, mut credentials, credential) = ready(now);
    let (call, _) = connection.open_call().unwrap();
    let effects = effects(credentials.response(&mut connection, credential, Signal::Revoked, now));
    assert!(effects.placeholder);
    assert_eq!(effects.invalidation.as_ref().unwrap().aborted, vec![call]);
    assert_eq!(credentials.current(), None);
    // Loading waits for the new generation to be persisted (S1.4).
    assert_eq!(connection.phase(), Some(Unavailable(U::LocalStorage)));
    assert_eq!(credentials.issue(&connection), None);
    persisted(&mut connection, &effects).unwrap();
    let ticket = credentials.issue(&connection).unwrap();
    // The issuance is refused: a revocation signal in `loading`.
    let refused = self::effects(credentials.issued(&mut connection, ticket, Issued::Refused, now));
    assert!(refused.placeholder && refused.invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    // And it stays there once the generation is persisted.
    assert_eq!(persisted(&mut connection, &refused), None);
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
}

#[test]
fn a_revocation_signal_elsewhere_ends_in_access_revoked_or_only_places_holders() {
    let now = Instant::now();
    // In `unavailable`, even one that does not retry.
    let (mut connection, mut credentials, credential) = ready(now);
    probed(&mut connection, Probe::Incompatible);
    let outcome = credentials.response(&mut connection, credential, Signal::Revoked, now);
    assert!(effects(outcome).invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    // In `loading`, from the authority's notice.
    let (mut connection, mut credentials) = loading(true, false);
    granted(&mut connection, &mut credentials, now);
    let outcome = credentials.revoke(&mut connection, now);
    assert!(effects(outcome).invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    assert_eq!(credentials.current(), None);
    // In `registered` or `disabled`: no increment, still a placeholder.
    let (mut connection, mut credentials, _) = ready(now);
    connection.leave();
    let effects = effects(credentials.revoke(&mut connection, now));
    assert_eq!(
        effects,
        Effects {
            placeholder: true,
            ..Effects::default()
        }
    );
    assert_eq!(connection.gate().reported(), 1);
    assert_eq!(connection.phase(), None);
}

#[test]
fn a_narrowed_scope_starts_a_new_loading_round_but_leaves_a_reason_that_does_not_retry() {
    let now = Instant::now();
    // A credential granted with a narrower scope is not adopted.
    let (mut connection, mut credentials, _) = ready(now);
    let effects = effects(credentials.revoke(&mut connection, now));
    persisted(&mut connection, &effects).unwrap();
    let ticket = credentials.issue(&connection).unwrap();
    let narrowed = Issued::Granted { narrowed: true };
    let effects = self::effects(credentials.issued(&mut connection, ticket, narrowed, now));
    assert!(effects.placeholder && effects.invalidation.is_some());
    assert_eq!(credentials.current(), None);
    assert_eq!(connection.phase(), Some(Unavailable(U::LocalStorage)));
    let round = persisted(&mut connection, &effects).unwrap();
    assert!(connection.is_current(round));
    assert_eq!(connection.phase(), Some(Loading));
    // The server narrows while the binding is `incompatible`: invalidated and
    // placeholders, but the reason stays.
    let (mut connection, mut credentials, _) = ready(now);
    probed(&mut connection, Probe::Incompatible);
    let effects = self::effects(credentials.narrow(&mut connection, now));
    assert!(effects.placeholder && effects.invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::Incompatible)));
    persisted(&mut connection, &effects);
    assert_eq!(connection.phase(), Some(Unavailable(U::Incompatible)));
}

#[test]
fn the_third_revocation_within_ten_minutes_stops_issuing_again() {
    let start = Instant::now();
    let (mut connection, mut credentials, _) = ready(start);
    let again = |connection: &mut Connection, credentials: &mut Credentials, at: Instant| {
        let effects = effects(credentials.narrow(connection, at));
        persisted(connection, &effects);
        effects
    };
    again(&mut connection, &mut credentials, start);
    assert_eq!(connection.phase(), Some(Loading));
    again(
        &mut connection,
        &mut credentials,
        start + Duration::from_secs(60),
    );
    assert_eq!(connection.phase(), Some(Loading));
    let third = again(
        &mut connection,
        &mut credentials,
        start + Duration::from_secs(599),
    );
    assert!(third.placeholder && third.invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    // Ten minutes after the first, the third is only the second in the window.
    let (mut connection, mut credentials, _) = ready(start);
    again(&mut connection, &mut credentials, start);
    again(
        &mut connection,
        &mut credentials,
        start + Duration::from_secs(300),
    );
    again(
        &mut connection,
        &mut credentials,
        start + Duration::from_secs(600),
    );
    assert_eq!(connection.phase(), Some(Loading));
    // Events outside the connection phase count too.
    let (mut connection, mut credentials, _) = ready(start);
    connection.leave();
    credentials.revoke(&mut connection, start);
    credentials.revoke(&mut connection, start + Duration::from_secs(60));
    connection.start_loading();
    credentials.narrow(&mut connection, start + Duration::from_secs(120));
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
}

#[test]
fn a_binding_never_servable_nor_revoked_is_credential_rejected_without_increment() {
    let now = Instant::now();
    // Its issuance refused in `loading`.
    let (mut connection, mut credentials) = loading(false, false);
    let ticket = credentials.issue(&connection).unwrap();
    assert_eq!(
        credentials.issued(&mut connection, ticket, Issued::Refused, now),
        Outcome::Done(Effects::default())
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::CredentialRejected)));
    assert_eq!(connection.gate().reported(), 1);
    // The credential newly issued in this round met with a revocation signal.
    let (mut connection, mut credentials) = loading(false, false);
    let credential = granted(&mut connection, &mut credentials, now);
    assert_eq!(
        credentials.response(&mut connection, credential, Signal::Revoked, now),
        Outcome::Done(Effects::default())
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::CredentialRejected)));
    assert_eq!(credentials.current(), None);
    // The issuing authority's explicit revocation is no refusal: revoked.
    let (mut connection, mut credentials) = loading(false, false);
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Revoked, now);
    assert!(effects(outcome).invalidation.is_some());
}

#[test]
fn the_exception_ends_once_servable_or_after_a_revocation() {
    let now = Instant::now();
    // Servable once.
    let (mut connection, mut credentials) = loading(false, false);
    let credential = granted(&mut connection, &mut credentials, now);
    probed(&mut connection, READY);
    assert!(connection.ever_servable());
    connection.confirm_once_servable();
    let outcome = credentials.response(&mut connection, credential, Signal::Revoked, now);
    assert!(effects(outcome).invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    // A revocation-class event since the binding was created, even one
    // outside the connection phase.
    let (mut connection, mut credentials) = loading(false, false);
    connection.leave();
    credentials.revoke(&mut connection, now);
    connection.start_loading();
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Refused, now);
    assert!(effects(outcome).invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    // Servable before, as persisted.
    let (mut connection, mut credentials) = loading(true, false);
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Refused, now);
    assert!(effects(outcome).invalidation.is_some());
    // No longer `loading`: a probe moved the binding within the round.
    let (mut connection, mut credentials) = loading(false, false);
    let ticket = credentials.issue(&connection).unwrap();
    probed(&mut connection, Probe::NoAnswer);
    let outcome = credentials.issued(&mut connection, ticket, Issued::Refused, now);
    assert!(effects(outcome).invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::AccessRevoked)));
    let (mut connection, mut credentials) = loading(false, false);
    let credential = granted(&mut connection, &mut credentials, now);
    probed(&mut connection, Probe::NoAnswer);
    let outcome = credentials.response(&mut connection, credential, Signal::Revoked, now);
    assert!(effects(outcome).invalidation.is_some());
    // Not persisted and unknown: the caller passes `true`.
    let (mut connection, mut credentials) = loading(false, true);
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Refused, now);
    assert!(effects(outcome).invalidation.is_some());
    // A credential of an earlier round is not newly issued.
    let (mut connection, mut credentials) = loading(false, false);
    let credential = granted(&mut connection, &mut credentials, now);
    connection.fail(U::Dependency);
    connection.start_loading();
    let outcome = credentials.response(&mut connection, credential, Signal::Revoked, now);
    assert!(effects(outcome).invalidation.is_some());
}

#[test]
fn an_unreachable_issuer_is_dependency_and_another_principal_is_handed_back() {
    let now = Instant::now();
    let (mut connection, mut credentials) = loading(true, false);
    let ticket = credentials.issue(&connection).unwrap();
    assert_eq!(
        credentials.issued(&mut connection, ticket, Issued::Unreachable, now),
        Outcome::Done(Effects::default())
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::Dependency)));
    assert_eq!(connection.gate().reported(), 1);
    // No issuance until the binding is `loading` again.
    assert_eq!(credentials.issue(&connection), None);
    // Late, after a probe found the binding incompatible within the round:
    // that reason stays.
    let (mut connection, mut credentials) = loading(true, false);
    let ticket = credentials.issue(&connection).unwrap();
    probed(&mut connection, Probe::Incompatible);
    credentials.issued(&mut connection, ticket, Issued::Unreachable, now);
    assert_eq!(connection.phase(), Some(Unavailable(U::Incompatible)));
    let (mut connection, mut credentials) = loading(true, false);
    let ticket = credentials.issue(&connection).unwrap();
    assert_eq!(
        credentials.issued(&mut connection, ticket, Issued::OtherPrincipal, now),
        Outcome::OtherPrincipal
    );
    assert_eq!(credentials.current(), None);
    assert_eq!(connection.phase(), Some(Loading));
    // Issuing again in the same round: a late copy of the earlier answer
    // does not count.
    let again = credentials.issue(&connection).unwrap();
    assert_eq!(
        credentials.issued(&mut connection, ticket, GRANTED, now),
        Outcome::Ignored
    );
    credentials.issued(&mut connection, again, GRANTED, now);
    assert!(credentials.current().is_some());
}

#[test]
fn a_revocation_signal_on_a_discarded_credential_asks_for_a_recheck_and_moves_nothing() {
    let now = Instant::now();
    let (mut connection, mut credentials, old) = ready(now);
    let effects = effects(credentials.revoke(&mut connection, now));
    persisted(&mut connection, &effects).unwrap();
    let current = granted(&mut connection, &mut credentials, now);
    assert_eq!(
        credentials.response(&mut connection, old, Signal::Revoked, now),
        Outcome::Recheck
    );
    assert_eq!(connection.phase(), Some(Loading));
    assert_eq!(credentials.current(), Some(current));
    assert_eq!(connection.gate().reported(), 2);
}

#[test]
fn an_expired_credential_is_renewed_without_increment() {
    let now = Instant::now();
    let (mut connection, mut credentials, credential) = ready(now);
    let renew = effects(credentials.response(&mut connection, credential, Signal::Expired, now))
        .renew
        .unwrap();
    // One renewal at a time.
    assert_eq!(
        credentials.response(&mut connection, credential, Signal::Expired, now),
        Outcome::Done(Effects::default())
    );
    assert_eq!(
        credentials.issued(&mut connection, renew, GRANTED, now),
        Outcome::Done(Effects::default())
    );
    assert_ne!(credentials.current(), Some(credential));
    assert_eq!(connection.phase(), Some(Ready));
    assert_eq!(connection.gate().reported(), 1);
    // In an `unavailable` that retries, a renewal goes back to `loading`.
    let (mut connection, mut credentials, credential) = ready(now);
    probed(&mut connection, Probe::NoAnswer);
    let renew = effects(credentials.response(&mut connection, credential, Signal::Expired, now))
        .renew
        .unwrap();
    credentials.issued(&mut connection, renew, GRANTED, now);
    assert_eq!(connection.phase(), Some(Loading));
    // A renewal granted after a probe found the binding incompatible within
    // the round: adopted, the reason stays.
    let (mut connection, mut credentials, credential) = ready(now);
    let renew = effects(credentials.response(&mut connection, credential, Signal::Expired, now))
        .renew
        .unwrap();
    probed(&mut connection, Probe::Incompatible);
    credentials.issued(&mut connection, renew, GRANTED, now);
    assert_eq!(connection.phase(), Some(Unavailable(U::Incompatible)));
    assert_ne!(credentials.current(), Some(credential));
    // In one that does not, nothing is renewed.
    let (mut connection, mut credentials, credential) = ready(now);
    probed(&mut connection, Probe::Incompatible);
    assert_eq!(
        credentials.response(&mut connection, credential, Signal::Expired, now),
        Outcome::Done(Effects::default())
    );
}

#[test]
fn a_renewal_refused_is_revoked_unreachable_is_dependency_another_principal_is_handed_back() {
    let now = Instant::now();
    let renewal = |answer: Issued| {
        let (mut connection, mut credentials, credential) = ready(now);
        let renew =
            effects(credentials.response(&mut connection, credential, Signal::Expired, now))
                .renew
                .unwrap();
        let outcome = credentials.issued(&mut connection, renew, answer, now);
        (connection, credentials, outcome)
    };
    let (connection, _, outcome) = renewal(Issued::Refused);
    assert!(effects(outcome).invalidation.is_some());
    assert_eq!(connection.phase(), Some(Unavailable(U::LocalStorage)));
    let (connection, credentials, outcome) = renewal(Issued::Unreachable);
    assert_eq!(outcome, Outcome::Done(Effects::default()));
    assert_eq!(connection.phase(), Some(Unavailable(U::Dependency)));
    assert_eq!(credentials.current(), None);
    let (connection, credentials, outcome) = renewal(Issued::OtherPrincipal);
    assert_eq!(outcome, Outcome::OtherPrincipal);
    assert_eq!(connection.phase(), Some(Ready));
    assert!(credentials.current().is_some());
}

#[test]
fn an_invalid_credential_is_dropped_and_renewed_once_in_a_new_round() {
    let now = Instant::now();
    let (mut connection, mut credentials, credential) = ready(now);
    let (call, _) = connection.open_call().unwrap();
    let before = connection.round();
    assert_eq!(
        credentials.response(&mut connection, credential, Signal::Invalid, now),
        Outcome::Done(Effects::default())
    );
    // No increment, nothing in flight aborted; admission stops at once.
    assert_eq!(connection.gate().reported(), 1);
    assert_eq!(connection.gate().may_append(call), Ok(()));
    assert_eq!(connection.phase(), Some(Loading));
    assert!(!connection.is_current(before));
    assert_eq!(credentials.current(), None);
    // The renewal refused: credential_rejected, with a placeholder.
    let ticket = credentials.issue(&connection).unwrap();
    let failed = Effects {
        placeholder: true,
        ..Effects::default()
    };
    assert_eq!(
        credentials.issued(&mut connection, ticket, Issued::Refused, now),
        Outcome::Done(failed)
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::CredentialRejected)));
    assert_eq!(connection.gate().reported(), 1);
}

#[test]
fn the_renewal_fails_too_only_within_its_round() {
    let now = Instant::now();
    let failed = || {
        Outcome::Done(Effects {
            placeholder: true,
            ..Effects::default()
        })
    };
    // The new credential of that round invalid again.
    let (mut connection, mut credentials, credential) = ready(now);
    credentials.response(&mut connection, credential, Signal::Invalid, now);
    let renewed = granted(&mut connection, &mut credentials, now);
    assert_eq!(
        credentials.response(&mut connection, renewed, Signal::Invalid, now),
        failed()
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::CredentialRejected)));
    // In a later round it is a first one again.
    let (mut connection, mut credentials, credential) = ready(now);
    credentials.response(&mut connection, credential, Signal::Invalid, now);
    let renewed = granted(&mut connection, &mut credentials, now);
    connection.fail(U::Dependency);
    connection.start_loading();
    probed(&mut connection, READY);
    let later = now + Duration::from_secs(600);
    assert_eq!(
        credentials.response(&mut connection, renewed, Signal::Invalid, later),
        Outcome::Done(Effects::default())
    );
    assert_eq!(connection.phase(), Some(Loading));
    // An explicit revocation in the renewal is a revocation; an unreachable
    // issuer is `dependency`, without a placeholder.
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Unreachable, later);
    assert_eq!(outcome, Outcome::Done(Effects::default()));
    assert_eq!(connection.phase(), Some(Unavailable(U::Dependency)));
    let (mut connection, mut credentials, credential) = ready(now);
    credentials.response(&mut connection, credential, Signal::Invalid, now);
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Revoked, now);
    assert!(effects(outcome).invalidation.is_some());
    // A refusal in a later round is a revocation signal, not the renewal
    // failing.
    let (mut connection, mut credentials, credential) = ready(now);
    credentials.response(&mut connection, credential, Signal::Invalid, now);
    connection.fail(U::Dependency);
    connection.start_loading();
    let ticket = credentials.issue(&connection).unwrap();
    let outcome = credentials.issued(&mut connection, ticket, Issued::Refused, now);
    assert!(effects(outcome).invalidation.is_some());
}

#[test]
fn the_third_invalid_credential_within_ten_minutes_fails_at_once() {
    let start = Instant::now();
    let (mut connection, mut credentials, _) = ready(start);
    for minute in [0, 4] {
        let at = start + Duration::from_secs(minute * 60);
        let credential = credentials.current().unwrap();
        credentials.response(&mut connection, credential, Signal::Invalid, at);
        granted(&mut connection, &mut credentials, at);
        // A later round, so it would be a first one again.
        connection.fail(U::Dependency);
        connection.start_loading();
    }
    let credential = credentials.current().unwrap();
    let third = start + Duration::from_secs(9 * 60);
    let outcome = credentials.response(&mut connection, credential, Signal::Invalid, third);
    assert!(effects(outcome).placeholder);
    assert_eq!(connection.phase(), Some(Unavailable(U::CredentialRejected)));
}

#[test]
fn an_invalid_credential_in_a_reason_that_does_not_retry_changes_nothing() {
    let now = Instant::now();
    let (mut connection, mut credentials, credential) = ready(now);
    probed(&mut connection, Probe::Incompatible);
    assert_eq!(
        credentials.response(&mut connection, credential, Signal::Invalid, now),
        Outcome::Done(Effects::default())
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::Incompatible)));
    assert_eq!(credentials.current(), Some(credential));
}

#[test]
fn a_response_sent_with_a_discarded_credential_moves_nothing() {
    let now = Instant::now();
    let (mut connection, mut credentials, old) = ready(now);
    credentials.response(&mut connection, old, Signal::Invalid, now);
    let current = granted(&mut connection, &mut credentials, now);
    for signal in [Signal::Expired, Signal::Invalid] {
        assert_eq!(
            credentials.response(&mut connection, old, signal, now),
            Outcome::Stale
        );
    }
    assert_eq!(
        credentials.response(&mut connection, old, Signal::Revoked, now),
        Outcome::Recheck
    );
    assert_eq!(connection.phase(), Some(Loading));
    assert_eq!(credentials.current(), Some(current));
    assert_eq!(connection.gate().reported(), 1);
}
