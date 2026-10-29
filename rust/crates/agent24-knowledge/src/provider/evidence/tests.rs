use super::*;

type E = Evidence<&'static str>;

fn at(kb: &'static str, epoch: u64) -> Delivered<&'static str> {
    Delivered {
        kbs: kbs(&[kb]),
        epoch,
    }
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
    assert!(evidence.register(["a"]));
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
    assert!(evidence.register(["b"]));
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
    assert!(evidence.register(["c"]));
    assert_eq!(evidence.recheck().map(|r| r.version), Some(3));
    assert!(!evidence.release(2));
    assert!(evidence.reverify_pending());
}

#[test]
fn the_third_timeout_in_a_row_stops_rechecks_until_a_retry() {
    let mut evidence = E::default();
    evidence.timed_out();
    assert!(evidence.register(["a"]));
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
    assert!(evidence.register(["b"]));
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
    assert!(evidence.register(["a"]));
    let rule = evidence.revoked();
    assert_eq!(rule.kbs, Kbs::All);
    assert!(!evidence.reverify_pending());
    assert_eq!(evidence.shown(false, &at("a", 0)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("b", 0)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &at("a", rule.epoch)), Shown::Body);
    // ST-18p: retired with a reverification still pending.
    assert!(evidence.register(["a"]));
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
    assert!(evidence.kept(&"b"));
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
    // A failed retry of the delete proves nothing (S5.12): still held back
    // until a recheck finds c listed.
    restored.tombstone("c");
    assert!(!restored.kept(&"c"));
    assert_eq!(restored.shown(false, &at("c", 0)), Shown::Withheld);
    restored.found(&"c");
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
        visible: kbs(&["d"]),
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
    assert!(evidence.register(["c"]));
    assert_eq!(evidence.recheck().map(|r| r.version), Some(8));
}

fn listed(all: &[&'static str]) -> Scope<&'static str> {
    Scope::Listed(kbs(all))
}

fn removed(all: &[&'static str]) -> BTreeSet<&'static str> {
    kbs(all)
}

#[test]
fn a_kb_asked_for_and_visible_but_left_out_was_removed() {
    let mut evidence = E::default();
    // A ready probe without scope only adds.
    let probe = evidence.observe(&kbs(&[]), &listed(&["a", "b"]), evidence.epoch(), true);
    assert_eq!(probe.added, kbs(&["a", "b"]));
    // ST-28b: Q = {a, b}, only a in effect.
    let out = evidence.observe(&kbs(&["a", "b"]), &listed(&["a"]), evidence.epoch(), true);
    assert_eq!(
        out,
        Observed {
            added: kbs(&[]),
            removed: removed(&["b"])
        }
    );
    // Out of the set: a second omission tells nothing new.
    let again = evidence.observe(&kbs(&["a", "b"]), &listed(&["a"]), evidence.epoch(), true);
    assert_eq!(again.removed, kbs(&[]));
    // ST-28c: another Q's answer is never compared; `empty_scope` removes
    // every KB of its own Q that was visible.
    let mut evidence = E::default();
    evidence.observe(
        &kbs(&["a", "b"]),
        &listed(&["a", "b"]),
        evidence.epoch(),
        true,
    );
    assert_eq!(
        evidence
            .observe(&kbs(&["a"]), &listed(&["a"]), 0, true)
            .removed,
        kbs(&[])
    );
    let out = evidence.observe(&kbs(&["b", "x"]), &Scope::Empty, evidence.epoch(), true);
    assert_eq!(out.removed, removed(&["b"]));
    // ST-28a: `empty_scope` over KBs never visible is no narrowing.
    assert_eq!(
        evidence
            .observe(&kbs(&["y"]), &Scope::Empty, evidence.epoch(), true)
            .removed,
        kbs(&[])
    );
}

#[test]
fn only_a_request_on_the_current_credential_tells_a_removal() {
    // EV-34: a request sent before the renewal comes back narrower.
    let mut evidence = E::default();
    evidence.observe(
        &kbs(&["a", "b"]),
        &listed(&["a", "b"]),
        evidence.epoch(),
        true,
    );
    let stale = evidence.observe(
        &kbs(&["a", "b", "c"]),
        &listed(&["a", "c"]),
        evidence.epoch(),
        false,
    );
    assert_eq!(
        stale,
        Observed {
            added: kbs(&["c"]),
            removed: kbs(&[])
        }
    );
    // EV-25: what joined is then compared on the current credential.
    let out = evidence.observe(
        &kbs(&["a", "b", "c"]),
        &listed(&["a"]),
        evidence.epoch(),
        true,
    );
    assert_eq!(out.removed, removed(&["b", "c"]));
}

#[test]
fn evidence_not_yet_placed_counts_as_visible() {
    // ST-28e: after a restart the set lacks b, but evidence of b stands.
    let mut evidence = E::restore(Saved::default());
    evidence.delivered(["b"], 0);
    assert_eq!(
        evidence
            .observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true)
            .removed,
        removed(&["b"])
    );
    // Placed by a rule since: no longer.
    let mut evidence = E::default();
    evidence.delivered(["b"], 0);
    evidence.place(Kbs::All);
    assert_eq!(
        evidence
            .observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true)
            .removed,
        kbs(&[])
    );
    // The latest delivery counts, whatever the order.
    evidence.delivered(["b"], 1);
    evidence.delivered(["b"], 0);
    assert_eq!(
        evidence
            .observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true)
            .removed,
        removed(&["b"])
    );
    // Restored as persisted.
    let saved = Saved {
        visible: kbs(&["d"]),
        ..Saved::default()
    };
    let mut restored = E::restore(saved);
    assert_eq!(
        restored
            .observe(&kbs(&["d"]), &Scope::Empty, restored.epoch(), true)
            .removed,
        removed(&["d"])
    );
}

#[test]
fn a_kb_being_deleted_neither_joins_nor_narrows() {
    // ST-28g: b visible, with evidence; the host deletes it.
    let mut evidence = E::default();
    evidence.observe(&kbs(&[]), &listed(&["a", "b"]), evidence.epoch(), true);
    evidence.delivered(["b"], 0);
    evidence.tombstone("b");
    let out = evidence.observe(
        &kbs(&["a", "b"]),
        &listed(&["a", "b"]),
        evidence.epoch(),
        true,
    );
    assert_eq!(out, Observed::default());
    let out = evidence.observe(&kbs(&["a", "b"]), &listed(&["a"]), evidence.epoch(), true);
    assert_eq!(out, Observed::default());
    // ST-28h: the delete failed. Its evidence makes it visible again.
    assert!(evidence.kept(&"b"));
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true);
    assert_eq!(out.removed, removed(&["b"]));
    // Deleted: covered by its rule, and gone from the set.
    let mut evidence = E::default();
    evidence.observe(&kbs(&[]), &listed(&["b"]), evidence.epoch(), true);
    evidence.delivered(["b"], 0);
    evidence.tombstone("b");
    evidence.deleted("b");
    // A search admitted before the deletion comes back listing b: it does
    // not join, so no later omission narrows (ST-28g).
    let late = evidence.observe(&kbs(&["b", "c"]), &listed(&["b", "c"]), 0, true);
    assert_eq!(late.added, kbs(&["c"]));
    assert_eq!(
        evidence.observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true),
        Observed::default()
    );
    // Admitted since, it is listed anew, and its omission narrows again.
    let fresh = evidence.observe(&kbs(&["b"]), &listed(&["b"]), evidence.epoch(), true);
    assert_eq!(fresh.added, kbs(&["b"]));
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true);
    assert_eq!(out.removed, removed(&["b"]));
    // A rule over every KB placed without invalidating (a renewal that fails
    // too): a response admitted before it still adds.
    let mut evidence = E::default();
    evidence.place(Kbs::All);
    let late = evidence.observe(&kbs(&["c"]), &listed(&["c"]), 0, true);
    assert_eq!(late.added, kbs(&["c"]));
    // Read back unresolved: the same.
    let saved = Saved {
        tombstones: kbs(&["b"]),
        visible: kbs(&["a"]),
        ..Saved::default()
    };
    let mut restored = E::restore(saved);
    restored.delivered(["b"], 0);
    let out = restored.observe(
        &kbs(&["a", "b"]),
        &listed(&["a", "b"]),
        restored.epoch(),
        true,
    );
    assert_eq!(out, Observed::default());
    assert_eq!(
        restored.observe(&kbs(&["b"]), &Scope::Empty, restored.epoch(), true),
        Observed::default()
    );
}

