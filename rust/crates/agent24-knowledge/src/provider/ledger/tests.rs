#![allow(clippy::unwrap_used)]

use super::*;

fn fresh(seal_present: bool) -> Ledger<&'static str> {
    Ledger::new(seal_present)
}

fn p(name: &'static str) -> Scope<&'static str> {
    Scope::profile(name)
}

#[test]
fn with_the_seal_present_a_tightening_commits_once_either_write_lands() {
    let mut ledger = fresh(true);
    let off = ledger.tighten(Scope::Host);
    assert!(off.unseal.is_some());
    assert!(!ledger.committed(off.ticket));
    // The record first: committed, the seal's deletion still in flight.
    ledger.record_durable(off.ticket);
    assert!(ledger.committed(off.ticket));

    let mut ledger = fresh(true);
    let off = ledger.tighten(Scope::Host);
    // A second tightening while the deletion is in flight waits on it too.
    let disable = ledger.tighten(p("a"));
    assert_eq!(disable.unseal, None);
    assert!(!ledger.committed(disable.ticket));
    // The deletion first: every tightening in effect is committed.
    ledger.seal_deleted(off.unseal.unwrap());
    assert!(ledger.committed(off.ticket) && ledger.committed(disable.ticket));
}

#[test]
fn once_the_seal_is_gone_a_tightening_is_committed_at_once() {
    let mut ledger = fresh(false);
    let t = ledger.tighten(p("a"));
    assert_eq!(t.unseal, None);
    assert!(ledger.committed(t.ticket));

    let mut ledger = fresh(true);
    let first = ledger.tighten(Scope::Host);
    ledger.seal_deleted(first.unseal.unwrap());
    ledger.record_durable(first.ticket);
    let next = ledger.tighten(p("a"));
    assert!(next.unseal.is_none() && ledger.committed(next.ticket));
}

#[test]
fn a_failed_deletion_is_retried_while_a_tightening_waits_on_it() {
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Host);
    let first = t.unseal.unwrap();
    let retry = ledger.seal_delete_failed(first).unwrap();
    assert_ne!(retry, first);
    assert!(!ledger.committed(t.ticket));
    ledger.seal_deleted(retry);
    assert!(ledger.committed(t.ticket));

    // With every record in, a failed deletion leaves the seal standing: the
    // next tightening deletes it again.
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Host);
    ledger.record_durable(t.ticket);
    assert_eq!(ledger.seal_delete_failed(t.unseal.unwrap()), None);
    let next = ledger.tighten(p("a"));
    assert!(next.unseal.is_some() && !ledger.committed(next.ticket));
}

#[test]
fn a_late_report_of_an_earlier_deletion_commits_nothing() {
    // The deletion is retried, the retry lands, the seal is rebuilt; then
    // the first attempt reports success while a new tightening waits.
    let mut ledger = fresh(true);
    let t1 = ledger.tighten(Scope::Host);
    let d1 = t1.unseal.unwrap();
    let d2 = ledger.seal_delete_failed(d1).unwrap();
    ledger.seal_deleted(d2);
    ledger.record_durable(t1.ticket);
    let rebuild = ledger.reseal().unwrap();
    assert_eq!(ledger.resealed(rebuild), None);
    let t2 = ledger.tighten(p("a"));
    ledger.seal_deleted(d1);
    assert!(!ledger.committed(t2.ticket));
    // A late failure of the earlier one asks for nothing either.
    assert_eq!(ledger.seal_delete_failed(d1), None);
    ledger.seal_deleted(t2.unseal.unwrap());
    assert!(ledger.committed(t2.ticket));
}

#[test]
fn a_failed_record_shows_unsaved_until_it_is_confirmed() {
    let mut ledger = fresh(true);
    let t = ledger.tighten(p("a"));
    assert!(!ledger.unsaved());
    ledger.record_failed(t.ticket);
    assert!(ledger.unsaved());
    // Still not committed: the seal's deletion has not landed either.
    assert!(!ledger.committed(t.ticket));
    ledger.record_durable(t.ticket);
    assert!(!ledger.unsaved() && ledger.committed(t.ticket));
}

