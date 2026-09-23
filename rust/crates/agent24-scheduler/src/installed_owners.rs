//! ME4-1.2.2b — §4.2/§9/v2 M5: which module owner names this daemon's
//! catalogue has at all this run (mounted, disabled in os.json, refused —
//! anything `mount_all` discovered). The tick records a delivery row only
//! for these; an owner absent from the catalogue (uninstalled) still gets
//! its `next_run_at` advanced but nothing else — so an uninstalled module's
//! schedules stop producing new delivery rows (bounded growth, §9).
//!
//! Set once, after `mount_all` returns and before the tick loop starts
//! (ME4-1.3.1 wires that call); unset means "record for everyone" — the
//! pre-v2 behaviour, and also this cut's behaviour, since nothing in
//! ME4-1.2.2b wires `mount_all` yet (out of scope; see the design's §13
//! task split).

use std::collections::HashSet;
use std::sync::OnceLock;

pub struct InstalledOwners(OnceLock<HashSet<String>>);

impl InstalledOwners {
    #[must_use]
    pub const fn new() -> Self {
        Self(OnceLock::new())
    }

    /// `false` only once the set is known AND does not contain `owner`.
    #[must_use]
    pub fn may_record(&self, owner: &str) -> bool {
        self.0.get().is_none_or(|set| set.contains(owner))
    }

    /// Idempotent: a second call is a no-op (mirrors `OnceLock::set`).
    pub fn set(&self, owners: HashSet<String>) {
        let _ = self.0.set(owners);
    }
}

impl Default for InstalledOwners {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unset_records_everyone_set_gates_membership() {
        let owners = InstalledOwners::new();
        assert!(owners.may_record("anything"));
        owners.set(HashSet::from(["mod-a".to_owned()]));
        assert!(owners.may_record("mod-a"));
        assert!(!owners.may_record("mod-b"));
        // idempotent: a second `set` does not clobber the first
        owners.set(HashSet::from(["mod-b".to_owned()]));
        assert!(!owners.may_record("mod-b"));
    }
}
