use std::collections::HashMap;
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::AtomicU32;

use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use agent24_models::router::ModelRouter;
use agent24_protocol::Run;
use agent24_store::Store;
use agent24_tools::ToolRegistry;
use agent24_workspace::WorkspaceService;

use crate::{EventSink, SessionMemory};

mod approval;
mod construct;
mod execute;
pub use execute::compact_evidence_for_tests;
mod session;
mod start;
mod turn;

#[cfg(test)]
pub(crate) use approval::restored_decision_is_consistent;

/// H8: the reserved tool name the model calls to submit a plan for approval.
/// Handled by the loop itself (not the registry), so it is never dispatchable
/// as an ordinary tool.
const PROPOSE_PLAN: &str = "propose_plan";

/// Outcome of a `propose_plan` submission (H8).
pub(crate) enum PlanOutcome {
    /// Human approved — content is the tool result fed back to the model, and
    /// the loop leaves read-only.
    Approved(String),
    /// Human declined (or it timed out, fail-closed) — the run ends; content is
    /// both the tool result and the run's closing output.
    Rejected(String),
    /// The run was cancelled / aborted while the plan approval was pending.
    Cancelled,
}

pub struct RunManager {
    pub(crate) store: Store,
    pub(crate) router: Arc<ModelRouter>,
    pub(crate) tools: Arc<ToolRegistry>,
    pub(crate) workspace: Option<Arc<WorkspaceService>>,
    pub(crate) sink: Arc<dyn EventSink>,
    /// Optional per-session conversation memory (D1). `None` = runs start from
    /// the bare prompt, exactly as before.
    pub(crate) memory: Option<SessionMemory>,
    /// Test harness injection for calls whose fixture models an explicitly
    /// authorized cloud source. Production keeps this absent unless a host
    /// policy service is wired in.
    #[cfg(test)]
    pub(crate) test_egress_policy: Option<(
        Arc<dyn agent24_domain::EgressGate>,
        Vec<agent24_domain::EgressResource>,
        u64,
    )>,
    /// Daemon-wide shutdown token; every run token is a child of it
    pub(crate) shutdown: CancellationToken,
    /// Live run cancellation tokens; entries removed when a run reaches a
    /// terminal state. tokio::sync::Mutex — no poisoning, so a panicked task
    /// can never silently disable cancellation (review C2).
    pub(crate) cancels: Mutex<HashMap<String, CancellationToken>>,
    /// Run ids started by [`crate::RunManager::start_run_inbound`]. `execute` reads
    /// the entry once and tags that run's user message `Unknown`. Ordinary
    /// `start_run` never inserts here. Process-local: a restart does not
    /// resume `execute` (orphan sweep cancels the run).
    pub(crate) inbound_origins: Mutex<HashMap<String, String>>,
    /// M1 retrieval first: a run's `knowledge_query`, read once by `execute`
    /// for its first model call. Process-local like `inbound_origins`.
    pub(crate) knowledge_queries: Mutex<HashMap<String, String>>,
    /// One-shot failures for [`crate::RunManager::start_run_inbound`]. Tests call
    /// [`crate::RunManager::fail_next_inbound_start`] so a stored explicit row can
    /// land `start_failed` without a SQL trigger. Compiled only for this
    /// crate's tests or the `test-support` feature. Each taken fault
    /// consumes one.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) inbound_start_faults: AtomicU32,
}

/// Process-local origin for [`crate::RunManager::start_run_inbound`]. Not a field of
/// [`agent24_protocol::RunCreate`]: HTTP deserialization cannot mint it.
#[derive(Debug, Clone)]
pub struct InboundOrigin {
    pub event_id: String,
    /// Session `channel` for the new session this message gets.
    pub channel: String,
}

/// [`crate::RunManager::start_run_inbound`] either armed one run or found the event
/// already linked. The second case committed nothing.
#[derive(Debug)]
#[allow(
    clippy::large_enum_variant,
    reason = "Run is moved once; boxing it would allocate on every inbound start"
)]
pub enum InboundRunStart {
    Started(Run),
    AlreadyLinked,
    /// The event is not `starting` and has no link. Nothing was committed.
    NotStarting,
}

#[allow(
    clippy::large_enum_variant,
    reason = "Run is moved once; boxing it would allocate on every ordinary start"
)]
pub(crate) enum RunStart {
    Ready(Run),
    InboundAlreadyLinked,
    InboundNotStarting,
}

pub(crate) enum PersistOutcome {
    Stored,
    InboundAlreadyLinked,
    InboundNotStarting,
    /// The skill has no pointer row any more. No run was written.
    SkillNotActive,
    /// The pointer moved between admission and insert. No run was written.
    SkillMoved,
}

pub(crate) enum ParkedCallStop {
    CancelRun,
    RecoveryStopped,
}

/// What one tool call hands back to the model, with the source tag that must
/// be durable before the model sees it (K1-6b.5f, ADR-K1-02 §2.1). `source`
/// is `None` only for fixed text the tool never produced: a refusal, a
/// cancellation, a timeout, unparseable arguments.
pub(crate) struct ToolAnswer {
    content: String,
    source: Option<ResultTag>,
}

/// A tool result's tag and the origin it names (`module:{id}`,
/// `tool:{name}`), which a session records for its later turns (K1-6b.5f-2).
pub(crate) struct ResultTag {
    origin: String,
    tag: agent24_store::SourceRef,
}

impl ToolAnswer {
    fn fixed(content: String) -> Self {
        Self {
            content,
            source: None,
        }
    }
}