#[test]
fn the_seal_is_rebuilt_only_when_gone_with_every_tightening_confirmed() {
    let mut ledger = fresh(true);
    assert_eq!(ledger.reseal(), None);
    let t = ledger.tighten(Scope::Host);
    ledger.seal_deleted(t.unseal.unwrap());
    assert_eq!(ledger.reseal(), None);
    ledger.record_durable(t.ticket);
    let rebuild = ledger.reseal().unwrap();
    assert_eq!(ledger.reseal(), None);
    assert_eq!(ledger.resealed(rebuild), None);
    // Present again: the next tightening deletes it.
    assert!(ledger.tighten(p("a")).unseal.is_some());
}

#[test]
fn a_tightening_during_a_rebuild_deletes_the_seal_again_after_it() {
    let mut ledger = fresh(false);
    let rebuild = ledger.reseal().unwrap();
    let t = ledger.tighten(p("a"));
    assert_eq!(t.unseal, None);
    // The rebuild may land: not committed by the seal being gone.
    assert!(!ledger.committed(t.ticket));
    let delete = ledger.resealed(rebuild).unwrap();
    assert!(!ledger.committed(t.ticket));
    ledger.seal_deleted(delete);
    assert!(ledger.committed(t.ticket));
}

#[test]
fn a_rebuild_with_no_tightening_meanwhile_leaves_the_seal_standing() {
    // The first rebuild had a tightening come in; the next one does not.
    let mut ledger = fresh(false);
    let first = ledger.reseal().unwrap();
    let t = ledger.tighten(p("a"));
    let delete = ledger.resealed(first).unwrap();
    ledger.seal_deleted(delete);
    ledger.record_durable(t.ticket);
    let second = ledger.reseal().unwrap();
    assert_eq!(ledger.resealed(second), None);
    assert!(ledger.tighten(p("a")).unseal.is_some());
}

#[test]
fn a_stray_report_never_moves_the_seal() {
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Host);
    let d = t.unseal.unwrap();
    ledger.seal_deleted(d);
    ledger.record_durable(t.ticket);
    let rebuild = ledger.reseal().unwrap();
    // The deletion reported again while rebuilding: still rebuilding.
    ledger.seal_deleted(d);
    let late = ledger.tighten(p("a"));
    assert!(!ledger.committed(late.ticket));
    // A rebuild report that is not this one is ignored as well.
    assert_eq!(ledger.resealed(d), None);
    assert!(ledger.resealed(rebuild).is_some());
}

#[test]
fn a_late_report_of_an_earlier_rebuild_is_not_taken_for_the_current_one() {
    let mut ledger = fresh(false);
    let first = ledger.reseal().unwrap();
    assert_eq!(ledger.resealed(first), None);
    let t = ledger.tighten(Scope::Host);
    ledger.seal_deleted(t.unseal.unwrap());
    ledger.record_durable(t.ticket);
    let second = ledger.reseal().unwrap();
    // The first rebuild reported again: the second is still in flight, so a
    // tightening now waits for it and deletes the seal after it.
    assert_eq!(ledger.resealed(first), None);
    let late = ledger.tighten(p("a"));
    assert!(late.unseal.is_none() && !ledger.committed(late.ticket));
    assert!(ledger.resealed(second).is_some());
}

#[test]
fn an_unconfirmed_tightening_refuses_loosenings_in_its_scope() {
    let mut ledger = fresh(true);
    let a = ledger.tighten(p("a"));
    // The same profile, a set naming it, and host level are refused.
    assert_eq!(
        ledger.loosen(p("a"), None, false),
        Err(Refused::LocalStorage)
    );
    let both = Scope::Profiles(vec!["b", "a"]);
    assert_eq!(ledger.loosen(both, None, false), Err(Refused::LocalStorage));
    assert_eq!(
        ledger.loosen(Scope::Host, None, false),
        Err(Refused::LocalStorage)
    );
    // With Off in effect the refusal reads knowledge_off.
    assert_eq!(
        ledger.loosen(Scope::Host, None, true),
        Err(Refused::KnowledgeOff)
    );
    // Another profile is not blocked; committing is not confirming.
    ledger.seal_deleted(a.unseal.unwrap());
    assert!(ledger.loosen(p("b"), None, false).is_ok());
    assert!(ledger.loosen(p("a"), None, false).is_err());
    ledger.record_durable(a.ticket);
    assert!(ledger.loosen(p("a"), None, false).is_ok());
    assert!(ledger.loosen(Scope::Host, None, false).is_ok());
}

