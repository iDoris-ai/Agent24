//! The evidence one binding delivered, and what the history sent to the model
//! shows of it (CONTRACT-STATE-v1 S5.6): the evidence epoch and placeholder
//! rules, pending re-verification (S3.1) and deletion tombstones (S5.10).
//! Pure: the host assembles the history, sends the recheck and the deletion,
//! and persists every change here (S1.10). The last visible set and the
//! narrowing it tells come with the next slice; the records, with the one
//! after.

use std::collections::{BTreeMap, BTreeSet};

/// The KBs a placeholder rule covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Kbs<K> {
    All,
    /// A KB the host itself deleted (S5.10).
    One(K),
}

/// Evidence admitted before `epoch`, of a KB in `kbs`, shows as a
/// placeholder. A tightening (S1.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Rule<K> {
    pub epoch: u64,
    pub kbs: Kbs<K>,
}

/// A piece of evidence the binding delivered: its KB, and the epoch when its
/// request was admitted.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivered<K> {
    pub kb: K,
    pub epoch: u64,
}

/// What the history sent to the model shows of a piece of evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shown {
    Body,
    /// No body for now; it may come back (S3.1, S5.10).
    Withheld,
    /// No body, for good.
    Placeholder,
}

/// A recheck to send with the current credential: a ready probe scoped to
/// `kbs` (S3.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recheck<K> {
    pub kbs: BTreeSet<K>,
    pub version: u64,
}

/// What a recheck that passed asks of the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Passed<K> {
    /// The version is still current: persist the release, then
    /// [`Evidence::release`] (a loosening, I1).
    Release,
    /// Registered again since: recheck the merged KBs. The recheck came
    /// back, so timeouts are no longer in a row ([`Evidence::retry`]).
    Again(Recheck<K>),
    /// Nothing is pending.
    Ignored,
}

/// Pending re-verification: the KBs questioned so far, and the rechecks in a
/// row that timed out.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pending<K> {
    pub kbs: BTreeSet<K>,
    pub timeouts: u8,
}

/// A deletion tombstone (S5.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tombstone {
    /// Recorded before the delete was sent.
    Pending,
    /// Read back at restart with no outcome: its KB is withheld until the
    /// host finds out how the delete went (S1.14).
    Unresolved,
}

/// The state as persisted, to restore at restart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved<K> {
    pub epoch: u64,
    pub rules: Vec<Rule<K>>,
    pub version: u64,
    pub pending: Option<Pending<K>>,
    pub tombstones: BTreeSet<K>,
}

/// S3.1: after the third recheck in a row that timed out, no recheck is sent
/// by itself until a manual retry or logging in again.
const TIMEOUT_LIMIT: u8 = 3;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evidence<K> {
    epoch: u64,
    rules: Vec<Rule<K>>,
    /// Advances with every registration and never comes back, released or
    /// not, so a recheck's answer counts only for the KBs it covered.
    version: u64,
    pending: Option<Pending<K>>,
    tombstones: BTreeMap<K, Tombstone>,
}

