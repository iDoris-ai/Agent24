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
    assert!(off.unseal);
    assert!(!ledger.committed(off.ticket));
    // The record first: committed, the seal's deletion still in flight.
    ledger.record_durable(off.ticket);
    assert!(ledger.committed(off.ticket));

    let mut ledger = fresh(true);
    let off = ledger.tighten(Scope::Host);
    // A second tightening while the deletion is in flight waits on it too.
    let disable = ledger.tighten(p("a"));
    assert!(!disable.unseal);
    assert!(!ledger.committed(disable.ticket));
    // The deletion first: every tightening in effect is committed.
    ledger.seal_deleted();
    assert!(ledger.committed(off.ticket) && ledger.committed(disable.ticket));
}

#[test]
fn once_the_seal_is_gone_a_tightening_is_committed_at_once() {
    let mut ledger = fresh(false);
    let t = ledger.tighten(p("a"));
    assert!(!t.unseal);
    assert!(ledger.committed(t.ticket));

    let mut ledger = fresh(true);
    let first = ledger.tighten(Scope::Host);
    ledger.seal_deleted();
    ledger.record_durable(first.ticket);
    let next = ledger.tighten(p("a"));
    assert!(!next.unseal && ledger.committed(next.ticket));
}

#[test]
fn a_failed_deletion_is_retried_while_a_tightening_waits_on_it() {
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Host);
    assert!(ledger.seal_delete_failed());
    assert!(!ledger.committed(t.ticket));
    ledger.seal_deleted();
    assert!(ledger.committed(t.ticket));

    // With every record in, a failed deletion leaves the seal standing: the
    // next tightening deletes it again.
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Host);
    ledger.record_durable(t.ticket);
    assert!(!ledger.seal_delete_failed());
    let next = ledger.tighten(p("a"));
    assert!(next.unseal && !ledger.committed(next.ticket));
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
    assert!(!ledger.reseal());
    let t = ledger.tighten(Scope::Host);
    ledger.seal_deleted();
    assert!(!ledger.reseal());
    ledger.record_durable(t.ticket);
    assert!(ledger.reseal());
    assert!(!ledger.resealed());
    // Present again: the next tightening deletes it.
    assert!(ledger.tighten(p("a")).unseal);
}

#[test]
fn a_tightening_during_a_rebuild_deletes_the_seal_again_after_it() {
    let mut ledger = fresh(false);
    assert!(ledger.reseal());
    let t = ledger.tighten(p("a"));
    assert!(t.unseal);
    // The rebuild may land: not committed by the seal being gone.
    assert!(!ledger.committed(t.ticket));
    assert!(ledger.resealed());
    assert!(!ledger.committed(t.ticket));
    ledger.seal_deleted();
    assert!(ledger.committed(t.ticket));
}

#[test]
fn a_stray_deletion_report_never_marks_a_rebuilt_seal_gone() {
    let mut ledger = fresh(false);
    assert!(ledger.reseal());
    ledger.seal_deleted();
    assert!(!ledger.resealed());
    ledger.seal_deleted();
    let t = ledger.tighten(p("a"));
    assert!(t.unseal && !ledger.committed(t.ticket));
}

#[test]
fn an_unconfirmed_tightening_refuses_loosenings_in_its_scope() {
    let mut ledger = fresh(true);
    let a = ledger.tighten(p("a"));
    // The same profile, a set naming it, and host level are refused.
    assert_eq!(
        ledger.loosen(&p("a"), None, false),
        Err(Refused::LocalStorage)
    );
    let both = Scope::Profiles(vec!["b", "a"]);
    assert_eq!(
        ledger.loosen(&both, None, false),
        Err(Refused::LocalStorage)
    );
    assert_eq!(
        ledger.loosen(&Scope::Host, None, false),
        Err(Refused::LocalStorage)
    );
    // With Off in effect the refusal reads knowledge_off.
    assert_eq!(
        ledger.loosen(&Scope::Host, None, true),
        Err(Refused::KnowledgeOff)
    );
    // Another profile is not blocked; committing is not confirming.
    ledger.seal_deleted();
    assert!(ledger.loosen(&p("b"), None, false).is_ok());
    assert!(ledger.loosen(&p("a"), None, false).is_err());
    ledger.record_durable(a.ticket);
    assert!(ledger.loosen(&p("a"), None, false).is_ok());
    assert!(ledger.loosen(&Scope::Host, None, false).is_ok());
}

