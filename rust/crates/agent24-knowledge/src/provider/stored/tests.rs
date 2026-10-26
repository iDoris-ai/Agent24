#![allow(clippy::unwrap_used)]

use std::collections::BTreeSet;

use super::*;
use crate::provider::replay::{Entry, Record, replay};

#[derive(Clone, Debug, PartialEq, Eq)]
struct T;

impl Ids for T {
    type Profile = String;
    type Binding = u32;
    type Run = u32;
    type Principal = String;
    type Identity = String;
    type Kb = String;
}

type C = Change<T>;

fn of(profile: &str, binding: u32) -> Of<T> {
    Of {
        profile: profile.into(),
        binding,
    }
}

fn kbs(kbs: &[&str]) -> BTreeSet<String> {
    kbs.iter().map(|kb| kb.to_string()).collect()
}

/// One of every change, in declaration order.
fn every_change() -> Vec<C> {
    let p = || "p".to_string();
    vec![
        C::Off,
        C::On,
        C::Prompt,
        C::PromptConfirmed,
        C::Disable(p()),
        C::Enable(p()),
        C::Unselect(p()),
        C::Select(p()),
        C::Await {
            profile: p(),
            awaited: Confirmation::PrincipalChanged,
        },
        C::Confirm {
            profile: p(),
            principal: "alice".into(),
            awaited: Confirmation::UnsavedSettings,
        },
        C::PlaceholderFailed {
            profile: p(),
            failure: PlaceholderFailure::Dependency,
        },
        C::PlaceholderCleared {
            profile: p(),
            failure: PlaceholderFailure::CredentialRejected,
        },
        C::Remove(p()),
        C::Retire(of("p", 1)),
        C::Bind {
            of: of("p", 2),
            identity: "id".into(),
            generation: 3,
            confirmed: Some("alice".into()),
        },
        C::Generation {
            of: of("p", 2),
            value: 4,
        },
        C::Servable(of("p", 2)),
        C::Logout(of("p", 2)),
        C::Relogin(of("p", 2)),
        C::Revoked(of("p", 2)),
        C::Stuck {
            of: of("p", 2),
            cause: StuckCause::Incompatible,
        },
        C::Unstuck {
            of: of("p", 2),
            cause: StuckCause::AccessRevoked,
        },
        C::RunUnbound {
            run: 7,
            profiles: vec![p(), "q".into()],
        },
        C::RunBound {
            run: 8,
            bindings: vec![of("p", 2), of("q", 5)],
            candidates: vec![("r".into(), "rid".into())],
        },
        C::RunBackfilled {
            run: 8,
            of: of("r", 6),
        },
        C::Placed {
            of: of("p", 2),
            rule: Rule {
                epoch: 9,
                kbs: Kbs::One("kb1".into()),
            },
        },
        C::Questioned {
            of: of("p", 2),
            kbs: kbs(&["kb1", "kb2"]),
            version: 10,
            timeouts: Some(2),
        },
        C::Released {
            of: of("p", 2),
            kbs: kbs(&["kb1"]),
            version: 10,
        },
        C::TimedOut {
            of: of("p", 2),
            count: 1,
        },
        C::TimeoutsCleared {
            of: of("p", 2),
            from: 3,
        },
        C::TimeoutsRestored {
            of: of("p", 2),
            count: 3,
        },
        C::Tombstone {
            of: of("p", 2),
            kb: "kb1".into(),
        },
        C::TombstoneWithdrawn {
            of: of("p", 2),
            kb: "kb1".into(),
        },
        C::Seen {
            of: of("p", 2),
            kbs: kbs(&["kb2"]),
        },
        C::Unseen {
            of: of("p", 2),
            kbs: kbs(&["kb2"]),
        },
        C::Started,
        C::Shutdown,
    ]
}

