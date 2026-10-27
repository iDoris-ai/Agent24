#![allow(clippy::unwrap_used)]

use super::super::tests::{C, H, T, durable, host, loading, of, write};
use super::*;
use crate::provider::binding::BindingState;
use crate::provider::connection::Connection;
use crate::provider::credential::Credentials;
use crate::provider::gate::Gate;
use crate::provider::host::Created;
use crate::provider::journal::Written;
use crate::provider::member::Member;
use crate::provider::placeholder::Placeholder;
use crate::provider::profile::{IdentityChange, Profile, Slot};
use crate::provider::replay::{Entry, Record, replay};

/// "a" bound to 7 ("ia") and loading; "b" a selected placeholder; "c" bound
/// to 8 ("ic"), selected and disabled; "d" bound to 9 ("id"), not selected.
/// Off is off and the seal present; the log's next record is 10.
fn runs() -> H {
    let mut host = H::new(false, true, 10, 1);
    host.insert(Member::new("a", loading(), Some((7, "ia")), false));
    let placeholder = Slot::Placeholder(Placeholder::new(None));
    host.insert(Member::new(
        "b",
        Profile::new(placeholder, true, None),
        None,
        false,
    ));
    host.insert(Member::new("c", loading(), Some((8, "ic")), true));
    let binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        false,
    );
    let d = Profile::new(Slot::Bound(Box::new(binding)), false, None);
    host.insert(Member::new("d", d, Some((9, "id")), false));
    host
}

fn candidates() -> Vec<(&'static str, &'static str)> {
    vec![("a", "ia"), ("c", "ic")]
}

fn bound(bindings: Vec<Of<T>>) -> C {
    C::RunBound {
        run: 1,
        bindings,
        candidates: candidates(),
    }
}

/// The next write, confirmed, kept for a replay.
fn step(host: &mut H, log: &mut Vec<Record<T>>) -> Vec<C> {
    let (attempt, record) = host.take_write().unwrap();
    log.push(record);
    host.written(attempt, Written::Durable);
    match &log.last().unwrap().entry {
        Entry::Changes(changes) => changes.clone(),
        other => panic!("{other:?}"),
    }
}

/// S4.7, I1: a run binds the bindings active at its start and the identity
/// of each selected profile's binding, disabled or not; its calls are
/// `local_storage` until that is confirmed persisted.
#[test]
fn a_run_binds_the_active_bindings_and_the_selected_candidates() {
    let mut host = runs();
    host.run_started(1);
    assert_eq!(host.run_call(&1), Some(RunCall::Unpersisted));
    let (attempt, _, changes) = write(&mut host);
    assert_eq!(changes, vec![bound(vec![of("a", 7)])]);
    assert_eq!(host.run_call(&1), Some(RunCall::Unpersisted));
    host.written(attempt, Written::Durable);
    assert_eq!(host.run_call(&1), Some(RunCall::Bound(vec![of("a", 7)])));
    assert!(host.take_write().is_none());
}

/// S4.7: the set is the run's, whatever later happens to its bindings; the
/// run is bound once.
#[test]
fn a_run_keeps_its_set() {
    let mut host = runs();
    host.run_started(1);
    durable(&mut host);
    host.disable("a");
    durable(&mut host);
    host.run_started(1);
    assert!(host.take_write().is_none());
    assert_eq!(host.run_call(&1), Some(RunCall::Bound(vec![of("a", 7)])));
}

/// S4.7: under Off a run binds no binding but its candidates; once On, a
/// call admitted on the matching binding backfills it, once, and the run's
/// calls are `local_storage` until that is confirmed; as a restart reads it.
#[test]
fn under_off_a_run_is_backfilled_once_on() {
    let mut log = Vec::new();
    let mut host = runs();
    host.turn_off();
    step(&mut host, &mut log);
    host.run_started(1);
    assert_eq!(step(&mut host, &mut log), vec![bound(vec![])]);
    assert_eq!(host.run_call(&1), Some(RunCall::Empty));
    // Not active while Off: nothing to backfill with.
    host.backfill(&1, of("a", 7));
    assert!(host.take_write().is_none());

    host.turn_on().unwrap();
    step(&mut host, &mut log);
    assert_eq!(host.run_call(&1), Some(RunCall::Candidate(of("a", 7))));
    // A call refused locally does not use it up.
    assert_eq!(host.run_call(&1), Some(RunCall::Candidate(of("a", 7))));
    assert!(host.take_write().is_none());

    host.backfill(&1, of("a", 7));
    assert_eq!(host.run_call(&1), Some(RunCall::Unpersisted));
    host.backfill(&1, of("a", 7));
    let backfilled = C::RunBackfilled {
        run: 1,
        of: of("a", 7),
    };
    assert_eq!(step(&mut host, &mut log), vec![backfilled]);
    assert_eq!(host.run_call(&1), Some(RunCall::Bound(vec![of("a", 7)])));
    host.backfill(&1, of("a", 7));
    assert!(host.take_write().is_none());

    let replayed = replay(10, log).unwrap();
    assert_eq!(
        replayed.runs,
        vec![(1, Some((vec![of("a", 7)], candidates())))]
    );
}

