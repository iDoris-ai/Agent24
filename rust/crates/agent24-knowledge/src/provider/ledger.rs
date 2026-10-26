//! Persistence bookkeeping (CONTRACT-STATE-v1 S1.10, I1–I3), pure state: the
//! caller writes what the ledger asks for and reports back. The host-level
//! seal holds "every tightening since its rebuild is persisted". A tightening
//! takes effect at once (S4.5) and is committed (I2) once deleting the seal or
//! writing its own record lands; so a seal present at restart means every
//! committed tightening has its record, and none means a global tightening.
//!
//! Wiring premise: the seal's writes run one at a time in the order issued,
//! and an outcome is reported unknown only once that write can no longer
//! land. A timed-out write still pending is not reported: a rebuild landing
//! after a later deletion would bring the seal back over a committed
//! tightening.

use std::collections::{HashMap, HashSet};

/// What a change covers. A binding's changes count for its profile: a
/// profile has at most one activated binding (S4.9), and I1 blocks by
/// profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope<P> {
    Host,
    Profiles(Vec<P>),
}

impl<P: PartialEq> Scope<P> {
    pub fn profile(profile: P) -> Self {
        Self::Profiles(vec![profile])
    }

    /// No profile at all: it would block nothing and be blocked by nothing.
    fn is_empty(&self) -> bool {
        matches!(self, Self::Profiles(p) if p.is_empty())
    }

    /// An unconfirmed tightening over `self` refuses a loosening over
    /// `other` (I1): host level either way, or a profile in common.
    fn blocks(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Host, _) | (_, Self::Host) => true,
            (Self::Profiles(a), Self::Profiles(b)) => a.iter().any(|p| b.contains(p)),
        }
    }
}

/// A tightening, from [`Ledger::tighten`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(u64);

/// A loosening, from [`Ledger::loosen`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Loosening(u64);

/// One deletion or rebuild of the seal: only the one in flight counts, so a
/// late report of an earlier one never moves the seal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SealOp(u64);

/// A tightening to write: its record (or a write outside the log such as
/// clearing a long-lived login, FI-7c), and the seal's deletion if `unseal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tightening {
    pub ticket: Ticket,
    pub unseal: Option<SealOp>,
}

/// A loosening refused (I1): dropped, not queued; the local result follows
/// §6's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    KnowledgeOff,
    LocalStorage,
}

/// What a loosening's confirmation does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confirmed {
    /// It takes effect now.
    Applied,
    /// Abandoned or unknown: it never takes effect.
    Ignored,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Seal {
    /// Rebuilt and not deleted since.
    Present,
    Deleting(SealOp),
    /// Deleted for sure: a restart tightens globally.
    Absent,
    Rebuilding(SealOp),
}

#[derive(Debug)]
struct Tight<P> {
    scope: Scope<P>,
    committed: bool,
    failed: bool,
}

#[derive(Debug)]
pub struct Ledger<P> {
    seal: Seal,
    /// Tightenings whose record is not yet confirmed.
    tight: HashMap<Ticket, Tight<P>>,
    /// Loosenings written and not yet confirmed. An abandoned one leaves at
    /// once: a late confirmation of it then reads as unknown.
    loose: HashMap<Loosening, Scope<P>>,
    /// Loosenings in effect: only these satisfy a dependency. One entry per
    /// loosening applied in this process.
    applied: HashSet<Loosening>,
    /// A tightening came during the rebuild: delete the seal once it is in.
    redelete: bool,
    next: u64,
}

impl<P: Clone + PartialEq> Ledger<P> {
    /// At start, with the seal as recovery left it.
    pub fn new(seal_present: bool) -> Self {
        Self {
            seal: if seal_present {
                Seal::Present
            } else {
                Seal::Absent
            },
            tight: HashMap::new(),
            loose: HashMap::new(),
            applied: HashSet::new(),
            redelete: false,
            next: 0,
        }
    }