/// The position of each kind of change in [`every_change`]. A new kind fails
/// to compile here until it has a stored shape and a place in the sample.
fn position(change: &C) -> usize {
    match change {
        C::Off => 0,
        C::On => 1,
        C::Prompt => 2,
        C::PromptConfirmed => 3,
        C::Disable(_) => 4,
        C::Enable(_) => 5,
        C::Unselect(_) => 6,
        C::Select(_) => 7,
        C::Await { .. } => 8,
        C::Confirm { .. } => 9,
        C::PlaceholderFailed { .. } => 10,
        C::PlaceholderCleared { .. } => 11,
        C::Remove(_) => 12,
        C::Retire(_) => 13,
        C::Bind { .. } => 14,
        C::Generation { .. } => 15,
        C::Servable(_) => 16,
        C::Logout(_) => 17,
        C::Relogin(_) => 18,
        C::Revoked(_) => 19,
        C::Stuck { .. } => 20,
        C::Unstuck { .. } => 21,
        C::RunUnbound { .. } => 22,
        C::RunBound { .. } => 23,
        C::RunBackfilled { .. } => 24,
        C::Placed { .. } => 25,
        C::Questioned { .. } => 26,
        C::Released { .. } => 27,
        C::TimedOut { .. } => 28,
        C::TimeoutsCleared { .. } => 29,
        C::TimeoutsRestored { .. } => 30,
        C::Tombstone { .. } => 31,
        C::TombstoneWithdrawn { .. } => 32,
        C::Seen { .. } => 33,
        C::Unseen { .. } => 34,
        C::Started => 35,
        C::Shutdown => 36,
    }
}

/// What version 1 wrote for [`every_change`]. Never edit: a record already
/// on disk reads the same, or the version changes.
const V1: &str = r#"{"version":1,"changes":[
"off","on","prompt","prompt_confirmed",
{"disable":"p"},{"enable":"p"},{"unselect":"p"},{"select":"p"},
{"await":{"profile":"p","awaited":"principal_changed"}},
{"confirm":{"profile":"p","principal":"alice","awaited":"unsaved_settings"}},
{"placeholder_failed":{"profile":"p","failure":"dependency"}},
{"placeholder_cleared":{"profile":"p","failure":"credential_rejected"}},
{"remove":"p"},
{"retire":{"profile":"p","binding":1}},
{"bind":{"of":{"profile":"p","binding":2},"identity":"id","generation":3,"confirmed":"alice"}},
{"generation":{"of":{"profile":"p","binding":2},"value":4}},
{"servable":{"profile":"p","binding":2}},
{"logout":{"profile":"p","binding":2}},
{"relogin":{"profile":"p","binding":2}},
{"revoked":{"profile":"p","binding":2}},
{"stuck":{"of":{"profile":"p","binding":2},"cause":"incompatible"}},
{"unstuck":{"of":{"profile":"p","binding":2},"cause":"access_revoked"}},
{"run_unbound":{"run":7,"profiles":["p","q"]}},
{"run_bound":{"run":8,"bindings":[{"profile":"p","binding":2},{"profile":"q","binding":5}],"candidates":[["r","rid"]]}},
{"run_backfilled":{"run":8,"of":{"profile":"r","binding":6}}},
{"placed":{"of":{"profile":"p","binding":2},"rule":{"epoch":9,"kbs":{"one":"kb1"}}}},
{"questioned":{"of":{"profile":"p","binding":2},"kbs":["kb1","kb2"],"version":10,"timeouts":2}},
{"released":{"of":{"profile":"p","binding":2},"kbs":["kb1"],"version":10}},
{"timed_out":{"of":{"profile":"p","binding":2},"count":1}},
{"timeouts_cleared":{"of":{"profile":"p","binding":2},"from":3}},
{"timeouts_restored":{"of":{"profile":"p","binding":2},"count":3}},
{"tombstone":{"of":{"profile":"p","binding":2},"kb":"kb1"}},
{"tombstone_withdrawn":{"of":{"profile":"p","binding":2},"kb":"kb1"}},
{"seen":{"of":{"profile":"p","binding":2},"kbs":["kb2"]}},
{"unseen":{"of":{"profile":"p","binding":2},"kbs":["kb2"]}},
"started","shutdown"
]}"#;

