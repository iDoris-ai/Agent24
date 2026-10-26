#![allow(clippy::unwrap_used)]

use super::super::tests::{C, H, T, durable, host, of, phase, write};
use super::*;
use crate::provider::journal::{SealWrite, Written};
use crate::provider::member::Member;
use crate::provider::placeholder::Placeholder;
use crate::provider::profile::{Profile, Slot};
use crate::provider::state::ConnectionPhase;

fn created(binding: u32, principal: &'static str) -> Created<T> {
    Created {
        binding,
        identity: "id",
        principal,
    }
}

fn bind(binding: u32, generation: u64, confirmed: Option<&'static str>) -> C {
    C::Bind {
        of: of("b", binding),
        identity: "id",
        generation,
        confirmed,
    }
}

/// The next write, confirmed: its changes and what the report gives.
fn confirmed(host: &mut H) -> (Vec<C>, Output<T>) {
    let (attempt, _, changes) = write(host);
    (changes, host.written(attempt, Written::Durable))
}

fn confirmed_principal(host: &H, id: &'static str) -> Option<&'static str> {
    let member = host.members.iter().find(|m| *m.id() == id).unwrap();
    member.confirmed().copied()
}

fn bound(host: &H, id: &'static str) -> bool {
    matches!(host.profile(&id).unwrap().slot(), Slot::Bound(_))
}

fn failure(host: &H, id: &'static str) -> Option<PlaceholderFailure> {
    match host.profile(&id).unwrap().slot() {
        Slot::Placeholder(placeholder) => placeholder.failure(),
        Slot::Bound(_) => panic!("{id} is bound"),
    }
}

/// "b" alone, a selected placeholder, with `confirmed` its last confirmed
/// principal and `confirmation` awaited; the log holds values up to
/// `highest`.
fn placeholder(
    highest: u64,
    confirmed: Option<&'static str>,
    confirmation: Option<Confirmation>,
) -> H {
    let mut host = H::new(false, true, 1, highest);
    let slot = Slot::Placeholder(Placeholder::new(None));
    let b = Profile::new(slot, true, confirmation);
    host.insert(Member::new("b", b, None, false).with_confirmed(confirmed));
    host
}

/// S2.5, I1: the new binding is written with its first value and binds,
/// selected and unblocked, once that is confirmed; a principal recorded
/// without a confirmation pending is confirmed (S1.13).
#[test]
fn a_new_binding_binds_once_its_first_value_is_confirmed() {
    let mut host = host();
    let attempt = host.attempt(&"b").unwrap();
    assert!(host.attempt(&"a").is_none());
    let out = host.identified("b", attempt, created(8, "alice"));
    assert_eq!(out, Output::default());
    assert_eq!(write(&mut host).2, vec![bind(8, 2, Some("alice"))]);
    assert!(!bound(&host, "b"));

    let mut host = host_with_creation();
    let out = durable(&mut host);
    assert!(bound(&host, "b"));
    assert_eq!(phase(&host, "b"), Some(ConnectionPhase::Loading));
    assert_eq!(out.loading.len(), 1);
    assert_eq!(out.loading[0].0, "b");
    assert!(host.take_write().is_none());
    assert_eq!(confirmed_principal(&host, "b"), Some("alice"));

    // Its id is the binding's from now on.
    host.disable("b");
    let generation = C::Generation {
        of: of("b", 8),
        value: 3,
    };
    assert!(write(&mut host).2.contains(&generation));
}

fn host_with_creation() -> H {
    let mut host = host();
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    host
}

/// S4.4, A.4 (G1, G2): the first value goes past every value written,
/// confirmed or not and of any binding; with none, it is 1.
#[test]
fn the_first_value_goes_past_every_value_written() {
    let mut host = host();
    // "a" (generation 1) is invalidated to 2, not yet confirmed.
    host.disable("a");
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    durable(&mut host);
    assert_eq!(write(&mut host).2, vec![bind(8, 3, Some("alice"))]);

    let mut host = placeholder(0, None, None);
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    assert_eq!(write(&mut host).2, vec![bind(8, 1, Some("alice"))]);

    // No value past the last: nothing is written.
    let mut host = placeholder(u64::MAX, None, None);
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    assert!(host.take_write().is_none());
    assert_eq!(failure(&host, "b"), Some(PlaceholderFailure::Dependency));
}

/// S1.13: another principal than the confirmed one awaits confirmation, a
/// tightening; the binding is refused meanwhile (I1) and, created later,
/// waits unconfirmed and inactive.
#[test]
fn another_principal_awaits_confirmation_before_its_binding() {
    let mut host = placeholder(1, Some("alice"), None);
    let attempt = host.attempt(&"b").unwrap();
    let out = host.identified("b", attempt, created(8, "bob"));
    assert!(matches!(out.seal, Some(SealWrite::Delete(_))));
    let awaited = C::Await {
        profile: "b",
        awaited: Confirmation::PrincipalChanged,
    };
    assert_eq!(confirmed(&mut host).0, vec![awaited]);
    assert_eq!(failure(&host, "b"), Some(PlaceholderFailure::Dependency));
    assert!(host.take_write().is_none());

    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(9, "bob"));
    let (changes, out) = confirmed(&mut host);
    assert_eq!(changes, vec![bind(9, 2, None)]);
    assert_eq!(confirmed_principal(&host, "b"), Some("alice"));
    assert!(bound(&host, "b"));
    assert!(out.loading.is_empty());

    // The confirmed principal again: nothing awaits.
    let mut host = placeholder(1, Some("alice"), None);
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    assert_eq!(write(&mut host).2, vec![bind(8, 2, Some("alice"))]);
}

