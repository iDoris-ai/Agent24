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
    // The binding could not be persisted.
    let mut placeholder = Placeholder::new(None);
    placeholder.creation_failed();
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
