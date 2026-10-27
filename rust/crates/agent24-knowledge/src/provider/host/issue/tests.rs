#![allow(clippy::unwrap_used)]

use std::time::Instant;

use super::super::tests::{C, H, T, host, of, phase, write};
use super::*;
use crate::provider::binding::BindingState;
use crate::provider::connection::Connection;
use crate::provider::credential::Credentials;
use crate::provider::gate::Gate;
use crate::provider::journal::{SealWrite, Written};
use crate::provider::member::Member;
use crate::provider::profile::{Profile, Slot};
use crate::provider::record::{Change, StuckCause};
use crate::provider::replay::{Entry, Record, replay};
use crate::provider::state::{ConnectionPhase, PlaceholderFailure, Switches, UnavailableCause};

/// The next write, confirmed: its changes and what the report gives.
fn confirmed(host: &mut H) -> (Vec<C>, Output<T>) {
    let (attempt, _, changes) = write(host);
    (changes, host.written(attempt, Written::Durable))
}

/// As [`confirmed`], keeping the record for a replay.
fn step(host: &mut H, log: &mut Vec<Record<T>>) -> Vec<C> {
    let (attempt, record) = host.take_write().unwrap();
    log.push(record);
    host.written(attempt, Written::Durable);
    match &log.last().unwrap().entry {
        Entry::Changes(changes) => changes.clone(),
        other => panic!("{other:?}"),
    }
}

/// The log before the host's: "a" bound to binding 7 at generation 1.
fn before() -> Vec<Record<T>> {
    let bind = C::Bind {
        of: of("a", 7),
        identity: "id",
        generation: 1,
        confirmed: None,
    };
    let entry = Entry::Changes(vec![C::On, C::Select("a"), bind]);
    vec![Record { seq: 9, entry }]
}

fn answer(host: &mut H, answer: Issued) -> Output<T> {
    let ticket = host.issue(&"a").unwrap();
    host.issued("a", ticket, answer, Instant::now())
}

fn stuck(cause: StuckCause) -> C {
    C::Stuck {
        of: of("a", 7),
        cause,
    }
}

fn unavailable(cause: UnavailableCause) -> Option<ConnectionPhase> {
    Some(ConnectionPhase::Unavailable(cause))
}

/// "a" bound to binding 7 and `loading`, never servable: S3.1's exception
/// applies.
fn never_servable() -> H {
    let mut binding = BindingState::new(
        Connection::new(Gate::new(1), false),
        Credentials::new(false),
        true,
    );
    let clear = Switches {
        off: false,
        profile_disabled: false,
    };
    assert!(binding.connect(clear).is_some());
    let profile = Profile::new(Slot::Bound(Box::new(binding)), true, None);
    let mut host = H::new(false, true, 10, 1);
    host.insert(Member::new("a", profile, Some(7), false));
    host
}

/// S3.1: a `loading` round's issuance, one at a time; none for a
/// placeholder or a profile not known.
#[test]
fn an_issuance_is_due_once_per_loading_round() {
    let mut host = host();
    assert!(host.issue(&"a").is_some());
    assert!(host.issue(&"a").is_none());
    assert!(host.issue(&"b").is_none());
    assert!(host.issue(&"x").is_none());
}

/// A credential granted is adopted, and nothing is written.
#[test]
fn a_grant_writes_nothing() {
    let mut host = host();
    let out = answer(&mut host, Issued::Granted { narrowed: false });
    assert_eq!(out, Output::default());
    assert!(host.take_write().is_none());
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
}

/// S3.1's exception: refused before ever servable or revoked, the binding is
/// stuck with `credential_rejected`, written, without a revocation, an
/// increment or a placeholder; as a restart reads it.
#[test]
fn a_refusal_under_the_exception_is_written_as_stuck() {
    let mut log = before();
    let mut host = never_servable();
    let out = answer(&mut host, Issued::Refused);
    assert!(matches!(out.seal, Some(SealWrite::Delete(_))));
    let rejected = unavailable(UnavailableCause::CredentialRejected);
    assert_eq!(phase(&host, "a"), rejected);
    assert_eq!(
        step(&mut host, &mut log),
        vec![stuck(StuckCause::CredentialRejected)]
    );
    let replayed = replay(9, log).unwrap();
    let a = &replayed.bindings[0];
    assert_eq!(a.stuck, Some(StuckCause::CredentialRejected));
    assert!(!a.revoked);
    assert_eq!(a.generation, 1);
}

