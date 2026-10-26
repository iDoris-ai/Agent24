//! The order the Provider's writes run in (KB-C1-04 6c), pure: the log's
//! records one at a time, by number, each reported back. A failed one is
//! taken again as it was, under its number (A.1: a retry under the same number
//! is idempotent), so the log never has a gap (A.3). The seal's writes follow
//! the ledger, which keeps one at a time in flight.

use std::collections::VecDeque;

use super::ledger::{Ledger, SealOp};
use super::record::{Change, Ids};
use super::replay::{Entry, Record};

/// How a write ended. `Failed` also when its outcome is unknown, once it can
/// no longer land (the ledger's premise).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    Durable,
    Failed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SealWrite {
    Delete(SealOp),
    Rebuild(SealOp),
}

/// What a report of the write in flight gives back: what it was written for.
#[derive(Debug, PartialEq, Eq)]
pub enum Report<'a, T> {
    /// Not the write in flight: nothing moves.
    Ignored,
    /// Still the next to take.
    Failed(&'a T),
    Durable(T),
}

#[derive(Debug)]
struct Queued<I: Ids, T> {
    seq: u64,
    changes: Vec<Change<I>>,
    then: T,
}

#[derive(Debug)]
pub struct Journal<I: Ids, T> {
    queue: VecDeque<Queued<I, T>>,
    in_flight: bool,
    next_seq: u64,
}

impl<I: Ids, T> Journal<I, T> {
    /// `next_seq` is the number the log's next record takes.
    pub fn new(next_seq: u64) -> Self {
        Self {
            queue: VecDeque::new(),
            in_flight: false,
            next_seq,
        }
    }

    /// One atomic write after every one pushed before it.
    pub fn push(&mut self, changes: Vec<Change<I>>, then: T) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.queue.push_back(Queued { seq, changes, then });
    }

    /// The next write to run, unless one is running.
    pub fn take(&mut self) -> Option<Record<I>> {
        if self.in_flight {
            return None;
        }
        let head = self.queue.front()?;
        self.in_flight = true;
        Some(Record {
            seq: head.seq,
            entry: Entry::Changes(head.changes.clone()),
        })
    }

    pub fn written(&mut self, seq: u64, outcome: Written) -> Report<'_, T> {
        if !self.in_flight || self.queue.front().is_none_or(|q| q.seq != seq) {
            return Report::Ignored;
        }
        self.in_flight = false;
        match outcome {
            Written::Failed => self
                .queue
                .front()
                .map_or(Report::Ignored, |q| Report::Failed(&q.then)),
            Written::Durable => self
                .queue
                .pop_front()
                .map_or(Report::Ignored, |q| Report::Durable(q.then)),
        }
    }
}

/// The seal write `write` ended: the next one to start, if any.
pub fn sealed<P: Clone + PartialEq>(
    ledger: &mut Ledger<P>,
    write: SealWrite,
    outcome: Written,
) -> Option<SealWrite> {
    match (write, outcome) {
        (SealWrite::Delete(op), Written::Durable) => {
            ledger.seal_deleted(op);
            ledger.reseal().map(SealWrite::Rebuild)
        }
        (SealWrite::Delete(op), Written::Failed) => {
            ledger.seal_delete_failed(op).map(SealWrite::Delete)
        }
        // Landed or not, the seal may be there now.
        (SealWrite::Rebuild(op), _) => ledger.resealed(op).map(SealWrite::Delete),
    }
}

#[cfg(test)]
mod tests;
