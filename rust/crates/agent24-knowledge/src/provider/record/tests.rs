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
    let stuck = UnavailableCause::AccessRevoked;
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
    let cause = UnavailableCause::Incompatible;
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
    // A run counts for every profile it names, each once.
    let bound = C::RunBound {
        run: 1,
        bindings: vec![of("a", 1), of("b", 2)],
        candidates: vec![("b", "j"), ("c", "k")],
    };
    assert_eq!(bound.scope(), Some(Scope::Profiles(vec!["a", "b", "c"])));
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
            Class::Loosening => false,
            Class::Tightening | Class::Generation => true,
            Class::Lifecycle => change == C::Started,
        };
        assert_eq!(change.outlives_void(), outlives, "{change:?}");
    }
}
