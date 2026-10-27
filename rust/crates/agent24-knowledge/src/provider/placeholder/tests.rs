#![allow(clippy::unwrap_used)]

use super::*;
use PlaceholderFailure as F;

const CLEAR: Switches = Switches {
    off: false,
    profile_disabled: false,
};

#[test]
fn only_the_attempt_in_flight_counts() {
    let mut placeholder = Placeholder::new(None);
    let early = placeholder.attempt();
    let late = placeholder.attempt();
    assert_eq!(
        placeholder.settle(early, Attempted::Identified),
        Settled::Ignored
    );
    assert_eq!(
        placeholder.settle(late, Attempted::Rejected),
        Settled::Failed(F::CredentialRejected)
    );
    // Settled once: a copy of the answer does not count again.
    assert_eq!(
        placeholder.settle(late, Attempted::Identified),
        Settled::Ignored
    );
    assert_eq!(placeholder.failure(), Some(F::CredentialRejected));
}

#[test]
fn each_failure_is_recorded_with_its_reason_and_an_identity_asks_for_a_binding() {
    for (attempted, failure) in [
        (Attempted::Unreachable, F::Dependency),
        (Attempted::Rejected, F::CredentialRejected),
        (Attempted::Unidentified, F::InstanceUnidentified),
    ] {
        let mut placeholder = Placeholder::new(None);
        let attempt = placeholder.attempt();
        assert_eq!(
            placeholder.settle(attempt, attempted),
            Settled::Failed(failure)
        );
        assert_eq!(placeholder.failure(), Some(failure));
    }
    // A reason recorded before stays while an attempt is in flight, and an
    // identity does not clear it: the binding replaces the placeholder.
    let mut placeholder = Placeholder::new(Some(F::Dependency));
    let attempt = placeholder.attempt();
    assert_eq!(placeholder.failure(), Some(F::Dependency));
    assert_eq!(
        placeholder.settle(attempt, Attempted::Identified),
        Settled::Create
    );
    assert_eq!(placeholder.failure(), Some(F::Dependency));
    // The binding could not be persisted.
    let mut placeholder = Placeholder::new(None);
    let attempt = placeholder.attempt();
    placeholder.settle(attempt, Attempted::Identified);
    placeholder.creation_failed(attempt);
    assert_eq!(placeholder.failure(), Some(F::Dependency));
}

#[test]
fn a_late_creation_failure_never_overwrites_a_newer_reason() {
    // #1230 note 1: Create, then a new attempt refused, then the failure to
    // persist the first one's binding arrives.
    let mut placeholder = Placeholder::new(None);
    let created = placeholder.attempt();
    placeholder.settle(created, Attempted::Identified);
    let refused = placeholder.attempt();
    placeholder.settle(refused, Attempted::Rejected);
    placeholder.creation_failed(created);
    assert_eq!(placeholder.failure(), Some(F::CredentialRejected));
    assert!(!placeholder.retries_by_itself(CLEAR, true, false));
    // Only the attempt that created counts, and only once; a cancel ends it.
    let mut placeholder = Placeholder::new(None);
    let created = placeholder.creating();
    placeholder.creation_failed(refused);
    assert_eq!(placeholder.failure(), None);
    placeholder.cancel(false);
    placeholder.creation_failed(created);
    assert_eq!(placeholder.failure(), None);
    let mut placeholder = Placeholder::new(None);
    let created = placeholder.creating();
    assert_eq!(
        placeholder.settle(created, Attempted::Rejected),
        Settled::Ignored
    );
    placeholder.creation_failed(created);
    assert_eq!(placeholder.failure(), Some(F::Dependency));
}

#[test]
fn only_dependency_retries_by_itself_and_only_when_nothing_holds_it() {
    let dependency = Placeholder::new(Some(F::Dependency));
    assert!(dependency.retries_by_itself(CLEAR, true, false));
    assert!(!dependency.retries_by_itself(CLEAR, false, false));
    assert!(!dependency.retries_by_itself(CLEAR, true, true));
    let off = Switches { off: true, ..CLEAR };
    let disabled = Switches {
        profile_disabled: true,
        ..CLEAR
    };
    assert!(!dependency.retries_by_itself(off, true, false));
    assert!(!dependency.retries_by_itself(disabled, true, false));
    for failure in [
        None,
        Some(F::CredentialRejected),
        Some(F::InstanceUnidentified),
    ] {
        assert!(!Placeholder::new(failure).retries_by_itself(CLEAR, true, false));
    }
}