#[test]
fn a_tightening_over_several_profiles_refuses_a_loosening_of_any_one() {
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Profiles(vec!["a", "c"]));
    assert!(ledger.loosen(p("a"), None, false).is_err());
    assert!(ledger.loosen(p("c"), None, false).is_err());
    assert!(ledger.loosen(p("b"), None, false).is_ok());
    ledger.record_durable(t.ticket);
    assert!(ledger.loosen(p("a"), None, false).is_ok());
}

#[test]
fn a_host_level_tightening_refuses_every_loosening() {
    let mut ledger = fresh(true);
    let off = ledger.tighten(Scope::Host);
    assert!(ledger.loosen(p("a"), None, false).is_err());
    ledger.record_durable(off.ticket);
    assert!(ledger.loosen(p("a"), None, false).is_ok());
}

#[test]
fn a_loosening_waits_for_the_loosening_it_depends_on() {
    let mut ledger = fresh(true);
    let bind = ledger.loosen(p("a"), None, false).unwrap();
    assert_eq!(
        ledger.loosen(p("a"), Some(bind), false),
        Err(Refused::LocalStorage)
    );
    assert_eq!(
        ledger.loosen(p("a"), Some(bind), true),
        Err(Refused::KnowledgeOff)
    );
    assert_eq!(ledger.loosening_durable(bind), Confirmed::Applied);
    assert!(ledger.loosen(p("a"), Some(bind), false).is_ok());
}

#[test]
fn a_loosening_never_touches_the_seal() {
    // FI-1a: an On written or not, the seal stands and recovery is normal.
    let mut ledger = fresh(true);
    let on = ledger.loosen(Scope::Host, None, true).unwrap();
    assert_eq!(ledger.reseal(), None);
    assert_eq!(ledger.loosening_durable(on), Confirmed::Applied);
    assert!(ledger.tighten(p("a")).unseal.is_some());
}

#[test]
fn an_abandoned_loosening_is_offset_over_its_own_scope() {
    let mut ledger = fresh(true);
    let on = ledger.loosen(Scope::Host, None, true).unwrap();
    let off = ledger.abandon(on).unwrap();
    assert!(off.unseal.is_some() && !ledger.committed(off.ticket));
    assert_eq!(ledger.abandon(on), None);
    // The offset is host level like the On it offsets: every loosening is
    // refused until it is confirmed.
    assert!(ledger.loosen(p("b"), None, true).is_err());
    ledger.record_durable(off.ticket);
    assert!(ledger.loosen(p("b"), None, true).is_ok());
}

#[test]
fn an_abandoned_profile_loosening_is_offset_over_that_profile_only() {
    let mut ledger = fresh(true);
    let bind = ledger.loosen(p("a"), None, false).unwrap();
    let offset = ledger.abandon(bind).unwrap();
    assert!(ledger.loosen(p("b"), None, false).is_ok());
    assert!(ledger.loosen(p("a"), None, false).is_err());
    ledger.record_durable(offset.ticket);
    assert!(ledger.loosen(p("a"), None, false).is_ok());
}

#[test]
fn an_abandoned_loosening_never_satisfies_a_dependency() {
    let mut ledger = fresh(false);
    let bind = ledger.loosen(p("a"), None, false).unwrap();
    let offset = ledger.abandon(bind).unwrap();
    assert!(ledger.loosen(p("b"), Some(bind), false).is_err());
    // Its late confirmation is ignored and leaves it out of effect.
    ledger.record_durable(offset.ticket);
    assert_eq!(ledger.loosening_durable(bind), Confirmed::Ignored);
    assert_eq!(
        ledger.loosen(p("b"), Some(bind), false),
        Err(Refused::LocalStorage)
    );
}

#[test]
fn a_confirmed_loosening_can_no_longer_be_abandoned() {
    let mut ledger = fresh(true);
    let on = ledger.loosen(Scope::Host, None, true).unwrap();
    assert_eq!(ledger.loosening_durable(on), Confirmed::Applied);
    assert_eq!(ledger.abandon(on), None);
    assert_eq!(ledger.loosening_durable(on), Confirmed::Ignored);
}

#[test]
#[should_panic]
fn an_empty_scope_is_a_caller_bug() {
    fresh(true).tighten(Scope::Profiles(Vec::new()));
}
