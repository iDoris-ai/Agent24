//! Provider-level budgets (KB-C1-04 6b). A call's total budget covers every
//! request it sends, its group requests and retries included (READONLY R3.3,
//! R3.5, R6.6); a page is a new call with a budget of its own (STATE S4.8).
//! Per binding (S4.9), calls wait for a slot in one of two lanes: the main
//! conversation's, and the bridge's low-priority one, which never takes a
//! main slot and turns a wait of over 10 seconds into `overloaded`
//! (UI-BRIDGE U7). Pure state: the caller passes `now` and holds the lock.
//! The numbers are the Provider's own; the contracts leave them open.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::PositiveInt;

/// How long one call may take, all its requests together.
pub const CALL_DEADLINE: Duration = Duration::from_secs(15);
/// Requests one call may send: the first, then group requests and retries.
pub const CALL_REQUESTS: u32 = 8;
/// Main-conversation calls a binding runs at once.
pub const MAIN_SLOTS: usize = 4;
/// Bridge reads a binding runs at once (U7).
pub const BRIDGE_SLOTS: usize = 2;
/// How long a bridge read may wait for a slot before it is refused (U7).
pub const BRIDGE_WAIT: Duration = Duration::from_secs(10);
/// The `retry_after_ms` a refused bridge read carries.
pub const BRIDGE_RETRY_AFTER_MS: u64 = 2_000;

/// What is left of one call's budget.
#[derive(Debug)]
pub struct CallBudget {
    deadline: Instant,
    requests: u32,
}

impl CallBudget {
    pub fn new(now: Instant) -> Self {
        Self {
            deadline: now + CALL_DEADLINE,
            requests: CALL_REQUESTS,
        }
    }

    /// Takes one request's share and returns its `deadline_ms`: the time the
    /// call has left, in whole milliseconds rounded down. `None` once the
    /// requests are spent or less than a millisecond is left; nothing is
    /// taken then.
    pub fn take(&mut self, now: Instant) -> Option<PositiveInt> {
        let left = self.deadline.checked_duration_since(now)?;
        let ms = u64::try_from(left.as_millis()).unwrap_or(u64::MAX);
        let ms = PositiveInt::new(ms)?;
        self.requests = self.requests.checked_sub(1)?;
        Some(ms)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lane {
    Main,
    Bridge,
}

/// A call in a lane, waiting or running.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(u64);

/// A running call's slot; given back with [`Lanes::leave`]. Not `Clone`, so
/// it is given back once.
#[derive(Debug, PartialEq, Eq)]
pub struct Slot {
    lane: Lane,
    ticket: Ticket,
}

impl Slot {
    pub fn ticket(&self) -> Ticket {
        self.ticket
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Entry {
    Run(Slot),
    Wait(Ticket),
}

#[derive(Debug)]
struct Queue {
    limit: usize,
    running: usize,
    waiting: VecDeque<(Ticket, Instant)>,
}

impl Queue {
    fn new(limit: usize) -> Self {
        Self {
            limit,
            running: 0,
            waiting: VecDeque::new(),
        }
    }
}

/// One binding's two lanes. A slot goes to the oldest waiter first (FIFO).
#[derive(Debug)]
pub struct Lanes {
    main: Queue,
    bridge: Queue,
    next: u64,
}

impl Default for Lanes {
    fn default() -> Self {
        Self {
            main: Queue::new(MAIN_SLOTS),
            bridge: Queue::new(BRIDGE_SLOTS),
            next: 0,
        }
    }
}

impl Lanes {
    fn queue(&mut self, lane: Lane) -> &mut Queue {
        match lane {
            Lane::Main => &mut self.main,
            Lane::Bridge => &mut self.bridge,
        }
    }

    /// A call asks for a slot: it runs at once when one is free. Nobody waits
    /// then, since a freed slot goes straight to the oldest waiter, so a
    /// newcomer never overtakes one.
    pub fn enter(&mut self, lane: Lane, now: Instant) -> Entry {
        self.next += 1;
        let ticket = Ticket(self.next);
        let queue = self.queue(lane);
        if queue.running < queue.limit {
            queue.running += 1;
            Entry::Run(Slot { lane, ticket })
        } else {
            queue.waiting.push_back((ticket, now));
            Entry::Wait(ticket)
        }
    }

    /// A call is done and gives its slot back; the slot goes to the oldest
    /// waiter of the same lane, if any, which the caller then runs.
    pub fn leave(&mut self, slot: Slot) -> Option<Slot> {
        let lane = slot.lane;
        let queue = self.queue(lane);
        match queue.waiting.pop_front() {
            Some((ticket, _)) => Some(Slot { lane, ticket }),
            None => {
                queue.running -= 1;
                None
            }
        }
    }

    /// A waiting call gives up (cancelled, or its own deadline passed).
    /// Whether it was still waiting.
    pub fn withdraw(&mut self, lane: Lane, ticket: Ticket) -> bool {
        let waiting = &mut self.queue(lane).waiting;
        let before = waiting.len();
        waiting.retain(|(t, _)| *t != ticket);
        waiting.len() != before
    }

    /// The bridge reads that have waited over [`BRIDGE_WAIT`] by `now`, taken
    /// out of the queue: each is answered `unavailable/overloaded` with
    /// [`BRIDGE_RETRY_AFTER_MS`]. Main calls wait on their own deadline.
    pub fn expire(&mut self, now: Instant) -> Vec<Ticket> {
        let waiting = &mut self.bridge.waiting;
        let mut expired = Vec::new();
        while let Some(&(ticket, since)) = waiting.front() {
            if now.saturating_duration_since(since) <= BRIDGE_WAIT {
                break;
            }
            waiting.pop_front();
            expired.push(ticket);
        }
        expired
    }
}

#[cfg(test)]
mod tests;
