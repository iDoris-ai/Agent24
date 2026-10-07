//! Well-known decision point IDs (see `docs/research/DECISION-POINTS-INVENTORY-2026-10-06.md`
//! category B — language-understanding judgements that may consult a decision
//! backend; category A judgements never get an ID here, they stay deterministic
//! code outside this crate).
//!
//! These are plain string constants, not an enum: new decision points are added
//! continuously (D1 记住意图/召回门控, D2 guardian.risk, D3 入站分类/PII/…) and a
//! closed enum would force a crate release per decision point. [`DecisionPoint`]
//! stays an open newtype; call sites that want compile-time safety can define
//! their own `const` using one of these strings.

/// D1: 记住意图（explicit remember intent）。
pub const RETAIN_INTENT: &str = "retain.intent";

/// D1: 召回门控（does this turn need memory recall at all）。
pub const RECALL_GATE: &str = "recall.gate";

/// D2: Guardian 工具风险评估。
pub const GUARDIAN_RISK: &str = "guardian.risk";