#[test]
fn evidence_of_several_kbs_shows_by_the_strictest_of_them() {
    // S5.6: a ContextPack belongs to every source KB.
    let pack = |epoch| Delivered {
        kbs: kbs(&["a", "b"]),
        epoch,
    };
    let mut evidence = E::default();
    assert!(evidence.register(["b"]));
    assert_eq!(evidence.shown(false, &pack(0)), Shown::Withheld);
    evidence.tombstone("a");
    evidence.deleted("a");
    assert_eq!(evidence.shown(false, &pack(0)), Shown::Placeholder);
    assert_eq!(evidence.shown(false, &pack(1)), Shown::Withheld);
    assert!(evidence.release(1));
    assert_eq!(evidence.shown(false, &pack(1)), Shown::Body);
    // Evidence naming no KB is held back (#1305 note 4).
    let none = Delivered {
        kbs: BTreeSet::new(),
        epoch: 1,
    };
    assert_eq!(evidence.shown(false, &none), Shown::Withheld);
    // Each of its KBs counts as delivered, and only those.
    let mut evidence = E::default();
    evidence.delivered(["a"], 0);
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, 0, true);
    assert_eq!(out, Observed::default());
    evidence.delivered(["a", "b"], 0);
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, 0, true);
    assert_eq!(out.removed, kbs(&["b"]));
    // A placeholder for good once a is deleted: b no longer counts by it,
    // though other evidence of b still would.
    let mut evidence = E::default();
    evidence.delivered(["a", "b"], 0);
    evidence.tombstone("a");
    evidence.deleted("a");
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, 1, true);
    assert_eq!(out, Observed::default());
    evidence.delivered(["b"], 0);
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, 1, true);
    assert_eq!(out.removed, kbs(&["b"]));
}

#[test]
fn an_empty_question_registers_nothing() {
    let mut evidence = E::default();
    assert!(!evidence.register(Vec::new()));
    assert!(!evidence.reverify_pending());
    assert_eq!(evidence.recheck(), None);
    assert_eq!(evidence.passed(0), Passed::Ignored);
}

#[test]
fn a_failed_retry_of_a_delete_read_back_does_not_bring_the_kb_back() {
    // PR-Daemon's probe on #1297 (G-A): the earlier delete may have gone
    // through; an omission must not then be judged a narrowing.
    let saved = Saved {
        tombstones: kbs(&["b"]),
        visible: kbs(&["a"]),
        ..Saved::default()
    };
    let mut evidence = E::restore(saved);
    evidence.delivered(["b"], 0);
    evidence.tombstone("b");
    assert!(!evidence.kept(&"b"));
    let out = evidence.observe(&kbs(&["b"]), &Scope::Empty, evidence.epoch(), true);
    assert_eq!(out, Observed::default());
    assert_eq!(evidence.shown(false, &at("b", 0)), Shown::Withheld);
}