/// S3.1, S4.5, S5.6: a revocation while `loading` invalidates and places a
/// rule, is stuck with `access_revoked` and ends the exception, all in one
/// record; the generation takes effect once it is confirmed.
#[test]
fn a_revocation_while_loading_is_written_with_its_generation() {
    let mut log = before();
    let mut host = host();
    answer(&mut host, Issued::Revoked);
    let revoked = unavailable(UnavailableCause::AccessRevoked);
    assert_eq!(phase(&host, "a"), revoked);
    let changes = step(&mut host, &mut log);
    let generation = C::Generation {
        of: of("a", 7),
        value: 2,
    };
    assert!(changes.contains(&generation));
    assert!(changes.iter().any(|c| matches!(c, C::Placed { .. })));
    assert!(changes.contains(&stuck(StuckCause::AccessRevoked)));
    assert!(changes.contains(&C::Revoked(of("a", 7))));
    let replayed = replay(9, log).unwrap();
    let a = &replayed.bindings[0];
    assert_eq!(a.stuck, Some(StuckCause::AccessRevoked));
    assert!(a.revoked);
    assert_eq!(a.generation, 2);
}

/// S3.1: only the first revocation-class event is written as such; a
/// narrowed grant renews in a new round once its generation is confirmed
/// (S4.5), not stuck.
#[test]
fn only_the_first_revocation_is_written_as_such() {
    let mut host = host();
    answer(&mut host, Issued::Granted { narrowed: true });
    let storage = unavailable(UnavailableCause::LocalStorage);
    assert_eq!(phase(&host, "a"), storage);
    let (changes, _) = confirmed(&mut host);
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
    assert!(changes.contains(&C::Revoked(of("a", 7))));
    assert!(!changes.iter().any(|c| matches!(c, C::Stuck { .. })));

    answer(&mut host, Issued::Revoked);
    let (changes, _) = confirmed(&mut host);
    assert!(!changes.contains(&C::Revoked(of("a", 7))));
    assert!(changes.contains(&stuck(StuckCause::AccessRevoked)));
}

/// S1.10: an unreachable issuer is `dependency`, retried by backoff: nothing
/// is written.
#[test]
fn an_unreachable_issuer_writes_nothing() {
    let mut host = host();
    let out = answer(&mut host, Issued::Unreachable);
    assert_eq!(out, Output::default());
    assert!(host.take_write().is_none());
    let dependency = unavailable(UnavailableCause::Dependency);
    assert_eq!(phase(&host, "a"), dependency);
}

/// An answer to a round already ended changes nothing.
#[test]
fn an_answer_out_of_its_round_changes_nothing() {
    let mut host = host();
    let ticket = host.issue(&"a").unwrap();
    host.disable("a");
    confirmed(&mut host);
    let out = host.issued("a", ticket, Issued::Revoked, Instant::now());
    assert_eq!(out, Output::default());
    assert!(host.take_write().is_none());
}

/// S3.5: granted for another principal, the binding is retired awaiting the
/// user's confirmation, and the placeholder logs in again by backoff.
#[test]
fn another_principal_retires_the_binding() {
    let mut host = host();
    answer(&mut host, Issued::OtherPrincipal);
    let (changes, _) = confirmed(&mut host);
    assert_eq!(changes[0], Change::Retire(of("a", 7)));
    assert!(changes.iter().any(|c| matches!(c, C::Await { .. })));
    let Slot::Placeholder(a) = host.profile(&"a").unwrap().slot() else {
        panic!("a is bound");
    };
    assert_eq!(a.failure(), Some(PlaceholderFailure::Dependency));
}
