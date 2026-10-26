#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use super::*;
use crate::provider::evidence::{Kbs, Pending, Rule, Saved};

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

fn bind(profile: &'static str, binding: u32, generation: u64) -> C {
    C::Bind {
        of: of(profile, binding),
        identity: "i",
        generation,
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

fn at(seq: u64, entry: Entry<T>) -> Record<T> {
    Record { seq, entry }
}

fn replayed(changes: Vec<C>) -> Replayed<T> {
    replay(1, log(changes)).unwrap()
}

fn binding(out: &Replayed<T>, id: u32) -> &Bound<T> {
    out.bindings.iter().find(|b| b.of.binding == id).unwrap()
}

#[test]
fn an_empty_log_is_off_with_nothing_else() {
    let out = replay(1, Vec::<Record<T>>::new()).unwrap();
    assert!(out.off && !out.prompt && !out.shut_down);
    assert!(out.profiles.is_empty() && out.bindings.is_empty() && out.runs.is_empty());
    assert_eq!(out.highest, 0);
    // The next record takes the number the log starts at.
    assert_eq!(out.next, 1);
    assert_eq!(replay(5, Vec::<Record<T>>::new()).unwrap().next, 5);
}

#[test]
fn the_records_give_back_the_state_in_their_order() {
    let a = of("a", 1);
    let cause = StuckCause::Incompatible;
    let out = replayed(vec![
        C::Started,
        C::On,
        C::Select("a"),
        C::Disable("a"),
        C::Enable("a"),
        C::Bind {
            of: a.clone(),
            identity: "i",
            generation: 3,
            confirmed: Some("u"),
        },
        C::Generation {
            of: a.clone(),
            value: 5,
        },
        C::Generation {
            of: a.clone(),
            value: 4,
        },
        C::Servable(a.clone()),
        C::Logout(a.clone()),
        C::Relogin(a.clone()),
        C::Revoked(a.clone()),
        C::Stuck {
            of: a.clone(),
            cause,
        },
        C::RunBound {
            run: 7,
            bindings: vec![a.clone()],
            candidates: vec![("a", "i")],
        },
        C::Prompt,
        C::Shutdown,
    ]);
    assert!(!out.off && out.prompt && out.shut_down);
    let p = &out.profiles["a"];
    assert!(p.selected && !p.disabled && !p.removed);
    assert_eq!(out.confirmed["a"], "u");
    let b = binding(&out, 1);
    assert_eq!((b.identity, b.generation), ("i", 5));
    assert!(b.servable && !b.logged_out && b.revoked && !b.retired);
    assert_eq!(b.stuck, Some(cause));
    assert_eq!(
        out.runs,
        vec![(7, Some((vec![a.clone()], vec![("a", "i")])))]
    );
    assert_eq!(out.highest, 5);

    let out = replayed(vec![
        bind("a", 1, 1),
        C::Unstuck {
            of: a.clone(),
            cause,
        },
        C::Stuck {
            of: a.clone(),
            cause,
        },
        C::Unstuck {
            of: a.clone(),
            cause,
        },
        C::Retire(a.clone()),
        C::Unselect("a"),
        C::Remove("a"),
        C::RunBound {
            run: 7,
            bindings: vec![a.clone()],
            candidates: vec![],
        },
        C::RunUnbound {
            run: 7,
            profiles: vec!["a"],
        },
        C::Off,
        C::Prompt,
        C::PromptConfirmed,
        C::Disable("a"),
    ]);
    assert!(out.off && !out.prompt);
    let p = &out.profiles["a"];
    assert!(!p.selected && p.removed && p.disabled);
    assert!(out.confirmed.is_empty());
    let b = binding(&out, 1);
    assert!(b.retired && b.stuck.is_none());
    assert_eq!(out.runs, vec![(7, None)]);
}

#[test]
fn a_write_found_with_its_void_keeps_only_what_outlives_it() {
    let new = bind("a", 2, 9);
    let retire = C::Retire(of("a", 1));
    let servable = C::Servable(of("a", 1));
    let write = || {
        Entry::Changes(vec![
            servable.clone(),
            retire.clone(),
            new.clone(),
            C::Shutdown,
        ])
    };
    // The void before the write or after it: the retirement stands, and so
    // does "once servable" (S3.1's exception stays closed); the new binding
    // and the clean shutdown do not.
    for pair in [[Entry::Void, write()], [write(), Entry::Void]] {
        let mut records = log(vec![bind("a", 1, 1)]);
        records.extend(pair.into_iter().map(|e| at(2, e)));
        let out = replay(1, records).unwrap();
        assert!(binding(&out, 1).retired && binding(&out, 1).servable);
        assert_eq!(out.bindings.len(), 1);
        assert!(!out.shut_down);
        // The new binding's first value may have been used all the same.
        assert_eq!(out.highest, 9);
    }
    // A generation value outlives it.
    let raised = C::Generation {
        of: of("a", 1),
        value: 6,
    };
    let mut records = log(vec![bind("a", 1, 1)]);
    records.extend([at(2, Entry::Void), at(2, Entry::Changes(vec![raised]))]);
    assert_eq!(binding(&replay(1, records).unwrap(), 1).generation, 6);
    // Without its void the whole write counts.
    let mut records = log(vec![bind("a", 1, 1)]);
    records.push(at(2, write()));
    let out = replay(1, records).unwrap();
    assert_eq!(out.bindings.len(), 2);
    assert!(out.shut_down);
}

#[test]
fn numbers_that_skip_go_back_or_disagree_are_damage() {
    let on = || Entry::Changes(vec![C::On]);
    let off = || Entry::Changes(vec![C::Off]);
    assert_eq!(replay(1, vec![at(1, on()), at(3, off())]), Err(Damaged));
    assert_eq!(replay(1, vec![at(2, on()), at(1, off())]), Err(Damaged));
    assert_eq!(replay(1, vec![at(1, on()), at(1, off())]), Err(Damaged));
    assert_eq!(
        replay(1, vec![at(1, Entry::Void), at(1, on()), at(1, off())]),
        Err(Damaged)
    );
    // A log missing its head, or numbers running past the last one.
    assert_eq!(replay(1, vec![at(2, on())]), Err(Damaged));
    assert_eq!(replay(4, vec![at(1, on())]), Err(Damaged));
    let last = vec![at(u64::MAX, off()), at(0, on())];
    assert_eq!(replay(u64::MAX, last), Err(Damaged));
    // No number would be left for the next record.
    assert_eq!(replay(u64::MAX, vec![at(u64::MAX, off())]), Err(Damaged));
    // The same write or void again is a retry.
    let out = replay(
        4,
        vec![
            at(4, on()),
            at(4, on()),
            at(5, Entry::Void),
            at(5, Entry::Void),
        ],
    );
    let out = out.unwrap();
    assert!(!out.off);
    // A void takes its number too.
    assert_eq!(out.next, 6);
}

#[test]
fn unsaved_settings_is_not_replaced_and_a_confirmation_answers_only_what_awaits() {
    let pc = Confirmation::PrincipalChanged;
    let unsaved = Confirmation::UnsavedSettings;
    let out = replayed(vec![
        C::Await {
            profile: "a",
            awaited: unsaved,
        },
        C::Await {
            profile: "a",
            awaited: pc,
        },
        C::Confirm {
            profile: "a",
            principal: "v",
            awaited: pc,
        },
    ]);
    assert_eq!(out.profiles["a"].awaiting, Some(unsaved));
    assert!(out.confirmed.is_empty());
    let out = replayed(vec![
        C::Await {
            profile: "a",
            awaited: unsaved,
        },
        C::Confirm {
            profile: "a",
            principal: "v",
            awaited: unsaved,
        },
        C::Await {
            profile: "a",
            awaited: pc,
        },
    ]);
    assert_eq!(out.profiles["a"].awaiting, Some(pc));
    // Clearing `unsaved_settings` confirms no principal (S1.13).
    assert!(out.confirmed.is_empty());
    let out = replayed(vec![
        C::Await {
            profile: "a",
            awaited: pc,
        },
        C::Confirm {
            profile: "a",
            principal: "v",
            awaited: pc,
        },
    ]);
    assert_eq!(out.profiles["a"].awaiting, None);
    assert_eq!(out.confirmed["a"], "v");
}

#[test]
fn a_clearing_clears_only_what_it_names() {
    let rejected = PlaceholderFailure::CredentialRejected;
    let dependency = PlaceholderFailure::Dependency;
    let out = replayed(vec![
        C::PlaceholderFailed {
            profile: "a",
            failure: rejected,
        },
        C::PlaceholderCleared {
            profile: "a",
            failure: dependency,
        },
    ]);
    assert_eq!(out.profiles["a"].failure, Some(rejected));
    let out = replayed(vec![
        C::PlaceholderFailed {
            profile: "a",
            failure: rejected,
        },
        C::PlaceholderCleared {
            profile: "a",
            failure: rejected,
        },
    ]);
    assert_eq!(out.profiles["a"].failure, None);

    let a = of("a", 1);
    let revoked = StuckCause::AccessRevoked;
    let out = replayed(vec![
        bind("a", 1, 1),
        C::Stuck {
            of: a.clone(),
            cause: revoked,
        },
        C::Unstuck {
            of: a,
            cause: StuckCause::Incompatible,
        },
    ]);
    assert_eq!(binding(&out, 1).stuck, Some(revoked));
}

#[test]
fn a_backfill_fills_only_a_set_recorded_empty() {
    let a = of("a", 1);
    let b = of("b", 2);
    let out = replayed(vec![
        C::RunBound {
            run: 1,
            bindings: vec![],
            candidates: vec![("a", "i")],
        },
        C::RunBackfilled {
            run: 1,
            of: a.clone(),
        },
        C::RunBackfilled {
            run: 1,
            of: b.clone(),
        },
        C::RunBound {
            run: 2,
            bindings: vec![b.clone()],
            candidates: vec![],
        },
        C::RunBackfilled {
            run: 2,
            of: a.clone(),
        },
        C::RunBackfilled {
            run: 3,
            of: a.clone(),
        },
    ]);
    assert_eq!(
        out.runs,
        vec![
            (1, Some((vec![a], vec![("a", "i")]))),
            (2, Some((vec![b], vec![]))),
        ]
    );
}

#[test]
fn a_change_to_a_binding_never_bound_changes_nothing() {
    let ghost = of("a", 9);
    let out = replayed(vec![
        C::Retire(ghost.clone()),
        bind("a", 1, 2),
        C::Servable(ghost.clone()),
        C::Generation {
            of: ghost,
            value: 40,
        },
    ]);
    assert_eq!(out.bindings.len(), 1);
    let b = binding(&out, 1);
    assert!(!b.retired && !b.servable && b.generation == 2);
    // Its value still counts toward what new values must pass (S4.4), as
    // does a new binding's first value.
    assert_eq!(out.highest, 40);
    assert_eq!(replayed(vec![bind("a", 1, 8)]).highest, 8);
}

#[test]
fn only_a_shutdown_last_of_all_is_clean() {
    assert!(!replayed(vec![C::Shutdown, C::Started]).shut_down);
    let mut records = log(vec![C::Shutdown]);
    records.push(at(2, Entry::Void));
    assert!(!replay(1, records).unwrap().shut_down);
}

#[test]
fn a_binding_bound_twice_or_below_its_profile_s_values_is_damage() {
    let damaged = |changes| replay(1, log(changes));
    // The same binding again (S2.6).
    let twice = vec![bind("a", 1, 1), C::Retire(of("a", 1)), bind("a", 1, 5)];
    assert_eq!(damaged(twice), Err(Damaged));
    // A first value not past one its profile used, retired or raised (S4.4).
    let below = vec![
        bind("a", 1, 1),
        C::Generation {
            of: of("a", 1),
            value: 9,
        },
        C::Retire(of("a", 1)),
        bind("a", 2, 9),
    ];
    assert_eq!(damaged(below), Err(Damaged));
    // No first value at all (S4.4: it starts at 1).
    assert_eq!(damaged(vec![bind("a", 1, 0)]), Err(Damaged));
    let sound = vec![bind("a", 1, 9), bind("a", 2, 10), bind("b", 3, 1)];
    assert_eq!(damaged(sound).unwrap().bindings.len(), 3);
}

fn kbs(all: &[&'static str]) -> BTreeSet<&'static str> {
    all.iter().copied().collect()
}

fn placed(binding: u32, epoch: u64, kbs: Kbs<&'static str>) -> C {
    C::Placed {
        of: of("a", binding),
        rule: Rule { epoch, kbs },
    }
}

#[test]
fn a_binding_s_evidence_comes_back_as_its_records_left_it() {
    let a = || of("a", 1);
    let changes = vec![
        bind("a", 1, 1),
        placed(1, 1, Kbs::All),
        C::Seen {
            of: a(),
            kbs: kbs(&["k", "l", "m"]),
        },
        // Questioned twice, then released at the first version only.
        C::Questioned {
            of: a(),
            kbs: kbs(&["k"]),
            version: 1,
            timeouts: Some(0),
        },
        C::Questioned {
            of: a(),
            kbs: kbs(&["l"]),
            version: 2,
            timeouts: Some(0),
        },
        C::Released {
            of: a(),
            kbs: kbs(&["k"]),
            version: 1,
        },
        C::TimedOut { of: a(), count: 2 },
        // A tombstone takes its KB out of the set; a deletion ends it.
        C::Tombstone { of: a(), kb: "l" },
        C::Tombstone { of: a(), kb: "n" },
        placed(1, 2, Kbs::One("n")),
        C::Tombstone { of: a(), kb: "o" },
        C::TombstoneWithdrawn { of: a(), kb: "o" },
        C::Unseen {
            of: a(),
            kbs: kbs(&["m"]),
        },
        // Not bound: nothing changes.
        C::Seen {
            of: of("a", 9),
            kbs: kbs(&["z"]),
        },
    ];
    let state = replay(1, log(changes)).unwrap();
    let expected = Saved {
        epoch: 2,
        rules: vec![
            Rule {
                epoch: 1,
                kbs: Kbs::All,
            },
            Rule {
                epoch: 2,
                kbs: Kbs::One("n"),
            },
        ],
        version: 2,
        pending: Some(Pending {
            kbs: kbs(&["k", "l"]),
            timeouts: 2,
        }),
        tombstones: kbs(&["l"]),
        visible: kbs(&["k"]),
    };
    assert_eq!(state.bindings[0].evidence, expected);
    // Released at the current version, and the count cleared.
    let more = |tail: Vec<C>| {
        let head = vec![
            bind("a", 1, 1),
            C::Questioned {
                of: a(),
                kbs: kbs(&["k"]),
                version: 3,
                timeouts: Some(0),
            },
            C::TimedOut { of: a(), count: 2 },
        ];
        replay(1, log([head, tail].concat())).unwrap().bindings[0]
            .evidence
            .clone()
    };
    let cleared = more(vec![C::TimeoutsCleared { of: a(), from: 2 }]);
    assert_eq!(cleared.pending.map(|p| p.timeouts), Some(0));
    let released = more(vec![C::Released {
        of: a(),
        kbs: kbs(&["k"]),
        version: 3,
    }]);
    assert_eq!(released.pending, None);
    // An offset re-registering an older version does not take it back.
    let older = more(vec![C::Questioned {
        of: a(),
        kbs: kbs(&["k"]),
        version: 2,
        timeouts: Some(0),
    }]);
    assert_eq!(older.version, 3);
    // A count with nothing pending is not kept.
    let idle = replay(
        1,
        log(vec![bind("a", 1, 1), C::TimedOut { of: a(), count: 1 }]),
    );
    assert_eq!(idle.unwrap().bindings[0].evidence.pending, None);
}

#[test]
fn an_epoch_that_does_not_advance_is_damage() {
    let changes = vec![
        bind("a", 1, 1),
        placed(1, 2, Kbs::All),
        placed(1, 2, Kbs::One("k")),
    ];
    assert_eq!(replay(1, log(changes)), Err(Damaged));
    // On a number given up the rule stands; a release does not.
    let a = || of("a", 1);
    let records = vec![
        Record {
            seq: 1,
            entry: Entry::Changes(vec![bind("a", 1, 1)]),
        },
        Record {
            seq: 2,
            entry: Entry::Changes(vec![C::Questioned {
                of: a(),
                kbs: kbs(&["k"]),
                version: 1,
                timeouts: Some(0),
            }]),
        },
        Record {
            seq: 3,
            entry: Entry::Changes(vec![
                placed(1, 1, Kbs::One("k")),
                C::Released {
                    of: a(),
                    kbs: kbs(&["k"]),
                    version: 1,
                },
            ]),
        },
        Record {
            seq: 3,
            entry: Entry::Void,
        },
    ];
    let evidence = &replay(1, records).unwrap().bindings[0].evidence;
    assert_eq!(evidence.epoch, 1);
    assert!(evidence.pending.is_some());
}

#[test]
fn a_release_that_landed_though_abandoned_keeps_the_count() {
    // ST-26g: two timeouts in a row persisted, then a release written and
    // abandoned (I1) that lands all the same.
    let a = || of("a", 1);
    let questioned = |version, timeouts| C::Questioned {
        of: a(),
        kbs: kbs(&["k"]),
        version,
        timeouts,
    };
    let released = C::Released {
        of: a(),
        kbs: kbs(&["k"]),
        version: 1,
    };
    let count = |between: Vec<C>, after: C| {
        let head = vec![
            bind("a", 1, 1),
            questioned(1, Some(0)),
            C::TimedOut { of: a(), count: 2 },
            released.clone(),
        ];
        let state = replay(1, log([head, between, vec![after]].concat())).unwrap();
        let evidence = &state.bindings[0].evidence;
        assert_eq!(evidence.version, 2);
        evidence.pending.as_ref().map(|p| p.timeouts)
    };
    let offset = || released.offset().unwrap();
    // Its offset brings the count back, or a registration meanwhile does.
    assert_eq!(count(vec![], offset()), Some(2));
    assert_eq!(count(vec![], questioned(2, Some(2))), Some(2));
    // Cleared by a retry, or timed out again, while the release pended: the
    // offset brings back the count as it then stood.
    let cleared = C::TimeoutsCleared { of: a(), from: 2 };
    assert_eq!(count(vec![cleared], offset()), Some(0));
    let again = C::TimedOut { of: a(), count: 3 };
    assert_eq!(count(vec![again], offset()), Some(3));
    // Released for good: a later registration starts from its own count.
    assert_eq!(count(vec![], questioned(2, Some(0))), Some(0));
}

#[test]
fn an_offset_or_a_repeat_takes_no_count_back() {
    let a = || of("a", 1);
    let head = || {
        vec![
            bind("a", 1, 1),
            C::Questioned {
                of: a(),
                kbs: kbs(&["k"]),
                version: 1,
                timeouts: Some(0),
            },
            C::TimedOut { of: a(), count: 2 },
        ]
    };
    let count = |tail: Vec<C>| {
        let state = replay(1, log([head(), tail].concat())).unwrap();
        let evidence = &state.bindings[0].evidence;
        evidence.pending.as_ref().map(|p| p.timeouts)
    };
    // A clearing written, a timeout meanwhile (the count still 2 in memory,
    // so 3), then the clearing abandoned though it landed: its offset keeps
    // 3.
    let cleared = C::TimeoutsCleared { of: a(), from: 2 };
    let offset = cleared.offset().unwrap();
    let again = C::TimedOut { of: a(), count: 3 };
    assert_eq!(count(vec![cleared.clone(), again, offset]), Some(3));
    // A clearing that stood: counted afresh from 0, even past a timeout
    // written while it pended.
    let again = C::TimedOut { of: a(), count: 3 };
    let anew = C::TimedOut { of: a(), count: 1 };
    assert_eq!(count(vec![cleared.clone(), again, anew.clone()]), Some(1));
    assert_eq!(count(vec![cleared, anew]), Some(1));
    // A release written twice keeps what the first set aside for its offset.
    let released = C::Released {
        of: a(),
        kbs: kbs(&["k"]),
        version: 1,
    };
    let back = released.offset().unwrap();
    assert_eq!(count(vec![released.clone(), released, back]), Some(2));
}
