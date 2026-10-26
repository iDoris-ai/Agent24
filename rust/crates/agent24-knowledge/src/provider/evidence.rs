//! The evidence one binding delivered, and what the history sent to the model
//! shows of it (CONTRACT-STATE-v1 S5.6): the evidence epoch and placeholder
//! rules, pending re-verification (S3.1), deletion tombstones, and the last
//! visible set with the narrowing it tells (S5.10). Pure: the host assembles
//! the history, sends the recheck and the deletion, and persists every
//! change here (S1.10). Wiring into the binding and the records come later.

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

/// What an accepted response tells of the KBs visible (S5.10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope<K> {
    /// The `effective_scope` of a search or read that succeeded, or the KBs
    /// a ready probe listed.
    Listed(BTreeSet<K>),
    /// `forbidden/empty_scope`: no KB asked for is visible. Another
    /// `forbidden` tells nothing.
    Empty,
}

/// What a response changed in the last visible set; the caller persists both.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed<K> {
    pub added: BTreeSet<K>,
    /// Not empty: the server narrowed the scope. The caller handles it as a
    /// narrowing (S3.1: `Credentials::narrow`), whose rule covers every KB.
    pub removed: BTreeSet<K>,
}

/// The state as persisted, to restore at restart.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Saved<K> {
    pub epoch: u64,
    pub rules: Vec<Rule<K>>,
    pub version: u64,
    pub pending: Option<Pending<K>>,
    pub tombstones: BTreeSet<K>,
    pub visible: BTreeSet<K>,
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
    visible: BTreeSet<K>,
    /// Per KB, the latest epoch its evidence was delivered at: not covered
    /// by a rule, it counts as visible (S5.10).
    delivered: BTreeMap<K, u64>,
}

impl<K> Default for Evidence<K> {
    fn default() -> Self {
        Self {
            epoch: 0,
            rules: Vec::new(),
            version: 0,
            pending: None,
            tombstones: BTreeMap::new(),
            visible: BTreeSet::new(),
            delivered: BTreeMap::new(),
        }
    }
}

impl<K: Ord + Clone> Evidence<K> {
    /// What was persisted: every tombstone is unresolved. The evidence in the
    /// history comes back through [`Evidence::delivered`].
    pub fn restore(saved: Saved<K>) -> Self {
        let tombstones = saved.tombstones.into_iter();
        Self {
            epoch: saved.epoch,
            rules: saved.rules,
            version: saved.version,
            pending: saved.pending,
            tombstones: tombstones.map(|kb| (kb, Tombstone::Unresolved)).collect(),
            visible: saved.visible,
            delivered: BTreeMap::new(),
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
        if retired || self.placed(&delivered.kb, delivered.epoch) {
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
        self.visible.remove(&kb);
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

    /// Evidence of `kb` admitted at `epoch` was delivered.
    pub fn delivered(&mut self, kb: K, epoch: u64) {
        let latest = self.delivered.entry(kb).or_insert(epoch);
        *latest = (*latest).max(epoch);
    }

    /// An accepted response to a request over `asked` (`asked` empty for a
    /// ready probe without scope), admitted at epoch `admitted`. What it
    /// lists joins the set; a KB asked for that it leaves out, and that was
    /// visible, was removed. Only a request sent with the current credential
    /// tells a removal (S3.1 ownership). A KB with a tombstone neither joins
    /// nor is removed, nor does one deleted since the request was admitted
    /// join (ST-28g). A rule over every KB does not keep a KB out: a renewal
    /// that fails too places one without invalidating, so what comes back
    /// after it is still accepted.
    pub fn observe(
        &mut self,
        asked: &BTreeSet<K>,
        scope: &Scope<K>,
        admitted: u64,
        current: bool,
    ) -> Observed<K> {
        let none = BTreeSet::new();
        let listed = match scope {
            Scope::Listed(kbs) => kbs,
            Scope::Empty => &none,
        };
        let live = |kb: &&K| !self.tombstones.contains_key(*kb);
        let removed: BTreeSet<K> = asked
            .iter()
            .filter(|kb| current && !listed.contains(*kb) && live(kb) && self.visible(kb))
            .cloned()
            .collect();
        let added: BTreeSet<K> = listed
            .iter()
            .filter(|kb| {
                live(kb) && !self.visible.contains(*kb) && !self.deleted_since(kb, admitted)
            })
            .cloned()
            .collect();
        self.visible.retain(|kb| !removed.contains(kb));
        self.visible.extend(added.iter().cloned());
        Observed { added, removed }
    }

    /// In the set, or of evidence delivered and not covered by a rule.
    fn visible(&self, kb: &K) -> bool {
        let uncovered = self.delivered.get(kb).is_some_and(|e| !self.placed(kb, *e));
        self.visible.contains(kb) || uncovered
    }

    fn deleted_since(&self, kb: &K, epoch: u64) -> bool {
        let one = |r: &Rule<K>| matches!(&r.kbs, Kbs::One(one) if one == kb);
        self.rules.iter().any(|r| epoch < r.epoch && one(r))
    }

    fn placed(&self, kb: &K, epoch: u64) -> bool {
        let covers = |r: &Rule<K>| match &r.kbs {
            Kbs::All => true,
            Kbs::One(one) => one == kb,
        };
        self.rules.iter().any(|r| epoch < r.epoch && covers(r))
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
