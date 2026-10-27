#![allow(clippy::unwrap_used)]

use super::super::tests::{C, H, T, host, of, phase, write};
use super::*;
use crate::provider::binding::BindingState;
use crate::provider::connection::Connection;
use crate::provider::credential::Credentials;
use crate::provider::gate::Gate;
use crate::provider::journal::SealWrite;
use crate::provider::member::Member;
use crate::provider::profile::{Profile, Slot};
use crate::provider::replay::{Entry, Record, replay};
use crate::provider::state::{ConnectionPhase, PlaceholderFailure};

fn created(binding: u32, principal: &'static str) -> Created<T> {
    Created {
        binding,
        identity: "id2",
        principal,
    }
}

fn bind(binding: u32, generation: u64, confirmed: Option<&'static str>) -> C {
    C::Bind {
        of: of("a", binding),
        identity: "id2",
        generation,
        confirmed,
    }
}

fn bound(host: &H) -> bool {
    matches!(host.profile(&"a").unwrap().slot(), Slot::Bound(_))
}

fn failure(host: &H) -> Option<PlaceholderFailure> {
    match host.profile(&"a").unwrap().slot() {
        Slot::Placeholder(placeholder) => placeholder.failure(),
        Slot::Bound(_) => panic!("a is bound"),
    }
}

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

/// The log before the host's, as [`restored`] holds it: On, "a" selected
/// and bound to binding 7 at generation 1, "alice" confirmed.
fn before() -> Vec<Record<T>> {
    let bind = C::Bind {
        of: of("a", 7),
        identity: "id",
        generation: 1,
        confirmed: Some("alice"),
    };
    let entry = Entry::Changes(vec![C::On, C::Select("a"), bind]);
    vec![Record { seq: 9, entry }]
}

/// What a restart from [`before`] holds; "a" is loading.
fn restored() -> H {
    let mut host = awaiting(None);
    let a = host.members.iter_mut().find(|m| *m.id() == "a").unwrap();
    a.confirm_principal(Some("alice"));
    host
}

/// Memory and a replay of `log` agree on "a"'s principal, selection and
/// awaited confirmation, and on Off.
fn agree(host: &H, log: Vec<Record<T>>) {
    let replayed = replay(9, log).unwrap();
    let a = host.members.iter().find(|m| *m.id() == "a").unwrap();
    assert_eq!(replayed.confirmed.get("a"), a.confirmed());
    let state = &replayed.profiles["a"];
    assert_eq!(state.selected, a.profile().selected());
    assert_eq!(state.awaiting, a.profile().confirmation());
    assert_eq!(replayed.off, host.is_off());
}

fn placed(changes: &[C]) -> bool {
    let retired = of("a", 7);
    changes
        .iter()
        .any(|c| matches!(c, C::Placed { of, .. } if *of == retired))
}

fn awaits(changes: &[C]) -> bool {
    changes.iter().any(|c| matches!(c, C::Await { .. }))
}

/// S3.5, S4.4, I1: another instance retires the binding at once, invalidated
/// in the connection phase and its evidence placeholders; the new binding is
/// written only once that is confirmed, past the retired one's values.
#[test]
fn another_instance_retires_the_binding_and_binds_after_it() {
    let mut log = before();
    let mut host = restored();
    let change = IdentityChange::Instance;
    let out = host.identity_changed("a", change, Some(created(9, "alice")));
    assert!(matches!(out.seal, Some(SealWrite::Delete(_))));
    assert!(!bound(&host));

    let changes = step(&mut host, &mut log);
    assert_eq!(changes[0], C::Retire(of("a", 7)));
    let generation = C::Generation {
        of: of("a", 7),
        value: 2,
    };
    assert!(changes.contains(&generation));
    assert!(placed(&changes));
    assert!(!awaits(&changes));
    assert!(!bound(&host));

    assert_eq!(step(&mut host, &mut log), vec![bind(9, 3, Some("alice"))]);
    assert!(bound(&host));
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
    assert!(host.take_write().is_none());

    let replayed = replay(9, log.clone()).unwrap();
    agree(&host, log);
    let found = |binding| replayed.bindings.iter().find(|b| b.of == of("a", binding));
    assert!(found(7).unwrap().retired);
    let new = found(9).unwrap();
    assert!(!new.retired);
    assert_eq!(new.generation, 3);
}

