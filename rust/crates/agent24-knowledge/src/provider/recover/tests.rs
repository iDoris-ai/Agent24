#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::record::Of;
use crate::provider::replay::{Entry, Record, replay};

#[derive(Clone, Debug, PartialEq, Eq)]
struct T;

impl Ids for T {
    type Profile = &'static str;
    type Binding = u32;
    type Run = u32;
    type Principal = &'static str;
    type Identity = &'static str;
    type Kb = &'static str;
}

type C = Change<T>;

fn of(profile: &'static str, binding: u32) -> Of<T> {
    Of { profile, binding }
}

fn bind(profile: &'static str, binding: u32) -> C {
    C::Bind {
        of: of(profile, binding),
        identity: "i",
        generation: 1,
        confirmed: None,
    }
}

/// One record per change, numbered from 1.
fn log(changes: Vec<C>) -> Vec<Record<T>> {
    (1..)
        .zip(changes)
        .map(|(seq, c)| Record {
            seq,
            entry: Entry::Changes(vec![c]),
        })
        .collect()
}

fn read(changes: Vec<C>) -> Option<Result<Replayed<T>, Damaged>> {
    Some(replay(1, log(changes)))
}

fn unsaved(profile: &'static str) -> C {
    C::Await {
        profile,
        awaited: Confirmation::UnsavedSettings,
    }
}

const SEALED: Seal = Seal::Present { next: 1 };

fn same(_: &Bound<T>) -> Found {
    Found::Same
}

#[test]
fn with_nothing_persisted_only_a_confirmed_first_install_starts_clean() {
    let first = recover(None, Seal::Missing, true, &[], same);
    assert_eq!(first, Recovery::FirstInstall);
    // Not confirmed (FI-3b), a profile configured (FI-3f), or a seal with no
    // log: damage.
    let damaged = Recovery::Damaged {
        writes: vec![
            C::Off,
            C::Prompt,
            C::Unselect("a"),
            C::Disable("a"),
            unsaved("a"),
        ],
    };
    assert_eq!(recover(None, Seal::Missing, false, &["a"], same), damaged);
    assert_eq!(recover(None, Seal::Missing, true, &["a"], same), damaged);
    assert_eq!(recover(None, SEALED, true, &["a"], same), damaged);
    let bare = Recovery::Damaged {
        writes: vec![C::Off, C::Prompt],
    };
    assert_eq!(recover(None, Seal::Missing, false, &[], same), bare);
    assert_eq!(recover(None, SEALED, true, &[], same), bare);
    // With a log the mark counts for nothing: an empty one is still a log.
    let empty = || Some(replay(1, Vec::<Record<T>>::new()));
    let out = recover(empty(), Seal::Missing, true, &[], same);
    assert!(matches!(out, Recovery::Tightened { .. }));
    let out = recover(empty(), SEALED, true, &[], same);
    assert!(matches!(out, Recovery::Normal { retire, .. } if retire.is_empty()));
}

#[test]
fn a_damaged_log_puts_every_profile_back_unselected_and_disabled() {
    let out = recover(Some(Err(Damaged)), SEALED, false, &["a", "b"], same);
    let writes = vec![
        C::Off,
        C::Prompt,
        C::Unselect("a"),
        C::Disable("a"),
        unsaved("a"),
        C::Unselect("b"),
        C::Disable("b"),
        unsaved("b"),
    ];
    assert_eq!(out, Recovery::Damaged { writes });
}

#[test]
fn without_the_seal_the_state_is_tightened_globally() {
    let changes = vec![C::On, bind("a", 1), bind("b", 2), C::Retire(of("b", 2))];
    let state = replay(1, log(changes.clone())).unwrap();
    let writes = vec![
        C::Off,
        C::Prompt,
        C::Retire(of("a", 1)),
        unsaved("a"),
        // Named only by its retired binding: it awaits all the same.
        unsaved("b"),
    ];
    let tightened = Recovery::Tightened {
        state,
        writes: writes.clone(),
    };
    assert_eq!(
        recover(read(changes.clone()), Seal::Missing, false, &["a"], same),
        tightened
    );
    // A seal past the log's end: records it had seen are gone (A.2).
    let past = Seal::Present { next: 6 };
    assert_eq!(
        recover(read(changes.clone()), past, false, &["a"], same),
        tightened
    );
    // At the end, or before it (loosenings since): the state stands.
    for next in [5, 2] {
        let seal = Seal::Present { next };
        let out = recover(read(changes.clone()), seal, false, &["a"], same);
        assert!(matches!(out, Recovery::Normal { retire, .. } if retire.is_empty()));
    }
}

