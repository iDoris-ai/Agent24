//! Agent24 run manager (C2 scope).
//!
//! The agent loop with cancellation as a first-class citizen (ADR-026 hard
//! constraint #1 — openfang's unfixable lesson): every run holds its own
//! CancellationToken, derived from the daemon shutdown token, cancellable at
//! every await point. Run/tool-call state is persisted through agent24-store
//! (whose transactions enforce the core transition matrix), and every
//! lifecycle change is emitted through an [`EventSink`].
//!
//! C3: the loop iterates provider completions, executing model tool calls
//! through the [`agent24_tools::ToolRegistry`] dispatch pipeline (whitelist +
//! fail-closed approval stub + timeout) up to `MAX_ITERATIONS` per run. Every
//! tool call is persisted, evented, and — when denied by policy — audited.

pub mod resume;
mod retain;
pub mod self_wake;
mod session_memory;
mod skill_inject;
pub mod subagent;

mod agent_error;
mod memory_outcome;
mod messages;
mod router_helpers;
mod run_manager;
mod source_policy;

pub use agent_error::AgentError;
pub use memory_outcome::{
    EventSink, ExplicitRememberState, MemoryFailure, MemoryWriteOutcome, RESUME_TTL,
};
pub use messages::normalize_for_provider;
pub use router_helpers::{RouterRewriter, RouterSummarizer};
pub use run_manager::{InboundOrigin, InboundRunStart, RunManager, compact_evidence_for_tests};
pub use session_memory::{EmbedBackfill, RECALL_END_MARKER, RECALL_PREFIX, SessionMemory};
pub use skill_inject::{SkillAdmissionError, SkillProvider, SkillProviderSource};
pub use source_policy::{MAX_ITERATIONS, MAX_TOOL_CALLS_PER_TURN, merge_source_policy};

use agent_error::{add_usage, restore_authority_error_must_propagate, workspace_now, zero_usage};
use memory_outcome::{MEMORY_WRITE_BUDGET, SessionContext};
use messages::{shape_host_recall, thread_to_messages, unanswered_calls_of_last_turn};
use source_policy::{
    PROVIDER_ROLE_MESSAGE, SKILL_LABEL_MESSAGE, SOURCE_UNPROVEN_CODE, SOURCE_UNPROVEN_MESSAGE,
    canonical_recall_ref, origin_allows_send, unproven_source_failure,
};

// Unit tests reach these crate-root names through `use super::*`.
#[cfg(test)]
use agent_error::{workspace_now_at, workspace_timestamp};
#[cfg(test)]
use agent24_core::util::{now_iso8601, ulid};
#[cfg(test)]
use agent24_memory::event::{Origin, Trust};
#[cfg(test)]
use agent24_memory::session::Summarizer;
#[cfg(test)]
use agent24_models::router::{ModelRouter, TaskProfile};
#[cfg(test)]
use agent24_models::{CompletionRequest, ModelError, Msg, ToolSpec};
#[cfg(test)]
use agent24_protocol::{
    Approval, ApprovalStatus, Decision, ErrorBody, EventBody, MemoryWriteSkippedPayload, RiskClass,
    Run, RunCreate, RunInput, RunMode, RunStatus, ToolCall, ToolCallStatus, Usage,
};
#[cfg(test)]
use agent24_store::{
    MessageOrigin, MessageOriginLookup, OriginatedRunMessage, RunAdmission, RunMessage, RunPatch,
    Store, StoreError, WorkspaceInstant, WorkspaceLeaseId, WorkspaceStoreError,
};
#[cfg(test)]
use agent24_tools::{ToolContext, ToolError, ToolRegistry};
#[cfg(test)]
use agent24_workspace::WorkspaceService;
#[cfg(test)]
use messages::prune_stale_recall_message;
#[cfg(test)]
use run_manager::{ParkedCallStop, restored_decision_is_consistent};
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod approval_tests;
