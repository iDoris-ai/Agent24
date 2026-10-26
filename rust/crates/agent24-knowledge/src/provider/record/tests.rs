#![allow(clippy::unwrap_used)]

use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
struct T;

impl Ids for T {
    type Profile = &'static str;
    type Binding = u32;
    type Run = u32;
    type Principal = &'static str;
    type Identity = &'static str;
}

type C = Change<T>;

fn of(profile: &'static str, binding: u32) -> Of<T> {
    Of { profile, binding }
}

/// One of every change, each with the class S1.10 (or the record's own
/// note, A.4, S4.4) gives it.
fn every() -> Vec<(C, Class)> {
    use Class::{Generation, Lifecycle, Loosening, Tightening};
    let a = of("a", 1);
    let pc = Confirmation::PrincipalChanged;
    let fail = PlaceholderFailure::CredentialRejected;
    let stuck = StuckCause::AccessRevoked;
    vec![
        (C::Off, Tightening),
        (C::On, Loosening),
        (C::Prompt, Tightening),
        (C::PromptConfirmed, Loosening),
        (C::Disable("a"), Tightening),
        (C::Enable("a"), Loosening),
        (C::Unselect("a"), Tightening),
        (C::Select("a"), Loosening),
        (
            C::Await {
                profile: "a",
                awaited: pc,
            },
            Tightening,
        ),
        (
            C::Confirm {
                profile: "a",
                principal: "u",
                awaited: pc,
            },
            Loosening,
        ),
        (
            C::PlaceholderFailed {
                profile: "a",
                failure: fail,
            },
            Tightening,
        ),
        (
            C::PlaceholderCleared {
                profile: "a",
                failure: fail,
            },
            Loosening,
        ),
        (C::Remove("a"), Tightening),
        (C::Retire(a.clone()), Tightening),
        (
            C::Bind {
                of: a.clone(),
                identity: "i",
                generation: 1,
                confirmed: Some("u"),
            },
            Loosening,
        ),
        (
            C::Generation {
                of: a.clone(),
                value: 2,
            },
            Generation,
        ),
        (C::Servable(a.clone()), Loosening),
        (C::Logout(a.clone()), Tightening),
        (C::Relogin(a.clone()), Loosening),
        (C::Revoked(a.clone()), Tightening),
        (
            C::Stuck {
                of: a.clone(),
                cause: stuck,
            },
            Tightening,
        ),
        (
            C::Unstuck {
                of: a.clone(),
                cause: stuck,
            },
            Loosening,
        ),
        (
            C::RunUnbound {
                run: 7,
                profiles: vec!["a"],
            },
            Tightening,
        ),
        (
            C::RunBound {
                run: 7,
                bindings: vec![a.clone()],
                candidates: vec![("b", "j")],
            },
            Loosening,
        ),
        (C::RunBackfilled { run: 7, of: a }, Loosening),
        (C::Started, Lifecycle),
        (C::Shutdown, Lifecycle),
    ]
}

#[test]
fn each_change_has_the_class_s1_10_gives_it() {
    for (change, class) in every() {
        assert_eq!(change.class(), class, "{change:?}");
    }
}

#[test]
fn an_abandoned_loosening_is_offset_by_its_counterpart() {
    let a = of("a", 1);
    let pc = Confirmation::UnsavedSettings;
    let fail = PlaceholderFailure::Dependency;
    let cause = StuckCause::Incompatible;
    let pairs = [
        (C::On, C::Off),
        (C::PromptConfirmed, C::Prompt),
        (C::Enable("a"), C::Disable("a")),
        (C::Select("a"), C::Unselect("a")),
        (
            C::Confirm {
                profile: "a",
                principal: "u",
                awaited: pc,
            },
            C::Await {
                profile: "a",
                awaited: pc,
            },
        ),
        (
            C::PlaceholderCleared {
                profile: "a",
                failure: fail,
            },
            C::PlaceholderFailed {
                profile: "a",
                failure: fail,
            },
        ),
        (
            C::Bind {
                of: a.clone(),
                identity: "i",
                generation: 3,
                confirmed: None,
            },
            C::Retire(a.clone()),
        ),
        (C::Relogin(a.clone()), C::Logout(a.clone())),
        (
            C::Unstuck {
                of: a.clone(),
                cause,
            },
            C::Stuck {
                of: a.clone(),
                cause,
            },
        ),
        (
            C::RunBound {
                run: 7,
                bindings: vec![a.clone(), of("a", 2)],
                candidates: vec![("b", "j")],
            },
            C::RunUnbound {
                run: 7,
                profiles: vec!["a", "b"],
            },
        ),
        (
            C::RunBackfilled { run: 7, of: a },
            C::RunUnbound {
                run: 7,
                profiles: vec!["a"],
            },
        ),
    ];
    for (loosening, offset) in pairs {
        assert_eq!(loosening.offset(), Some(offset), "{loosening:?}");
    }
    // "Once servable" is never offset: it errs on the strict side either way.
    assert_eq!(C::Servable(of("a", 1)).offset(), None);
}