/// S3.5, S1.13: another principal awaits the user's confirmation, written
/// with the retirement; the new binding waits unconfirmed and inactive, in
/// memory as a restart reads it. Another instance bringing another principal
/// is one too.
#[test]
fn another_principal_awaits_confirmation_and_its_binding_waits() {
    for change in [IdentityChange::Principal, IdentityChange::Instance] {
        let mut log = before();
        let mut host = restored();
        host.identity_changed("a", change, Some(created(9, "bob")));
        let awaited = C::Await {
            profile: "a",
            awaited: Confirmation::PrincipalChanged,
        };
        assert!(step(&mut host, &mut log).contains(&awaited), "{change:?}");
        assert_eq!(step(&mut host, &mut log), vec![bind(9, 3, None)]);
        assert!(bound(&host));
        assert_eq!(phase(&host, "a"), None);

        let awaiting = Some(Confirmation::PrincipalChanged);
        assert_eq!(host.profile(&"a").unwrap().confirmation(), awaiting);
        agree(&host, log);
    }
}

/// S3.5: another endpoint or tenant leaves the placeholder, to log in again.
#[test]
fn another_endpoint_leaves_the_placeholder() {
    let mut host = host();
    host.identity_changed("a", IdentityChange::Endpoint, None);
    let (changes, _) = confirmed(&mut host);
    assert_eq!(changes[0], C::Retire(of("a", 7)));
    assert!(host.take_write().is_none());
    assert!(!bound(&host));
    assert_eq!(failure(&host), None);
    assert!(host.attempt(&"a").is_some());
}

/// S3.5, S2.5: a retirement not confirmed drops the new binding, and the
/// placeholder retries by backoff; the retirement itself is taken again.
#[test]
fn an_unconfirmed_retirement_drops_the_new_binding() {
    let mut host = host();
    let change = IdentityChange::Instance;
    host.identity_changed("a", change, Some(created(9, "alice")));
    let (attempt, seq, changes) = write(&mut host);
    host.written(attempt, Written::Failed);
    assert_eq!(failure(&host), Some(PlaceholderFailure::Dependency));

    let (again, seq_again, changes_again) = write(&mut host);
    assert_eq!((seq_again, changes_again), (seq, changes));
    host.written(again, Written::Durable);
    assert!(host.take_write().is_none());
    assert!(!bound(&host));
}

/// Without the new binding's identity, the placeholder retries with
/// `dependency`.
#[test]
fn a_new_instance_without_its_binding_retries() {
    let mut host = host();
    host.identity_changed("a", IdentityChange::Instance, None);
    assert_eq!(failure(&host), Some(PlaceholderFailure::Dependency));
    confirmed(&mut host);
    assert!(host.take_write().is_none());
    assert!(!bound(&host));
}

/// S2.5: a later attempt on the placeholder drops the waiting binding.
#[test]
fn a_later_attempt_drops_the_waiting_binding() {
    let mut host = host();
    let change = IdentityChange::Instance;
    host.identity_changed("a", change, Some(created(9, "alice")));
    host.attempt(&"a").unwrap();
    confirmed(&mut host);
    assert!(host.take_write().is_none());
    assert!(!bound(&host));
}

/// S3.5 on a placeholder: nothing to retire, nothing written; the attempt in
/// flight no longer counts and is retried by backoff.
#[test]
fn on_a_placeholder_the_attempt_in_flight_no_longer_counts() {
    let mut host = host();
    let attempt = host.attempt(&"b").unwrap();
    let change = IdentityChange::Instance;
    let out = host.identity_changed("b", change, Some(created(9, "alice")));
    assert_eq!(out, Output::default());
    assert!(host.take_write().is_none());
    let late = Created {
        binding: 8,
        identity: "id",
        principal: "alice",
    };
    host.identified("b", attempt, late);
    assert!(host.take_write().is_none());
    let Slot::Placeholder(b) = host.profile(&"b").unwrap().slot() else {
        panic!("b is bound");
    };
    assert_eq!(b.failure(), Some(PlaceholderFailure::Dependency));

    // Another endpoint waits for a new login instead.
    let mut host = super::super::tests::host();
    host.attempt(&"b").unwrap();
    host.identity_changed("b", IdentityChange::Endpoint, None);
    let Slot::Placeholder(b) = host.profile(&"b").unwrap().slot() else {
        panic!("b is bound");
    };
    assert_eq!(b.failure(), None);
}

/// S3.5: another change while the retirement is unconfirmed drops the new
/// binding and retries by backoff; the retry binds after the retirement.
#[test]
fn a_change_while_the_retirement_is_unconfirmed_retries() {
    let mut host = host();
    let change = IdentityChange::Instance;
    host.identity_changed("a", change, Some(created(9, "alice")));
    let out = host.identity_changed("a", change, Some(created(10, "alice")));
    assert_eq!(out, Output::default());
    assert_eq!(failure(&host), Some(PlaceholderFailure::Dependency));
    assert_eq!(confirmed(&mut host).0[0], C::Retire(of("a", 7)));
    assert!(host.take_write().is_none());

    let attempt = host.attempt(&"a").unwrap();
    host.identified("a", attempt, created(10, "alice"));
    assert_eq!(confirmed(&mut host).0, vec![bind(10, 3, Some("alice"))]);
    assert!(bound(&host));
}

