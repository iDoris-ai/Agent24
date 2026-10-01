//! ME4-1.2.2b — §4.2: the deterministic `fire_id`.
//!
//! See `docs/design/ME4-S1-scheduler-callback.md` §4.2. Reference
//! implementation: the scratch check crate's `src/fire.rs` (design frozen
//! v3.1), ported here verbatim (module path only changes: this crate already
//! has `next_fire::fmt_iso`, so no external dependency on itself).

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};

use crate::next_fire::fmt_iso;
use agent24_store::FireTrigger;

/// `fire_` + 32 lowercase hex = the first 16 bytes of
/// `SHA-256("agent24-fire-v2\0" || trigger || "\0" || schedule_id || "\0" || fmt_iso(scheduled_for))`.
/// `trigger` (`tick` / `run_now`) is part of the domain, so a `run_now` in the
/// same second as a tick slot is a different fire, not a silent duplicate
/// (design v2, L3).
///
/// Deterministic in `(trigger, schedule_id, scheduled_for)` only — never in
/// the attempt, the tick instant, or the generation — so every retry and
/// every post-crash redelivery of one slot carries the same id (S1-8). Not a
/// secret (design §4.2).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FireId(String);

impl FireId {
    pub const PREFIX: &'static str = "fire_";
    const DOMAIN: &'static [u8] = b"agent24-fire-v2\0";

    #[must_use]
    pub fn derive(trigger: FireTrigger, schedule_id: &str, scheduled_for: DateTime<Utc>) -> Self {
        let mut h = Sha256::new();
        h.update(Self::DOMAIN);
        h.update(trigger.as_str().as_bytes());
        h.update(b"\0");
        h.update(schedule_id.as_bytes());
        h.update(b"\0");
        // Canonical second-precision `Z` form — the same string the store
        // keeps in `next_run_at`, so a slot has exactly one spelling.
        h.update(fmt_iso(scheduled_for).as_bytes());
        let digest = h.finalize();
        Self(format!("{}{}", Self::PREFIX, hex::encode(&digest[..16])))
    }

    /// For rows read back from `schedule_deliveries` (`fire_id` is already a
    /// stored, valid id there — no re-derivation).
    #[must_use]
    pub fn from_stored(s: String) -> Self {
        Self(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for FireId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::next_fire::parse_iso;

    /// C: fire_id determinism (design §4.2) — same `(schedule, scheduled_for,
    /// trigger)` -> same id; trigger is in the domain so a `run_now` and a
    /// tick slot in the same second never collide (design v2, L3 / v1's C
    /// bug it fixed). Mutation: drop `trigger` from the hash input -> this
    /// assertion goes red.
    #[test]
    fn same_slot_same_id_other_slot_or_trigger_other_id() {
        let t = parse_iso("2026-09-23T09:00:00Z").unwrap();
        let a = FireId::derive(FireTrigger::Tick, "sch_A", t);
        assert_eq!(a, FireId::derive(FireTrigger::Tick, "sch_A", t));
        // same slot, other trigger: a different fire (tick vs run_now in the
        // same second must not collide).
        let run_now_same_second = FireId::derive(FireTrigger::RunNow, "sch_A", t);
        assert_ne!(a, run_now_same_second);
        // sub-second noise folds into the same canonical (second-precision) slot
        let t_ms = parse_iso("2026-09-23T09:00:00.900Z").unwrap();
        assert_eq!(a, FireId::derive(FireTrigger::Tick, "sch_A", t_ms));
        // positive controls: next slot / other schedule differ
        let t2 = parse_iso("2026-09-23T09:01:00Z").unwrap();
        assert_ne!(a, FireId::derive(FireTrigger::Tick, "sch_A", t2));
        assert_ne!(a, FireId::derive(FireTrigger::Tick, "sch_B", t));
        assert_eq!(a.as_str().len(), 5 + 32);
        assert!(a.as_str().starts_with(FireId::PREFIX));
    }

    /// Known-answer test (review H1): pins `FireId::derive` to the exact
    /// bytes design §4.2 specifies, computed independently (Python
    /// `hashlib.sha256`) rather than by re-deriving with this same code.
    /// Mutation: dropping the `"agent24-fire-v2\0"` domain prefix, or
    /// reordering the concatenation, changes these hashes — this test goes
    /// red where the pure-Rust round-trip test above cannot (it only checks
    /// internal consistency, never an external oracle).
    #[test]
    fn known_answer_matches_the_independently_computed_design_hash() {
        let t = parse_iso("2026-09-23T09:00:00Z").unwrap();
        assert_eq!(
            FireId::derive(FireTrigger::Tick, "sch_A", t).as_str(),
            "fire_8d5458c6d9386057e407072576e2d791"
        );
        assert_eq!(
            FireId::derive(FireTrigger::RunNow, "sch_A", t).as_str(),
            "fire_f70986e012407d1b55f90f173b1d1a8e"
        );
    }
}
