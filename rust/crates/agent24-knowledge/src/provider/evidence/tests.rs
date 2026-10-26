use super::*;

type E = Evidence<&'static str>;

fn at(kb: &'static str, epoch: u64) -> Delivered<&'static str> {
    Delivered { kb, epoch }
}

fn kbs(all: &[&'static str]) -> BTreeSet<&'static str> {
    all.iter().copied().collect()
}

#[test]
fn a_rule_covers_what_was_admitted_before_it_in_its_kbs() {
    let mut evidence = E::default();
    let before = at("a", evidence.epoch());
    let rule = evidence.place(Kbs::All);
    assert_eq!(
        rule,
        Rule {
            epoch: 1,
            kbs: Kbs::All
        }
    );
    // Admitted at the rule's own epoch: after the trigger.
    let after = at("a", evidence.epoch());
    assert_eq!(after.epoch, 1);
    assert_eq!(evidence.shown(false, &before), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &after), Shown::Body);
    // A deletion's rule covers its KB alone.
    evidence.tombstone("b");
    assert_eq!(evidence.deleted("b").epoch, 2);
    assert_eq!(evidence.shown(false, &at("b", 1)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("a", 1)), Shown::Body);
    assert_eq!(evidence.shown(false, &at("b", 2)), Shown::Body);
    // A retired binding's evidence, whatever its epoch.
    assert_eq!(evidence.shown(true, &at("a", 2)), Shown::Placeholder);
}

#[test]
fn a_pending_reverification_holds_back_its_kbs_until_the_current_version_passes() {
    let mut evidence = E::default();
    assert_eq!(evidence.recheck(), None);
    assert_eq!(evidence.passed(0), Passed::Ignored);
    evidence.register(["a"]);
    assert!(evidence.reverify_pending());
    let first = Recheck {
        kbs: kbs(&["a"]),
        version: 1,
    };
    assert_eq!(evidence.recheck(), Some(first));
    // Delivered after the registration as well; another KB is shown.
    assert_eq!(evidence.shown(false, &at("a", 0)), Shown::Withheld);
    assert_eq!(evidence.shown(false, &at("a", 5)), Shown::Withheld);
    assert_eq!(evidence.shown(false, &at("b", 0)), Shown::Body);
    // ST-18r: registered again while version 1 is rechecked.
    evidence.register(["b"]);
    let merged = Recheck {
        kbs: kbs(&["a", "b"]),
        version: 2,
    };
    assert_eq!(evidence.passed(1), Passed::Again(merged.clone()));
    assert!(!evidence.release(1));
    assert!(evidence.reverify_pending());
    assert_eq!(evidence.shown(false, &at("b", 0)), Shown::Withheld);
    assert_eq!(evidence.passed(2), Passed::Release);
    assert!(evidence.release(2));
    assert!(!evidence.reverify_pending());
    assert_eq!(evidence.shown(false, &at("a", 0)), Shown::Body);
    // A late answer to a version released counts for nothing.
    assert_eq!(evidence.passed(2), Passed::Ignored);
    assert!(!evidence.release(2));
    // Versions go on: a new registration is not released by an old answer.
    evidence.register(["c"]);
    assert_eq!(evidence.recheck().map(|r| r.version), Some(3));
    assert!(!evidence.release(2));
    assert!(evidence.reverify_pending());
}

#[test]
fn the_third_timeout_in_a_row_stops_rechecks_until_a_retry() {
    let mut evidence = E::default();
    evidence.timed_out();
    evidence.register(["a"]);
    // A timeout with nothing pending counted for nothing.
    for _ in 0..2 {
        evidence.timed_out();
        assert!(evidence.recheck().is_some());
    }
    evidence.timed_out();
    assert_eq!(evidence.recheck(), None);
    // Still pending: no admission, still held back.
    assert!(evidence.reverify_pending());
    assert_eq!(evidence.shown(false, &at("a", 0)), Shown::Withheld);
    evidence.retry();
    assert!(evidence.recheck().is_some());
    // Timed out twice, then an older version passed: not three in a row
    // once the caller persists the reset.
    evidence.timed_out();
    evidence.timed_out();
    evidence.register(["b"]);
    assert!(matches!(evidence.passed(1), Passed::Again(_)));
    evidence.retry();
    evidence.timed_out();
    assert!(evidence.recheck().is_some());
    // ST-26g: two in a row persisted, then a restart; one more stops them.
    evidence.timed_out();
    evidence.timed_out();
    let saved = Saved {
        epoch: evidence.epoch(),
        version: 1,
        pending: Some(Pending {
            kbs: kbs(&["a"]),
            timeouts: 2,
        }),
        ..Saved::default()
    };
    let mut restored = E::restore(saved);
    assert!(restored.recheck().is_some());
    assert_eq!(restored.passed(1), Passed::Release);
    restored.timed_out();
    assert_eq!(restored.recheck(), None);
}

#[test]
fn a_recheck_revoked_turns_what_it_held_back_into_placeholders() {
    let mut evidence = E::default();
    evidence.register(["a"]);
    let rule = evidence.revoked();
    assert_eq!(rule.kbs, Kbs::All);
    assert!(!evidence.reverify_pending());
    assert_eq!(evidence.shown(false, &at("a", 0)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("b", 0)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("a", rule.epoch)), Shown::Body);
    // ST-18p: retired with a reverification still pending.
    evidence.register(["a"]);
    assert_eq!(
        evidence.shown(true, &at("a", rule.epoch)),
        Shown::Placeholder
    );
}

#[test]
fn a_tombstone_withholds_only_once_read_back_unresolved() {
    let mut evidence = E::default();
    evidence.tombstone("b");
    assert_eq!(evidence.tombstone_of(&"b"), Some(Tombstone::Pending));
    // ST-28h: while deleting the evidence stays; a failure takes it back.
    assert_eq!(evidence.shown(false, &at("b", 0)), Shown::Body);
    evidence.kept(&"b");
    assert_eq!(evidence.tombstone_of(&"b"), None);
    assert_eq!(evidence.epoch(), 0);
    // FI-5d: read back with no outcome, held back until the host finds out.
    let saved = Saved {
        tombstones: kbs(&["b", "c"]),
        ..Saved::default()
    };
    let mut restored = E::restore(saved);
    assert_eq!(restored.tombstone_of(&"b"), Some(Tombstone::Unresolved));
    assert_eq!(restored.shown(false, &at("b", 0)), Shown::Withheld);
    assert_eq!(restored.shown(false, &at("a", 0)), Shown::Body);
    // Deleting again does not bring the evidence back meanwhile.
    restored.tombstone("b");
    assert_eq!(restored.tombstone_of(&"b"), Some(Tombstone::Unresolved));
    assert_eq!(restored.shown(false, &at("b", 0)), Shown::Withheld);
    restored.deleted("b");
    assert_eq!(restored.shown(false, &at("b", 0)), Shown::Placeholder);
    assert_eq!(restored.tombstone_of(&"b"), None);
    restored.kept(&"c");
    assert_eq!(restored.shown(false, &at("c", 0)), Shown::Body);
}

#[test]
fn what_was_persisted_comes_back_as_it_was() {
    let rules = vec![
        Rule {
            epoch: 3,
            kbs: Kbs::One("b"),
        },
        Rule {
            epoch: 4,
            kbs: Kbs::All,
        },
    ];
    let saved = Saved {
        epoch: 4,
        rules,
        version: 7,
        pending: Some(Pending {
            kbs: kbs(&["c"]),
            timeouts: 0,
        }),
        tombstones: BTreeSet::new(),
    };
    let mut evidence = E::restore(saved);
    assert_eq!(evidence.epoch(), 4);
    assert_eq!(evidence.shown(false, &at("b", 2)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("a", 3)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("c", 4)), Shown::Withheld);
    assert_eq!(evidence.shown(false, &at("a", 4)), Shown::Body);
    assert_eq!(evidence.place(Kbs::All).epoch, 5);
    // The version goes on from the one persisted.
    assert_eq!(evidence.passed(7), Passed::Release);
    evidence.register(["c"]);
    assert_eq!(evidence.recheck().map(|r| r.version), Some(8));
}
