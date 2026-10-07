//! Agent24 decision service (D0-1 scope).
//!
//! `docs/agent/PLAN-DECIDE.md` and `docs/research/DECISION-MODELS.md` §7.3
//! are the authoritative design docs; `docs/decision.md` ADR-033 records
//! where this crate sits in the architecture. This crate only defines the
//! contract and a deterministic-code floor backend — **it is not wired into
//! `agent24d` and changes no existing call site** (that is D1).
//!
//! Hard constraints carried over from `PLAN-DECIDE.md` §0 (do not relax
//! these when extending the crate):
//!
//! - Category-A judgements (WriteGate, provenance tagging, owner/active
//!   filtering, session import, the Authorizer, capability tokens, session
//!   views) never go through this crate. A decision produced here can only
//!   trigger a *suggestion* or a *stricter* action, never a laxer one.
//! - An unavailable backend returns [`types::Outcome::Unavailable`] — the
//!   cascade never silently downgrades to a different backend or invents a
//!   result. It carries forward the most recent non-empty floor a prior
//!   layer actually left behind (see [`service::DecisionService`]).
//! - An LLM asked to simulate a calibrated classifier is tagged
//!   `backend = llm_simulation` and its answers can never report
//!   `calibrated = true`. **Enforced by [`service::DecisionService::decide`]**,
//!   which re-stamps every answer against the evaluating backend's own
//!   `kind()` — not merely by [`types::Answer::new`]'s own check, which only
//!   holds if a backend's `evaluate()` tells the truth about its own kind
//!   (review #688 M1: it does not have to, so the service does not trust it).
//! - There is no global default threshold; [`threshold::ThresholdBands`] must
//!   be constructed explicitly per decision point, and that construction
//!   path is the ONLY way to get one — `Deserialize` goes through it too
//!   (`#[serde(try_from = "...")]`, review #688 M2), so a hand-edited config
//!   file cannot bypass the range/order checks the way a plain derive would.

pub mod backend;
pub mod log;
pub mod points;
pub mod service;
pub mod threshold;
pub mod types;

pub use backend::{BackendOutcome, DecisionBackend, RuleBackend};
pub use log::{
    DECISION_LOG_SCHEMA_VERSION, DEFAULT_DECISION_LOG_RETENTION_DAYS, DecisionLog,
    DecisionLogError, DeleteSelector, ExportFilter, FinalAction, LogEntry, LoggedLayer,
    OutcomeEntry, OutcomeQuality, OutcomeSignal,
};
pub use service::DecisionService;
pub use threshold::{Action, ThresholdBands, ThresholdError};
pub use types::{
    Answer, AnswerValue, BackendKind, Decision, DecisionPoint, DecisionRequest, ModelRef, Outcome,
    Question, SideEffectClass,
};
