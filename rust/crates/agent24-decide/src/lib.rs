//! Agent24 decision service (D0-1 + D0-3 scope).
//!
//! `docs/agent/PLAN-DECIDE.md` and `docs/research/DECISION-MODELS.md` §7.3
//! are the authoritative design docs; `docs/decision.md` ADR-033 records
//! where this crate sits in the architecture. This crate defines the
//! decision contract (D0-1), hardware probing + tiering + the model catalog
//! schema (D0-3), and a deterministic-code floor backend — **D0-1/D0-3 do
//! not change the behavior of any existing call site**; `agent24d` only
//! reads [`hw`]/[`tier`] to serve `GET /api/v1/decide/profile` (D0-3's own
//! acceptance line), nothing in the agent loop consumes [`service`] yet
//! (that is D1).
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
pub mod catalog;
pub mod hw;
pub mod points;
pub mod service;
pub mod threshold;
pub mod tier;
pub mod types;

pub use backend::{BackendOutcome, DecisionBackend, RuleBackend};
pub use catalog::{CatalogEntry, CatalogError, ModelCatalog, Runtime as CatalogRuntime};
pub use hw::{Accelerator, FakeHardwareProbe, HardwareProbe, HardwareProfile, SystemProbe};
pub use service::DecisionService;
pub use threshold::{Action, ThresholdBands, ThresholdError};
pub use tier::{DownloadConsent, Tier, TierDecision, TierPolicy};
pub use types::{
    Answer, AnswerValue, BackendKind, Decision, DecisionPoint, DecisionRequest, ModelRef, Outcome,
    Question, SideEffectClass,
};
