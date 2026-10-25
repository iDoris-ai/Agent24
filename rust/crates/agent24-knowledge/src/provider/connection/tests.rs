#![allow(clippy::unwrap_used)]

use super::*;
use crate::provider::state::DataOperation;
use ConnectionPhase::{Degraded, Loading, Ready, Unavailable};
use UnavailableCause as U;

const READY: Probe = Probe::Answered {
    live: true,
    search_ready: true,
    read_ready: true,
};
const SEARCH_ONLY: Probe = Probe::Answered {
    live: true,
    search_ready: true,
    read_ready: false,
};

fn loading(once_servable: bool) -> Connection {
    let mut connection = Connection::new(Gate::new(1), once_servable);
    connection.start_loading();
    connection
}

#[test]
fn a_probe_moves_the_phase_without_touching_the_generation_or_calls() {
    let mut connection = loading(true);
    let (call, _) = connection.gate_mut().open_call().unwrap();
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, READY),
        ProbeApplied::Moved(Ready)
    );
    let ticket = connection.probe().unwrap();
    let down = Unavailable(U::Dependency);
    assert_eq!(
        connection.apply_probe(ticket, Probe::NoAnswer),
        ProbeApplied::Moved(down)
    );
    assert_eq!(connection.phase(), Some(down));
    // Degraded and unavailable abort nothing and keep the generation (S3.4).
    assert_eq!(connection.gate().reported(), 1);
    assert_eq!(connection.gate().may_append(call), Ok(()));
    // A retrying unavailable is probed again and recovers.
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, SEARCH_ONLY),
        ProbeApplied::Moved(Degraded {
            servable: DataOperation::Search
        })
    );
}

#[test]
fn only_a_result_sent_later_than_the_last_applied_one_counts_and_only_in_its_round() {
    let mut connection = loading(true);
    let early = connection.probe().unwrap();
    let late = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(late, READY),
        ProbeApplied::Moved(Ready)
    );
    // The earlier probe's result arrives after: not applied.
    assert_eq!(
        connection.apply_probe(early, Probe::NoAnswer),
        ProbeApplied::Ignored
    );
    assert_eq!(connection.phase(), Some(Ready));
    // A probe of an ended round does not count either.
    let stale = connection.probe().unwrap();
    connection.start_loading();
    assert_eq!(connection.apply_probe(stale, READY), ProbeApplied::Ignored);
    assert_eq!(connection.phase(), Some(Loading));
    // Nor once the binding failed into a phase that probes would move: a
    // late result of the round before must not undo the failure.
    let stale = connection.probe().unwrap();
    connection.fail(U::Dependency);
    assert_eq!(connection.apply_probe(stale, READY), ProbeApplied::Ignored);
    assert_eq!(connection.phase(), Some(Unavailable(U::Dependency)));
    let stale = connection.probe().unwrap();
    connection.fail(U::AccessRevoked);
    connection.start_loading();
    assert_eq!(connection.apply_probe(stale, READY), ProbeApplied::Ignored);
}

#[test]
fn a_reason_that_waits_for_the_user_and_the_outside_send_no_probe_and_take_no_result() {
    for cause in [
        U::Incompatible,
        U::InstanceUnidentified,
        U::CredentialRejected,
        U::AccessRevoked,
        U::LocalStorage,
    ] {
        let mut connection = loading(true);
        let ticket = connection.probe().unwrap();
        connection.phase = Some(Unavailable(cause));
        assert_eq!(connection.probe(), None, "{cause:?}");
        assert_eq!(connection.apply_probe(ticket, READY), ProbeApplied::Ignored);
        assert_eq!(connection.phase(), Some(Unavailable(cause)));
    }
    let mut outside = Connection::new(Gate::new(1), true);
    assert_eq!(outside.probe(), None);
    let mut connection = loading(true);
    let ticket = connection.probe().unwrap();
    connection.phase = None;
    assert_eq!(connection.apply_probe(ticket, READY), ProbeApplied::Ignored);
    assert_eq!(outside.phase(), None);
    // Nor while a generation change is pending (S1.5).
    let mut connection = loading(true);
    connection.gate_mut().invalidate();
    assert!(connection.persist_pending());
    assert_eq!(connection.probe(), None);
}

#[test]
fn the_first_servable_result_waits_for_once_servable_to_be_persisted() {
    let mut connection = loading(false);
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, SEARCH_ONLY),
        ProbeApplied::PersistOnceServable
    );
    assert_eq!(connection.phase(), Some(Unavailable(U::LocalStorage)));
    assert!(connection.persist_pending());
    assert_eq!(connection.probe(), None);
    connection.confirm_once_servable();
    assert!(!connection.persist_pending());
    assert_eq!(connection.phase(), Some(Loading));
    // From now on servable results apply at once, and it is never undone.
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, READY),
        ProbeApplied::Moved(Ready)
    );
    connection.leave();
    connection.start_loading();
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, READY),
        ProbeApplied::Moved(Ready)
    );
    // A result that is not servable needs nothing persisted.
    let mut connection = loading(false);
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, Probe::Incompatible),
        ProbeApplied::Moved(Unavailable(U::Incompatible))
    );
    assert!(!connection.persist_pending());
    // A confirmation nobody asked for changes nothing: the next servable
    // result still waits for its own.
    connection.confirm_once_servable();
    assert_eq!(connection.phase(), Some(Unavailable(U::Incompatible)));
    connection.start_loading();
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, READY),
        ProbeApplied::PersistOnceServable
    );
}

#[test]
fn a_confirmation_after_leaving_records_once_servable_without_entering_loading() {
    let mut connection = loading(false);
    let ticket = connection.probe().unwrap();
    connection.apply_probe(ticket, READY);
    connection.leave();
    connection.confirm_once_servable();
    assert_eq!(connection.phase(), None);
    connection.start_loading();
    let ticket = connection.probe().unwrap();
    assert_eq!(
        connection.apply_probe(ticket, READY),
        ProbeApplied::Moved(Ready)
    );
}