    fn next(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn deleting(&mut self) -> SealOp {
        let op = SealOp(self.next());
        self.seal = Seal::Deleting(op);
        op
    }

    /// A tightening has taken effect in memory. With the seal already gone it
    /// is committed at once; otherwise once the seal's deletion or its record
    /// lands. During a rebuild the deletion comes from [`Ledger::resealed`].
    pub fn tighten(&mut self, scope: Scope<P>) -> Tightening {
        debug_assert!(!scope.is_empty());
        let ticket = Ticket(self.next());
        let unseal = match self.seal {
            Seal::Present => Some(self.deleting()),
            Seal::Rebuilding(_) => {
                self.redelete = true;
                None
            }
            Seal::Deleting(_) | Seal::Absent => None,
        };
        let committed = self.seal == Seal::Absent;
        let entry = Tight {
            scope,
            committed,
            failed: false,
        };
        self.tight.insert(ticket, entry);
        Tightening { ticket, unseal }
    }

    /// Committed (I2): the crash guarantee holds from here on. A ticket this
    /// ledger does not hold (confirmed, or never issued by it) reads
    /// committed, so this is no proof that anything is on disk.
    pub fn committed(&self, ticket: Ticket) -> bool {
        self.tight.get(&ticket).is_none_or(|t| t.committed)
    }

    /// The record of `ticket` is confirmed persisted.
    pub fn record_durable(&mut self, ticket: Ticket) {
        self.tight.remove(&ticket);
    }

    /// The record's write failed or its outcome is unknown: still
    /// unconfirmed, retried by the caller without waiting for the user (I2);
    /// "some settings not saved" shows meanwhile.
    pub fn record_failed(&mut self, ticket: Ticket) {
        if let Some(t) = self.tight.get_mut(&ticket) {
            t.failed = true;
        }
    }

    /// A tightening's write failed and is not yet confirmed (I2's prompt).
    pub fn unsaved(&self) -> bool {
        self.tight.values().any(|t| t.failed)
    }

    /// The deletion `op` is confirmed: every tightening in effect is
    /// committed.
    pub fn seal_deleted(&mut self, op: SealOp) {
        if self.seal != Seal::Deleting(op) {
            return;
        }
        self.seal = Seal::Absent;
        for t in self.tight.values_mut() {
            t.committed = true;
        }
    }

    /// The deletion `op` failed or its outcome is unknown: the seal may still
    /// be there. While some tightening is not committed, returns the deletion
    /// to try again. Unknown only once the write can no longer land (module
    /// premise).
    pub fn seal_delete_failed(&mut self, op: SealOp) -> Option<SealOp> {
        if self.seal != Seal::Deleting(op) {
            return None;
        }
        if self.tight.values().any(|t| !t.committed) {
            return Some(self.deleting());
        }
        self.seal = Seal::Present;
        None
    }

    /// The rebuild to start now, if any: the seal is gone and every
    /// tightening is confirmed, writes outside the log included.
    pub fn reseal(&mut self) -> Option<SealOp> {
        if self.seal != Seal::Absent || !self.tight.is_empty() {
            return None;
        }
        let op = SealOp(self.next());
        self.seal = Seal::Rebuilding(op);
        self.redelete = false;
        Some(op)
    }

    /// The rebuild `op` landed, or its outcome is unknown: either way the
    /// seal may be there now. Returns the deletion to start if a tightening
    /// came meanwhile. Unknown only once the write can no longer land (module
    /// premise).
    pub fn resealed(&mut self, op: SealOp) -> Option<SealOp> {
        if self.seal != Seal::Rebuilding(op) {
            return None;
        }
        if self.redelete {
            return Some(self.deleting());
        }
        self.seal = Seal::Present;
        None
    }

    /// I1: whether a loosening over `scope` may be written. Refused while a
    /// tightening it depends on is unconfirmed, or while the loosening
    /// `after` it depends on is not in effect.
    pub fn loosen(
        &mut self,
        scope: Scope<P>,
        after: Option<Loosening>,
        off: bool,
    ) -> Result<Loosening, Refused> {
        debug_assert!(!scope.is_empty());
        let blocked = self.tight.values().any(|t| t.scope.blocks(&scope));
        let waiting = after.is_some_and(|a| !self.applied.contains(&a));
        if blocked || waiting {
            return Err(if off {
                Refused::KnowledgeOff
            } else {
                Refused::LocalStorage
            });
        }
        let ticket = Loosening(self.next());
        self.loose.insert(ticket, scope);
        Ok(ticket)
    }

    /// The loosening's record is confirmed persisted.
    pub fn loosening_durable(&mut self, ticket: Loosening) -> Confirmed {
        if self.loose.remove(&ticket).is_none() {
            return Confirmed::Ignored;
        }
        self.applied.insert(ticket);
        Confirmed::Applied
    }

    /// The user acted against a loosening still pending, or its write failed
    /// and the caller gives it up: it is abandoned, and a tightening over its
    /// own scope offsets it (I1). `None` once it is confirmed or abandoned
    /// already; the opposite action is then an ordinary tightening.
    pub fn abandon(&mut self, ticket: Loosening) -> Option<Tightening> {
        let scope = self.loose.remove(&ticket)?;
        Some(self.tighten(scope))
    }
}

#[cfg(test)]
mod tests;