#[test]
fn a_global_tightening_replayed_retires_everything_and_awaits_the_user() {
    // A new binding awaiting `principal_changed` (FI-8i).
    let changes = vec![
        C::On,
        C::Select("a"),
        bind("a", 1),
        C::Await {
            profile: "a",
            awaited: Confirmation::PrincipalChanged,
        },
        bind("b", 2),
    ];
    let Recovery::Tightened { writes, .. } = recover(
        read(changes.clone()),
        Seal::Missing,
        false,
        &["a", "b"],
        same,
    ) else {
        panic!("not tightened");
    };
    let state = replay(1, log([changes, writes].concat())).unwrap();
    assert!(state.off && state.prompt);
    assert!(state.bindings.iter().all(|b| b.retired));
    for p in ["a", "b"] {
        let awaited = state.profiles[p].awaiting;
        assert_eq!(awaited, Some(Confirmation::UnsavedSettings), "{p}");
    }
    // Once those writes are confirmed the seal is rebuilt: the next start
    // recovers them as they are and prompts again (FI-8d).
    let next = Seal::Present { next: state.next };
    let out = recover(Some(Ok(state.clone())), next, false, &["a", "b"], same);
    let retire = vec![];
    assert_eq!(out, Recovery::Normal { state, retire });
}

#[test]
fn a_binding_no_longer_configured_as_it_was_is_retired_at_restart() {
    let changes = vec![
        C::On,
        bind("a", 1),
        bind("b", 2),
        bind("c", 3),
        bind("d", 4),
        bind("e", 5),
        bind("f", 6),
        bind("g", 7),
        C::Retire(of("e", 5)),
        C::Remove("c"),
        C::Remove("g"),
    ];
    let awaiting: Vec<C> = ["d", "f", "g"]
        .map(|profile| C::Await {
            profile,
            awaited: Confirmation::PrincipalChanged,
        })
        .into();
    let found = |b: &Bound<T>| match b.of.profile {
        "a" | "f" => Found::Gone,
        "b" | "d" | "e" | "g" => Found::OtherPrincipal,
        _ => Found::Same,
    };
    let configured = ["a", "b", "c", "d", "f", "g"];
    let out = recover(
        read([changes, awaiting].concat()),
        SEALED,
        false,
        &configured,
        found,
    );
    let Recovery::Normal { retire, .. } = out else {
        panic!("not normal");
    };
    // Gone, another principal's login, or the profile removed. Not `d`: its
    // principal is checked at confirmation, but only the principal (`f`, `g`).
    // Not `e`: retired already.
    let expected = vec![
        C::Retire(of("a", 1)),
        C::Retire(of("b", 2)),
        C::Retire(of("c", 3)),
        C::Retire(of("f", 6)),
        C::Retire(of("g", 7)),
    ];
    assert_eq!(retire, expected);
    // Logged out, or the same principal: it stands (FI-6g).
    let out = recover(read(vec![bind("a", 1)]), SEALED, false, &["a"], same);
    assert!(matches!(out, Recovery::Normal { retire, .. } if retire.is_empty()));
    // Only `principal_changed` defers the check: under `unsaved_settings`
    // another principal's login retires it.
    let changes = vec![bind("h", 8), unsaved("h")];
    let other = |_: &Bound<T>| Found::OtherPrincipal;
    let out = recover(read(changes), SEALED, false, &["h"], other);
    let Recovery::Normal { retire, .. } = out else {
        panic!("not normal");
    };
    assert_eq!(retire, vec![C::Retire(of("h", 8))]);
}

#[test]
fn a_profile_removed_before_its_removal_was_persisted_still_awaits() {
    // FI-6d: P left the configuration, its `Remove` never landed. A
    // placeholder that awaited a principal is named by the state alone.
    let changes = vec![
        C::On,
        bind("p", 1),
        C::Await {
            profile: "q",
            awaited: Confirmation::PrincipalChanged,
        },
    ];
    let Recovery::Tightened { writes, .. } =
        recover(read(changes), Seal::Missing, false, &["a"], same)
    else {
        panic!("not tightened");
    };
    let expected = vec![
        C::Off,
        C::Prompt,
        C::Retire(of("p", 1)),
        unsaved("a"),
        unsaved("p"),
        unsaved("q"),
    ];
    assert_eq!(writes, expected);
}