#[test]
fn a_tightening_over_several_profiles_refuses_a_loosening_of_any_one() {
    let mut ledger = fresh(true);
    let t = ledger.tighten(Scope::Profiles(vec!["a", "c"]));
    assert!(ledger.loosen(&p("a"), None, false).is_err());
    assert!(ledger.loosen(&p("c"), None, false).is_err());
    assert!(ledger.loosen(&p("b"), None, false).is_ok());
    ledger.record_durable(t.ticket);
    assert!(ledger.loosen(&p("a"), None, false).is_ok());
}

#[test]
fn a_host_level_tightening_refuses_every_loosening() {
    let mut ledger = fresh(true);
    let off = ledger.tighten(Scope::Host);
    assert!(ledger.loosen(&p("a"), None, false).is_err());
    ledger.record_durable(off.ticket);
    assert!(ledger.loosen(&p("a"), None, false).is_ok());
}

#[test]
fn a_loosening_waits_for_the_loosening_it_depends_on() {
    let mut ledger = fresh(true);
    let bind = ledger.loosen(&p("a"), None, false).unwrap();
    assert_eq!(
        ledger.loosen(&p("a"), Some(bind), false),
        Err(Refused::LocalStorage)
    );
    assert_eq!(
        ledger.loosen(&p("a"), Some(bind), true),
        Err(Refused::KnowledgeOff)
    );
    assert_eq!(ledger.loosening_durable(bind), Confirmed::Applied);
    assert!(ledger.loosen(&p("a"), Some(bind), false).is_ok());
}

#[test]
fn a_loosening_never_touches_the_seal() {
    // FI-1a: an On written or not, the seal stands and recovery is normal.
    let mut ledger = fresh(true);
    let on = ledger.loosen(&Scope::Host, None, true).unwrap();
    assert!(!ledger.reseal());
    assert_eq!(ledger.loosening_durable(on), Confirmed::Applied);
    assert!(ledger.tighten(p("a")).unseal);
}

#[test]
fn an_abandoned_loosening_is_offset_and_its_late_confirmation_ignored() {
    let mut ledger = fresh(true);
    let on = ledger.loosen(&Scope::Host, None, true).unwrap();
    let off = ledger.abandon(on, Scope::Host).unwrap();
    assert!(off.unseal && !ledger.committed(off.ticket));
    assert_eq!(ledger.abandon(on, Scope::Host), None);
    // The offset is a tightening like any other: it refuses loosenings.
    assert!(ledger.loosen(&p("a"), None, true).is_err());
    assert_eq!(ledger.loosening_durable(on), Confirmed::Ignored);
    // Depending on an abandoned loosening is refused too.
    let mut ledger = fresh(false);
    let bind = ledger.loosen(&p("a"), None, false).unwrap();
    ledger.abandon(bind, p("a")).unwrap();
    assert!(ledger.loosen(&p("b"), Some(bind), false).is_err());
}

#[test]
fn a_confirmed_loosening_can_no_longer_be_abandoned() {
    let mut ledger = fresh(true);
    let on = ledger.loosen(&Scope::Host, None, true).unwrap();
    assert_eq!(ledger.loosening_durable(on), Confirmed::Applied);
    assert_eq!(ledger.abandon(on, Scope::Host), None);
    assert_eq!(ledger.loosening_durable(on), Confirmed::Ignored);
}
