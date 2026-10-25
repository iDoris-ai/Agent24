#![allow(clippy::unwrap_used)]

use super::*;

fn sending(gate: &mut Gate, request_id: &str) -> CallId {
    let (call, _) = gate.open_call().unwrap();
    gate.send(call, request_id).unwrap();
    call
}

#[test]
fn a_call_carries_the_generation_in_effect_and_its_response_is_taken() {
    let mut gate = Gate::new(4);
    let (call, generation) = gate.open_call().unwrap();
    assert_eq!(generation, 4);
    assert_eq!(gate.send(call, "r1"), Ok(4));
    assert_eq!(gate.accept(call, "r1", Some(4)), Ok(()));
    // An error envelope that echoes `null` is paired by its exchange (R6.1).
    assert_eq!(gate.accept(call, "r1", None), Ok(()));
    assert_eq!(gate.may_append(call), Ok(()));
    // Not a request of this call, or a wrong echo, is not taken.
    assert_eq!(gate.accept(call, "r2", Some(4)), Err(Dropped::Cancelled));
    assert_eq!(gate.accept(call, "r1", Some(3)), Err(Dropped::Cancelled));
    gate.finish_request(call, "r1");
    assert_eq!(gate.accept(call, "r1", Some(4)), Err(Dropped::Cancelled));
}

#[test]
fn an_invalidation_closes_admission_and_drops_everything_in_flight_at_once() {
    let mut gate = Gate::new(4);
    let first = sending(&mut gate, "r1");
    let second = sending(&mut gate, "r2");
    let Invalidation {
        to_persist,
        mut aborted,
    } = gate.invalidate();
    let next = to_persist.unwrap();
    aborted.sort_by_key(|c| c.0);
    assert_eq!(next, 5);
    assert_eq!(aborted, vec![first, second]);
    // The old value stays in effect until the new one is persisted, but
    // nothing is admitted or sent and no response is taken (S4.5).
    assert_eq!(gate.generation(), 4);
    assert_eq!(gate.reported(), 5);
    assert!(gate.generation_pending());
    assert!(!gate.admission_open());
    assert_eq!(gate.open_call(), Err(Closed));
    assert_eq!(gate.send(first, "r3"), Err(Closed));
    assert_eq!(gate.accept(first, "r1", Some(4)), Err(Dropped::Cancelled));
    assert_eq!(gate.may_append(second), Err(Dropped::Cancelled));
    // The cancelled answer echoes the request and reports the pending value.
    let cancelled = gate.cancelled("r1", 4);
    assert_eq!(cancelled.error.details.request_id, "r1");
    assert_eq!(cancelled.error.details.generation, 4);
    assert_eq!(cancelled.error.details.current_generation, 5);
    assert!(!bool::from(cancelled.error.details.retryable));
    let wire = serde_json::to_value(&cancelled).unwrap();
    assert_eq!(wire["error"]["code"], "cancelled");
}

#[test]
fn the_new_value_takes_effect_only_once_persisted_and_admission_reopens() {
    let mut gate = Gate::new(4);
    let call = sending(&mut gate, "r1");
    let next = gate.invalidate().to_persist.unwrap();
    assert!(gate.confirm(next));
    assert_eq!(gate.generation(), 5);
    assert!(!gate.generation_pending());
    assert!(gate.admission_open());
    // The calls it dropped are forgotten, and stay dropped.
    assert!(gate.calls.is_empty());
    assert_eq!(gate.accept(call, "r1", Some(5)), Err(Dropped::Cancelled));
    assert_eq!(gate.send(call, "r2"), Err(Closed));
    // A request sent under 4 and answered now echoes 4 and reports 5.
    let cancelled = gate.cancelled("r1", 4).error.details;
    assert_eq!((cancelled.generation, cancelled.current_generation), (4, 5));
    let (fresh, generation) = gate.open_call().unwrap();
    assert_eq!(generation, 5);
    assert_eq!(gate.send(fresh, "r3"), Ok(5));
}