/// Under `unsaved_settings` a binding never binds until the host's
/// confirmation (S1.13): nothing is written.
#[test]
fn under_unsaved_settings_nothing_is_written() {
    let unsaved = Some(Confirmation::UnsavedSettings);
    let mut host = placeholder(1, None, unsaved);
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    assert!(host.take_write().is_none());
    assert_eq!(failure(&host, "b"), Some(PlaceholderFailure::Dependency));
}

/// I1: a tightening of the profile unconfirmed refuses the binding; another
/// profile's does not.
#[test]
fn an_unconfirmed_tightening_of_the_profile_refuses_the_binding() {
    let mut host = host();
    host.unselect("b");
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    assert_eq!(confirmed(&mut host).0, vec![C::Unselect("b")]);
    assert!(host.take_write().is_none());
    assert_eq!(failure(&host, "b"), Some(PlaceholderFailure::Dependency));
}

/// A.5: the binding written for an attempt superseded meanwhile, or for a
/// profile no longer there, never binds; it is retired at once, a tightening
/// that deletes the seal.
#[test]
fn a_binding_whose_attempt_was_superseded_is_retired() {
    let mut host = host_with_creation();
    host.attempt(&"b").unwrap();
    let out = durable(&mut host);
    assert!(!bound(&host, "b"));
    assert!(out.loading.is_empty());
    assert!(matches!(out.seal, Some(SealWrite::Delete(_))));
    assert_eq!(write(&mut host).2, vec![C::Retire(of("b", 8))]);

    // The later attempt's binding goes past the first's value, and binds.
    let mut host = host_with_creation();
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(9, "alice"));
    durable(&mut host);
    let (changes, out) = confirmed(&mut host);
    assert_eq!(changes, vec![bind(9, 3, Some("alice"))]);
    assert_eq!(out.loading.len(), 1);
    assert_eq!(write(&mut host).2, vec![C::Retire(of("b", 8))]);
}

/// S2.5, S1.10: a failed attempt's reason is written, a tightening, but
/// `dependency`, kept in memory; the binding created later clears the
/// written one in the same write.
#[test]
fn a_failed_attempt_writes_its_reason_and_the_binding_clears_it() {
    let mut host = host();
    let attempt = host.attempt(&"b").unwrap();
    host.attempt_failed("b", attempt, Attempted::Unreachable);
    assert!(host.take_write().is_none());
    assert_eq!(failure(&host, "b"), Some(PlaceholderFailure::Dependency));
    // Never written, so nothing to clear.
    let mut unreachable = host_after(Attempted::Unreachable);
    let attempt = unreachable.attempt(&"b").unwrap();
    unreachable.identified("b", attempt, created(8, "alice"));
    assert_eq!(write(&mut unreachable).2, vec![bind(8, 2, Some("alice"))]);

    let attempt = host.attempt(&"b").unwrap();
    let out = host.attempt_failed("b", attempt, Attempted::Rejected);
    assert!(matches!(out.seal, Some(SealWrite::Delete(_))));
    let rejected = PlaceholderFailure::CredentialRejected;
    let failed = C::PlaceholderFailed {
        profile: "b",
        failure: rejected,
    };
    assert_eq!(confirmed(&mut host).0, vec![failed]);

    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    let cleared = C::PlaceholderCleared {
        profile: "b",
        failure: rejected,
    };
    assert_eq!(write(&mut host).2, vec![bind(8, 2, Some("alice")), cleared]);
}

fn host_after(attempted: Attempted) -> H {
    let mut host = host();
    let attempt = host.attempt(&"b").unwrap();
    host.attempt_failed("b", attempt, attempted);
    host
}

/// Only the attempt in flight counts.
#[test]
fn an_answer_to_an_earlier_attempt_changes_nothing() {
    let mut host = host();
    let earlier = host.attempt(&"b").unwrap();
    host.attempt(&"b").unwrap();
    host.identified("b", earlier, created(8, "alice"));
    host.attempt_failed("b", earlier, Attempted::Rejected);
    assert!(host.take_write().is_none());
    assert_eq!(failure(&host, "b"), None);
}

/// Off (S1.2): the binding binds but does not connect.
#[test]
fn a_binding_created_while_off_does_not_connect() {
    let mut host = host();
    host.turn_off();
    durable(&mut host);
    let attempt = host.attempt(&"b").unwrap();
    host.identified("b", attempt, created(8, "alice"));
    let out = durable(&mut host);
    assert!(bound(&host, "b"));
    assert!(out.loading.is_empty());
    assert_eq!(phase(&host, "b"), None);
}
