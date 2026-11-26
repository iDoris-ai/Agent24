#![allow(clippy::unwrap_used)]

use std::time::Instant;

use super::super::tests::{C, H, T, host, of, phase, write};
use super::*;
use crate::provider::credential::Issued;
use crate::provider::journal::Written;
use crate::provider::member::Member;
use crate::provider::placeholder::Placeholder;
use crate::provider::profile::{Profile, Slot};
use crate::provider::state::{ConnectionPhase, PlaceholderFailure, UnavailableCause};

fn unavailable(cause: UnavailableCause) -> Option<ConnectionPhase> {
    Some(ConnectionPhase::Unavailable(cause))
}

fn created(identity: &'static str) -> Created<T> {
    Created {
        binding: 8,
        identity,
        principal: "me",
    }
}

#[test]
fn the_desktop_states_map_one_to_one() {
    let all = [
        ("starting", Sidecar::Starting),
        ("ready", Sidecar::Ready),
        ("healthy", Sidecar::Healthy),
        ("degraded", Sidecar::Degraded),
        ("failed", Sidecar::Failed),
        ("stopping", Sidecar::Stopping),
        ("stopped", Sidecar::Stopped),
    ];
    for (name, state) in all {
        assert_eq!(Sidecar::from_state(name), Some(state), "{name}");
    }
    for bad in ["", "Healthy", "running", "gave-up"] {
        assert_eq!(Sidecar::from_state(bad), None, "{bad}");
    }
}

/// S2.2: down is `unavailable(dependency)` at once, nothing written;
/// `starting` takes it back to `loading` in a new round.
#[test]
fn a_crash_and_restart_move_the_binding_without_writing() {
    let mut host = host();
    let (out, attempt) = host.sidecar(&"a", Sidecar::Failed);
    assert_eq!((out, attempt), (Output::default(), None));
    assert_eq!(phase(&host, "a"), unavailable(UnavailableCause::Dependency));
    let (out, attempt) = host.sidecar(&"a", Sidecar::Starting);
    assert_eq!(attempt, None);
    assert_eq!(out.loading.len(), 1);
    assert_eq!(out.loading[0].0, "a");
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
    assert!(host.take_write().is_none());
}

/// The restarted instance's new key is the round's credential: the binding
/// is not revoked. (Taken as a rotation it would be `access_revoked` and
/// stay so: the case `rotated` must not be used for.)
#[test]
fn each_restarts_key_is_the_rounds_credential_not_a_rotation() {
    let mut host = host();
    host.sidecar(&"a", Sidecar::Failed);
    host.sidecar(&"a", Sidecar::Starting);
    assert_eq!(host.handshake("a", created("id")), Output::default());
    let (of_a, ticket) = host.issue(&"a").unwrap();
    assert_eq!(of_a, of("a", 7));
    host.issued(
        of_a,
        ticket,
        Issued::Granted { narrowed: false },
        Instant::now(),
    );
    assert_eq!(phase(&host, "a"), Some(ConnectionPhase::Loading));
    assert!(host.take_write().is_none());

    let mut wrong = super::super::tests::host();
    wrong.sidecar(&"a", Sidecar::Failed);
    wrong.sidecar(&"a", Sidecar::Starting);
    wrong.rotated(&"a", Instant::now());
    assert_eq!(
        phase(&wrong, "a"),
        unavailable(UnavailableCause::AccessRevoked)
    );
}

/// S2.4: another instance identifier retires the binding; the new one waits
/// for the retirement to be persisted (S3.5).
#[test]
fn another_instance_retires_the_binding() {
    let mut host = host();
    host.handshake("a", created("id2"));
    assert!(matches!(
        host.profile(&"a").unwrap().slot(),
        Slot::Placeholder(_)
    ));
    let (attempt, _, changes) = write(&mut host);
    assert_eq!(changes[0], C::Retire(of("a", 7)));
    host.written(attempt, Written::Durable);
    let (_, _, changes) = write(&mut host);
    assert!(
        changes.iter().any(|c| matches!(c, C::Bind { of, identity: "id2", .. } if *of == super::super::tests::of("a", 8))),
        "{changes:?}"
    );
}

/// On a placeholder `handshake` does nothing: its handshake is `identified`.
#[test]
fn a_placeholders_handshake_is_not_an_identity_change() {
    let mut host = host();
    assert_eq!(host.handshake("b", created("id2")), Output::default());
    assert!(host.take_write().is_none());
}

/// S2.5: `starting` makes a placeholder held by `dependency` try a handshake.
#[test]
fn starting_retries_a_placeholder_held_by_dependency() {
    let mut host = H::new(false, true, 10, 1);
    let placeholder = Slot::Placeholder(Placeholder::new(Some(PlaceholderFailure::Dependency)));
    host.insert(Member::new(
        "c",
        Profile::new(placeholder, true, None),
        None,
        false,
    ));
    let (out, attempt) = host.sidecar(&"c", Sidecar::Starting);
    assert_eq!(out, Output::default());
    assert!(attempt.is_some());
    // Unknown profiles are ignored.
    assert_eq!(
        host.sidecar(&"zz", Sidecar::Starting),
        (Output::default(), None)
    );
}

/// S5.4: a rotation while the instance runs is a revocation, written.
#[test]
fn a_rotation_while_running_is_written_as_a_revocation() {
    let mut host = host();
    let out = host.rotated(&"a", Instant::now());
    assert!(out.loading.is_empty());
    let (_, _, changes) = write(&mut host);
    assert!(changes.contains(&C::Revoked(of("a", 7))), "{changes:?}");
    // Nothing for a placeholder or an unknown profile.
    assert_eq!(host.rotated(&"b", Instant::now()), Output::default());
    assert_eq!(host.rotated(&"zz", Instant::now()), Output::default());
}
