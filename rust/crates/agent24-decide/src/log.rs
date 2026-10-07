//! D0-2 decision log contract (`docs/agent/PLAN-DECIDE.md` §2.1-§2.3).
//!
//! This module defines plain DTOs and an async trait only — no SQLite, no
//! `agent24-store` dependency. That is deliberate, the same way D0-1 kept
//! this crate dependency-free (ADR-033 #2): `agent24-store` is L2 and
//! `agent24-decide` is L3, and `docs/ARCHITECTURE-LAYERS.md` §1.1 is
//! explicit that dependencies only ever point "down" (L3 may depend on L2,
//! never the reverse) — so `agent24-store` cannot depend on this crate to
//! implement [`DecisionLog`]. The concrete SQLite-backed implementation is
//! composed in `agent24d` (L5), which already depends on both `agent24-store`
//! and (as of this change) `agent24-decide`; see `docs/decision.md` ADR-034
//! for the full reasoning and the alternative this rejected.
//!
//! D0-2 ships this trait with **zero production callers**, same as D0-1's
//! `RuleBackend` — nothing in `agent24d` constructs a decision today
//! (`PLAN-DECIDE.md` §4: "D0 不改任何现有调用点的行为"). D1 is what actually
//! calls [`DecisionLog::record`] after a real `DecisionService::decide`.

use serde::{Deserialize, Serialize};

use crate::types::{BackendKind, DecisionPoint, ModelRef};

/// The decision log's own schema version (§2.1's `schema_version` field).
/// Bump this whenever a breaking change is made to the shape a
/// [`DecisionLog`] implementation persists or exports — `export`'s JSONL
/// output always carries the version each row was recorded under, not the
/// crate's current one, so an old row never silently gets relabeled.
pub const DECISION_LOG_SCHEMA_VERSION: u32 = 1;

/// Default retention window (§2.3: "保留期默认有界（具体天数 D0 定）") before
/// a sweep scrubs `input`/`context` from a decision. **待 jason
/// 定（pending confirmation）** — 180 days is this PR's proposal, chosen to
/// comfortably outlast D1's accumulation target (§2.4: ~200 labeled examples
/// per decision point before a personal model is even considered) while
/// still being a bounded window rather than "forever". Nothing reads this
/// constant yet; D0 wires no caller to the sweep (`PLAN-DECIDE.md` §4).
pub const DEFAULT_DECISION_LOG_RETENTION_DAYS: u32 = 180;

/// §2.1 "layers[]": one backend layer's contribution to a decision, as
/// recorded for the log. Independent of [`crate::types::Decision`], which
/// today only retains the LAST layer that ran (review #688 L3) — D1 will
/// need to extend `DecisionService` to capture every layer before a real run
/// can populate this; D0-2 only defines the shape it will populate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LoggedLayer {
    pub backend: BackendKind,
    pub model: Option<ModelRef>,
    pub label: Option<String>,
    pub p: Option<f32>,
    pub latency_ms: u64,
}

/// §2.1 "final": the action the cascade ultimately took. Serializes as
/// `"final"` (not `"final_action"`) to match the PLAN's field name exactly —
/// `final_action` is the Rust-side name only, since `final` reads oddly as
/// an identifier even though it is not a reserved word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinalAction {
    Execute,
    Abstain,
    Ask,
    Escalate,
}

impl FinalAction {
    /// The exact strings the storage layer's CHECK constraint accepts
    /// (`migrations/0014_decision_log.sql`) — kept here, next to the type
    /// that is the single source of truth for the closed set, so a
    /// `DecisionLog` implementation never has to hand-roll this mapping.
    pub fn as_db_str(&self) -> &'static str {
        match self {
            FinalAction::Execute => "execute",
            FinalAction::Abstain => "abstain",
            FinalAction::Ask => "ask",
            FinalAction::Escalate => "escalate",
        }
    }
}

/// One decision, ready to hand to [`DecisionLog::record`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LogEntry {
    pub decision_id: String,
    pub ts: String,
    pub schema_version: u32,
    pub point: DecisionPoint,
    /// User's original text — local-only, never leaves the device
    /// (§2.3). `None` means "nothing to record" (e.g. a synthetic test
    /// decision); it is NOT how retention scrubbing is expressed — that is a
    /// store-side operation on an already-written row, not a value a caller
    /// constructs.
    pub input: Option<String>,
    pub context: serde_json::Value,
    pub question: serde_json::Value,
    pub layers: Vec<LoggedLayer>,
    #[serde(rename = "final")]
    pub final_action: FinalAction,
    pub hw_tier: Option<String>,
}

/// §2.2 "标签从哪来": the six signals a label can arrive by, none of them
/// requiring the user to do anything they weren't already doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSignal {
    ClarifyAnswer,
    UserRetract,
    UserSaysWrong,
    ApprovalDenied,
    ApprovalGranted,
    RecalledUncorrected,
}

