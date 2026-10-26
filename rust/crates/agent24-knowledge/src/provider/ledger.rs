//! Persistence bookkeeping (CONTRACT-STATE-v1 S1.10, I1–I3): when a
//! tightening is committed, when a loosening may be written and when its
//! confirmation counts, and when the host-level seal is deleted and rebuilt.
//! Pure state: the caller writes what the ledger asks for and reports the
//! outcome back, one event at a time.
//!
//! The seal holds "every tightening that took effect since it was rebuilt is
//! persisted". A tightening takes effect at once (S4.5); it is committed (I2)
//! once either of two independent writes lands: deleting the seal, or its own
//! record. So a seal present at restart means every committed tightening has
//! its record, and a missing one means a global tightening (S1.14).

use std::collections::HashMap;

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

    /// An unconfirmed tightening over `self` refuses a loosening over
    /// `other` (I1): host level either way, or a profile in common.
    fn blocks(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Host, _) | (_, Self::Host) => true,
            (Self::Profiles(a), Self::Profiles(b)) => a.iter().any(|p| b.contains(p)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ticket(u64);

/// A tightening to write: its record (or a write outside the log such as
/// clearing a long-lived login, FI-7c), and the seal's deletion when
/// `unseal`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tightening {
    pub ticket: Ticket,
    pub unseal: bool,
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
    /// A deletion in flight.
    Deleting,
    /// Deleted for sure: a restart tightens globally.
    Absent,
    /// A rebuild in flight; a tightening that came meanwhile deletes it
    /// again once the rebuild is through (the host orders both under one
    /// lock).
    Rebuilding { then_delete: bool },
}

#[derive(Debug)]
struct Tight<P> {
    scope: Scope<P>,
    committed: bool,
    failed: bool,
}

#[derive(Debug)]
struct Loose {
    abandoned: bool,
}

#[derive(Debug)]
pub struct Ledger<P> {
    seal: Seal,
    /// Tightenings whose record is not yet confirmed.
    tight: HashMap<Ticket, Tight<P>>,
    /// Loosenings written and not yet confirmed, abandoned ones included.
    loose: HashMap<Ticket, Loose>,
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
            next: 0,
        }
    }

    fn ticket(&mut self) -> Ticket {
        self.next += 1;
        Ticket(self.next)
    }

    /// A tightening has taken effect in memory. With the seal already gone it
    /// is committed at once; otherwise once the seal's deletion or its record
    /// lands.
    pub fn tighten(&mut self, scope: Scope<P>) -> Tightening {
        let ticket = self.ticket();
        let unseal = match self.seal {
            Seal::Present => {
                self.seal = Seal::Deleting;
                true
            }
            Seal::Rebuilding { .. } => {
                self.seal = Seal::Rebuilding { then_delete: true };
                true
            }
            Seal::Deleting | Seal::Absent => false,
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

    /// Committed (I2): the crash guarantee holds from here on.
    pub fn committed(&self, ticket: Ticket) -> bool {
        self.tight.get(&ticket).is_none_or(|t| t.committed)
    }

    /// The record of `ticket` is confirmed persisted.
    pub fn record_durable(&mut self, ticket: Ticket) {
        self.tight.remove(&ticket);
    }

    /// The record's write failed or its outcome is unknown: still
    /// unconfirmed, retried by the caller; "some settings not saved" shows.
    pub fn record_failed(&mut self, ticket: Ticket) {
        if let Some(t) = self.tight.get_mut(&ticket) {
            t.failed = true;
        }
    }

    /// A tightening's write failed and is not yet confirmed (I2's prompt).
    pub fn unsaved(&self) -> bool {
        self.tight.values().any(|t| t.failed)
    }

    /// The seal's deletion is confirmed: every tightening in effect is
    /// committed.
    pub fn seal_deleted(&mut self) {
        if self.seal != Seal::Deleting {
            return;
        }
        self.seal = Seal::Absent;
        for t in self.tight.values_mut() {
            t.committed = true;
        }
    }

    /// The deletion failed or its outcome is unknown: the seal may still be
    /// there. Returns whether to try again: some tightening is not committed.
    pub fn seal_delete_failed(&mut self) -> bool {
        if self.seal != Seal::Deleting {
            return false;
        }
        let retry = self.tight.values().any(|t| !t.committed);
        self.seal = if retry { Seal::Deleting } else { Seal::Present };
        retry
    }

    /// Whether to rebuild the seal now: it is gone and every tightening is
    /// confirmed, writes outside the log included.
    pub fn reseal(&mut self) -> bool {
        if self.seal != Seal::Absent || !self.tight.is_empty() {
            return false;
        }
        self.seal = Seal::Rebuilding { then_delete: false };
        true
    }

    /// The rebuild landed, or its outcome is unknown: either way the seal may
    /// be there now. Returns whether to delete it again right away.
    pub fn resealed(&mut self) -> bool {
        let Seal::Rebuilding { then_delete } = self.seal else {
            return false;
        };
        self.seal = if then_delete {
            Seal::Deleting
        } else {
            Seal::Present
        };
        then_delete
    }

    /// I1: whether a loosening over `scope` may be written. Refused while a
    /// tightening it depends on is unconfirmed, or while the loosening
    /// `after` it depends on is not yet in effect.
    pub fn loosen(
        &mut self,
        scope: &Scope<P>,
        after: Option<Ticket>,
        off: bool,
    ) -> Result<Ticket, Refused> {
        let blocked = self.tight.values().any(|t| t.scope.blocks(scope));
        let waiting = after.is_some_and(|a| self.loose.contains_key(&a));
        if blocked || waiting {
            return Err(if off {
                Refused::KnowledgeOff
            } else {
                Refused::LocalStorage
            });
        }
        let ticket = self.ticket();
        self.loose.insert(ticket, Loose { abandoned: false });
        Ok(ticket)
    }

    /// The loosening's record is confirmed persisted.
    pub fn loosening_durable(&mut self, ticket: Ticket) -> Confirmed {
        match self.loose.remove(&ticket) {
            Some(Loose { abandoned: false }) => Confirmed::Applied,
            _ => Confirmed::Ignored,
        }
    }

    /// The user acted against a loosening still pending, or its write failed
    /// and the caller gives it up: it is abandoned, and the tightening that
    /// offsets it is written (I1). `None` once it is confirmed or abandoned
    /// already; the opposite action is then an ordinary tightening.
    pub fn abandon(&mut self, ticket: Ticket, offset: Scope<P>) -> Option<Tightening> {
        let loose = self.loose.get_mut(&ticket).filter(|l| !l.abandoned)?;
        loose.abandoned = true;
        Some(self.tighten(offset))
    }
}

#[cfg(test)]
mod tests;