#[test]
fn increments_while_pending_stack_and_only_the_newest_takes_effect() {
    let mut gate = Gate::new(4);
    let first = gate.invalidate().to_persist.unwrap();
    let Invalidation {
        to_persist,
        aborted,
    } = gate.invalidate();
    let second = to_persist.unwrap();
    assert_eq!((first, second), (5, 6));
    assert!(aborted.is_empty());
    // The overtaken confirmation is ignored; admission stays closed.
    assert!(!gate.confirm(first));
    assert_eq!(gate.generation(), 4);
    assert!(!gate.admission_open());
    assert!(!gate.confirm(7));
    assert!(gate.confirm(second));
    assert_eq!(gate.generation(), 6);
    // A confirmation that comes again changes nothing.
    assert!(!gate.confirm(second));
}

#[test]
fn a_restored_binding_admits_nothing_until_the_starts_increment_is_persisted() {
    let mut gate = Gate::restored(9);
    assert_eq!(gate.generation(), 9);
    assert_eq!(gate.reported(), 10);
    assert_eq!(gate.open_call(), Err(Closed));
    // An invalidation before that goes on from the pending value.
    let next = gate.invalidate().to_persist.unwrap();
    assert_eq!(next, 11);
    assert!(gate.confirm(11));
    assert!(gate.open_call().is_ok());
}

#[test]
fn a_call_is_dropped_as_a_whole_and_its_fill_up_never_leaves() {
    let mut gate = Gate::new(2);
    let call = sending(&mut gate, "r1");
    gate.send(call, "r2").unwrap();
    gate.invalidate();
    // Neither sub-request is taken, and no fill-up goes out (S4.8).
    assert_eq!(gate.accept(call, "r1", Some(2)), Err(Dropped::Cancelled));
    assert_eq!(gate.accept(call, "r2", Some(2)), Err(Dropped::Cancelled));
    assert_eq!(gate.send(call, "r3"), Err(Closed));
}

#[test]
fn cancelling_one_run_drops_only_its_call_and_keeps_the_generation() {
    let mut gate = Gate::new(3);
    let mine = sending(&mut gate, "r1");
    let other = sending(&mut gate, "r2");
    gate.close_call(mine);
    assert_eq!(gate.accept(mine, "r1", Some(3)), Err(Dropped::Cancelled));
    assert_eq!(gate.send(mine, "r3"), Err(Closed));
    assert_eq!(gate.may_append(mine), Err(Dropped::Cancelled));
    assert_eq!(gate.accept(other, "r2", Some(3)), Ok(()));
    assert_eq!(gate.reported(), 3);
    assert!(gate.admission_open());
}

#[test]
fn a_result_is_appended_only_under_the_generation_it_was_admitted_with() {
    let mut gate = Gate::new(1);
    let call = sending(&mut gate, "r1");
    assert_eq!(gate.may_append(call), Ok(()));
    gate.invalidate();
    assert_eq!(gate.may_append(call), Err(Dropped::Cancelled));
}

#[test]
fn a_generation_is_never_used_twice_so_an_exhausted_binding_stays_closed() {
    // At the top: the invalidation still takes effect, but there is no value
    // to persist and admission never reopens.
    let mut gate = Gate::new(u64::MAX);
    let call = sending(&mut gate, "r1");
    let Invalidation {
        to_persist,
        aborted,
    } = gate.invalidate();
    assert_eq!(to_persist, None);
    assert_eq!(aborted, vec![call]);
    assert!(gate.exhausted());
    assert!(!gate.admission_open());
    assert_eq!(
        gate.accept(call, "r1", Some(u64::MAX)),
        Err(Dropped::Cancelled)
    );
    assert!(!gate.confirm(u64::MAX));
    assert_eq!(gate.open_call(), Err(Closed));
    // One below: the last value is pending, and the next invalidation leaves
    // nothing, so not even that value's late confirmation reopens.
    let mut gate = Gate::new(u64::MAX - 1);
    assert_eq!(gate.invalidate().to_persist, Some(u64::MAX));
    assert_eq!(gate.invalidate().to_persist, None);
    assert!(!gate.confirm(u64::MAX));
    assert!(!gate.admission_open());
    assert_eq!(gate.invalidate().to_persist, None);
    // Restored at the top: the start's increment has nowhere to go.
    let mut gate = Gate::restored(u64::MAX);
    assert!(gate.exhausted());
    assert!(!gate.confirm(u64::MAX));
    assert_eq!(gate.open_call(), Err(Closed));
    // Short of the top nothing is exhausted.
    assert!(!Gate::restored(u64::MAX - 1).exhausted());
}