/// S3.5: outside the connection phase the retirement does not invalidate;
/// a disabled profile's new binding does not connect.
#[test]
fn outside_the_connection_phase_nothing_is_invalidated() {
    let mut host = host();
    host.disable("a");
    confirmed(&mut host);
    let change = IdentityChange::Instance;
    host.identity_changed("a", change, Some(created(9, "alice")));
    let (changes, _) = confirmed(&mut host);
    assert_eq!(changes[0], C::Retire(of("a", 7)));
    assert!(!changes.iter().any(|c| matches!(c, C::Generation { .. })));
    let (changes, out) = confirmed(&mut host);
    assert_eq!(changes, vec![bind(9, 3, Some("alice"))]);
    assert!(out.loading.is_empty());
    assert!(bound(&host));
}

/// "a" bound to binding 7 and loading, awaiting `confirmation`.
fn awaiting(confirmation: Option<Confirmation>) -> H {
    let mut binding = BindingState::new(
        Connection::new(Gate::new(1), true),
        Credentials::new(false),
        true,
    );
    let clear = crate::provider::state::Switches {
        off: false,
        profile_disabled: false,
    };
    binding.connect(clear);
    let slot = Slot::Bound(Box::new(binding));
    let profile = Profile::new(slot, true, confirmation);
    let mut host = H::new(false, true, 10, 1);
    host.insert(Member::new("a", profile, Some(7), false));
    host
}

/// S1.13: under `unsaved_settings` nothing binds until the host confirms;
/// it stays in the place of `principal_changed`.
#[test]
fn under_unsaved_settings_the_waiting_binding_is_not_written() {
    let mut host = awaiting(Some(Confirmation::UnsavedSettings));
    let change = IdentityChange::Principal;
    host.identity_changed("a", change, Some(created(9, "bob")));
    let (changes, _) = confirmed(&mut host);
    assert!(!awaits(&changes));
    assert!(host.take_write().is_none());
    assert_eq!(failure(&host), Some(PlaceholderFailure::Dependency));
    let awaiting = Some(Confirmation::UnsavedSettings);
    assert_eq!(host.profile(&"a").unwrap().confirmation(), awaiting);
}

/// S1.13: a principal already awaited is not written again.
#[test]
fn an_awaited_principal_is_not_awaited_again() {
    let mut host = awaiting(Some(Confirmation::PrincipalChanged));
    let change = IdentityChange::Principal;
    host.identity_changed("a", change, Some(created(9, "bob")));
    let (changes, _) = confirmed(&mut host);
    assert_eq!(changes[0], C::Retire(of("a", 7)));
    assert!(!awaits(&changes));
    assert_eq!(confirmed(&mut host).0, vec![bind(9, 3, None)]);
}

/// I1: another tightening confirmed first leaves the new binding waiting for
/// its own retirement.
#[test]
fn another_write_does_not_release_the_waiting_binding() {
    let mut host = host();
    host.unselect("b");
    let change = IdentityChange::Instance;
    host.identity_changed("a", change, Some(created(9, "alice")));
    assert_eq!(confirmed(&mut host).0, vec![C::Unselect("b")]);
    assert_eq!(confirmed(&mut host).0[0], C::Retire(of("a", 7)));
    assert_eq!(confirmed(&mut host).0, vec![bind(9, 3, Some("alice"))]);
    assert!(bound(&host));
}

/// S1.10, S2.5: an identity change during a manual retry drops it; the
/// written reason gives way to `dependency` once its clearing is written, as
/// a restart reads it.
#[test]
fn a_dropped_manual_retry_clears_the_written_reason_first() {
    let rejected = PlaceholderFailure::CredentialRejected;
    let mut log = vec![Record {
        seq: 9,
        entry: Entry::Changes(vec![C::PlaceholderFailed {
            profile: "a",
            failure: rejected,
        }]),
    }];
    let mut host = H::new(false, true, 10, 1);
    let slot = Slot::Placeholder(crate::provider::placeholder::Placeholder::new(Some(
        rejected,
    )));
    host.insert(Member::new(
        "a",
        Profile::new(slot, true, None),
        None,
        false,
    ));
    host.attempt(&"a").unwrap();
    host.identity_changed("a", IdentityChange::Instance, Some(created(9, "alice")));
    assert_eq!(failure(&host), Some(rejected));
    let cleared = C::PlaceholderCleared {
        profile: "a",
        failure: rejected,
    };
    assert_eq!(step(&mut host, &mut log), vec![cleared]);
    assert_eq!(failure(&host), Some(PlaceholderFailure::Dependency));
    assert_eq!(replay(9, log).unwrap().profiles["a"].failure, None);
}