#[test]
fn a_following_placeholder_numbers_on_and_a_cancelled_attempt_does_not_count() {
    let mut earlier = Placeholder::new(None);
    let old = earlier.attempt();
    let mut later = Placeholder::following(earlier.next());
    let fresh = later.attempt();
    assert_ne!(fresh, old);
    assert_eq!(later.settle(old, Attempted::Identified), Settled::Ignored);
    assert_eq!(later.next(), earlier.next() + 1);
    later.cancel(false);
    assert_eq!(later.settle(fresh, Attempted::Identified), Settled::Ignored);
    assert_eq!(later.failure(), None);
}

/// S3.5: an identity change drops the attempt or the creation in flight and
/// retries by backoff; with neither in flight, nothing changes.
#[test]
fn a_cancel_that_retries_records_dependency_for_what_it_dropped() {
    let mut placeholder = Placeholder::new(None);
    placeholder.cancel(true);
    assert_eq!(placeholder.failure(), None);
    placeholder.attempt();
    placeholder.cancel(true);
    assert_eq!(placeholder.failure(), Some(F::Dependency));
    let mut placeholder = Placeholder::new(None);
    let created = placeholder.creating();
    placeholder.cancel(true);
    assert!(!placeholder.creates(created));
    assert_eq!(placeholder.failure(), Some(F::Dependency));
}

/// S1.10, S2.5: `dependency` is not written, so it takes the place of a
/// reason that waits for a manual retry only once that one's clearing is.
#[test]
fn dependency_takes_the_place_of_a_written_reason_once_it_is_cleared() {
    for failure in [F::CredentialRejected, F::InstanceUnidentified] {
        let mut placeholder = Placeholder::new(Some(failure));
        let attempt = placeholder.attempt();
        assert_eq!(
            placeholder.settle(attempt, Attempted::Unreachable),
            Settled::Failed(F::Dependency)
        );
        let created = placeholder.attempt();
        placeholder.settle(created, Attempted::Identified);
        placeholder.creation_failed(created);
        placeholder.attempt();
        placeholder.cancel(true);
        assert_eq!(placeholder.failure(), Some(failure));
        assert!(!placeholder.retries_by_itself(CLEAR, true, false));
        // Its clearing written, `dependency` takes its place, once.
        assert_eq!(placeholder.take_leaving(), Some(failure));
        assert_eq!(placeholder.take_leaving(), None);
        let [ticket, other] = tickets();
        placeholder.clearing(ticket);
        assert!(placeholder.clears());
        placeholder.left(other);
        assert_eq!(placeholder.failure(), Some(failure));
        placeholder.left(ticket);
        assert!(!placeholder.clears());
        assert_eq!(placeholder.failure(), Some(F::Dependency));
        assert!(placeholder.retries_by_itself(CLEAR, true, false));
    }
    // A reason written while its clearing is on the way stays.
    let mut placeholder = Placeholder::new(Some(F::CredentialRejected));
    let attempt = placeholder.attempt();
    placeholder.settle(attempt, Attempted::Unreachable);
    assert_eq!(placeholder.take_leaving(), Some(F::CredentialRejected));
    let [ticket, _] = tickets();
    placeholder.clearing(ticket);
    let attempt = placeholder.attempt();
    placeholder.settle(attempt, Attempted::Rejected);
    placeholder.left(ticket);
    assert_eq!(placeholder.failure(), Some(F::CredentialRejected));
    // Nothing waits: nothing to clear.
    assert_eq!(Placeholder::new(None).take_leaving(), None);
}

/// Two loosenings' tickets, as the ledger hands them out.
fn tickets() -> [crate::provider::ledger::Loosening; 2] {
    use crate::provider::ledger::{Ledger, Scope};
    let mut ledger = Ledger::new(true);
    [(); 2].map(|()| ledger.loosen(Scope::profile("p"), None, false).unwrap())
}