/// A version 1 write holding `changes`, spelled as given.
fn write(changes: &str) -> String {
    format!(r#"{{"version":1,"changes":[{changes}]}}"#)
}

fn damaged(text: &str) -> bool {
    decode::<T>(text) == Err(Damaged)
}

fn json(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

#[test]
fn the_sample_holds_every_kind_once() {
    let sample = every_change();
    let positions: Vec<usize> = sample.iter().map(position).collect();
    assert_eq!(positions, (0..sample.len()).collect::<Vec<_>>());
}

#[test]
fn a_version_1_record_reads_back_and_is_written_the_same() {
    assert_eq!(decode::<T>(V1).unwrap(), every_change());
    assert_eq!(json(&encode(&every_change()).unwrap()), json(V1));
}

#[test]
fn what_is_written_reads_back() {
    for change in every_change() {
        let text = encode(std::slice::from_ref(&change)).unwrap();
        assert_eq!(decode::<T>(&text).unwrap(), vec![change]);
    }
    let none = vec![
        C::Bind {
            of: of("p", 2),
            identity: "id".into(),
            generation: 0,
            confirmed: None,
        },
        C::Placed {
            of: of("p", 2),
            rule: Rule {
                epoch: 1,
                kbs: Kbs::All,
            },
        },
        C::Questioned {
            of: of("p", 2),
            kbs: BTreeSet::new(),
            version: 1,
            timeouts: None,
        },
    ];
    let text = encode(&none).unwrap();
    assert_eq!(decode::<T>(&text).unwrap(), none);
    assert_eq!(decode::<T>(&write("")).unwrap(), vec![]);
}

#[test]
fn another_version_is_damaged() {
    for version in [r#"0"#, r#"2"#, r#""1""#, r#"null"#] {
        let text = format!(r#"{{"version":{version},"changes":["off"]}}"#);
        assert!(damaged(&text), "{text}");
    }
    assert!(damaged(r#"{"changes":["off"]}"#));
}

#[test]
fn a_kind_or_value_this_build_does_not_know_is_damaged() {
    for change in [
        r#""offline""#,
        r#"{"vanish":"p"}"#,
        r#"{"await":{"profile":"p","awaited":"later"}}"#,
        r#"{"placeholder_failed":{"profile":"p","failure":"other"}}"#,
        r#"{"stuck":{"of":{"profile":"p","binding":2},"cause":"other"}}"#,
        r#"{"placed":{"of":{"profile":"p","binding":2},"rule":{"epoch":9,"kbs":"some"}}}"#,
        r#"{"timed_out":{"of":{"profile":"p","binding":2},"count":256}}"#,
    ] {
        assert!(damaged(&write(change)), "{change}");
    }
}

#[test]
fn a_field_unknown_missing_or_doubled_is_damaged() {
    for change in [
        // Unknown.
        r#"{"generation":{"of":{"profile":"p","binding":2},"value":4,"extra":1}}"#,
        r#"{"retire":{"profile":"p","binding":1,"extra":1}}"#,
        r#"{"placed":{"of":{"profile":"p","binding":2},"rule":{"epoch":9,"kbs":"all","extra":1}}}"#,
        // Missing, an `Option` among them.
        r#"{"generation":{"of":{"profile":"p","binding":2}}}"#,
        r#"{"retire":{"profile":"p"}}"#,
        r#"{"bind":{"of":{"profile":"p","binding":2},"identity":"id","generation":3}}"#,
        r#"{"questioned":{"of":{"profile":"p","binding":2},"kbs":[],"version":1}}"#,
        // Doubled, or two kinds in one change.
        r#"{"generation":{"of":{"profile":"p","binding":2},"value":4,"value":5}}"#,
        r#"{"disable":"p","enable":"p"}"#,
    ] {
        assert!(damaged(&write(change)), "{change}");
    }
    assert!(damaged(r#"{"version":1,"changes":[],"extra":1}"#));
}

#[test]
fn text_that_is_not_a_record_is_damaged() {
    for text in [
        "",
        "null",
        "[]",
        r#"{"version":1}"#,
        r#"{"version":1,"changes":["off"]"#,
    ] {
        assert!(damaged(text), "{text}");
    }
}

#[test]
fn the_records_read_back_replay_as_written() {
    let written: Vec<Record<T>> = (1..)
        .zip(every_change())
        .map(|(seq, change)| Record {
            seq,
            entry: Entry::Changes(vec![change]),
        })
        .collect();
    let read: Vec<Record<T>> = written
        .iter()
        .map(|record| match &record.entry {
            Entry::Changes(changes) => Record {
                seq: record.seq,
                entry: Entry::Changes(decode(&encode(changes).unwrap()).unwrap()),
            },
            Entry::Void => record.clone(),
        })
        .collect();
    let expected = replay(1, written).unwrap();
    assert!(!expected.bindings.is_empty());
    assert_eq!(replay(1, read).unwrap(), expected);
}