/// S4.7: only an active binding matching a candidate exactly backfills; a
/// restored run is persisted, and one unbound binds nothing.
#[test]
fn a_backfill_matches_a_candidate_exactly() {
    let mut host = runs();
    host.restore_run(2, Some((vec![], vec![("a", "other"), ("c", "ic")])));
    assert_eq!(host.run_call(&2), Some(RunCall::Empty));
    host.backfill(&2, of("a", 7));
    host.backfill(&2, of("c", 8));
    assert!(host.take_write().is_none());

    host.restore_run(3, Some((vec![], vec![("a", "ia")])));
    assert_eq!(host.run_call(&3), Some(RunCall::Candidate(of("a", 7))));
    host.backfill(&3, of("a", 8));
    assert!(host.take_write().is_none());

    host.restore_run(4, None);
    assert_eq!(host.run_call(&4), Some(RunCall::Empty));
    host.restore_run(5, Some((vec![of("c", 8)], vec![])));
    assert_eq!(host.run_call(&5), Some(RunCall::Bound(vec![of("c", 8)])));
    host.run_started(5);
    assert!(host.take_write().is_none());
}

/// I1: a set refused while a tightening over its profiles is unconfirmed is
/// written again at the run's next call, as it was at the start.
#[test]
fn a_refused_set_is_written_again_at_the_next_call() {
    let mut host = runs();
    host.disable("a");
    host.run_started(1);
    let (attempt, _, _) = write(&mut host);
    assert_eq!(host.run_call(&1), Some(RunCall::Unpersisted));
    host.written(attempt, Written::Durable);
    assert!(host.take_write().is_none());

    assert_eq!(host.run_call(&1), Some(RunCall::Unpersisted));
    let (attempt, _, changes) = write(&mut host);
    assert_eq!(changes, vec![bound(vec![])]);
    host.written(attempt, Written::Durable);
    assert_eq!(host.run_call(&1), Some(RunCall::Empty));
}

/// A failed write is taken again; the run waits for it.
#[test]
fn a_failed_set_is_written_again() {
    let mut host = runs();
    host.run_started(1);
    let (attempt, seq, _) = write(&mut host);
    host.written(attempt, Written::Failed);
    assert_eq!(host.run_call(&1), Some(RunCall::Unpersisted));
    let (attempt, again, changes) = write(&mut host);
    assert_eq!((again, changes), (seq, vec![bound(vec![of("a", 7)])]));
    host.written(attempt, Written::Durable);
    assert_eq!(host.run_call(&1), Some(RunCall::Bound(vec![of("a", 7)])));
}

#[test]
fn an_unknown_run_has_no_call() {
    let mut host = runs();
    assert_eq!(host.run_call(&1), None);
    host.backfill(&1, of("a", 7));
    assert!(host.take_write().is_none());
}

/// S2.4, S3.5: a retired binding is no candidate; a new one points at the
/// identity it was created with.
#[test]
fn a_candidate_is_the_identity_of_the_binding_now() {
    let mut host = host();
    host.identity_changed("a", IdentityChange::Endpoint, None);
    durable(&mut host);
    let attempt = host.attempt(&"b").unwrap();
    let created = Created {
        binding: 11,
        identity: "ib",
        principal: "alice",
    };
    host.identified("b", attempt, created);
    durable(&mut host);
    host.run_started(1);
    let (_, _, changes) = write(&mut host);
    let set = C::RunBound {
        run: 1,
        bindings: vec![of("b", 11)],
        candidates: vec![("b", "ib")],
    };
    assert_eq!(changes, vec![set]);
}
