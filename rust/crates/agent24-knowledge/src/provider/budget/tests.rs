#![allow(clippy::unwrap_used)]

use super::*;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn run(entry: Entry) -> Slot {
    match entry {
        Entry::Run(slot) => slot,
        Entry::Wait(ticket) => panic!("{ticket:?} waits"),
    }
}

fn wait(entry: Entry) -> Ticket {
    match entry {
        Entry::Wait(ticket) => ticket,
        Entry::Run(slot) => panic!("{slot:?} runs"),
    }
}

/// R3.5, R6.6: every request of a call, retries included, comes out of one
/// budget; each carries the time the call has left.
#[test]
fn a_call_budget_counts_every_request_and_shares_one_deadline() {
    let start = Instant::now();
    let mut budget = CallBudget::new(start);
    let first = budget.take(start).unwrap();
    assert_eq!(Duration::from_millis(first.get()), CALL_DEADLINE);
    let later = budget.take(start + ms(4_500)).unwrap();
    assert_eq!(
        Duration::from_millis(later.get()),
        CALL_DEADLINE - ms(4_500)
    );
    for _ in 2..CALL_REQUESTS {
        assert!(budget.take(start + ms(5_000)).is_some());
    }
    assert_eq!(budget.take(start + ms(5_000)), None, "requests spent");
}

#[test]
fn a_call_past_its_deadline_sends_nothing_and_takes_nothing() {
    let start = Instant::now();
    let mut budget = CallBudget::new(start);
    let end = start + CALL_DEADLINE;
    assert_eq!(budget.take(end), None);
    assert_eq!(
        budget.take(end - Duration::from_micros(999)),
        None,
        "under 1 ms"
    );
    assert_eq!(budget.take(end + ms(1)), None);
    // Nothing was taken: every request is still there before the deadline.
    for _ in 0..CALL_REQUESTS {
        assert_eq!(budget.take(end - ms(1)).map(PositiveInt::get), Some(1));
    }
}

#[test]
fn a_lane_runs_up_to_its_slots_then_hands_them_on_in_order() {
    let now = Instant::now();
    let mut lanes = Lanes::default();
    let mut running: Vec<Slot> = (0..MAIN_SLOTS)
        .map(|_| run(lanes.enter(Lane::Main, now)))
        .collect();
    let first = wait(lanes.enter(Lane::Main, now));
    let second = wait(lanes.enter(Lane::Main, now));
    let next = lanes.leave(running.pop().unwrap()).unwrap();
    assert_eq!(next.ticket(), first);
    let next = lanes.leave(next).unwrap();
    assert_eq!(next.ticket(), second);
    // Nobody waits: the slot is free again, and a new call runs at once.
    assert_eq!(lanes.leave(next), None);
    run(lanes.enter(Lane::Main, now));
    assert!(matches!(lanes.enter(Lane::Main, now), Entry::Wait(_)));
}

/// A slot freed while others wait goes to the oldest, never to a newcomer.
#[test]
fn a_newcomer_does_not_overtake_a_waiter() {
    let now = Instant::now();
    let mut lanes = Lanes::default();
    let slots: Vec<Slot> = (0..BRIDGE_SLOTS)
        .map(|_| run(lanes.enter(Lane::Bridge, now)))
        .collect();
    let waiter = wait(lanes.enter(Lane::Bridge, now));
    let mut slots = slots.into_iter();
    let handed = lanes.leave(slots.next().unwrap()).unwrap();
    assert_eq!(handed.ticket(), waiter);
    wait(lanes.enter(Lane::Bridge, now));
}

/// U7: the bridge has its own two slots and never takes the main lane's.
#[test]
fn the_lanes_do_not_share_slots() {
    let now = Instant::now();
    let mut lanes = Lanes::default();
    let _main: Vec<Slot> = (0..MAIN_SLOTS)
        .map(|_| run(lanes.enter(Lane::Main, now)))
        .collect();
    let bridge: Vec<Slot> = (0..BRIDGE_SLOTS)
        .map(|_| run(lanes.enter(Lane::Bridge, now)))
        .collect();
    let waiting_bridge = wait(lanes.enter(Lane::Bridge, now));
    let waiting_main = wait(lanes.enter(Lane::Main, now));
    let handed = lanes.leave(bridge.into_iter().next().unwrap()).unwrap();
    assert_eq!(handed.ticket(), waiting_bridge);
    assert!(lanes.withdraw(Lane::Main, waiting_main));
}

#[test]
fn a_withdrawn_waiter_is_skipped() {
    let now = Instant::now();
    let mut lanes = Lanes::default();
    let slots: Vec<Slot> = (0..BRIDGE_SLOTS)
        .map(|_| run(lanes.enter(Lane::Bridge, now)))
        .collect();
    let gone = wait(lanes.enter(Lane::Bridge, now));
    let stays = wait(lanes.enter(Lane::Bridge, now));
    assert!(lanes.withdraw(Lane::Bridge, gone));
    assert!(!lanes.withdraw(Lane::Bridge, gone), "only once");
    assert!(!lanes.withdraw(Lane::Main, stays), "not in that lane");
    let handed = lanes.leave(slots.into_iter().next().unwrap()).unwrap();
    assert_eq!(handed.ticket(), stays);
}

/// U7: a bridge read that has waited over 10 seconds is refused; one at
/// exactly 10 seconds still waits. Main calls never expire here.
#[test]
fn a_bridge_read_that_waits_over_ten_seconds_is_refused() {
    let start = Instant::now();
    let mut lanes = Lanes::default();
    let slots: Vec<Slot> = (0..BRIDGE_SLOTS)
        .map(|_| run(lanes.enter(Lane::Bridge, start)))
        .collect();
    let _main: Vec<Slot> = (0..MAIN_SLOTS)
        .map(|_| run(lanes.enter(Lane::Main, start)))
        .collect();
    let main_waiter = wait(lanes.enter(Lane::Main, start));
    let old = wait(lanes.enter(Lane::Bridge, start));
    let young = wait(lanes.enter(Lane::Bridge, start + ms(3_000)));
    assert!(lanes.expire(start + BRIDGE_WAIT).is_empty());
    assert_eq!(lanes.expire(start + BRIDGE_WAIT + ms(1)), vec![old]);
    assert!(lanes.expire(start + BRIDGE_WAIT + ms(2_000)).is_empty());
    assert_eq!(lanes.expire(start + ms(13_001)), vec![young]);
    // Expired waiters are gone: a freed slot is simply free.
    assert_eq!(lanes.leave(slots.into_iter().next().unwrap()), None);
    assert!(lanes.withdraw(Lane::Main, main_waiter));
}