impl<K> Default for Evidence<K> {
    fn default() -> Self {
        Self {
            epoch: 0,
            rules: Vec::new(),
            version: 0,
            pending: None,
            tombstones: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Clone> Evidence<K> {
    /// What was persisted: every tombstone is unresolved.
    pub fn restore(saved: Saved<K>) -> Self {
        let tombstones = saved.tombstones.into_iter();
        Self {
            epoch: saved.epoch,
            rules: saved.rules,
            version: saved.version,
            pending: saved.pending,
            tombstones: tombstones.map(|kb| (kb, Tombstone::Unresolved)).collect(),
        }
    }

    /// The epoch to record on a request admitted now.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// A trigger of S5.6 (a revocation-class event, a renewal that fails too,
    /// unselecting, logging out, disabling or removing the profile, an
    /// identity change), or a deletion that succeeded: the epoch advances and
    /// the rule returned covers what came before. In any state, even
    /// `disabled` or `registered`.
    pub fn place(&mut self, kbs: Kbs<K>) -> Rule<K> {
        self.epoch += 1;
        let rule = Rule {
            epoch: self.epoch,
            kbs,
        };
        self.rules.push(rule.clone());
        rule
    }

    /// For the history sent to the model. Evidence of a retired binding is a
    /// placeholder, held back or not (S3.1); evidence whose binding is not in
    /// the state after a restart is the caller's to replace (I5).
    pub fn shown(&self, retired: bool, delivered: &Delivered<K>) -> Shown {
        let covers = |r: &Rule<K>| match &r.kbs {
            Kbs::All => true,
            Kbs::One(kb) => *kb == delivered.kb,
        };
        let placed = self
            .rules
            .iter()
            .any(|r| delivered.epoch < r.epoch && covers(r));
        if retired || placed {
            return Shown::Placeholder;
        }
        // Evidence delivered after the registration is held back too.
        let questioned = self
            .pending
            .as_ref()
            .is_some_and(|p| p.kbs.contains(&delivered.kb));
        let unresolved = self.tombstones.get(&delivered.kb) == Some(&Tombstone::Unresolved);
        if questioned || unresolved {
            Shown::Withheld
        } else {
            Shown::Body
        }
    }

    /// The binding admits no new call while this holds (§6).
    pub fn reverify_pending(&self) -> bool {
        self.pending.is_some()
    }

    /// A revocation signal came back on a discarded credential for a request
    /// over `kbs` (credential `Outcome::Recheck`). A tightening: the KBs
    /// merge into those pending and the version advances.
    pub fn register(&mut self, kbs: impl IntoIterator<Item = K>) {
        self.version += 1;
        let pending = self.pending.get_or_insert_with(|| Pending {
            kbs: BTreeSet::new(),
            timeouts: 0,
        });
        pending.kbs.extend(kbs);
    }

    /// The recheck due, if one is pending and rechecks have not stopped. The
    /// caller sends it each time the binding has a current credential in the
    /// connection phase, backing off after a timeout.
    pub fn recheck(&self) -> Option<Recheck<K>> {
        self.pending
            .as_ref()
            .filter(|p| p.timeouts < TIMEOUT_LIMIT)
            .map(|p| self.covering(p))
    }

    /// The recheck of `version` passed.
    pub fn passed(&self, version: u64) -> Passed<K> {
        match &self.pending {
            None => Passed::Ignored,
            Some(_) if version == self.version => Passed::Release,
            Some(p) => Passed::Again(self.covering(p)),
        }
    }

    /// The release of `version` is persisted: admission and the evidence come
    /// back. False if registered again meanwhile; the caller offsets it.
    pub fn release(&mut self, version: u64) -> bool {
        let current = self.pending.is_some() && version == self.version;
        if current {
            self.pending = None;
        }
        current
    }

    /// A recheck timed out. A tightening; the third in a row stops them.
    pub fn timed_out(&mut self) {
        if let Some(p) = &mut self.pending {
            p.timeouts = p.timeouts.saturating_add(1);
        }
    }

    /// Timeouts are no longer in a row: a manual retry, logging in again, or
    /// a recheck that came back without timing out yet left it pending
    /// ([`Passed::Again`], an invalid-class answer). Rechecks go on. A
    /// loosening, once persisted.
    pub fn retry(&mut self) {
        if let Some(p) = &mut self.pending {
            p.timeouts = 0;
        }
    }

    /// The recheck came back revoked: the caller handles a revocation, whose
    /// rule covers all held back; nothing pends any longer. Persisted in one
    /// write with that rule.
    pub fn revoked(&mut self) -> Rule<K> {
        self.pending = None;
        self.place(Kbs::All)
    }

    /// Before the host deletes `kb` (S5.10). A tightening. One read back
    /// unresolved stays so while the host deletes again: the earlier delete
    /// may have gone through.
    pub fn tombstone(&mut self, kb: K) {
        self.tombstones.entry(kb).or_insert(Tombstone::Pending);
    }

    /// The deletion succeeded: its evidence becomes placeholders, without
    /// invalidating and without counting towards the window.
    pub fn deleted(&mut self, kb: K) -> Rule<K> {
        self.tombstones.remove(&kb);
        self.place(Kbs::One(kb))
    }

    /// The deletion failed, its withdrawal persisted (a loosening): the KB is
    /// as before.
    pub fn kept(&mut self, kb: &K) {
        self.tombstones.remove(kb);
    }

    pub fn tombstone_of(&self, kb: &K) -> Option<Tombstone> {
        self.tombstones.get(kb).copied()
    }

    fn covering(&self, pending: &Pending<K>) -> Recheck<K> {
        Recheck {
            kbs: pending.kbs.clone(),
            version: self.version,
        }
    }
}

#[cfg(test)]
mod tests;