/// §2.2's quality column — how much a labeled outcome should be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeQuality {
    High,
    Medium,
    Low,
}

/// One outcome, ready to hand to [`DecisionLog::append_outcome`]. A single
/// decision can accumulate several of these over time (§2.2) — this is an
/// append, never an upsert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OutcomeEntry {
    pub decision_id: String,
    pub ts: String,
    pub signal: OutcomeSignal,
    pub label: serde_json::Value,
    pub quality: OutcomeQuality,
}

/// [`DecisionLog::export`]'s filter. Every field `None` means "everything".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ExportFilter {
    pub point: Option<DecisionPoint>,
    pub since: Option<String>,
}

/// [`DecisionLog::delete`]'s selector — exactly one of the three
/// granularities §2.3 asks for ("按条删除、按 point 删除、全部删除").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DeleteSelector {
    ById(String),
    ByPoint(DecisionPoint),
    All,
}

#[derive(Debug, thiserror::Error)]
pub enum DecisionLogError {
    #[error("decision log unavailable: {0}")]
    Unavailable(String),
}

/// Write/append/export/delete contract for the decision log (§2.1-§2.3).
/// See the module docs for why this is a trait rather than a concrete
/// SQLite type, and why the implementation lives in `agent24d`, not here or
/// in `agent24-store`.
#[async_trait::async_trait]
pub trait DecisionLog: Send + Sync {
    /// Records one decision. Implementations must not silently drop a
    /// write — a failure is reported, never swallowed (same spirit as
    /// `docs/agent/PLAN-DECIDE.md` §0's "不静默降级" for the decision
    /// cascade itself).
    async fn record(&self, entry: LogEntry) -> Result<(), DecisionLogError>;

    /// Appends one outcome to an already-recorded decision.
    async fn append_outcome(&self, outcome: OutcomeEntry) -> Result<(), DecisionLogError>;

    /// One JSONL line per matching decision (including every outcome
    /// recorded against it), filters applied server-side.
    async fn export(&self, filter: ExportFilter) -> Result<Vec<String>, DecisionLogError>;

    /// Deletes decisions matching `selector`. Returns how many were deleted
    /// (0 or 1 for [`DeleteSelector::ById`]) — deleting an unknown id is not
    /// an error, it deletes zero rows.
    async fn delete(&self, selector: DeleteSelector) -> Result<u64, DecisionLogError>;
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn final_action_db_strings_match_the_migrations_check_constraint() {
        assert_eq!(FinalAction::Execute.as_db_str(), "execute");
        assert_eq!(FinalAction::Abstain.as_db_str(), "abstain");
        assert_eq!(FinalAction::Ask.as_db_str(), "ask");
        assert_eq!(FinalAction::Escalate.as_db_str(), "escalate");
    }

    #[test]
    fn log_entry_serializes_final_action_under_the_key_final_not_final_action() {
        let entry = LogEntry {
            decision_id: "d1".to_owned(),
            ts: "2026-10-07T00:00:00Z".to_owned(),
            schema_version: DECISION_LOG_SCHEMA_VERSION,
            point: DecisionPoint::new("retain.intent"),
            input: Some("记住我对花生过敏".to_owned()),
            context: serde_json::json!({}),
            question: serde_json::json!([]),
            layers: vec![],
            final_action: FinalAction::Execute,
            hw_tier: None,
        };
        let v = serde_json::to_value(&entry).unwrap();
        assert_eq!(v["final"], "execute");
        assert!(
            v.get("final_action").is_none(),
            "the wire shape must use PLAN-DECIDE.md's own field name"
        );
    }

    #[test]
    fn log_entry_round_trips_through_json() {
        let entry = LogEntry {
            decision_id: "d1".to_owned(),
            ts: "2026-10-07T00:00:00Z".to_owned(),
            schema_version: 1,
            point: DecisionPoint::new("recall.gate"),
            input: None,
            context: serde_json::json!({"turn": 3}),
            question: serde_json::json!([{"kind": "noul", "id": "q1", "prompt": "p"}]),
            layers: vec![LoggedLayer {
                backend: BackendKind::Rule,
                model: None,
                label: Some("true".to_owned()),
                p: Some(0.9),
                latency_ms: 2,
            }],
            final_action: FinalAction::Escalate,
            hw_tier: Some("t2".to_owned()),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: LogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn delete_selector_covers_the_three_granularities_plan_decide_asks_for() {
        let by_id = DeleteSelector::ById("d1".to_owned());
        let by_point = DeleteSelector::ByPoint(DecisionPoint::new("retain.intent"));
        let all = DeleteSelector::All;
        assert_ne!(
            serde_json::to_string(&by_id).unwrap(),
            serde_json::to_string(&by_point).unwrap()
        );
        assert_ne!(
            serde_json::to_string(&by_point).unwrap(),
            serde_json::to_string(&all).unwrap()
        );
    }
}