#[test]
fn every_offset_is_a_tightening_over_the_loosening_s_scope() {
    for (change, class) in every() {
        let offset = change.offset();
        if class != Class::Loosening {
            assert_eq!(offset, None, "{change:?}");
            continue;
        }
        // "Once servable" errs on the strict side either way (A.5).
        let Some(offset) = offset else {
            assert_eq!(change, C::Servable(of("a", 1)));
            continue;
        };
        assert_eq!(offset.class(), Class::Tightening, "{change:?}");
        assert_eq!(offset.scope(), change.scope(), "{change:?}");
    }
}

#[test]
fn a_change_counts_for_its_profile_or_the_host() {
    assert_eq!(C::Off.scope(), Some(Scope::Host));
    assert_eq!(C::PromptConfirmed.scope(), Some(Scope::Host));
    assert_eq!(C::Enable("a").scope(), Some(Scope::profile("a")));
    // A binding's change counts for its profile.
    assert_eq!(C::Retire(of("b", 9)).scope(), Some(Scope::profile("b")));
    assert_eq!(C::Remove("c").scope(), Some(Scope::profile("c")));
    // A run counts for every profile it names, each once.
    let bound = C::RunBound {
        run: 1,
        bindings: vec![of("a", 1), of("b", 2)],
        candidates: vec![("b", "j"), ("c", "k"), ("d", "l")],
    };
    let named = Scope::Profiles(vec!["a", "b", "c", "d"]);
    assert_eq!(bound.scope(), Some(named.clone()));
    assert_eq!(bound.offset().unwrap().scope(), Some(named));
    // A run naming no profile is host level, never an empty scope.
    let empty = C::RunBound {
        run: 1,
        bindings: vec![],
        candidates: vec![],
    };
    assert_eq!(empty.scope(), Some(Scope::Host));
    assert_eq!(empty.offset().unwrap().scope(), Some(Scope::Host));
    // Generation and lifecycle records are outside I1.
    assert_eq!(
        C::Generation {
            of: of("a", 1),
            value: 5
        }
        .scope(),
        None
    );
    assert_eq!(C::Started.scope(), None);
    assert_eq!(C::Shutdown.scope(), None);
}

#[test]
fn on_a_number_given_up_the_stricter_record_stands() {
    for (change, class) in every() {
        let outlives = match class {
            // Keeping "once servable" closes S3.1's exception: the stricter.
            Class::Loosening => change == C::Servable(of("a", 1)),
            Class::Tightening | Class::Generation => true,
            Class::Lifecycle => change == C::Started,
        };
        assert_eq!(change.outlives_void(), outlives, "{change:?}");
    }
}

#[test]
fn only_a_reason_that_holds_across_restarts_is_persisted() {
    use StuckCause as S;
    use UnavailableCause as U;
    let held = [
        (S::Incompatible, U::Incompatible),
        (S::InstanceUnidentified, U::InstanceUnidentified),
        (S::CredentialRejected, U::CredentialRejected),
        (S::AccessRevoked, U::AccessRevoked),
    ];
    for (stuck, cause) in held {
        assert_eq!(U::from(stuck), cause);
    }
    let all = [
        U::Dependency,
        U::NotReady,
        U::Incompatible,
        U::InstanceUnidentified,
        U::CredentialRejected,
        U::AccessRevoked,
        U::LocalStorage,
    ];
    for cause in all {
        let holds = !cause.retries_automatically() && cause != U::LocalStorage;
        assert_eq!(held.iter().any(|(_, c)| *c == cause), holds, "{cause:?}");
    }
}

#[test]
fn every_reason_has_the_same_class_and_counterpart() {
    use PlaceholderFailure as F;
    use StuckCause as S;
    let a = of("a", 1);
    for cause in [
        S::Incompatible,
        S::InstanceUnidentified,
        S::CredentialRejected,
        S::AccessRevoked,
    ] {
        let into = C::Stuck {
            of: a.clone(),
            cause,
        };
        let out = C::Unstuck {
            of: a.clone(),
            cause,
        };
        assert_eq!(into.class(), Class::Tightening, "{cause:?}");
        assert_eq!(out.class(), Class::Loosening, "{cause:?}");
        assert_eq!(out.offset(), Some(into), "{cause:?}");
    }
    // S1.10 counts every failure the placeholder records, whatever retries.
    for failure in [
        F::Dependency,
        F::CredentialRejected,
        F::InstanceUnidentified,
    ] {
        let failed = C::PlaceholderFailed {
            profile: "a",
            failure,
        };
        let cleared = C::PlaceholderCleared {
            profile: "a",
            failure,
        };
        assert_eq!(failed.class(), Class::Tightening, "{failure:?}");
        assert_eq!(cleared.class(), Class::Loosening, "{failure:?}");
        assert_eq!(cleared.offset(), Some(failed), "{failure:?}");
    }
}
