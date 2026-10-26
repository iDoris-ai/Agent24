#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::ledger::Scope;

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

type J = Journal<T, &'static str>;
type C = Change<T>;

fn take(journal: &mut J) -> (Attempt, u64, Vec<C>) {
    match journal.take() {
        Some((
            attempt,
            Record {
                seq,
                entry: Entry::Changes(changes),
            },
        )) => (attempt, seq, changes),
        other => panic!("{other:?}"),
    }
}

/// One write at a time, by number; a report of anything but the attempt in
/// flight moves nothing.
#[test]
fn writes_run_one_at_a_time_in_order() {
    let mut journal = J::new(10);
    journal.push(vec![C::Off], "off");
    journal.push(vec![C::Disable("a")], "disable");
    let (off, seq, changes) = take(&mut journal);
    assert_eq!((seq, changes), (10, vec![C::Off]));
    assert_eq!(journal.take(), None, "one is running");
    assert_eq!(
        journal.written(off, Written::Durable),
        Report::Durable("off")
    );
    assert_eq!(journal.written(off, Written::Durable), Report::Ignored);
    let (disable, seq, changes) = take(&mut journal);
    assert_eq!((seq, changes), (11, vec![C::Disable("a")]));
    assert_eq!(journal.written(off, Written::Failed), Report::Ignored);
    assert_eq!(journal.take(), None, "still running");
    assert_eq!(
        journal.written(disable, Written::Durable),
        Report::Durable("disable")
    );
    assert_eq!(journal.take(), None);
    journal.push(vec![C::On], "on");
    assert_eq!(take(&mut journal).1, 12);
}

/// A.1: a failed write is taken again as it was, under its number, before
/// any later one; a late report of the failed attempt moves nothing.
#[test]
fn a_failed_write_is_taken_again_under_its_number() {
    let mut journal = J::new(1);
    journal.push(vec![C::Off], "off");
    journal.push(vec![C::On], "on");
    let (first, seq, changes) = take(&mut journal);
    assert_eq!(
        journal.written(first, Written::Failed),
        Report::Failed(&"off")
    );
    assert_eq!(journal.written(first, Written::Failed), Report::Ignored);
    let (retry, again, same) = take(&mut journal);
    assert_eq!((again, &same), (seq, &changes));
    assert_ne!(retry, first);
    for late in [Written::Failed, Written::Durable] {
        assert_eq!(journal.written(first, late), Report::Ignored);
    }
    assert_eq!(journal.take(), None, "the retry still runs");
    assert_eq!(
        journal.written(retry, Written::Durable),
        Report::Durable("off")
    );
    assert_eq!(take(&mut journal).1, 2);
}

/// The ledger's seal through its writes: a deletion is tried again while a
/// tightening is uncommitted; the rebuild waits for every record, and a
/// tightening during it deletes the seal again once it is in.
#[test]
fn the_seal_writes_follow_the_ledger() {
    let mut ledger = Ledger::new(true);
    let first = ledger.tighten(Scope::Host);
    let delete = SealWrite::Delete(first.unseal.unwrap());
    let again = sealed(&mut ledger, delete, Written::Failed).unwrap();
    assert!(matches!(again, SealWrite::Delete(_)) && again != delete);
    assert_eq!(
        sealed(&mut ledger, again, Written::Durable),
        None,
        "record due"
    );
    assert!(ledger.committed(first.ticket));
    ledger.record_durable(first.ticket);
    let rebuild = ledger.reseal().map(SealWrite::Rebuild).unwrap();
    let during = ledger.tighten(Scope::profile("a"));
    assert_eq!(during.unseal, None);
    let redelete = sealed(&mut ledger, rebuild, Written::Durable).unwrap();
    assert!(matches!(redelete, SealWrite::Delete(_)));
    // A deletion that lands with every record in rebuilds at once.
    ledger.record_durable(during.ticket);
    let rebuild = sealed(&mut ledger, redelete, Written::Durable).unwrap();
    assert!(matches!(rebuild, SealWrite::Rebuild(_)));
    // Unknown, the rebuild may have landed: the seal counts as there.
    assert_eq!(sealed(&mut ledger, rebuild, Written::Failed), None);
    assert!(ledger.tighten(Scope::Host).unseal.is_some());
}
