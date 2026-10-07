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
//!   result (see [`service::DecisionService`]).
//! - An LLM asked to simulate a calibrated classifier is tagged
//!   `backend = llm_simulation` and its answers can never report
//!   `calibrated = true` — enforced at construction, see [`types::Answer::new`].
//! - There is no global default threshold; [`threshold::ThresholdBands`] must
//!   be constructed explicitly per decision point.

pub mod backend;
pub mod points;
pub mod service;
pub mod threshold;
pub mod types;

pub use backend::{BackendOutcome, DecisionBackend, RuleBackend};
pub use service::DecisionService;
pub use threshold::{Action, ThresholdBands, ThresholdError};
pub use types::{
    Answer, AnswerValue, BackendKind, Decision, DecisionPoint, DecisionRequest, ModelRef, Outcome,
    Question, SideEffectClass,
};
