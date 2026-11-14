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
pub use session_memory::{EmbedBackfill, RECALL_END_MARKER, RECALL_PREFIX, SessionMemory};
pub use skill_inject::{SkillAdmissionError, SkillProvider, SkillProviderSource};

use std::collections::HashMap;
use std::sync::Arc;
#[cfg(any(test, feature = "test-support"))]
use std::sync::atomic::{AtomicU32, Ordering};

use tokio::sync::Mutex;

use agent24_core::util::{now_iso8601, ulid};
use agent24_memory::event::{Origin, Trust};
use agent24_memory::session::Summarizer;
use agent24_models::router::{ModelRouter, TaskProfile};
use agent24_models::{CompletionRequest, ModelError, Msg, ToolCallRequest, ToolSpec};
use agent24_protocol::{
    Approval, ApprovalStatus, Decision, ErrorBody, EventBody, MemoryRecalledPayload,
    MemoryWriteSkippedPayload, ModelDeltaPayload, RiskClass, Run, RunCancelledPayload,
    RunCompletedPayload, RunCreate, RunFailedPayload, RunInput, RunMode, RunOutputPayload,
    RunStartedPayload, RunStatus, ToolCall, ToolCallStatus, ToolCompletedPayload,
    ToolCompletedStatus, ToolStartedPayload, Usage,
};
use agent24_store::{
    MessageOrigin, MessageOriginLookup, OriginatedRunMessage, RunAdmission, RunAdmissionDenial,
    RunMessage, RunPatch, RunTerminalTransition, SnapshotRunMessage, Store, StoreError,
    WorkspaceInstant, WorkspaceLeaseId, WorkspaceStoreError,
};
use agent24_tools::{
    GateDecision, ToolContext, ToolError, ToolRegistry, summarize_input, truncate,
};
use agent24_workspace::WorkspaceService;
use tokio_util::sync::CancellationToken;

pub fn merge_source_policy(
    mut profile: TaskProfile,
    mode: agent24_store::SourceMode,
) -> TaskProfile {
    if mode == agent24_store::SourceMode::LocalOnly {
        profile.privacy = agent24_models::router::Privacy::LocalOnly;
    }
    profile
}

/// Completion→tools round trips per run before the run is failed. A model
/// stuck asking for tools forever must terminate deterministically.
pub const MAX_ITERATIONS: usize = 10;

const SOURCE_UNPROVEN_CODE: &str = "source_unproven";
const SOURCE_UNPROVEN_MESSAGE: &str =
    "无法证明本线程已落盘消息的来源，已停止发送。请新建会话。不要自动复制旧 recall 或待执行工具。";
/// Refuses to run a tool for a run whose pinned skill has lost its label.
struct SkillLabelGuard {
    store: Store,
    run_id: String,
}

#[async_trait::async_trait]
impl agent24_tools::ExecutionGuard for SkillLabelGuard {
    async fn permit(&self) -> Result<(), String> {
        if skill_inject::label_missing(&self.store, &self.run_id).await {
            Err("skill source label unavailable".to_owned())
        } else {
            Ok(())
        }
    }
}

const SKILL_LABEL_MESSAGE: &str =
    "这次运行钉了技能，但读不到它的来源标签，已停止。缺标签不会按普通来源处理。";
const PROVIDER_ROLE_MESSAGE: &str = "模型返回的角色不是 assistant，已停止。不会执行这一轮的工具。";

fn origin_allows_send(lookup: &MessageOriginLookup) -> bool {
    match lookup {
        // A stored HostRecall with a SQL NULL recall_ref is still unproven.
        // Present HostSystem is sent verbatim (R05-5 section 3).
        MessageOriginLookup::Present(stored) => !matches!(
            stored.origin,
            MessageOrigin::HostRecall { recall_ref: None }
        ),
        MessageOriginLookup::Missing | MessageOriginLookup::Unreadable { .. } => false,
    }
}

/// Compact JSON array of every assertion id, sorted lexicographically.
/// One id is `["id"]`. `serde_json::to_string` emits no spaces.
fn canonical_recall_ref(ids: &[String]) -> Result<String, serde_json::Error> {
    let mut sorted = ids.to_vec();
    sorted.sort();
    serde_json::to_string(&sorted)
}

fn unproven_source_failure(run: &Run) -> bool {
    run.error
        .as_ref()
        .is_some_and(|error| error.code == SOURCE_UNPROVEN_CODE)
}

/// Cap for the externally-visible `output_summary` (full output goes back to
/// the model; full input is audit-only in the store row).
const SUMMARY_MAX_BYTES: usize = 500;

/// Tool calls executed per assistant turn; the rest are answered with a
/// "skipped" tool result so the wire protocol stays balanced.
pub const MAX_TOOL_CALLS_PER_TURN: usize = 16;

/// M1-T14: what [`RunManager::chat_memory_prelude`] found out about THIS
/// prompt — independent of the messages it returns to prepend. The caller
/// (`/api/v1/chat`) needs this to build a deterministic `memory_receipt`
/// after the turn is committed, since the model's own reply text must never
/// be the source of that signal (it has been observed claiming success on a
/// turn the server actually skipped).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExplicitRememberState {
    /// The prompt is an explicit remember, and personal memory is currently
    /// paused — the write will be skipped.
    Paused,
    /// The prompt is an explicit remember, and personal memory is active —
    /// the write is expected to land (success/failure determined by the
    /// actual write that follows).
    Active,
}

/// fix683 (PR #683 review): the LIVE result of [`RunManager::remember_exchange`]
/// / [`RunManager::chat_remember_turn`] — a tri-state (four-state, counting
/// failure) replacing the old bare `bool`, which could only say
/// "write_gate didn't error", not "paused" vs "committed". `/api/v1/chat`'s
/// `memory_receipt` must be built from THIS, never from the pre-model-call
/// [`ExplicitRememberState`] snapshot — personal memory can be paused or
/// un-paused while the model is still generating, after that snapshot was
/// taken but before this turn's write actually runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryWriteOutcome {
    /// The turn's explicit-remember assertion is durably persisted.
    Saved,
    /// Personal memory was paused at the moment of commit; nothing was
    /// persisted for this turn's explicit remember.
    SkippedPaused,
    /// The session append (the conversation turn) reported an error; NOTHING of
    /// this turn was written. Carries the typed cause, never its text.
    Failed(MemoryFailure),
    /// P1-02c: the conversation turn WAS saved, but the explicit-remember
    /// assertion was not. Distinct from [`Self::Failed`] so a receipt never
    /// says "nothing saved" about a turn that is in the history, nor "saved"
    /// about an assertion that is not.
    Partial(MemoryFailure),
    /// There was nothing to write: no configured memory, no session, the
    /// prompt was not an explicit remember, or the origin was untrusted.
    NotApplicable,
}

/// P1-02c: the typed cause of a failed memory write — a closed code and, for a
/// quota code, its dimension. Carries no owner key and no storage text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryFailure {
    pub code: agent24_memory::MemoryErrorCode,
}

impl MemoryFailure {
    pub fn from_error(err: &agent24_memory::MemoryError) -> Self {
        Self { code: err.code() }
    }

    /// The wire form of the code. Exhaustive on purpose: a new
    /// `MemoryErrorCode` is a compile error here until the protocol knows it.
    pub fn wire_code(self) -> agent24_protocol::MemoryFailureCode {
        use agent24_memory::MemoryErrorCode as M;
        use agent24_protocol::MemoryFailureCode as W;
        match self.code {
            M::QuotaRows => W::QuotaRows,
            M::QuotaBytes => W::QuotaBytes,
            M::DiskLow => W::DiskLow,
            M::DiskFull => W::DiskFull,
            M::ReservedKind => W::ReservedKind,
            M::RetractIdTooLong => W::RetractIdTooLong,
            M::Internal => W::Internal,
        }
    }

    fn wire_code_str(self) -> &'static str {
        self.code.as_str()
    }

    fn event(
        self,
        session_id: &str,
        kind: Option<&str>,
        partial: bool,
    ) -> agent24_protocol::MemoryWriteFailedPayload {
        agent24_protocol::MemoryWriteFailedPayload {
            session_id: session_id.to_owned(),
            // Sanitized transitional `reason`: the code string, nothing else.
            reason: Some(self.code.as_str().to_owned()),
            code: Some(self.wire_code()),
            dimension: self.code.dimension().map(str::to_owned),
            owner_kind: Some(agent24_protocol::MemoryOwnerKind::Personal),
            kind: kind.map(str::to_owned),
            partial,
        }
    }
}

/// What loading a session's prior context came to (see
/// [`RunManager::session_context`]).
enum SessionContext {
    Ready(Vec<Msg>),
    Cancelled,
    ImportIncomplete(MemoryFailure),
}

#[cfg(test)]
impl SessionContext {
    /// Test convenience: the loaded messages, panicking on any other outcome.
    fn unwrap(self) -> Vec<Msg> {
        match self {
            Self::Ready(context) => context,
            Self::Cancelled => panic!("session context load was cancelled"),
            Self::ImportIncomplete(f) => panic!("session import incomplete: {}", f.code),
        }
    }
}

impl MemoryWriteOutcome {
    /// P1-02c: `false` iff a memory write this run attempted failed (wholly or
    /// partly). Paused and not-applicable are not failures.
    pub fn persisted(self) -> bool {
        !matches!(self, Self::Failed(_) | Self::Partial(_))
    }
}

/// H8: the reserved tool name the model calls to submit a plan for approval.
/// Handled by the loop itself (not the registry), so it is never dispatchable
/// as an ordinary tool.
const PROPOSE_PLAN: &str = "propose_plan";

/// Outcome of a `propose_plan` submission (H8).
enum PlanOutcome {
    /// Human approved — content is the tool result fed back to the model, and
    /// the loop leaves read-only.
    Approved(String),
    /// Human declined (or it timed out, fail-closed) — the run ends; content is
    /// both the tool result and the run's closing output.
    Rejected(String),
    /// The run was cancelled / aborted while the plan approval was pending.
    Cancelled,
}

/// Where lifecycle events go (the daemon adapts this onto its WS hub).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, body: EventBody);
}

/// Budget for preparing and compacting the post-completion memory write.
/// Ordering demands it happen before `run.completed`; append transaction
/// confirmation can exceed this budget because its outcome must be observed.
const MEMORY_WRITE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

/// How long a parked approval remains resumable (H3). A restored approval older
/// than this is too stale to safely continue — the world it was queued against
/// has likely moved on — so `assess_restore` aborts it. Generous: an overnight
/// scheduled run must still be answerable the next morning.
pub const RESUME_TTL: std::time::Duration = std::time::Duration::from_secs(72 * 3600);

/// A [`Summarizer`] backed by the model router.
///
/// Session compaction currently folds with `LocalOnly`: source tags are
/// attached to individual runs, while a session can contain messages from
/// multiple runs and this interface does not carry a run ID. Until that
/// provenance can be resolved across the compacted messages, fail closed.
pub struct RouterSummarizer {
    router: Arc<ModelRouter>,
    /// Daemon shutdown token — compaction can call a slow provider, and a stuck
    /// summarizer must not outlive shutdown (review D5b).
    shutdown: CancellationToken,
}

/// Per-message budget in the summarization transcript. Elision is marked
/// so the summarizer knows content was cut. Full originals
/// remain in the session event log.
const SUMMARY_MSG_MAX_CHARS: usize = 8000;
/// Whole-transcript budget, so a huge fold can't build an unbounded prompt.
const SUMMARY_TRANSCRIPT_MAX_CHARS: usize = 32_000;

impl RouterSummarizer {
    pub fn new(router: Arc<ModelRouter>, shutdown: CancellationToken) -> Self {
        Self { router, shutdown }
    }
}

#[async_trait::async_trait]
impl Summarizer for RouterSummarizer {
    async fn summarize(
        &self,
        prior: Option<&str>,
        messages: &[Msg],
    ) -> std::result::Result<String, String> {
        let mut transcript = String::new();
        for m in messages {
            let content = m.content.as_deref().unwrap_or("");
            if content.is_empty() {
                continue;
            }
            // Mark elision explicitly; originals remain in the event log.
            let (body, elided) = if content.chars().count() > SUMMARY_MSG_MAX_CHARS {
                let kept: String = content.chars().take(SUMMARY_MSG_MAX_CHARS).collect();
                (kept, true)
            } else {
                (content.to_owned(), false)
            };
            transcript.push_str(&format!("{}: {body}", m.role));
            if elided {
                transcript.push_str(" …[truncated for summarization]");
            }
            transcript.push('\n');
            if transcript.chars().count() >= SUMMARY_TRANSCRIPT_MAX_CHARS {
                transcript.push_str("…[earlier messages omitted]\n");
                break;
            }
        }
        let prompt = match prior {
            Some(prior) => format!(
                "Update this running summary of a conversation so it still captures \
                 everything needed to continue. Reply with the updated summary only.\n\n\
                 EXISTING SUMMARY:\n{prior}\n\nNEW MESSAGES:\n{transcript}"
            ),
            None => format!(
                "Summarize this conversation so it can be continued later, keeping \
                 decisions, facts and open threads. Reply with the summary only.\n\n\
                 {transcript}"
            ),
        };
        let req = CompletionRequest {
            messages: vec![Msg::user(prompt)],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
            disable_thinking: false,
            idoris_headroom_confirmed: false,
            temperature: None,
            progress: None,
        };
        let profile =
            merge_source_policy(TaskProfile::default(), agent24_store::SourceMode::LocalOnly);
        let (_provider, res) = self
            .router
            .complete(profile, &req, &self.shutdown)
            .await
            .map_err(|e| e.to_string())?;
        let summary = res.message.content.unwrap_or_default().trim().to_owned();
        if summary.is_empty() {
            return Err("summarizer returned an empty summary".to_owned());
        }
        Ok(summary)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceStoreError),
    #[error("workspace run admission denied: {0:?}")]
    WorkspaceAdmission(RunAdmissionDenial),
    #[error(transparent)]
    WorkspaceService(#[from] agent24_workspace::WorkspaceError),
    #[error("workspace authority service unavailable")]
    WorkspaceServiceUnavailable,
    #[error("session not found: {0}")]
    SessionNotFound(String),
    /// A run that names a skill was refused before any row was written.
    #[error(transparent)]
    Skill(#[from] SkillAdmissionError),
}

fn zero_usage() -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens: 0,
        cost_usd: 0.0,
    }
}

#[cfg(test)]
fn workspace_timestamp(raw: String) -> String {
    if raw.len() == 20 {
        raw.strip_suffix('Z')
            .map(|prefix| format!("{prefix}.000Z"))
            .unwrap_or(raw)
    } else {
        raw
    }
}

fn workspace_now_at(time: std::time::SystemTime) -> Result<String, AgentError> {
    agent24_core::util::iso8601_millis_at(time).map_err(|_| {
        AgentError::Workspace(WorkspaceStoreError::InvalidValue { field: "timestamp" })
    })
}

fn workspace_now() -> Result<String, AgentError> {
    workspace_now_at(std::time::SystemTime::now())
}

fn restore_authority_error_must_propagate(error: &AgentError) -> bool {
    matches!(
        error,
        AgentError::Workspace(WorkspaceStoreError::Database)
            | AgentError::WorkspaceService(
                agent24_workspace::WorkspaceError::InfrastructureUnavailable { .. }
            )
    )
}

fn add_usage(mut total: Usage, delta: &Usage) -> Usage {
    total.prompt_tokens = total.prompt_tokens.saturating_add(delta.prompt_tokens);
    total.completion_tokens = total
        .completion_tokens
        .saturating_add(delta.completion_tokens);
    total.total_tokens = total.total_tokens.saturating_add(delta.total_tokens);
    total.cost_usd += delta.cost_usd;
    total
}

/// Rebuild the in-memory conversation from a persisted run thread (H3 resume).
/// The inverse of the agent loop's per-message persistence: a malformed
/// `tool_calls` value degrades to "no calls" rather than erroring — a row we
/// cannot parse must never resurrect as a phantom tool request.
fn thread_to_messages(thread: &[RunMessage]) -> Vec<Msg> {
    thread
        .iter()
        .map(|m| Msg {
            role: m.role.clone(),
            content: m.content.clone(),
            tool_calls: serde_json::from_value(m.tool_calls.clone()).unwrap_or_default(),
            tool_call_id: m.tool_call_id.clone(),
        })
        .collect()
}

/// Request-only message normalization (M1-T07.1 H1 fix). The persisted
/// snapshot (recall block, then the session's prior context, then this
/// turn's prompt) can legally contain `system` anywhere — a compaction
/// summary is itself injected as `system` — and, when there is no prior
/// context, two adjacent `user` turns (the recall block immediately followed
/// by this run's own prompt). Several HF-strict chat templates (the
/// Gemma/Mistral family) reject that shape outright ("roles must alternate"
/// / system must lead). This NEVER touches the persisted thread or the
/// snapshot fail-closed contract — it only reshapes what is SENT to the
/// provider on THIS call, rebuilt fresh from `messages` every iteration of
/// [`RunManager::run_loop`]:
/// 1. every `system` message is pulled out and merged into ONE, placed first
///    (dropped entirely if there were none);
/// 2. the remaining messages keep their relative order; adjacent `user`
///    turns (now possibly newly adjacent, since removing an in-between
///    `system` message can create a fresh adjacency) are merged with `\n\n`.
pub fn normalize_for_provider(messages: &[Msg]) -> Vec<Msg> {
    let mut system_parts = Vec::new();
    let mut rest = Vec::with_capacity(messages.len());
    for msg in messages {
        if msg.role == "system" {
            if let Some(content) = &msg.content {
                system_parts.push(content.clone());
            }
        } else {
            rest.push(msg.clone());
        }
    }
    let mut normalized = Vec::with_capacity(rest.len() + 1);
    if !system_parts.is_empty() {
        normalized.push(Msg::system(system_parts.join("\n\n")));
    }
    for msg in rest {
        if msg.role == "user"
            && let Some(last) = normalized.last_mut()
            && last.role == "user"
        {
            last.content = Some(match (last.content.take(), msg.content) {
                (Some(a), Some(b)) => format!("{a}\n\n{b}"),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => String::new(),
            });
            continue;
        }
        normalized.push(msg);
    }
    normalized
}

/// M1-T07.1 M4: a resumed thread's recall message was captured at the run's
/// FIRST model call and may now name an id the owner has since retracted or
/// superseded — resume must never resurrect a forgotten fact. Re-validates
/// every `id=` against `active` and keeps only the still-active lines,
/// rebuilding the block's header + end marker around them; drops the whole
/// message if nothing survives. A block this parser cannot make sense of
/// (missing the end marker, no `id=` tokens) is dropped outright — treated
/// as unverifiable, not trusted by default.
fn prune_stale_recall_message(msg: Msg, active: &std::collections::HashSet<String>) -> Option<Msg> {
    let Some(content) = msg.content.as_deref() else {
        return Some(msg);
    };
    if msg.role != "user" || !content.starts_with(RECALL_PREFIX) {
        return Some(msg);
    }
    // Round 2 ②: a message that starts with the header but does NOT end
    // with the marker is not a real recall block (it is left untouched,
    // not dropped) — a real one always has this exact shape; only a
    // malformed/foreign message fails this, and the fail-safe direction
    // for something we cannot parse as a recall block is to PRESERVE it,
    // never silently delete it.
    let Some(body) = content.strip_suffix(RECALL_END_MARKER) else {
        return Some(msg);
    };
    let mut parts = body.split("\n- [id=");
    let Some(header) = parts.next() else {
        return Some(msg);
    };
    let mut kept = String::from(header);
    let mut any = false;
    for entry in parts {
        let Some(space) = entry.find(' ') else {
            continue;
        };
        if active.contains(&entry[..space]) {
            kept.push_str("\n- [id=");
            kept.push_str(entry);
            any = true;
        }
    }
    if any {
        kept.push_str(RECALL_END_MARKER);
        Some(Msg::user(kept))
    } else {
        None
    }
}

/// Shape one message that is PROVEN (by its persisted origin) to be a
/// `HostRecall`. Unlike [`prune_stale_recall_message`], which deliberately
/// passes a malformed lookalike through for the old text-based decision, a
/// proven HostRecall that is not a well-formed recall block is dropped: it
/// cannot be re-validated, so it must not reach the provider.
fn shape_host_recall(msg: Msg, active: &std::collections::HashSet<String>) -> Option<Msg> {
    let content = msg.content.as_deref()?;
    if msg.role != "user" || !content.starts_with(RECALL_PREFIX) {
        return None;
    }
    let body = content.strip_suffix(RECALL_END_MARKER)?;
    let has_id = body
        .split("\n- [id=")
        .skip(1)
        .any(|entry| entry.find(' ').is_some_and(|space| space > 0));
    if !has_id {
        return None;
    }
    prune_stale_recall_message(msg, active)
}

/// The still-unanswered tool calls of the reconstructed thread's LAST
/// tool-calling assistant turn, in request order. `None` when there is no such
/// turn (an inconsistent resume — the caller aborts). Returns the full ordered
/// remainder, so a partially answered fan-out resumes EVERY leftover call, not
/// just the first — otherwise the model would be handed an assistant turn with
/// dangling unanswered tool_calls.
fn unanswered_calls_of_last_turn(messages: &[Msg]) -> Option<Vec<ToolCallRequest>> {
    let idx = messages
        .iter()
        .rposition(|m| m.role == "assistant" && !m.tool_calls.is_empty())?;
    let answered: std::collections::HashSet<&str> = messages[idx + 1..]
        .iter()
        .filter(|m| m.role == "tool")
        .filter_map(|m| m.tool_call_id.as_deref())
        .collect();
    Some(
        messages[idx]
            .tool_calls
            .iter()
            .filter(|c| !answered.contains(c.id.as_str()))
            .cloned()
            .collect(),
    )
}

pub struct RunManager {
    store: Store,
    router: Arc<ModelRouter>,
    tools: Arc<ToolRegistry>,
    workspace: Option<Arc<WorkspaceService>>,
    sink: Arc<dyn EventSink>,
    /// Optional per-session conversation memory (D1). `None` = runs start from
    /// the bare prompt, exactly as before.
    memory: Option<SessionMemory>,
    /// Test harness injection for calls whose fixture models an explicitly
    /// authorized cloud source. Production keeps this absent unless a host
    /// policy service is wired in.
    #[cfg(test)]
    test_egress_policy: Option<(
        Arc<dyn agent24_domain::EgressGate>,
        Vec<agent24_domain::EgressResource>,
        u64,
    )>,
    /// Daemon-wide shutdown token; every run token is a child of it
    shutdown: CancellationToken,
    /// Live run cancellation tokens; entries removed when a run reaches a
    /// terminal state. tokio::sync::Mutex — no poisoning, so a panicked task
    /// can never silently disable cancellation (review C2).
    cancels: Mutex<HashMap<String, CancellationToken>>,
    /// Run ids started by [`RunManager::start_run_inbound`]. `execute` reads
    /// the entry once and tags that run's user message `Unknown`. Ordinary
    /// `start_run` never inserts here. Process-local: a restart does not
    /// resume `execute` (orphan sweep cancels the run).
    inbound_origins: Mutex<HashMap<String, String>>,
    /// One-shot failures for [`RunManager::start_run_inbound`]. Tests call
    /// [`RunManager::fail_next_inbound_start`] so a stored explicit row can
    /// land `start_failed` without a SQL trigger. Compiled only for this
    /// crate's tests or the `test-support` feature. Each taken fault
    /// consumes one.
    #[cfg(any(test, feature = "test-support"))]
    inbound_start_faults: AtomicU32,
}

/// Process-local origin for [`RunManager::start_run_inbound`]. Not a field of
/// [`RunCreate`]: HTTP deserialization cannot mint it.
#[derive(Debug, Clone)]
pub struct InboundOrigin {
    pub event_id: String,
    /// Session `channel` for the new session this message gets.
    pub channel: String,
}

/// [`RunManager::start_run_inbound`] either armed one run or found the event
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
enum RunStart {
    Ready(Run),
    InboundAlreadyLinked,
    InboundNotStarting,
}

enum PersistOutcome {
    Stored,
    InboundAlreadyLinked,
    InboundNotStarting,
    /// The skill has no pointer row any more. No run was written.
    SkillNotActive,
    /// The pointer moved between admission and insert. No run was written.
    SkillMoved,
}

enum ParkedCallStop {
    CancelRun,
    RecoveryStopped,
}

/// What one tool call hands back to the model, with the source tag that must
/// be durable before the model sees it (K1-6b.5f, ADR-K1-02 §2.1). `source`
/// is `None` only for fixed text the tool never produced: a refusal, a
/// cancellation, a timeout, unparseable arguments.
struct ToolAnswer {
    content: String,
    source: Option<ResultTag>,
}

/// A tool result's tag and the origin it names (`module:{id}`,
/// `tool:{name}`), which a session records for its later turns (K1-6b.5f-2).
struct ResultTag {
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

impl RunManager {
    // Keep the invalid version case explicit: mapping it to zero makes the
    // source fail `EgressRequest::authorize` instead of widening access.
    #[allow(clippy::manual_unwrap_or, clippy::manual_unwrap_or_default)]
    fn egress_policy_version(policy_version: i64) -> u64 {
        match u64::try_from(policy_version) {
            Ok(version) => version,
            Err(_) => 0,
        }
    }

    pub async fn task_profile_for_run(&self, run_id: &str, base: TaskProfile) -> TaskProfile {
        let mode = match self.store.run_policy_snapshot(run_id).await {
            Ok(snapshot) => snapshot.effective_mode,
            Err(err) => {
                tracing::warn!(
                    run_id,
                    error = %err,
                    "run source policy lookup failed; restricting model call to LocalOnly"
                );
                agent24_store::SourceMode::LocalOnly
            }
        };
        merge_source_policy(base, mode)
    }

    async fn persist_new_run(&self, run: &Run) -> Result<(), AgentError> {
        let Some(_) = run.workspace_id.as_ref() else {
            self.store.insert_run(run).await?;
            return Ok(());
        };
        let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid()))?;
        let acquired_at = WorkspaceInstant::parse(&run.created_at)?;
        match self
            .store
            .insert_run_with_workspace_admission(run, Some(lease_id), &acquired_at)
            .await?
        {
            RunAdmission::Admitted { lease_id: Some(_) } => Ok(()),
            RunAdmission::Admitted { lease_id: None } => Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            }
            .into()),
            RunAdmission::Denied(denial) => Err(AgentError::WorkspaceAdmission(denial)),
        }
    }

    pub fn new(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Self::with_memory_and_workspace(store, router, tools, sink, shutdown, None, None)
    }

    /// Build with optional per-session conversation memory (D1).
    pub fn with_memory(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
        memory: Option<SessionMemory>,
    ) -> Arc<Self> {
        Self::with_memory_and_workspace(store, router, tools, sink, shutdown, memory, None)
    }

    /// Build with optional session memory and workspace authority service.
    pub fn with_memory_and_workspace(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
        memory: Option<SessionMemory>,
        workspace: Option<Arc<WorkspaceService>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            router,
            tools,
            workspace,
            sink,
            memory,
            #[cfg(test)]
            test_egress_policy: None,
            shutdown,
            cancels: Mutex::new(HashMap::new()),
            inbound_origins: Mutex::new(HashMap::new()),
            #[cfg(any(test, feature = "test-support"))]
            inbound_start_faults: AtomicU32::new(0),
        })
    }

    /// Fail the next [`Self::start_run_inbound`] before it writes a run.
    ///
    /// The inbound event row is already committed by the caller. This is the
    /// transient start error that marks that row `start_failed`. Not present
    /// in a release build: the method exists only under `cfg(test)` or the
    /// `test-support` feature.
    #[cfg(any(test, feature = "test-support"))]
    pub fn fail_next_inbound_start(&self) {
        self.inbound_start_faults.fetch_add(1, Ordering::AcqRel);
    }

    #[cfg(any(test, feature = "test-support"))]
    fn take_inbound_start_fault(&self) -> bool {
        let mut current = self.inbound_start_faults.load(Ordering::Acquire);
        while current > 0 {
            match self.inbound_start_faults.compare_exchange(
                current,
                current - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
        false
    }

    async fn tool_context_for(
        &self,
        run: &Run,
        tool_call_id: impl Into<String>,
    ) -> Result<ToolContext, AgentError> {
        let tool_call_id = tool_call_id.into();
        let lease_id = self.store.active_workspace_run_lease_id(&run.id).await?;
        let context = if run.workspace_id.is_none() {
            if lease_id.is_some() {
                return Err(WorkspaceStoreError::CorruptRow {
                    table: "runs",
                    field: "workspace_id",
                }
                .into());
            }
            ToolContext::legacy(
                run.id.clone(),
                run.session_id.clone(),
                run.schedule_id.clone(),
                tool_call_id,
            )
        } else {
            let lease_id = lease_id.ok_or(WorkspaceStoreError::CorruptRow {
                table: "workspace_leases",
                field: "row",
            })?;
            let service = self
                .workspace
                .as_ref()
                .ok_or(AgentError::WorkspaceServiceUnavailable)?;
            let authority = service.bind_run_authority(&run.id, &lease_id).await?;
            ToolContext::workspace_bound(
                run.id.clone(),
                run.session_id.clone(),
                run.schedule_id.clone(),
                tool_call_id,
                authority,
            )
        };
        // Asked again right before any tool runs, after an approval wait too.
        let context = context.with_execution_guard(Arc::new(SkillLabelGuard {
            store: self.store.clone(),
            run_id: run.id.clone(),
        }));
        let context = match self.store.run_policy_snapshot(&run.id).await {
            // A pinned skill without its label is never read as Ordinary.
            Ok(_) if self.skill_label_missing(&run.id).await => {
                context.with_egress_policy_unavailable()
            }
            Ok(snapshot) if snapshot.sources.is_empty() => context.with_egress_policy_unavailable(),
            Ok(snapshot) => {
                let resources = snapshot
                    .sources
                    .into_iter()
                    // ADR-K1-02 §2.1.1: only the host-assigned classification
                    // decides whether a source is tool egress material, never
                    // its kind. `Ordinary` sources (user input today, ordinary
                    // recalled memory later) do not restrict tools; Restricted
                    // and Unknown sources, of any kind, keep the strict policy.
                    .filter(|source| {
                        source.classification != agent24_store::SourceClassification::Ordinary
                    })
                    .map(|source| agent24_domain::EgressResource {
                        resource_id: source.source_id,
                        revision: source.revision_digest,
                        local_only: source.mode == agent24_store::SourceMode::LocalOnly,
                        authorization_ref: source.authorization_ref,
                        policy_version: Self::egress_policy_version(source.policy_version),
                    })
                    .collect();
                context.with_egress_resources(resources)
            }
            Err(err) => {
                tracing::warn!(run_id = %run.id, error = %err, "run source policy lookup failed; restricting tool egress");
                context.with_egress_policy_unavailable()
            }
        };
        #[cfg(test)]
        let context = if let Some((gate, resources, generation)) = &self.test_egress_policy {
            context.with_egress_policy(Arc::clone(gate), resources.clone(), *generation)
        } else {
            context
        };
        Ok(context)
    }

    #[cfg(test)]
    fn with_test_egress_policy(
        mut self: Arc<Self>,
        gate: Arc<dyn agent24_domain::EgressGate>,
        resources: Vec<agent24_domain::EgressResource>,
        generation: u64,
    ) -> Arc<Self> {
        if let Some(manager) = Arc::get_mut(&mut self) {
            manager.test_egress_policy = Some((gate, resources, generation));
        }
        self
    }

    async fn cancel_workspace_resume_recovery(&self, run_id: &str) {
        let cancelled = match workspace_now() {
            Ok(ended_at) => match WorkspaceInstant::parse(&ended_at) {
                Ok(ended_at) => self
                    .store
                    .cancel_workspace_run_recovery(run_id, &ended_at)
                    .await
                    .map_err(AgentError::from),
                Err(err) => Err(AgentError::from(err)),
            },
            Err(err) => Err(err),
        };
        match cancelled {
            Ok(RunTerminalTransition::Applied(_)) => {
                self.sink.emit(EventBody::RunCancelled(RunCancelledPayload {
                    run_id: run_id.to_owned(),
                }));
                self.note_inbound_terminal(run_id).await;
            }
            Ok(RunTerminalTransition::Conflict) => {
                tracing::warn!("run {run_id}: recovery cancel conflicted");
            }
            Err(err) => tracing::error!("run {run_id}: recovery cancel failed: {err}"),
        }
    }

    /// Create a run (202 semantics: persisted queued, executed in background).
    /// The session's prior context, or empty when memory is off / this run has
    /// no session. Best-effort: a memory failure degrades to a fresh context
    /// rather than failing the run.
    /// Returns `None` if the run was cancelled while waiting — the caller must
    /// then finish it cancelled rather than proceed with an empty context.
    ///
    /// CANCEL-AWARE by necessity: this takes the per-session lock, and another
    /// run can hold that lock beyond MEMORY_WRITE_BUDGET while transaction
    /// confirmation completes. Compaction is budgeted. A run parked here has
    /// not reached its model call, so blocking it uncancellably would break the
    /// C2 contract that cancel works in ANY non-terminal state — the same reason
    /// the model call and the memory write race the token (review D5b).
    async fn session_context(
        &self,
        session_id: Option<&str>,
        cancel: &CancellationToken,
    ) -> SessionContext {
        let (Some(memory), Some(sid)) = (self.memory.as_ref(), session_id) else {
            return SessionContext::Ready(Vec::new());
        };
        let load = memory.context(sid);
        let loaded = tokio::select! {
            result = load => result,
            () = cancel.cancelled() => return SessionContext::Cancelled,
        };
        match loaded {
            Ok(context) => SessionContext::Ready(context),
            // W3: the legacy import failed, so this session's history exists but
            // could not be brought in. Running on as if the session were empty
            // would answer without context the user believes is there, and the
            // exchange would then be appended to a history missing its past.
            // Nothing was written (the import is one transaction), so a retry
            // starts clean.
            Err(err @ agent24_memory::MemoryError::SessionImportIncomplete(_)) => {
                let failure = MemoryFailure::from_error(&err);
                tracing::error!(session_id = sid, code = %failure.code, error = %err, "session import failed");
                self.sink.emit(EventBody::MemoryWriteFailed(failure.event(
                    sid,
                    Some("session.import"),
                    false,
                )));
                SessionContext::ImportIncomplete(failure)
            }
            Err(err) => {
                tracing::warn!("session {sid} memory load failed: {err}");
                SessionContext::Ready(Vec::new())
            }
        }
    }

    /// Commit the original exchange before best-effort compaction. Memory
    /// failures are observable but never fail an already-answered run.
    /// fix683: returns the LIVE [`MemoryWriteOutcome`] — Saved/SkippedPaused/
    /// Failed/NotApplicable — instead of the old bare `bool`, which collapsed
    /// "committed" and "paused rollback" into the same `true`. M1-T14: the
    /// chat surface uses this (and ONLY this, never the pre-call pause
    /// snapshot) to build a deterministic `memory_receipt`.
    async fn remember_exchange(
        &self,
        session_id: Option<&str>,
        prompt: &str,
        answer: &str,
        prompt_origin: Origin,
        source_ref: agent24_store::AssertionSourceRef,
    ) -> MemoryWriteOutcome {
        let (Some(memory), Some(sid)) = (self.memory.as_ref(), session_id) else {
            return MemoryWriteOutcome::NotApplicable;
        };
        match memory
            .remember(sid, prompt, answer, prompt_origin, source_ref)
            .await
        {
            Ok(report) => {
                self.report_summary_failure(sid, report.compaction_failure.as_ref());
                match report.retain {
                    retain::RetainOutcome::Saved => MemoryWriteOutcome::Saved,
                    retain::RetainOutcome::SkippedPaused => MemoryWriteOutcome::SkippedPaused,
                    retain::RetainOutcome::NotApplicable => MemoryWriteOutcome::NotApplicable,
                }
            }
            Err(failure_of) => {
                let failure = MemoryFailure::from_error(&failure_of.error);
                // W3: the legacy import is its own kind of failed write.
                let kind = matches!(
                    failure_of.error,
                    agent24_memory::MemoryError::SessionImportIncomplete(_)
                )
                .then_some("session.import");
                tracing::error!(session_id = sid, code = %failure.code, error = %failure_of.error, "session memory write failed");
                self.sink.emit(EventBody::MemoryWriteFailed(failure.event(
                    sid,
                    kind,
                    failure_of.turn_saved,
                )));
                // W2 on top of a failed retain: independent events, so a
                // double failure never loses the summary one.
                self.report_summary_failure(sid, failure_of.compaction_failure.as_ref());
                if failure_of.turn_saved {
                    MemoryWriteOutcome::Partial(failure)
                } else {
                    MemoryWriteOutcome::Failed(failure)
                }
            }
        }
    }

    /// W2: the turn is durable, but its compaction summary could not be written.
    /// Observable, with the typed cause only.
    fn report_summary_failure(&self, sid: &str, err: Option<&agent24_memory::MemoryError>) {
        let Some(err) = err else { return };
        let failure = MemoryFailure::from_error(err);
        tracing::error!(session_id = sid, code = %failure.code, error = %err, "session summary write failed");
        self.sink.emit(EventBody::MemoryWriteFailed(failure.event(
            sid,
            Some("session.summary"),
            false,
        )));
    }

    /// M1-T12: the SAME recall + pause-notice decision `drive_new` makes at
    /// run start (above), reused for the stateless `/api/v1/chat` surface
    /// instead of duplicated there. Returns the messages to prepend — a
    /// recall data block (if anything was found) followed by the paused
    /// write notice (if this prompt is an explicit remember and memory is
    /// paused) — in the SAME order `drive_new`'s snapshot uses, so a caller
    /// that runs this through [`normalize_for_provider`] gets byte-identical
    /// ordering rules (system merged first, adjacent `user` turns merged).
    /// `run_id` is only used to tag the `memory.recalled`/`memory.write_skipped`
    /// events — this never touches the run/store tables `drive_new` does.
    /// Empty/`None` when there is no configured memory (`self.memory` is
    /// `None`). M1-T14: the second element of the tuple is this prompt's
    /// [`ExplicitRememberState`] (`None` when it is not an explicit
    /// remember at all) — the caller needs it, independent of the model's
    /// own reply, to build a deterministic `memory_receipt` once the turn is
    /// committed. K1-6b.4c: an error means the session floor could not be
    /// recorded; the caller must not call the model.
    pub async fn chat_memory_prelude(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        prompt: &str,
        has_non_user_history: bool,
    ) -> Result<
        (
            Vec<Msg>,
            Option<ExplicitRememberState>,
            agent24_store::AssertionSourceRef,
        ),
        StoreError,
    > {
        // K1-6b.4b: this stateless surface persists no run tags, so the
        // sources of its model context are folded here: the caller's own
        // messages (user input, Ordinary) plus every recalled fact.
        let mut sources = vec![(
            format!("user_input:{run_id}"),
            agent24_store::SourceClassification::Ordinary,
        )];
        let Some(memory) = self.memory.as_ref() else {
            return Ok((
                Vec::new(),
                None,
                agent24_store::AssertionSourceRef::from_sources(sources),
            ));
        };
        let mut prelude = Vec::new();
        match memory.recall(prompt).await {
            Ok(Some((msg, recalled))) => {
                sources.extend(
                    recalled
                        .iter()
                        .map(|hit| (format!("memory_assertion:{}", hit.id), hit.classification)),
                );
                self.sink
                    .emit(EventBody::MemoryRecalled(MemoryRecalledPayload {
                        run_id: run_id.to_owned(),
                        ids: recalled.into_iter().map(|hit| hit.id).collect(),
                    }));
                prelude.push(msg);
            }
            Ok(None) => {}
            Err(err) => {
                tracing::warn!(run_id = %run_id, error = %err, "chat memory recall failed");
            }
        }
        let explicit_state = if retain::explicit_remember(prompt).is_some() {
            let paused = match memory.kv().memory_enabled(memory.owner()).await {
                Ok(enabled) => !enabled,
                Err(err) => {
                    tracing::warn!(run_id = %run_id, error = %err, "chat memory pause check failed; assuming enabled");
                    false
                }
            };
            if paused {
                self.sink
                    .emit(EventBody::MemoryWriteSkipped(MemoryWriteSkippedPayload {
                        run_id: run_id.to_owned(),
                        session_id: session_id.map(str::to_owned),
                        reason: "paused".to_owned(),
                    }));
                prelude.push(Msg::system(retain::PAUSED_WRITE_NOTICE));
                Some(ExplicitRememberState::Paused)
            } else {
                Some(ExplicitRememberState::Active)
            }
        } else {
            None
        };
        // K1-6b.4c (ADR-K1-02 §2.2): assistant turns the client sends back
        // are this session's earlier replies, classified by its floor (no
        // stored floor: Unknown). The merge happens before the model call;
        // a failure fails the request.
        if let Some(sid) = session_id {
            let own = agent24_store::AssertionSourceRef::from_sources(sources.clone());
            let floor = self
                .store
                .merge_session_floor(
                    sid,
                    own.classification,
                    has_non_user_history,
                    &now_iso8601(),
                )
                .await?;
            sources.push((format!("session_floor:{sid}"), floor));
            // K1-6b.5f-2: what earlier /runs turns of the session took in
            // from tools (/chat itself offers none).
            for origin in self.store.session_tool_result_origins(sid).await? {
                let tag =
                    agent24_store::SourceRef::session_tool_result(&origin, sid, now_iso8601());
                sources.push((tag.source_id, tag.classification));
            }
        }
        Ok((
            prelude,
            explicit_state,
            agent24_store::AssertionSourceRef::from_sources(sources),
        ))
    }

    /// Expire a session's memory (inbound text retention) under the session's
    /// lock; `None` when this manager has no session memory. See
    /// [`SessionMemory::purge_session`].
    pub async fn purge_session_memory(
        &self,
        session_id: &str,
        not_after: &str,
    ) -> Option<agent24_memory::Result<agent24_memory::session_log::SessionPurge>> {
        let memory = self.memory.as_ref()?;
        Some(memory.purge_session(session_id, not_after).await)
    }

    /// M1-T12: the SAME post-answer commit `drive_new`'s run loop performs
    /// (`remember_exchange`, used as-is — not duplicated), exposed for the
    /// `/api/v1/chat` surface. `source: "chat"` distinguishes this turn's
    /// provenance from the agent loop's `"agent_loop"` in the audit trail.
    /// Trust is `UserSaid` unless the session channel is an inbound source
    /// ([`INBOUND_SOURCE_CHANNELS`]) or one of its runs is linked in
    /// `inbound_runs`. A missing session row has no inbound channel, so it
    /// stays `UserSaid`. Any other channel (`desktop`, `cli`, `tui`,
    /// `creative`, or an unknown name) is the account owner. A failed lookup
    /// is `Model` — retain then skips the qualified assertion. A failed
    /// lookup of an explicit remember also emits `memory.write_skipped` with
    /// reason `origin_unavailable`. Retain/log failures emit
    /// `memory.write_failed` and otherwise never propagate — the chat
    /// response the caller already has must still reach its client. Returns
    /// the LIVE [`MemoryWriteOutcome`] — see [`RunManager::remember_exchange`].
    /// `run_id` is the transient id `/chat` already put on `run.started`, so
    /// the skip event lines up with that call.
    pub async fn chat_remember_turn(
        &self,
        run_id: &str,
        session_id: &str,
        prompt: &str,
        answer: &str,
        source_ref: agent24_store::AssertionSourceRef,
    ) -> MemoryWriteOutcome {
        let origin = match self.chat_session_external(session_id).await {
            Ok(false) => Origin {
                source: "chat".into(),
                trust: Trust::UserSaid,
            },
            Ok(true) => Origin {
                source: "chat".into(),
                trust: Trust::Model,
            },
            Err(err) => {
                tracing::warn!(
                    run_id,
                    session_id,
                    error = %err,
                    "chat prompt origin lookup failed; not recording the prompt as the user"
                );
                self.note_origin_unavailable(run_id, Some(session_id), prompt);
                Origin {
                    source: "chat".into(),
                    trust: Trust::Model,
                }
            }
        };
        self.remember_exchange(Some(session_id), prompt, answer, origin, source_ref)
            .await
    }

    /// `true` when `/chat` must not treat this session's prompt as the account
    /// owner. No session row is not evidence of an external channel.
    async fn chat_session_external(&self, session_id: &str) -> Result<bool, StoreError> {
        let Some(session) = self.store.get_session(session_id).await? else {
            return Ok(false);
        };
        if Self::inbound_source_channel(&session.channel) {
            return Ok(true);
        }
        self.store.session_has_inbound_run(session_id).await
    }

    /// Session channels that name an inbound source. Same set as `agent24d`'s
    /// X11-2f route allowlist `ALLOWLIST_SOURCES`
    /// (`rust/apps/agent24d/src/inbound/routes.rs`): `hyphae`, `wechat`,
    /// `remote`. `scheduler` and `self` are not on that list. A channel
    /// outside this set is the person at this machine unless `inbound_runs`
    /// links one of the session's runs. Inbound start uses the source name
    /// as the session channel and refuses a name that is not in this set.
    pub const INBOUND_SOURCE_CHANNELS: &[&str] = &["hyphae", "wechat", "remote"];

    fn inbound_source_channel(channel: &str) -> bool {
        Self::INBOUND_SOURCE_CHANNELS.contains(&channel)
    }

    /// K1-6b.4b (ADR-K1-02 §2.1.1 item 4): the classification a memory
    /// written from this run carries — the strictest of every source
    /// persisted for the run. A failed read is no evidence, so `Unknown`.
    async fn run_assertion_source_ref(&self, run_id: &str) -> agent24_store::AssertionSourceRef {
        let sources = match self.store.run_policy_snapshot(run_id).await {
            Ok(snapshot) => snapshot.sources,
            Err(err) => {
                tracing::warn!(run_id, error = %err, "run source policy lookup failed; memory written as unknown");
                Vec::new()
            }
        };
        agent24_store::AssertionSourceRef::from_sources(
            sources
                .into_iter()
                .map(|source| (source.source_id, source.classification)),
        )
    }

    /// Derive prompt provenance only from durable run metadata. Scheduled runs
    /// are conservative: self-wake prompts ultimately originate in model output
    /// and cannot authorize qualified personal memory.
    ///
    /// An external prompt is also not the account owner. The durable signals
    /// are a non-Ordinary `UserInput` tag (the general case) and an
    /// `inbound_runs` row (the inbound case, including a resume that never
    /// repopulates the process map). Either one, or a failed lookup, yields
    /// [`Trust::Model`]. Retain commits a qualified assertion only for
    /// [`Trust::UserSaid`], and there is no separate external trust tier. A
    /// lookup error must not fall through to `UserSaid`. The same origin is
    /// what the session log stores for the user message, so that turn is not
    /// recorded as `UserSaid` either. When that error also drops an explicit
    /// remember, the skip is `memory.write_skipped` / `origin_unavailable`.
    async fn run_prompt_origin(&self, run: &Run) -> Origin {
        if run.schedule_id.is_some() {
            return Origin {
                source: "scheduler".into(),
                trust: Trust::Model,
            };
        }
        match self.external_user_prompt(&run.id).await {
            Ok(false) => Origin {
                source: "agent_loop".into(),
                trust: Trust::UserSaid,
            },
            Ok(true) => Origin {
                source: "inbound".into(),
                trust: Trust::Model,
            },
            Err(err) => {
                tracing::warn!(
                    run_id = %run.id,
                    error = %err,
                    "prompt origin lookup failed; not recording the prompt as the user"
                );
                self.note_origin_unavailable(&run.id, run.session_id.as_deref(), &run.input.prompt);
                Origin {
                    source: "agent_loop".into(),
                    trust: Trust::Model,
                }
            }
        }
    }

    /// The qualified assertion was not written because provenance could not
    /// be read. Ordinary turns have no assertion to skip, so they only warn
    /// at the call site.
    fn note_origin_unavailable(&self, run_id: &str, session_id: Option<&str>, prompt: &str) {
        if retain::explicit_remember(prompt).is_none() {
            return;
        }
        self.sink
            .emit(EventBody::MemoryWriteSkipped(MemoryWriteSkippedPayload {
                run_id: run_id.to_owned(),
                session_id: session_id.map(str::to_owned),
                reason: "origin_unavailable".to_owned(),
            }));
    }

    /// `true` when this run's user prompt must not be treated as the account
    /// owner. A non-Ordinary `UserInput` tag is the general signal; an
    /// `inbound_runs` row covers an inbound run whose tag is not visible yet.
    async fn external_user_prompt(&self, run_id: &str) -> Result<bool, StoreError> {
        let tags = self.store.list_run_source_tags(run_id).await?;
        let user_input_not_ordinary = tags.iter().any(|tag| {
            tag.source.kind == agent24_store::SourceKind::UserInput
                && tag.source.classification != agent24_store::SourceClassification::Ordinary
        });
        if user_input_not_ordinary {
            return Ok(true);
        }
        Ok(self.store.inbound_event_id_for_run(run_id).await?.is_some())
    }

    /// Fail closed: a store error is external, so the caller does not tag the
    /// prompt Ordinary or inject the paused-memory notice.
    async fn persisted_external_prompt(&self, run_id: &str) -> bool {
        match self.external_user_prompt(run_id).await {
            Ok(external) => external,
            Err(err) => {
                tracing::warn!(
                    run_id,
                    error = %err,
                    "inbound prompt lookup failed; tagging the prompt as external"
                );
                true
            }
        }
    }

    /// The source tag for what `tool` returned on call `tool_call_id`.
    fn tool_result_tag(&self, tool: &str, tool_call_id: &str) -> ResultTag {
        let source = self.tools.tool_result_source(tool);
        let tag = agent24_store::SourceRef::tool_result(
            &source.origin,
            tool_call_id,
            source.classification,
            now_iso8601(),
        );
        ResultTag {
            origin: source.origin,
            tag,
        }
    }

    /// Persist `answer`'s source tag, then hand its content to the model as
    /// the result of `call`. A tag that cannot be written ends the run before
    /// the content reaches the model; returns false then. In a session, a
    /// non-ordinary result's origin is recorded with the tag, so the
    /// session's later turns carry it (K1-6b.5f-2). It is recorded whether or
    /// not memory is configured now: a run parked without memory may finish,
    /// and feed later turns, after a restart that has it.
    async fn answer_tool_call(
        &self,
        run: &Run,
        messages: &mut Vec<Msg>,
        call: &ToolCallRequest,
        answer: ToolAnswer,
    ) -> bool {
        let run_id = run.id.as_str();
        let session = run.session_id.as_deref();
        if let Some(result) = &answer.source
            && let Err(err) = self
                .store
                .tag_tool_result(
                    run_id,
                    messages.len() as i64,
                    &result.tag,
                    session.map(|sid| (sid, result.origin.as_str())),
                    &now_iso8601(),
                )
                .await
        {
            self.finish_failed(
                run_id,
                "source_policy_unavailable",
                &format!("failed to persist a tool result's source: {err}"),
            )
            .await;
            return false;
        }
        let result = Msg::tool_result(call.id.clone(), answer.content);
        if !self.persist_for_next_send(run_id, &result).await {
            return false;
        }
        messages.push(result);
        true
    }

    /// Append one message to the run's durable thread (H3/G1 foundation).
    /// A caller that then pushes the message into the next provider request
    /// must use [`Self::persist_for_next_send`] instead of ignoring this error.
    async fn persist_message(&self, run_id: &str, msg: &Msg) -> Result<(), StoreError> {
        let tool_calls =
            serde_json::to_value(&msg.tool_calls).unwrap_or_else(|_| serde_json::json!([]));
        let message = agent24_store::PendingRunMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls,
            tool_call_id: msg.tool_call_id.clone(),
        };
        let appended = match msg.role.as_str() {
            "assistant" => {
                self.store
                    .append_originated_run_messages(
                        run_id,
                        &[OriginatedRunMessage {
                            message,
                            origin: MessageOrigin::Assistant,
                        }],
                        &now_iso8601(),
                    )
                    .await
            }
            "tool" => {
                self.store
                    .append_originated_run_messages(
                        run_id,
                        &[OriginatedRunMessage {
                            message,
                            origin: MessageOrigin::ToolResult,
                        }],
                        &now_iso8601(),
                    )
                    .await
            }
            _ => {
                self.store
                    .append_run_message(
                        run_id,
                        &message.role,
                        message.content.as_deref(),
                        &message.tool_calls,
                        message.tool_call_id.as_deref(),
                        &now_iso8601(),
                    )
                    .await
            }
        };
        if let Err(err) = appended {
            tracing::warn!("run {run_id}: message thread append failed: {err}");
            return Err(err);
        }
        Ok(())
    }

    /// Persist a message the next provider request would include.
    /// A failed write ends the run so the caller must not push it.
    async fn persist_for_next_send(&self, run_id: &str, msg: &Msg) -> bool {
        if let Err(err) = self.persist_message(run_id, msg).await {
            self.finish_failed(
                run_id,
                "internal",
                &format!("failed to persist run message: {err}"),
            )
            .await;
            return false;
        }
        true
    }

    pub async fn start_run(self: &Arc<Self>, create: RunCreate) -> Result<Run, AgentError> {
        self.start_run_with_schedule(create, None).await
    }

    /// As [`start_run`], but tags the run with the schedule that fired it
    /// (the scheduler uses this so every run traces back to its trigger).
    pub async fn start_run_with_schedule(
        self: &Arc<Self>,
        create: RunCreate,
        schedule_id: Option<String>,
    ) -> Result<Run, AgentError> {
        self.ready_run(
            self.start_run_inner(create, schedule_id, |_| {}, None, None)
                .await?,
        )
    }

    /// As [`start_run`], with `before_execute` called once the run is stored
    /// and before it starts executing, so whatever it records about the run
    /// is there before the run's first step.
    pub async fn start_run_then(
        self: &Arc<Self>,
        create: RunCreate,
        before_execute: impl FnOnce(&Run),
    ) -> Result<Run, AgentError> {
        self.start_run_then_with(create, before_execute, None).await
    }

    /// As [`start_run_then`], with the lookup used to resolve the providers of
    /// the capabilities a skill requires (`create.skill_id`). Without one, a
    /// skill that requires any capability is refused as missing it.
    pub async fn start_run_then_with(
        self: &Arc<Self>,
        create: RunCreate,
        before_execute: impl FnOnce(&Run),
        providers: Option<&dyn SkillProviderSource>,
    ) -> Result<Run, AgentError> {
        self.ready_run(
            self.start_run_inner(create, None, before_execute, None, providers)
                .await?,
        )
    }

    /// Start a run for one inbound event. `origin` is not part of [`RunCreate`].
    ///
    /// The session, the run, and `inbound_runs` commit in one transaction
    /// ([`agent24_store::Store::insert_session_run_for_inbound`]).
    /// `persist_new_run` commits on its own, so this path does not call it.
    /// `execute` tags the user message [`agent24_store::SourceRef::inbound_external`].
    /// A second call for the same event links nothing and starts nothing.
    pub async fn start_run_inbound(
        self: &Arc<Self>,
        create: RunCreate,
        origin: InboundOrigin,
    ) -> Result<InboundRunStart, AgentError> {
        #[cfg(any(test, feature = "test-support"))]
        if self.take_inbound_start_fault() {
            return Err(AgentError::Store(StoreError::Conflict(
                "injected inbound start failure".to_owned(),
            )));
        }
        match self
            .start_run_inner(create, None, |_| {}, Some(origin), None)
            .await?
        {
            RunStart::Ready(run) => Ok(InboundRunStart::Started(run)),
            RunStart::InboundAlreadyLinked => Ok(InboundRunStart::AlreadyLinked),
            RunStart::InboundNotStarting => Ok(InboundRunStart::NotStarting),
        }
    }

    fn ready_run(&self, started: RunStart) -> Result<Run, AgentError> {
        match started {
            RunStart::Ready(run) => Ok(run),
            RunStart::InboundAlreadyLinked => Err(AgentError::Store(StoreError::Conflict(
                "ordinary start linked an inbound event".to_owned(),
            ))),
            RunStart::InboundNotStarting => Err(AgentError::Store(StoreError::Conflict(
                "ordinary start saw an inbound event that is not starting".to_owned(),
            ))),
        }
    }

    async fn start_run_inner(
        self: &Arc<Self>,
        create: RunCreate,
        schedule_id: Option<String>,
        before_execute: impl FnOnce(&Run),
        inbound: Option<InboundOrigin>,
        providers: Option<&dyn SkillProviderSource>,
    ) -> Result<RunStart, AgentError> {
        // Skill admission comes first and writes nothing. Inbound runs,
        // self-wake runs and workspace-bound runs cannot carry a skill.
        let skill_pin = match create.skill_id.as_deref() {
            None => None,
            Some(skill_id) => {
                let reason = if inbound.is_some() {
                    Some("inbound runs cannot carry skill_id")
                } else if schedule_id.is_some() {
                    Some("scheduled runs cannot carry skill_id")
                } else if create.workspace_id.is_some() {
                    Some("workspace-bound runs cannot carry skill_id in v1")
                } else if let Some(session_id) = create.session_id.as_deref()
                    && self
                        .store
                        .get_session(session_id)
                        .await?
                        .is_some_and(|session| session.workspace_id.is_some())
                {
                    Some("workspace-bound sessions cannot carry skill_id in v1")
                } else {
                    None
                };
                if let Some(reason) = reason {
                    return Err(SkillAdmissionError::NotAdmitted { reason }.into());
                }
                let admitted = skill_inject::admit(&self.store, providers, skill_id).await?;
                Some(agent24_store::SkillPinRequest {
                    skill_id: skill_id.to_owned(),
                    expected_body_revision_id: admitted.body_revision_id,
                    providers_json: admitted.providers_json,
                })
            }
        };
        // Inbound creates the session in the same transaction as the run.
        // Checking first would always miss it.
        if inbound.is_none()
            && let Some(session_id) = &create.session_id
            && self.store.get_session(session_id).await?.is_none()
        {
            return Err(AgentError::SessionNotFound(session_id.clone()));
        }

        let created_at = if create.workspace_id.is_some() {
            workspace_now()?
        } else {
            now_iso8601()
        };
        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: create.session_id.clone(),
            workspace_id: create.workspace_id.clone(),
            status: RunStatus::Queued,
            input: RunInput {
                prompt: create.prompt,
                workspace_id: create.workspace_id,
                model_override: create.model_override,
                mode: create.mode,
                knowledge_binding_id: create.knowledge_binding_id,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id,
            created_at,
            started_at: None,
            ended_at: None,
        };
        // Token registered BEFORE the row becomes discoverable — a client
        // racing list_runs+cancel can never observe a token-less live run
        let token = self.shutdown.child_token();
        self.cancels
            .lock()
            .await
            .insert(run.id.clone(), token.clone());
        let persisted = match self
            .persist_created_run(&run, inbound.as_ref(), skill_pin.as_ref())
            .await
        {
            Ok(outcome) => outcome,
            Err(err) => {
                self.cancels.lock().await.remove(&run.id);
                return Err(err);
            }
        };
        match persisted {
            PersistOutcome::Stored => {}
            PersistOutcome::InboundAlreadyLinked => {
                self.cancels.lock().await.remove(&run.id);
                return Ok(RunStart::InboundAlreadyLinked);
            }
            PersistOutcome::InboundNotStarting => {
                self.cancels.lock().await.remove(&run.id);
                return Ok(RunStart::InboundNotStarting);
            }
            PersistOutcome::SkillNotActive => {
                self.cancels.lock().await.remove(&run.id);
                return Err(SkillAdmissionError::NotActive.into());
            }
            PersistOutcome::SkillMoved => {
                self.cancels.lock().await.remove(&run.id);
                return Err(AgentError::Store(StoreError::SkillPointerConflict));
            }
        }
        if let Some(origin) = &inbound {
            // Before spawn, so execute cannot finish (and try to mark done)
            // while the row is still `starting`.
            self.inbound_origins
                .lock()
                .await
                .insert(run.id.clone(), origin.event_id.clone());
            match self.store.mark_inbound_started(&origin.event_id).await {
                Ok(true) => {}
                Ok(false) => tracing::warn!(
                    event_id = %origin.event_id,
                    "inbound mark started matched no starting row"
                ),
                Err(err) => tracing::warn!(
                    event_id = %origin.event_id,
                    error = %err,
                    "inbound mark started failed"
                ),
            }
        }
        before_execute(&run);

        // Supervised execution: execute() runs in its OWN task whose join
        // result is observed. A panic anywhere in the execution path (the
        // ModelProvider trait object is effectively arbitrary code) must
        // still land the run in a terminal state and clean the cancels map —
        // otherwise the run is wedged non-terminal for the process lifetime
        // and cancel_run cannot recover it (review #36).
        let manager = Arc::clone(self);
        let run_id = run.id.clone();
        tokio::spawn(async move {
            let task = tokio::spawn({
                let manager = Arc::clone(&manager);
                let run_id = run_id.clone();
                async move {
                    manager
                        .execute(run_id, token, create.idoris_headroom_confirmed)
                        .await
                }
            });
            if let Err(err) = task.await
                && err.is_panic()
            {
                tracing::error!("run {run_id}: execution task panicked");
                manager
                    .finish_failed(&run_id, "internal", "run execution panicked")
                    .await;
            }
            manager.cancels.lock().await.remove(&run_id);
        });

        Ok(RunStart::Ready(run))
    }

    async fn persist_created_run(
        &self,
        run: &Run,
        inbound: Option<&InboundOrigin>,
        skill_pin: Option<&agent24_store::SkillPinRequest>,
    ) -> Result<PersistOutcome, AgentError> {
        if let Some(pin) = skill_pin {
            // The pin is derived from the pointer in the same transaction as
            // the run row. Admission already refused inbound.
            return Ok(
                match self.store.insert_run_with_skill_pin(run, pin).await? {
                    agent24_store::SkillPinOutcome::Pinned(_) => PersistOutcome::Stored,
                    agent24_store::SkillPinOutcome::NotActive => PersistOutcome::SkillNotActive,
                    agent24_store::SkillPinOutcome::PointerMoved => PersistOutcome::SkillMoved,
                },
            );
        }
        let Some(origin) = inbound else {
            self.persist_new_run(run).await?;
            return Ok(PersistOutcome::Stored);
        };
        let session_id = run.session_id.clone().ok_or_else(|| {
            AgentError::Store(StoreError::Conflict(
                "inbound run requires a session".to_owned(),
            ))
        })?;
        if origin.channel.is_empty() || origin.event_id.is_empty() {
            return Err(AgentError::Store(StoreError::Conflict(
                "inbound origin is empty".to_owned(),
            )));
        }
        let session = agent24_protocol::Session {
            id: session_id,
            title: String::new(),
            channel: origin.channel.clone(),
            workspace_id: None,
            created_at: run.created_at.clone(),
            updated_at: run.created_at.clone(),
        };
        match self
            .store
            .insert_session_run_for_inbound(&session, run, &origin.event_id)
            .await?
        {
            agent24_store::InboundRunLink::Linked => Ok(PersistOutcome::Stored),
            agent24_store::InboundRunLink::AlreadyLinked => {
                Ok(PersistOutcome::InboundAlreadyLinked)
            }
            agent24_store::InboundRunLink::NotStarting => Ok(PersistOutcome::InboundNotStarting),
        }
    }

    /// Idempotent cancellation: any state, any time. Terminal runs are
    /// returned unchanged (202 semantics at the REST layer).
    pub async fn cancel_run(&self, id: &str) -> Result<Run, AgentError> {
        let run = self
            .store
            .get_run(id)
            .await?
            .ok_or_else(|| AgentError::Store(StoreError::NotFound(format!("run {id}"))))?;
        if agent24_core::run_is_terminal(run.status) {
            return Ok(run);
        }
        // Executor-owned cancellation when a token exists; token-less
        // non-terminal runs (e.g. persisted rows from a previous daemon
        // process) are landed terminal HERE — "cancel works in any state"
        // must never leave a run non-terminal forever (review C2).
        let token = self.cancels.lock().await.get(id).cloned();
        match token {
            Some(token) => {
                token.cancel();
                // The executor lands the transition asynchronously (<1s)
            }
            None => {
                if run.workspace_id.is_some() {
                    let ended_at = workspace_now()?;
                    let ended_at = WorkspaceInstant::parse(&ended_at)?;
                    if matches!(
                        self.store
                            .cancel_workspace_run_recovery(id, &ended_at)
                            .await?,
                        RunTerminalTransition::Applied(_)
                    ) {
                        self.sink.emit(EventBody::RunCancelled(RunCancelledPayload {
                            run_id: id.to_owned(),
                        }));
                    }
                } else {
                    self.finish_cancelled(id).await;
                }
            }
        }
        self.store
            .get_run(id)
            .await?
            .ok_or_else(|| AgentError::Store(StoreError::NotFound(format!("run {id}"))))
    }

    /// Recover runs that are still parked after their durable approval row was
    /// timed out by the daemon sweep. Live executors keep their existing
    /// in-memory timeout behavior; only token-less parked runs are landed
    /// cancelled here. A failed cancellation leaves the durable candidate in
    /// place, so the next scan retries it.
    pub async fn recover_timed_out_approval_runs(&self) -> Result<u64, AgentError> {
        let run_ids = self.store.timed_out_approval_recovery_run_ids().await?;
        let mut cancelled = 0u64;
        for run_id in run_ids {
            if self.cancels.lock().await.contains_key(&run_id) {
                continue;
            }
            let Some(run) = self.store.get_run(&run_id).await? else {
                continue;
            };
            if run.status != RunStatus::AwaitingApproval {
                continue;
            }

            let result = if run.workspace_id.is_some() {
                let ended_at = workspace_now()
                    .and_then(|raw| WorkspaceInstant::parse(&raw).map_err(AgentError::from));
                match ended_at {
                    Ok(ended_at) => self
                        .store
                        .cancel_workspace_run_recovery(&run_id, &ended_at)
                        .await
                        .map_err(AgentError::from)
                        .map(|outcome| match outcome {
                            RunTerminalTransition::Applied(_) => true,
                            RunTerminalTransition::Conflict => false,
                        }),
                    Err(err) => Err(err),
                }
            } else {
                let ended_at = now_iso8601();
                self.store
                    .transition_run(
                        &run_id,
                        RunStatus::Cancelled,
                        RunPatch {
                            ended_at: Some(ended_at),
                            ..Default::default()
                        },
                    )
                    .await
                    .map(|_| true)
                    .map_err(AgentError::from)
            };

            match result {
                Ok(true) => {
                    self.sink.emit(EventBody::RunCancelled(RunCancelledPayload {
                        run_id: run_id.clone(),
                    }));
                    // The run never returns to `finish_cancelled`. Without this,
                    // a restarted approval timeout leaves the inbound row in
                    // `started` and the sender's in-flight slot stays taken.
                    self.note_inbound_terminal(&run_id).await;
                    cancelled += 1;
                }
                Ok(false) => {
                    tracing::debug!("run {run_id}: timed-out approval recovery already settled");
                }
                Err(err) => {
                    tracing::error!(
                        "run {run_id}: timed-out approval recovery cancel failed; will retry: {err}"
                    );
                }
            }
        }
        Ok(cancelled)
    }

    /// Startup durable-resume sweep (H3): decide every approval left `pending` by
    /// a previous process, in place of the old abort-everything sweep.
    ///
    /// A restorable approval is RE-BROADCAST (so a freshly-connected inbox shows
    /// the queued item again) and kept pending; its run stays parked until a
    /// human answers, when `resume_run` continues it. A non-restorable one (tool
    /// gone, payload drift, past the resume TTL, or a vanished run) is aborted
    /// fail-closed — its now-approval-less run is then cancelled by the orphan
    /// sweep, which is why THIS must run first. Returns `(restored, aborted)`.
    pub async fn restore_pending_approvals(&self) -> Result<(u64, u64), AgentError> {
        let pending = self
            .store
            .list_approvals(Some(ApprovalStatus::Pending))
            .await?;
        let cutoff = agent24_core::util::iso8601_before(RESUME_TTL);
        let (mut restored, mut aborted) = (0u64, 0u64);
        for approval in pending {
            let thread = self.store.list_run_messages(&approval.run_id).await?;
            let run = match self.store.get_run(&approval.run_id).await? {
                Some(run) => run,
                // The run row is gone — there is nothing to resume.
                None => {
                    self.abort_one_approval(&approval).await?;
                    aborted += 1;
                    continue;
                }
            };
            // NB: the suspension point is derived from the thread's provider
            // ids, never from the approval's `tool_call_id`; that id and the
            // tool its `tool_calls` row names only confirm the approval is the
            // parked call's own (with the payload).
            let approved_tool = self.approved_tool(&approval).await?;
            match crate::resume::assess_restore(
                &crate::resume::ApprovedCall {
                    payload: &approval.payload,
                    tool_call_id: &approval.tool_call_id,
                    tool: approved_tool.as_deref(),
                },
                &approval.created_at,
                run.status,
                &thread,
                |name| self.tools.tool_risk_class(name).is_some(),
                &cutoff,
            ) {
                crate::resume::RestoreDecision::Restore { tool_call_id } => {
                    if run.workspace_id.is_some()
                        && let Err(err) = self.tool_context_for(&run, tool_call_id).await
                    {
                        if restore_authority_error_must_propagate(&err) {
                            return Err(err);
                        }
                        tracing::warn!(
                            "restore sweep: aborting approval {} — workspace authority unavailable: {err}",
                            approval.id
                        );
                        self.abort_one_approval(&approval).await?;
                        aborted += 1;
                        continue;
                    }
                    // Re-announce the queued item; the row stays pending.
                    self.sink
                        .emit(EventBody::ApprovalRequired(Box::new(approval.clone())));
                    restored += 1;
                }
                crate::resume::RestoreDecision::Abort { reason } => {
                    tracing::warn!(
                        "restore sweep: aborting approval {} — {reason}",
                        approval.id
                    );
                    self.abort_one_approval(&approval).await?;
                    aborted += 1;
                }
            }
        }
        Ok((restored, aborted))
    }

    async fn abort_one_approval(&self, approval: &Approval) -> Result<(), AgentError> {
        self.store
            .resolve_approval(&approval.id, ApprovalStatus::Aborted, None, now_iso8601())
            .await?;
        Ok(())
    }

    /// Resume a run parked awaiting approval whose in-memory task is gone — the
    /// durable crash-recovery entry point (H3). Called when a human answers an
    /// approval that a restart re-broadcast: the decision is already on the row,
    /// so this reconstructs the conversation from the persisted thread, settles
    /// the parked tool call from that decision, and continues the loop.
    ///
    /// Idempotent and supervised, like [`start_run_with_schedule`]: a run not in
    /// `awaiting_approval`, or one that already has a live task, is a no-op (a
    /// stale or duplicate trigger must never re-drive); the continuation runs in
    /// its own task that lands the run terminal on panic and registers a cancel
    /// token so `cancel_run` works.
    pub async fn resume_run(
        self: &Arc<Self>,
        run_id: String,
        approval_id: String,
    ) -> Result<(), AgentError> {
        let run = self
            .store
            .get_run(&run_id)
            .await?
            .ok_or_else(|| AgentError::Store(StoreError::NotFound(format!("run {run_id}"))))?;
        // Only a parked run resumes; anything else is a stale/duplicate trigger.
        if run.status != RunStatus::AwaitingApproval {
            return Ok(());
        }
        let approval = self
            .store
            .get_approval(&approval_id)
            .await?
            .ok_or_else(|| {
                AgentError::Store(StoreError::NotFound(format!("approval {approval_id}")))
            })?;
        if approval.run_id != run.id {
            return Err(AgentError::Store(StoreError::Conflict(
                "approval does not belong to run".to_owned(),
            )));
        }
        // M1-T10 review M2: resume reconstructs from this PERSISTED thread
        // only — it never re-calls `memory.recall` or re-checks the pause
        // switch. The recalled-context snapshot and the paused-write notice
        // (`execute`, both persisted at run start) are already in here if
        // this run had either; the personal-memory pause switch therefore
        // only takes effect for a NEWLY STARTED run, never for one being
        // resumed after an approval wait — same rule a schema/doc change
        // under this switch's own PUT states.
        let thread = self.store.list_run_messages(&run_id).await?;

        // Register the cancel token BEFORE spawning, and refuse if one already
        // exists: a parked run has no live task, so a present token means another
        // resume is already in flight — a duplicate trigger must not spawn a second.
        {
            let mut cancels = self.cancels.lock().await;
            if cancels.contains_key(&run_id) {
                return Ok(());
            }
            cancels.insert(run_id.clone(), self.shutdown.child_token());
        }
        let token = self
            .cancels
            .lock()
            .await
            .get(&run_id)
            .cloned()
            .unwrap_or_else(|| self.shutdown.child_token());

        let manager = Arc::clone(self);
        tokio::spawn(async move {
            let rid = run.id.clone();
            let task = tokio::spawn({
                let manager = Arc::clone(&manager);
                async move { manager.drive_resume(run, thread, approval, token).await }
            });
            if let Err(err) = task.await
                && err.is_panic()
            {
                tracing::error!("run {rid}: resume task panicked");
                manager
                    .finish_failed(&rid, "internal", "run resume panicked")
                    .await;
            }
            manager.cancels.lock().await.remove(&rid);
        });
        Ok(())
    }

    /// The tool `approval` was asked for: the one its `tool_calls` row names,
    /// written before the call parked. `None` when there is no such row, as
    /// for an approval asked again while a resume settled the call.
    async fn approved_tool(&self, approval: &Approval) -> Result<Option<String>, AgentError> {
        Ok(self
            .store
            .list_tool_calls(&approval.run_id)
            .await?
            .into_iter()
            .find(|call| call.id == approval.tool_call_id)
            .map(|call| call.tool))
    }

    /// Body of a resume: re-validate, reconstruct, settle the parked turn's
    /// unanswered calls, then hand off to the shared loop.
    async fn drive_resume(
        &self,
        run: Run,
        thread: Vec<RunMessage>,
        approval: Approval,
        cancel: CancellationToken,
    ) {
        let run_id = run.id.clone();

        // 1+2. Re-validate off the persisted thread and locate the suspension
        // point. The point is a PROVIDER id derived from the thread (NOT the
        // approval's `tool_call_id`); the approval is tied to that call by
        // payload equality and call identity inside assess_restore. Done
        // BEFORE any state change, so an unrestorable run
        // never emits a spurious RunStarted→Cancelled pair.
        let cutoff = agent24_core::util::iso8601_before(RESUME_TTL);
        let approved_tool = match self.approved_tool(&approval).await {
            Ok(tool) => tool,
            Err(err) => {
                tracing::warn!("run {run_id}: resume aborted — approval's call unreadable: {err}");
                self.finish_cancelled(&run_id).await;
                return;
            }
        };
        let tool_call_id = match crate::resume::assess_restore(
            &crate::resume::ApprovedCall {
                payload: &approval.payload,
                tool_call_id: &approval.tool_call_id,
                tool: approved_tool.as_deref(),
            },
            &approval.created_at,
            run.status,
            &thread,
            |name| self.tools.tool_risk_class(name).is_some(),
            &cutoff,
        ) {
            crate::resume::RestoreDecision::Restore { tool_call_id } => tool_call_id,
            crate::resume::RestoreDecision::Abort { reason } => {
                tracing::warn!("run {run_id}: resume aborted — {reason}");
                self.finish_cancelled(&run_id).await;
                return;
            }
        };

        // A bound run must still have fresh workspace authority before restart
        // recovery may make it live again. The parked approval may have waited
        // long enough for its workspace/lease authority to become unavailable.
        if run.workspace_id.is_some()
            && let Err(err) = self.tool_context_for(&run, tool_call_id.clone()).await
        {
            tracing::warn!("run {run_id}: resume workspace authority unavailable — {err}");
            self.cancel_workspace_resume_recovery(&run_id).await;
            return;
        }

        // Missing or unreadable origins must not settle a parked tool or
        // dispatch a sibling such as explore, and neither may a run started
        // before tool results were tagged. Failed is legal only from Running,
        // so the refusal enters Running without RunStarted.
        if !self.persisted_origins_allow_send(&run_id).await
            || !self.resume_sources_proven(&run).await
        {
            if let Err(err) = self
                .store
                .transition_run(&run_id, RunStatus::Running, RunPatch::default())
                .await
            {
                tracing::error!(
                    "run {run_id}: resume source refusal could not enter running: {err}"
                );
                self.release_inbound_after_abandoned_start(&run_id).await;
                return;
            }
            self.finish_failed(&run_id, SOURCE_UNPROVEN_CODE, SOURCE_UNPROVEN_MESSAGE)
                .await;
            return;
        }
        // A pinned skill must still have its label. The pointer is not read
        // and nothing is reclassified: a missing label ends the run.
        if self.skill_label_missing(&run_id).await {
            if let Err(err) = self
                .store
                .transition_run(&run_id, RunStatus::Running, RunPatch::default())
                .await
            {
                tracing::error!("run {run_id}: skill label refusal could not enter running: {err}");
                self.release_inbound_after_abandoned_start(&run_id).await;
                return;
            }
            self.finish_failed(&run_id, "source_policy_unavailable", SKILL_LABEL_MESSAGE)
                .await;
            return;
        }

        // 3. Reconstruct the conversation and return the run to Running.
        // M1-T07.1 M4 (review #674 round 2 ③): re-validating the recall
        // block used to happen ONLY here, which misses the common case of
        // an approval answered while the daemon is still running (the task
        // stays live and never goes through `drive_resume` at all — see
        // the in-memory `ApprovalBroker` wake-up path). That check now
        // lives in `run_loop` itself, re-run fresh every iteration
        // regardless of how this call got here, so this reconstruction
        // stays a plain, unmodified replay of the persisted thread.
        let mut messages = thread_to_messages(&thread);
        if let Err(err) = self
            .store
            .transition_run(&run_id, RunStatus::Running, RunPatch::default())
            .await
        {
            tracing::error!("run {run_id}: resume transition failed: {err}");
            self.release_inbound_after_abandoned_start(&run_id).await;
            return;
        }
        self.sink.emit(EventBody::RunStarted(RunStartedPayload {
            run_id: run_id.clone(),
            session_id: run.session_id.clone(),
            schedule_id: run.schedule_id.clone(),
        }));

        // 4. Answer the parked turn's still-unanswered calls, in order. The first
        // is the one this approval decided (settle from the row, no second ask);
        // any calls after it in the same fan-out were never approved, so they go
        // through the normal gate via run_tool_call (which may itself re-park).
        // Only one call is settled: a provider can repeat an id within a turn,
        // and a second call sharing it would otherwise run with no row of its
        // own, so the peer view could not see it (K1-6b.5f-3, #1349 F1).
        let Some(unanswered) = unanswered_calls_of_last_turn(&messages) else {
            tracing::warn!("run {run_id}: no unanswered parked turn on resume; aborting");
            self.finish_cancelled(&run_id).await;
            return;
        };
        let mut settled = false;
        for call in unanswered {
            if cancel.is_cancelled() {
                self.finish_cancelled(&run_id).await;
                return;
            }
            let answer = if !settled && call.id == tool_call_id {
                settled = true;
                if call.name.trim() == PROPOSE_PLAN {
                    // H8: the parked call is a plan approval (the only thing a
                    // plan-mode run can park on — read tools never gate, and no
                    // other tool is advertised until a plan is approved). resume
                    // runs post-decision (H3), so the row is resolved: approve →
                    // continue with the full set; anything else → the run ends
                    // having done nothing but read.
                    if !matches!(approval.status, ApprovalStatus::Approved) {
                        let content = "Plan rejected.".to_owned();
                        let result = Msg::tool_result(call.id.clone(), content.clone());
                        let _ = self.persist_message(&run_id, &result).await;
                        self.finish_completed(&run_id, &content, run.usage.clone())
                            .await;
                        return;
                    }
                    ToolAnswer::fixed(
                        "Plan approved. The full tool set is now available — carry out the plan."
                            .to_owned(),
                    )
                } else {
                    match self
                        .settle_parked_call(&run, &approval, &call, &cancel)
                        .await
                    {
                        Ok(answer) => answer,
                        Err(ParkedCallStop::CancelRun) => {
                            self.finish_cancelled(&run_id).await;
                            return;
                        }
                        Err(ParkedCallStop::RecoveryStopped) => return,
                    }
                }
            } else {
                match self.run_tool_call(&run, &call, &cancel, true).await {
                    Ok(answer) => answer,
                    Err(ParkedCallStop::CancelRun) => {
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
                    Err(ParkedCallStop::RecoveryStopped) => return,
                }
            };
            if !self
                .answer_tool_call(&run, &mut messages, &call, answer)
                .await
            {
                return;
            }
        }

        // 5. Continue the shared loop for subsequent model iterations. Always
        // out of plan mode: the only thing a plan-mode run can park on is its
        // plan approval, and reaching here means that was approved (a rejection
        // returned above), so the full tool set is unlocked (H8).
        self.run_loop(run, messages, false, cancel, false).await;
    }

    /// Settle the ONE parked call whose approval is already decided (H3): apply
    /// the recorded decision rather than asking again. Returns the tool-result
    /// content to feed the model, or `Err(())` when the decision was to abort the
    /// whole run.
    ///
    async fn settle_parked_call(
        &self,
        run: &Run,
        approval: &Approval,
        call: &ToolCallRequest,
        cancel: &CancellationToken,
    ) -> Result<ToolAnswer, ParkedCallStop> {
        let decision = approval.decision.clone().unwrap_or_else(|| Decision {
            kind: "deny".to_owned(),
            reason: Some("no decision was recorded".to_owned()),
            extra: serde_json::Map::new(),
        });
        if !restored_decision_is_consistent(approval, &decision) {
            tracing::warn!(
                "approval {}: restored decision/status provenance is inconsistent",
                approval.id
            );
            return Err(ParkedCallStop::CancelRun);
        }
        match decision.kind.as_str() {
            "approve" | "approve_for_session" | "approve_for_target" => {
                let ctx = match self.tool_context_for(run, call.id.clone()).await {
                    Ok(ctx) => ctx,
                    Err(err) => {
                        if run.workspace_id.is_some() {
                            tracing::warn!(
                                "run {}: settled resume workspace authority unavailable — {err}",
                                run.id
                            );
                            self.cancel_workspace_resume_recovery(&run.id).await;
                            return Err(ParkedCallStop::RecoveryStopped);
                        }
                        return Ok(ToolAnswer::fixed(format!(
                            "tool error: workspace authority unavailable: {err}"
                        )));
                    }
                };
                self.tools
                    .settle_resumed(
                        &call.name,
                        &ctx,
                        &approval.payload,
                        &approval.id,
                        &decision,
                        approval.standing_target.as_deref(),
                    )
                    .await;
                // Run EXACTLY what was approved — the payload the human signed
                // off (assess_restore already proved it equals the thread's
                // rebuilt input), so using it directly makes "run A, not B"
                // structural rather than a re-parse we must trust.
                let outcome = match self
                    .tools
                    .execute_preapproved(&call.name, &ctx, &approval.payload, cancel)
                    .await
                {
                    // The approval names no grant (K1-6a-G §2.3): ask again,
                    // with the run shown as awaiting that answer, as a fresh
                    // call would be.
                    Err(ToolError::ApprovalStale) => {
                        self.reask_parked_call(&run.id, call, &ctx, &approval.payload, cancel)
                            .await
                    }
                    outcome => outcome,
                };
                // As in `run_tool_call`: only `Ok` and `Failed` carry what the
                // tool produced.
                let tag = || Some(self.tool_result_tag(&call.name, &approval.tool_call_id));
                match outcome {
                    Ok(content) => Ok(ToolAnswer {
                        content,
                        source: tag(),
                    }),
                    // The whole-run abort choice is honoured even on resume.
                    Err(ToolError::AbortRun(_)) | Err(ToolError::Cancelled) => {
                        Err(ParkedCallStop::CancelRun)
                    }
                    Err(err @ ToolError::Failed(_)) => Ok(ToolAnswer {
                        content: format!("tool error: {err}"),
                        source: tag(),
                    }),
                    Err(err) => Ok(ToolAnswer::fixed(format!("tool error: {err}"))),
                }
            }
            "deny" => Ok(ToolAnswer::fixed(format!(
                "denied by user: {}",
                decision.reason.as_deref().unwrap_or("no reason given")
            ))),
            // "abort" or anything unexpected → cancel the run (fail-closed).
            _ => Err(ParkedCallStop::CancelRun),
        }
    }

    async fn reask_parked_call(
        &self,
        run_id: &str,
        call: &ToolCallRequest,
        ctx: &ToolContext,
        input: &serde_json::Map<String, serde_json::Value>,
        cancel: &CancellationToken,
    ) -> Result<String, ToolError> {
        if let Err(err) = self
            .store
            .transition_run(run_id, RunStatus::AwaitingApproval, RunPatch::default())
            .await
        {
            tracing::error!("run {run_id}: awaiting_approval transition failed: {err}");
        }
        let outcome = self.tools.dispatch(&call.name, ctx, input, cancel).await;
        if !matches!(
            outcome,
            Err(ToolError::AbortRun(_)) | Err(ToolError::Cancelled)
        ) && let Err(err) = self
            .store
            .transition_run(run_id, RunStatus::Running, RunPatch::default())
            .await
        {
            tracing::error!("run {run_id}: back-to-running transition failed: {err}");
        }
        outcome
    }

    async fn execute(
        &self,
        run_id: String,
        cancel: CancellationToken,
        idoris_headroom_confirmed: bool,
    ) {
        // Taken once, before any return, so a cancelled start does not leave
        // the id in the map. The map is only a hint: `execute` also runs for a
        // path that never called `start_run_inner`. `inbound_runs` and a
        // non-Ordinary user-input tag are the durable signal. A lookup error
        // is external, so the tag cannot fall through to Ordinary.
        let mapped_inbound = self.inbound_origins.lock().await.remove(&run_id).is_some();
        let inbound_external = mapped_inbound || self.persisted_external_prompt(&run_id).await;
        // Cancelled before starting? queued → cancelled directly.
        if cancel.is_cancelled() {
            self.finish_cancelled(&run_id).await;
            return;
        }

        let started_at = now_iso8601();
        let run = match self
            .store
            .transition_run(
                &run_id,
                RunStatus::Running,
                RunPatch {
                    started_at: Some(started_at),
                    ..Default::default()
                },
            )
            .await
        {
            Ok(run) => run,
            Err(err) => {
                tracing::error!("run {run_id}: failed to start: {err}");
                self.release_inbound_after_abandoned_start(&run_id).await;
                return;
            }
        };
        self.sink.emit(EventBody::RunStarted(RunStartedPayload {
            run_id: run_id.clone(),
            session_id: run.session_id.clone(),
            schedule_id: run.schedule_id.clone(),
        }));

        // Assertion recall is fresh-run context. Keep the audit ids tied to the
        // exact facts that made it into this message; a recall failure must not
        // prevent an otherwise valid run from reaching its provider.
        let recall = if let Some(memory) = self.memory.as_ref() {
            let load = memory.recall(&run.input.prompt);
            tokio::select! {
                result = load => match result {
                    Ok(recall) => recall,
                    Err(err) => {
                        tracing::warn!(run_id = %run_id, error = %err, "memory recall failed");
                        None
                    }
                },
                () = cancel.cancelled() => {
                    self.finish_cancelled(&run_id).await;
                    return;
                }
            }
        } else {
            None
        };

        // M1-T10 review H1: at run start, if this prompt is an explicit
        // "记住……" and personal memory is currently paused, tell the MODEL
        // so it does not go on to claim it remembered something it did not
        // — `retain::persist`'s own early check (the actual write gate)
        // makes the identical decision later; this surfaces that decision
        // to the model NOW instead of leaving it to discover the absence on
        // its own. A pause-check failure is treated as "enabled" (fail
        // open on the NOTICE only — the real write gate inside `persist`
        // fails closed independently; this is belt-and-braces, not load-
        // bearing).
        // An inbound "记住" is not the account owner asking. The pause notice
        // would tell the model to report that the user's memory was skipped.
        let write_skipped = if inbound_external {
            false
        } else {
            match self.memory.as_ref() {
                Some(memory) if retain::explicit_remember(&run.input.prompt).is_some() => {
                    match memory.kv().memory_enabled(memory.owner()).await {
                        Ok(enabled) => !enabled,
                        Err(err) => {
                            tracing::warn!(run_id = %run_id, error = %err, "memory pause check failed; assuming enabled");
                            false
                        }
                    }
                }
                _ => false,
            }
        };

        // D1: a session's prior (compacted) context precedes this turn, so a
        // session actually remembers. Empty when memory is off or session-less.
        // A cancel while waiting on a concurrent run's session lock ends the run
        // here rather than proceeding without its own context.
        let prior_context = match self
            .session_context(run.session_id.as_deref(), &cancel)
            .await
        {
            SessionContext::Ready(context) => context,
            SessionContext::Cancelled => {
                self.finish_cancelled(&run_id).await;
                return;
            }
            SessionContext::ImportIncomplete(failure) => {
                self.finish_failed(
                    &run_id,
                    failure.wire_code_str(),
                    "this session's earlier history could not be imported; nothing was changed; if retrying still fails, the session's old records may be corrupted",
                )
                .await;
                return;
            }
        };
        // M1-T07.1: the FULL first-call input — recall block, then the
        // session's prior (compacted) context, then this run's prompt — is the
        // immutable snapshot a restart-time resume must reproduce. It is
        // persisted to the durable run thread BEFORE the model is ever called,
        // in order, and fail-closed: if any message fails to persist, the run
        // never reaches the provider, because `drive_resume` only has
        // `thread_to_messages(&thread)` to rebuild from — a partially
        // persisted snapshot would silently resume with less context than the
        // first call saw. M3: the write itself is now ONE transaction
        // (`append_snapshot_run_messages`), not N independent single-row appends —
        // a mid-batch failure must leave NOTHING behind, not an unpredictable
        // prefix, which is what fail-closed actually requires.
        let pending_of = |msg: &Msg| agent24_store::PendingRunMessage {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: serde_json::to_value(&msg.tool_calls)
                .unwrap_or_else(|_| serde_json::json!([])),
            tool_call_id: msg.tool_call_id.clone(),
        };
        let originated = |msg: &Msg, origin: MessageOrigin| {
            SnapshotRunMessage::Originated(OriginatedRunMessage {
                message: pending_of(msg),
                origin,
            })
        };
        let prior_context_len = prior_context.len();
        let mut snapshot = Vec::with_capacity(prior_context_len + 3);
        let mut rows = Vec::with_capacity(prior_context_len + 3);
        let mut recalled_ids = None;
        let mut recalled_tags = Vec::new();
        if let Some((recalled, hits)) = recall {
            let ids: Vec<String> = hits.iter().map(|hit| hit.id.clone()).collect();
            let recall_seq = snapshot.len() as i64;
            recalled_tags = hits
                .iter()
                .map(|hit| {
                    (
                        recall_seq,
                        agent24_store::SourceRef::recalled_memory(
                            &hit.id,
                            hit.classification,
                            now_iso8601(),
                        ),
                    )
                })
                .collect();
            let encoded = match canonical_recall_ref(&ids) {
                Ok(encoded) => encoded,
                Err(err) => {
                    self.finish_failed(
                        &run_id,
                        "internal",
                        &format!("failed to encode recall ref: {err}"),
                    )
                    .await;
                    return;
                }
            };
            rows.push(originated(
                &recalled,
                MessageOrigin::HostRecall {
                    recall_ref: Some(encoded),
                },
            ));
            snapshot.push(recalled);
            recalled_ids = Some(ids);
        }
        if write_skipped {
            // M1-T10 review H1/M2: part of the SAME atomic snapshot as the
            // recalled block above, for the same reason — an approval
            // resume (M2) must replay exactly what THIS run's first call
            // saw, not re-decide anything against whatever the pause switch
            // says by the time it resumes.
            let notice = Msg::system(retain::PAUSED_WRITE_NOTICE);
            rows.push(SnapshotRunMessage::PauseNotice(pending_of(&notice)));
            snapshot.push(notice);
        }
        // SL-5 (plan §4.7): the skill segment, from the revision pinned when
        // the run was admitted, after the pause notice and before the session
        // history. A run without a pin row used no skill. A pin that cannot be
        // read or does not match its revision ends the run before the model.
        let mut skill_tag = None;
        match self.store.get_run_skill_pin(&run_id).await {
            Ok(None) => {}
            Ok(Some(pinned)) => match skill_inject::render(&pinned) {
                Ok(injection) => {
                    let message = Msg::system(injection.segment.clone());
                    let seq = snapshot.len() as i64;
                    rows.push(originated(&message, MessageOrigin::SkillText));
                    snapshot.push(message);
                    // The label is written with the snapshot, in one
                    // transaction, and before the user-input label below.
                    let tag = agent24_store::SourceRef::skill_text(
                        &injection.skill_id,
                        injection.origin,
                        &injection.content_digest,
                        now_iso8601(),
                    );
                    skill_tag = Some((seq, tag));
                }
                Err(detail) => {
                    self.finish_failed(&run_id, "skill_pin_invalid", &detail)
                        .await;
                    return;
                }
            },
            Err(err) => {
                self.finish_failed(
                    &run_id,
                    "skill_pin_invalid",
                    &format!("failed to read the run's skill pin: {err}"),
                )
                .await;
                return;
            }
        }
        // M3 growth control: the seq range `prior_context` will occupy in
        // THIS run's own `run_messages`, so it can be reclaimed once the run
        // is confirmed terminal (see the cleanup call after `run_loop`
        // below) — those rows are a pure duplicate of what `SessionLog`
        // already holds, not unique to this run.
        let prior_context_range = (!prior_context.is_empty()).then(|| {
            let start = snapshot.len() as i64;
            (start, start + prior_context_len as i64 - 1)
        });
        for msg in prior_context {
            rows.push(originated(&msg, MessageOrigin::SessionHistory));
            snapshot.push(msg);
        }
        let prompt = Msg::user(run.input.prompt.clone());
        rows.push(originated(&prompt, MessageOrigin::UserPrompt));
        snapshot.push(prompt);
        // K1-6b.1 (ADR-K1-02 §6): the seq this run's own user-input message
        // will be assigned by the batch append below. Valid because THIS
        // call is always the run's first `append_snapshot_run_messages` (a fresh
        // run's `MAX(seq)` starts at -1), so each row's assigned seq equals
        // its index in `snapshot`/`rows` — the user message is pushed
        // last, so its seq is `snapshot.len() - 1`.
        let user_msg_seq = (snapshot.len() - 1) as i64;
        let skill_tags: Vec<(i64, &agent24_store::SourceRef)> = skill_tag
            .as_ref()
            .map(|(seq, tag)| (*seq, tag))
            .into_iter()
            .collect();
        if let Err(err) = self
            .store
            .append_snapshot_run_messages_tagged(&run_id, &rows, &skill_tags, &now_iso8601())
            .await
        {
            // With a skill, the snapshot and the skill label are one write:
            // a failure is a label failure as far as the run is concerned.
            let (code, what) = if skill_tag.is_some() {
                (
                    "source_policy_unavailable",
                    "run input snapshot and skill source policy",
                )
            } else {
                ("memory_snapshot_persist_failed", "run input snapshot")
            };
            self.finish_failed(&run_id, code, &format!("failed to persist {what}: {err}"))
                .await;
            return;
        }
        // K1-6b.1 (ADR-K1-02 §6, §2.1): tag the run's user input at entry.
        // Inbound text is Unknown for the whole prompt. Ordinary runs stay
        // user_input / Ordinary. Resume does not call execute, so it does not
        // retag. `inbound_external` is the map hint or the durable
        // `inbound_runs` / non-Ordinary user-input signal, so a caller that
        // reaches here without `start_run_inner` does not land Ordinary.
        let user_source_tag = if inbound_external {
            agent24_store::SourceRef::inbound_external(&run_id, now_iso8601())
        } else {
            agent24_store::SourceRef::user_input(&run_id, now_iso8601())
        };
        if let Err(err) = self
            .store
            .tag_run_source(&run_id, user_msg_seq, &user_source_tag, &now_iso8601())
            .await
        {
            self.finish_failed(
                &run_id,
                "source_policy_unavailable",
                &format!("failed to persist run source policy: {err}"),
            )
            .await;
            return;
        }
        // K1-6b.4b (ADR-K1-02 §2.1.1 item 2): each recalled fact is a source
        // of this run with the classification stored with it, so a
        // non-Ordinary recall restricts the run and anything it remembers.
        for (seq, tag) in &recalled_tags {
            if let Err(err) = self
                .store
                .tag_run_source(&run_id, *seq, tag, &now_iso8601())
                .await
            {
                self.finish_failed(
                    &run_id,
                    "source_policy_unavailable",
                    &format!("failed to persist recalled memory source policy: {err}"),
                )
                .await;
                return;
            }
        }
        // K1-6b.4d (ADR-K1-02 §2.2): a run started by a kernel-created
        // schedule (a self-wake) carries the classification recorded when it
        // was created, re-read on every firing. A schedule the user created
        // has none. A failed lookup is not safe to re-validate: fail before
        // the model call.
        if let Some(schedule_id) = run.schedule_id.as_deref() {
            let origin = match self.store.schedule_origin_classification(schedule_id).await {
                Ok(origin) => origin,
                Err(err) => {
                    self.finish_failed(
                        &run_id,
                        "source_policy_unavailable",
                        &format!("failed to read schedule source origin: {err}"),
                    )
                    .await;
                    return;
                }
            };
            if let Some(classification) = origin {
                let tag = agent24_store::SourceRef::schedule_origin(
                    schedule_id,
                    classification,
                    now_iso8601(),
                );
                if let Err(err) = self
                    .store
                    .tag_run_source(&run_id, user_msg_seq, &tag, &now_iso8601())
                    .await
                {
                    self.finish_failed(
                        &run_id,
                        "source_policy_unavailable",
                        &format!("failed to persist schedule source policy: {err}"),
                    )
                    .await;
                    return;
                }
            }
        }
        // K1-6b.4c (ADR-K1-02 §2.2): earlier turns of the session are a
        // source of this run. Merge this run's own classification into the
        // session floor before the model call and tag the run with the
        // result, so what the run remembers and which tools it may use both
        // follow the strictest turn of its session. Without configured memory
        // the floor is not kept: `session_context` loads nothing and nothing
        // is recalled or remembered, so no earlier turn reaches the model.
        if let (Some(_), Some(sid)) = (self.memory.as_ref(), run.session_id.as_deref()) {
            // This run's own classification, read back from the tags just
            // written. A failed read must fail the run rather than fold to
            // `Unknown`: a restricted recall would otherwise enter the floor
            // one step looser than it is.
            let floor = match self.store.run_policy_snapshot(&run_id).await {
                Ok(snapshot) => {
                    let own = agent24_store::strictest_classification(
                        snapshot.sources.iter().map(|source| source.classification),
                    );
                    self.store
                        .merge_session_floor(
                            sid,
                            own,
                            prior_context_range.is_some(),
                            &now_iso8601(),
                        )
                        .await
                }
                Err(err) => Err(err),
            };
            let floor = match floor {
                Ok(floor) => floor,
                Err(err) => {
                    self.finish_failed(
                        &run_id,
                        "source_policy_unavailable",
                        &format!("failed to read or persist session source floor: {err}"),
                    )
                    .await;
                    return;
                }
            };
            let seq = prior_context_range.map_or(user_msg_seq, |(start, _)| start);
            let tag = agent24_store::SourceRef::session_floor(sid, floor, now_iso8601());
            if let Err(err) = self
                .store
                .tag_run_source(&run_id, seq, &tag, &now_iso8601())
                .await
            {
                self.finish_failed(
                    &run_id,
                    "source_policy_unavailable",
                    &format!("failed to persist session source policy: {err}"),
                )
                .await;
                return;
            }
            // K1-6b.5f-2: tool results earlier turns took in, one source per
            // origin. They stay out of the floor merged above, so a module
            // can still be sent back what it returned itself.
            let inherited = match self.store.session_tool_result_origins(sid).await {
                Ok(origins) => origins,
                Err(err) => {
                    self.finish_failed(
                        &run_id,
                        "source_policy_unavailable",
                        &format!("failed to read session tool result origins: {err}"),
                    )
                    .await;
                    return;
                }
            };
            for origin in inherited {
                let tag =
                    agent24_store::SourceRef::session_tool_result(&origin, sid, now_iso8601());
                if let Err(err) = self
                    .store
                    .tag_run_source(&run_id, seq, &tag, &now_iso8601())
                    .await
                {
                    self.finish_failed(
                        &run_id,
                        "source_policy_unavailable",
                        &format!("failed to persist session source policy: {err}"),
                    )
                    .await;
                    return;
                }
            }
        }
        // The audit event names exactly the ids that made it into the
        // now-durable snapshot — emitted only once the snapshot is safely on
        // disk, matching the fail-closed contract above.
        if let Some(ids) = recalled_ids {
            tracing::info!(run_id = %run_id, ids = ?ids, "memory recalled");
            self.sink
                .emit(EventBody::MemoryRecalled(MemoryRecalledPayload {
                    run_id: run_id.clone(),
                    ids,
                }));
        }
        // M1-T10 review H1: the notice (now part of `snapshot`, persisted
        // atomically above) is audited the SAME way the recall above is —
        // only once it is safely on disk.
        if write_skipped {
            self.sink
                .emit(EventBody::MemoryWriteSkipped(MemoryWriteSkippedPayload {
                    run_id: run_id.clone(),
                    session_id: run.session_id.clone(),
                    reason: "paused".to_owned(),
                }));
        }
        // H8: a fresh plan-mode run starts read-only; a Normal run never is.
        let plan_mode = matches!(run.input.mode, RunMode::Plan);
        self.run_loop(run, snapshot, plan_mode, cancel, idoris_headroom_confirmed)
            .await;
        // M3 growth control, chosen approach: reclaim the prior-context copy
        // ONLY along the path that never parked for approval (`run_loop`
        // only returns here once this SAME invocation reached a terminal
        // state OR parked; we re-check the row to tell which). A run that
        // DID park keeps its full thread until it is later driven to
        // completion by `drive_resume` — which does not repeat this cleanup,
        // since the seq range is only known here, in this call's own stack.
        // Rejected alternative ("only persist for a run that may need
        // approval"): undecidable in advance — nothing knows a tool call
        // will need approval before the model actually asks for one, so the
        // fail-closed write above must always happen. The accepted gap this
        // leaves: a run parked when the daemon restarts, then resumed to
        // completion in a LATER process, never gets this cleanup (the range
        // lived only in this now-gone task's stack) — its prior-context rows
        // persist for that run's lifetime. Parked runs are the sparse case
        // and are bounded by "how many are currently awaiting a human", so
        // this is accepted rather than adding cross-restart persisted state
        // for a Medium-severity growth concern. Also EVENTUAL, not atomic
        // with the terminal transition above: this cleanup runs in its own
        // subsequent await on the SAME task, after the run is ALREADY
        // observable as Completed/Failed/Cancelled through the store row —
        // an external reader that queries `list_run_messages` in the
        // instant right after observing the terminal status can still see
        // the un-cleaned thread. Acceptable for housekeeping; a caller that
        // needs the final, pruned shape synchronously with "terminal" would
        // need the cleanup moved inside the SAME transaction as that
        // transition, which this PR does not do.
        if let Some((start, end)) = prior_context_range
            && let Ok(Some(final_run)) = self.store.get_run(&run_id).await
            && matches!(
                final_run.status,
                RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
            )
            // Refusing the send keeps the snapshot. Reclaiming prior-context
            // rows here would delete bytes this failure promised to leave.
            && !unproven_source_failure(&final_run)
            && let Err(err) = self
                .store
                .delete_run_message_range(&run_id, start, end)
                .await
        {
            tracing::warn!(
                "run {run_id}: prior-context cleanup failed (non-fatal, thread still correct): {err}"
            );
        }
    }

    /// R05-5 4b: the request-only send copy. Each message is shaped by the
    /// origin kind PERSISTED for its stored seq (`list_run_messages` plus
    /// `read_run_message_origin`), never by its text or its in-memory index:
    ///
    /// - `HostRecall` is re-validated against the CURRENT ledger line by line
    ///   (a retracted id must not reach the model again). A HostRecall that is
    ///   fully stale, malformed (role not user, no content, no header / end
    ///   marker, no parsable id), or cannot be re-validated (`active_ids()`
    ///   failed, or there is no memory) is dropped from the copy with a warn.
    /// - every other kind (user prompt, session history, assistant, tool
    ///   result, and any future kind) is sent verbatim, even when its text is
    ///   shaped exactly like a recall block.
    ///
    /// `messages` is the run loop's in-memory thread; it must be provably the
    /// stored thread (same length, same bytes per seq) or the send is refused
    /// (`None`) — position is never trusted. Called fresh on EVERY iteration so
    /// a cold resume, an approval answered in-process, and a long approval wait
    /// all see the current ledger. Never mutates the store or `messages`; the
    /// shaping (before `normalize_for_provider`, which merges adjacent user
    /// turns and would erase the kind boundaries) only affects this copy.
    async fn request_messages_for(&self, messages: &[Msg], run_id: &str) -> Option<Vec<Msg>> {
        let stored = match self.store.list_run_messages(run_id).await {
            Ok(stored) => stored,
            Err(err) => {
                tracing::warn!(run_id, error = %err, "send copy: could not read the stored thread");
                return None;
            }
        };
        if stored.len() != messages.len()
            || thread_to_messages(&stored)
                .iter()
                .zip(messages)
                .any(|(a, b)| a != b)
        {
            tracing::warn!(
                run_id,
                "send copy: the in-memory thread does not match the stored thread; refusing to send"
            );
            return None;
        }
        // Resolved lazily, once, and only if a HostRecall is actually present.
        let mut active: Option<Option<std::collections::HashSet<String>>> = None;
        let mut shaped = Vec::with_capacity(messages.len());
        for (row, msg) in stored.iter().zip(messages) {
            let origin = match self.store.read_run_message_origin(run_id, row.seq).await {
                Ok(MessageOriginLookup::Present(stored_origin)) => stored_origin.origin,
                Ok(_) => return None,
                Err(err) => {
                    tracing::warn!(run_id, seq = row.seq, error = %err, "message origin read failed");
                    return None;
                }
            };
            match origin {
                MessageOrigin::HostRecall { recall_ref: None } => return None,
                MessageOrigin::HostRecall {
                    recall_ref: Some(_),
                } => {
                    if active.is_none() {
                        active = Some(match self.memory.as_ref() {
                            None => {
                                tracing::warn!(
                                    run_id,
                                    "run {run_id}: no memory to re-validate HostRecall; dropping it from the send copy"
                                );
                                None
                            }
                            Some(memory) => match memory.active_ids().await {
                                Ok(ids) => Some(ids),
                                Err(err) => {
                                    tracing::warn!(
                                        run_id,
                                        "run {run_id}: could not re-validate HostRecall; dropping it from the send copy: {err}"
                                    );
                                    None
                                }
                            },
                        });
                    }
                    let kept = match active.as_ref().and_then(Option::as_ref) {
                        Some(ids) => shape_host_recall(msg.clone(), ids),
                        None => None,
                    };
                    match kept {
                        Some(kept) => shaped.push(kept),
                        None => {
                            tracing::warn!(
                                run_id,
                                seq = row.seq,
                                "dropping a HostRecall row from the send copy (stale, malformed or unverifiable)"
                            );
                        }
                    }
                }
                _ => shaped.push(msg.clone()),
            }
        }
        Some(normalize_for_provider(&shaped))
    }

    /// Whether a parked run may be resumed under the tags it has (K1-R3
    /// release review). A build before migration 0031 tagged no tool result,
    /// and 0031 gave a session that had called tools a `tool:legacy` origin
    /// only for its later runs. A run that build started and parked, resumed
    /// after the upgrade, would replay those results as ordinary and pass the
    /// egress gate. Migration 0033 lists every run unfinished at the upgrade;
    /// a listed run is not resumed, and a read failure is the same refusal.
    /// The list decides, not the run's creation time: a wall clock may be set
    /// back across the upgrade.
    async fn resume_sources_proven(&self, run: &Run) -> bool {
        match self.store.run_predates_tool_result_tagging(&run.id).await {
            Ok(false) => true,
            Ok(true) => {
                tracing::warn!(run_id = %run.id, "resume: the run predates tool result tagging");
                false
            }
            Err(err) => {
                tracing::warn!(run_id = %run.id, error = %err, "resume: tool result tagging record unreadable");
                false
            }
        }
    }

    /// `true` when the run has a skill pin and no usable `SkillText` label for
    /// it, or when either cannot be read. A run without a pin has nothing to
    /// check. Only stored rows are read: not the pointer, and nothing is
    /// reclassified. An imported skill's label must not be `Ordinary`.
    async fn skill_label_missing(&self, run_id: &str) -> bool {
        skill_inject::label_missing(&self.store, run_id).await
    }

    /// Stored seq order, not message text. A read error is the same refusal
    /// as a missing or unreadable row: the provider is not called.
    async fn persisted_origins_allow_send(&self, run_id: &str) -> bool {
        let messages = match self.store.list_run_messages(run_id).await {
            Ok(messages) => messages,
            Err(err) => {
                tracing::warn!(run_id, error = %err, "message origin read failed");
                return false;
            }
        };
        for message in messages {
            match self
                .store
                .read_run_message_origin(run_id, message.seq)
                .await
            {
                Ok(lookup) if origin_allows_send(&lookup) => {}
                Ok(_) => return false,
                Err(err) => {
                    tracing::warn!(run_id, seq = message.seq, error = %err, "message origin read failed");
                    return false;
                }
            }
        }
        true
    }

    /// The completion↔tool-execution loop shared by a fresh run ([`execute`]) and
    /// a resumed one ([`resume_run`]): each builds the `messages` prefix its own
    /// way (fresh: prior context + prompt; resumed: reconstructed thread + the
    /// settled parked call), then hands it here. Bounded by MAX_ITERATIONS;
    /// usage accumulates across iterations.
    async fn run_loop(
        &self,
        run: Run,
        mut messages: Vec<Msg>,
        mut plan_mode: bool,
        cancel: CancellationToken,
        mut idoris_headroom_confirmed: bool,
    ) {
        let run_id = run.id.clone();
        let mut usage_total = zero_usage();

        for _ in 0..MAX_ITERATIONS {
            if !self.persisted_origins_allow_send(&run_id).await {
                self.finish_failed(&run_id, SOURCE_UNPROVEN_CODE, SOURCE_UNPROVEN_MESSAGE)
                    .await;
                return;
            }
            if self.skill_label_missing(&run_id).await {
                self.finish_failed(&run_id, "source_policy_unavailable", SKILL_LABEL_MESSAGE)
                    .await;
                return;
            }
            // Recomputed each turn: in plan mode only the read-only subset plus
            // `propose_plan` is offered; the instant a plan is approved
            // `plan_mode` flips false and the full set is advertised (H8).
            // H1/M4: `messages` itself (the persisted/growing thread) is
            // re-validated against the CURRENT ledger and reshaped fresh
            // every iteration via `request_messages_for` — request-only,
            // never applied back to `messages` itself.
            let Some(send_messages) = self.request_messages_for(&messages, &run_id).await else {
                self.finish_failed(&run_id, SOURCE_UNPROVEN_CODE, SOURCE_UNPROVEN_MESSAGE)
                    .await;
                return;
            };
            let request = CompletionRequest {
                messages: send_messages,
                model: run.input.model_override.clone(),
                tools: self
                    .tool_specs_for(plan_mode, run.input.knowledge_binding_id.is_some())
                    .await,
                response_format: None,
                max_tokens: None,
                disable_thinking: false,
                idoris_headroom_confirmed: std::mem::take(&mut idoris_headroom_confirmed),
                temperature: None,
                progress: None,
            };
            let outcome = tokio::select! {
                r = self.router.complete(
                    self.task_profile_for_run(&run_id, TaskProfile::default()).await,
                    &request,
                    &cancel,
                ) => r,
                () = cancel.cancelled() => Err(ModelError::Cancelled),
            };

            let res = match outcome {
                Ok((provider, res)) => {
                    tracing::debug!("run {run_id} served by {provider}");
                    res
                }
                Err(ModelError::Cancelled) => {
                    self.finish_cancelled(&run_id).await;
                    return;
                }
                Err(err) => {
                    // H12: the model layer now returns messages a user can act
                    // on. Pass them through as-is rather than re-wrapping them
                    // in "internal", which discarded the very detail (bad key,
                    // wrong model, rate limit) that tells them what to fix.
                    let (code, message) = match &err {
                        ModelError::Unavailable(msg) => (
                            "provider_unavailable",
                            format!("No provider could serve this request. {msg}"),
                        ),
                        ModelError::Provider(msg) => ("provider_error", msg.clone()),
                        // ME4-S2 L3: same handling as `Provider` — same `Display`
                        // text, so the agent loop's output is unchanged.
                        ModelError::Rejected { message, .. } => ("provider_error", message.clone()),
                        ModelError::IdorisHeadroomConfirmationRequired { .. } => {
                            ("idoris_headroom_confirmation_required", err.to_string())
                        }
                        ModelError::IdorisSupervisorBusy => {
                            ("idoris_supervisor_busy", err.to_string())
                        }
                        ModelError::Cancelled => ("internal", err.to_string()),
                    };
                    self.finish_failed(&run_id, code, &message).await;
                    return;
                }
            };
            usage_total = add_usage(usage_total, &res.usage);

            if res.message.role != "assistant" {
                self.finish_failed(&run_id, "provider_error", PROVIDER_ROLE_MESSAGE)
                    .await;
                return;
            }

            if res.message.tool_calls.is_empty() {
                // Final answer
                let text = res.message.content.clone().unwrap_or_default();
                // Record the closing assistant turn so the durable thread is a
                // complete, self-contained transcript (H3).
                let _ = self.persist_message(&run_id, &res.message).await;
                self.sink.emit(EventBody::ModelDelta(ModelDeltaPayload {
                    run_id: run_id.clone(),
                    text: text.clone(),
                }));
                // Persist memory BEFORE the run becomes observable as completed
                // — through the STORE ROW as well as the event. A client polling
                // get_run/list_runs could otherwise see `completed`, start the
                // next run in this session, win the session lock and read stale
                // memory (review D5b). Preparation and compaction are budgeted
                // by MEMORY_WRITE_BUDGET; append confirmation can exceed it.
                //
                // This widens the window in which the run is still non-terminal,
                // so it MUST stay cancellable: `cancel works in any non-terminal
                // state` is the C2 contract. If cancellation drops this waiter
                // after append starts, SessionMemory's detached transaction task
                // keeps the session lock until SQLx confirms the append outcome.
                let memory_persisted = tokio::select! {
                    outcome = async {
                        let source_ref = self.run_assertion_source_ref(&run_id).await;
                        let prompt_origin = self.run_prompt_origin(&run).await;
                        self.remember_exchange(
                            run.session_id.as_deref(),
                            &run.input.prompt,
                            &text,
                            prompt_origin,
                            source_ref,
                        )
                        .await
                    } => Some(outcome.persisted()),
                    () = cancel.cancelled() => {
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
                };
                // Re-check: a cancel that landed just as the write finished must
                // still win rather than be overwritten by Completed.
                //
                // A cancel arriving between THIS check and the transition below
                // still loses — an inherent check-then-act window that predates
                // this change (it has always existed between the loop's last
                // check and finalization). The memory write above is what could
                // have widened it to 30s, which is why that is cancellable;
                // closing the remaining microsecond window would need the store
                // to make cancel-vs-complete a single atomic transition.
                if cancel.is_cancelled() {
                    self.finish_cancelled(&run_id).await;
                    return;
                }
                let ended_at = now_iso8601();
                match self
                    .transition_terminal(
                        &run_id,
                        RunStatus::Completed,
                        RunPatch {
                            output: Some(agent24_protocol::RunOutput { text: text.clone() }),
                            usage: Some(usage_total.clone()),
                            ended_at: Some(ended_at.clone()),
                            ..Default::default()
                        },
                        &ended_at,
                    )
                    .await
                {
                    Ok(_) => {
                        self.sink.emit(EventBody::RunCompleted(RunCompletedPayload {
                            run_id: run_id.clone(),
                            output: RunOutputPayload { text },
                            usage: usage_total,
                            memory_persisted,
                        }));
                        self.note_inbound_terminal(&run_id).await;
                    }
                    Err(err) => tracing::error!("run completion persist failed: {err}"),
                }
                return;
            }

            // Tool round trip: echo the assistant turn, then answer every call.
            // Every call gets a tool message (protocol requirement) but only
            // the first MAX_TOOL_CALLS_PER_TURN execute — a runaway fanout is
            // answered, not obeyed.
            let calls = res.message.tool_calls.clone();
            // Persist the assistant turn (with its tool_calls) BEFORE running any
            // call. This is the row H3 keys resume off: an assistant turn on disk
            // whose trailing tool_call has no answering `tool` row is exactly a
            // run that died awaiting approval.
            if !self.persist_for_next_send(&run_id, &res.message).await {
                return;
            }
            messages.push(res.message);
            for (idx, call) in calls.iter().enumerate() {
                if cancel.is_cancelled() {
                    self.finish_cancelled(&run_id).await;
                    return;
                }
                if idx >= MAX_TOOL_CALLS_PER_TURN {
                    let skipped = Msg::tool_result(
                        call.id.clone(),
                        format!(
                            "skipped: per-turn tool call limit ({MAX_TOOL_CALLS_PER_TURN}) exceeded"
                        ),
                    );
                    if !self.persist_for_next_send(&run_id, &skipped).await {
                        return;
                    }
                    messages.push(skipped);
                    continue;
                }
                // H8: propose_plan is a loop construct, not a registry tool. It
                // gates the read-only → full-tools transition behind a
                // human-only approval instead of executing a side effect.
                if call.name.trim() == PROPOSE_PLAN {
                    match self
                        .run_plan_proposal(&run_id, run.session_id.as_deref(), call, &cancel)
                        .await
                    {
                        PlanOutcome::Approved(content) => {
                            plan_mode = false; // full tool set from the next turn
                            let result = Msg::tool_result(call.id.clone(), content);
                            if !self.persist_for_next_send(&run_id, &result).await {
                                return;
                            }
                            messages.push(result);
                        }
                        PlanOutcome::Rejected(content) => {
                            // The user declined the plan — the run ends here,
                            // having done nothing but read.
                            let result = Msg::tool_result(call.id.clone(), content.clone());
                            let _ = self.persist_message(&run_id, &result).await;
                            self.finish_completed(&run_id, &content, usage_total.clone())
                                .await;
                            return;
                        }
                        PlanOutcome::Cancelled => {
                            self.finish_cancelled(&run_id).await;
                            return;
                        }
                    }
                    continue;
                }
                // H8: enforce read-only at the DISPATCH boundary, not only at the
                // advertising layer. A rogue / hallucinating model (or one fed
                // adversarial content through a read tool) could emit a write /
                // exec / external call that was never offered; routed through the
                // normal gate it might be auto-satisfied by a pre-existing H4
                // standing grant or a Guardian fast-path — executing with no plan
                // ever approved. Fail-closed here, before any of that runs, so
                // `propose_plan` stays the only way out of read-only. (`None` =
                // unknown tool → also denied.)
                if plan_mode && self.tools.tool_risk_class(&call.name) != Some(RiskClass::Read) {
                    let denied = Msg::tool_result(
                        call.id.clone(),
                        "denied: plan mode is read-only — call propose_plan and have it \
                         approved before using this tool"
                            .to_owned(),
                    );
                    if !self.persist_for_next_send(&run_id, &denied).await {
                        return;
                    }
                    messages.push(denied);
                    continue;
                }
                match self.run_tool_call(&run, call, &cancel, false).await {
                    Ok(answer) => {
                        if !self
                            .answer_tool_call(&run, &mut messages, call, answer)
                            .await
                        {
                            return;
                        }
                    }
                    Err(ParkedCallStop::CancelRun) => {
                        // Cancelled mid-tool, or the user chose abort on an
                        // approval — either way the run lands cancelled
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
                    Err(ParkedCallStop::RecoveryStopped) => return,
                }
            }
        }

        self.finish_failed(
            &run_id,
            "max_iterations",
            &format!("run exceeded {MAX_ITERATIONS} completion iterations without a final answer"),
        )
        .await;
    }

    /// Execute one model-requested tool call through the registry pipeline:
    /// persist running → dispatch → persist terminal + event (+ audit on
    /// policy denial). Returns the content handed back to the model, or
    /// `Err(())` when the run was cancelled mid-call.
    async fn run_tool_call(
        &self,
        run: &Run,
        call: &agent24_models::ToolCallRequest,
        cancel: &CancellationToken,
        recovery_resume: bool,
    ) -> Result<ToolAnswer, ParkedCallStop> {
        let run_id = run.id.as_str();
        let (input, parse_error) = if call.arguments.trim().is_empty() {
            (serde_json::Map::new(), None)
        } else {
            match serde_json::from_str::<serde_json::Value>(&call.arguments) {
                Ok(serde_json::Value::Object(map)) => (map, None),
                Ok(other) => {
                    // Preserved raw for audit; the call itself is rejected
                    let mut m = serde_json::Map::new();
                    m.insert("_raw".to_owned(), other);
                    (m, Some("tool arguments must be a JSON object".to_owned()))
                }
                Err(err) => {
                    let mut m = serde_json::Map::new();
                    m.insert(
                        "_raw".to_owned(),
                        serde_json::Value::String(call.arguments.clone()),
                    );
                    (m, Some(format!("tool arguments are not valid JSON: {err}")))
                }
            }
        };

        let tc = ToolCall {
            id: format!("tc_{}", ulid()),
            run_id: run_id.to_owned(),
            tool: call.name.clone(),
            input: input.clone(),
            status: ToolCallStatus::Running,
            output_summary: None,
            started_at: now_iso8601(),
            ended_at: None,
        };
        let recovery_ctx = if recovery_resume && run.workspace_id.is_some() && parse_error.is_none()
        {
            match self.tool_context_for(run, tc.id.clone()).await {
                Ok(ctx) => Some(ctx),
                Err(err) => {
                    tracing::warn!(
                        "run {run_id}: resumed tool workspace authority unavailable — {err}"
                    );
                    self.cancel_workspace_resume_recovery(run_id).await;
                    return Err(ParkedCallStop::RecoveryStopped);
                }
            }
        } else {
            None
        };
        if let Err(err) = self.store.insert_tool_call(&tc).await {
            tracing::error!("tool call persist failed: {err}");
            return Ok(ToolAnswer::fixed(
                "tool error: internal persistence failure".to_owned(),
            ));
        }
        self.sink.emit(EventBody::ToolStarted(ToolStartedPayload {
            run_id: run_id.to_owned(),
            tool_call_id: tc.id.clone(),
            tool: call.name.clone(),
            input_summary: summarize_input(&input),
        }));

        // SPEC-002 §1.2: a run blocked on an interactive approval is
        // `awaiting_approval`, not `running` — REST pollers must see it.
        let awaiting = parse_error.is_none()
            && self.tools.gate_is_interactive()
            && self.tools.tool_requires_approval(&call.name);
        if awaiting
            && let Err(err) = self
                .store
                .transition_run(run_id, RunStatus::AwaitingApproval, RunPatch::default())
                .await
        {
            tracing::error!("run {run_id}: awaiting_approval transition failed: {err}");
        }

        // Only content the tool itself produced carries a source tag.
        let dispatched = parse_error.is_none();
        let outcome = match parse_error {
            Some(msg) => Err(ToolError::Invalid(msg)),
            None => match recovery_ctx {
                Some(ctx) => self.tools.dispatch(&call.name, &ctx, &input, cancel).await,
                None => match self.tool_context_for(run, tc.id.clone()).await {
                    Ok(ctx) => self.tools.dispatch(&call.name, &ctx, &input, cancel).await,
                    Err(err) => Err(ToolError::Denied(format!(
                        "workspace authority unavailable: {err}"
                    ))),
                },
            },
        };

        // Back to running unless the run is about to land cancelled (the
        // awaiting_approval → cancelled edge is taken by finish_cancelled)
        if awaiting
            && !matches!(
                outcome,
                Err(ToolError::AbortRun(_)) | Err(ToolError::Cancelled)
            )
            && let Err(err) = self
                .store
                .transition_run(run_id, RunStatus::Running, RunPatch::default())
                .await
        {
            tracing::error!("run {run_id}: back-to-running transition failed: {err}");
        }

        // `Invalid` is the host's own argument check or an unknown name;
        // what an MCP server or a module sends back arrives as `Ok` or
        // `Failed`.
        let produced = dispatched && matches!(outcome, Ok(_) | Err(ToolError::Failed(_)));
        let (status, summary, content, cancelled) = match outcome {
            Ok(output) => {
                let summary = truncate(&output, SUMMARY_MAX_BYTES);
                (ToolCallStatus::Completed, summary, output, false)
            }
            Err(ToolError::Denied(msg)) => {
                // Fail-closed policy denial — audited, and the model is told
                let detail = serde_json::json!({
                    "run_id": run_id,
                    "tool_call_id": tc.id,
                    "tool": call.name,
                    "reason": msg,
                });
                if let Err(err) = self
                    .store
                    .append_audit(&now_iso8601(), "policy", "tool.denied", &detail)
                    .await
                {
                    tracing::error!("audit append failed: {err}");
                }
                let content = format!("denied by policy: {msg}");
                (ToolCallStatus::Denied, content.clone(), content, false)
            }
            Err(ToolError::Cancelled) => {
                let content = "cancelled".to_owned();
                (ToolCallStatus::Failed, content.clone(), content, true)
            }
            Err(ToolError::AbortRun(msg)) => {
                // User chose abort: this call lands denied and the whole run
                // is cancelled by the caller (SPEC-002 §1.4)
                let content = format!("denied by policy: {msg}");
                (ToolCallStatus::Denied, content.clone(), content, true)
            }
            Err(err) => {
                let content = format!("tool error: {err}");
                (ToolCallStatus::Failed, content.clone(), content, false)
            }
        };

        // The store is authoritative: no terminal event unless the terminal
        // state actually persisted — a completed event over a row still
        // `running` would break WS/REST reconciliation (review C3)
        match self
            .store
            .finish_tool_call(&tc.id, status, Some(summary.clone()), now_iso8601())
            .await
        {
            Ok(()) => self
                .sink
                .emit(EventBody::ToolCompleted(ToolCompletedPayload {
                    run_id: run_id.to_owned(),
                    tool_call_id: tc.id.clone(),
                    status: match status {
                        ToolCallStatus::Completed => ToolCompletedStatus::Completed,
                        ToolCallStatus::Denied => ToolCompletedStatus::Denied,
                        _ => ToolCompletedStatus::Failed,
                    },
                    output_summary: Some(summary),
                })),
            Err(err) => tracing::error!("tool call finish persist failed: {err}"),
        }

        if cancelled {
            return Err(ParkedCallStop::CancelRun);
        }
        Ok(ToolAnswer {
            content,
            source: produced.then(|| self.tool_result_tag(&call.name, &tc.id)),
        })
    }

    /// The tool specs advertised this turn: the full advert set normally, or the
    /// read-only subset plus `propose_plan` in plan mode (H8). A run that names
    /// no Knowledge binding is offered no tool that needs one (`knowledge_bound`).
    async fn tool_specs_for(&self, plan_mode: bool, knowledge_bound: bool) -> Vec<ToolSpec> {
        let adverts = if plan_mode {
            self.tools.live_plan_adverts(knowledge_bound).await
        } else {
            self.tools.live_adverts(knowledge_bound).await
        };
        let mut specs: Vec<ToolSpec> = adverts
            .into_iter()
            .map(|a| ToolSpec {
                name: a.name,
                description: a.description,
                parameters: a.parameters,
            })
            .collect();
        if plan_mode {
            specs.push(ToolSpec {
                name: PROPOSE_PLAN.to_owned(),
                description: "You are in plan mode and may only READ — write/exec/network \
                     tools are withheld. When you have a concrete plan, call propose_plan with \
                     it. A human approves or rejects the plan; on approval the full tool set \
                     unlocks and you carry it out, on rejection the run ends."
                    .to_owned(),
                parameters: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "plan": {
                            "type": "string",
                            "description": "The plan you intend to carry out, detailed enough for a human to approve."
                        }
                    },
                    "required": ["plan"]
                }),
            });
        }
        specs
    }

    /// Handle a `propose_plan` call (H8): record it as a tool call, park the run
    /// on a human-only plan approval, and translate the decision into a
    /// [`PlanOutcome`]. Never routes through the registry — a plan is a mode
    /// gate, not a side effect.
    async fn run_plan_proposal(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        call: &ToolCallRequest,
        cancel: &CancellationToken,
    ) -> PlanOutcome {
        let plan = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .ok()
            .and_then(|v| v.get("plan").and_then(|p| p.as_str()).map(str::to_owned))
            .unwrap_or_default();
        let mut input = serde_json::Map::new();
        input.insert("plan".to_owned(), serde_json::Value::String(plan.clone()));

        let tc = ToolCall {
            id: format!("tc_{}", ulid()),
            run_id: run_id.to_owned(),
            tool: PROPOSE_PLAN.to_owned(),
            input: input.clone(),
            status: ToolCallStatus::Running,
            output_summary: None,
            started_at: now_iso8601(),
            ended_at: None,
        };
        if let Err(err) = self.store.insert_tool_call(&tc).await {
            tracing::error!("plan tool call persist failed: {err}");
            return PlanOutcome::Rejected("plan could not be recorded (fail-closed)".to_owned());
        }
        self.sink.emit(EventBody::ToolStarted(ToolStartedPayload {
            run_id: run_id.to_owned(),
            tool_call_id: tc.id.clone(),
            tool: PROPOSE_PLAN.to_owned(),
            input_summary: truncate(&plan, SUMMARY_MAX_BYTES),
        }));

        // The run parks on a human decision — REST pollers must see it.
        if let Err(err) = self
            .store
            .transition_run(run_id, RunStatus::AwaitingApproval, RunPatch::default())
            .await
        {
            tracing::error!("run {run_id}: plan awaiting_approval transition failed: {err}");
        }

        let ask = format!("Approve this plan?\n{}", truncate(&plan, SUMMARY_MAX_BYTES));
        let decision = self
            .tools
            .request_plan(run_id, session_id, &tc.id, ask, input, cancel)
            .await;

        // Back to running unless the approval aborted the whole run.
        if !matches!(decision, GateDecision::AbortRun(_))
            && let Err(err) = self
                .store
                .transition_run(run_id, RunStatus::Running, RunPatch::default())
                .await
        {
            tracing::error!("run {run_id}: plan back-to-running transition failed: {err}");
        }

        let (status, outcome) = match decision {
            GateDecision::Allow => (
                ToolCallStatus::Completed,
                PlanOutcome::Approved(
                    "Plan approved. The full tool set is now available — carry out the plan."
                        .to_owned(),
                ),
            ),
            GateDecision::Deny(reason) => (
                ToolCallStatus::Denied,
                PlanOutcome::Rejected(format!("Plan rejected: {reason}")),
            ),
            GateDecision::AbortRun(_) => (ToolCallStatus::Denied, PlanOutcome::Cancelled),
        };
        let summary = match &outcome {
            PlanOutcome::Approved(c) | PlanOutcome::Rejected(c) => c.clone(),
            PlanOutcome::Cancelled => "aborted".to_owned(),
        };
        match self
            .store
            .finish_tool_call(&tc.id, status, Some(summary.clone()), now_iso8601())
            .await
        {
            Ok(()) => self
                .sink
                .emit(EventBody::ToolCompleted(ToolCompletedPayload {
                    run_id: run_id.to_owned(),
                    tool_call_id: tc.id.clone(),
                    status: match status {
                        ToolCallStatus::Completed => ToolCompletedStatus::Completed,
                        _ => ToolCompletedStatus::Denied,
                    },
                    output_summary: Some(summary),
                })),
            Err(err) => tracing::error!("plan tool call finish persist failed: {err}"),
        }
        outcome
    }

    async fn transition_terminal(
        &self,
        run_id: &str,
        to: RunStatus,
        mut patch: RunPatch,
        _ended_at: &str,
    ) -> Result<Run, AgentError> {
        let run = match self.store.active_workspace_run_lease_id(run_id).await? {
            None => Ok(self.store.transition_run(run_id, to, patch).await?),
            Some(lease_id) => {
                let ended_at = workspace_now()?;
                patch.ended_at = Some(ended_at.clone());
                let ended_at = WorkspaceInstant::parse(&ended_at)?;
                match self
                    .store
                    .transition_workspace_run_terminal(run_id, to, patch, &lease_id, &ended_at)
                    .await?
                {
                    RunTerminalTransition::Applied(run) => Ok(*run),
                    RunTerminalTransition::Conflict => Err(AgentError::Store(
                        StoreError::Conflict("workspace run terminal transition".to_owned()),
                    )),
                }
            }
        }?;
        Ok(run)
    }

    /// Drop a linked inbound row out of the in-flight index.
    ///
    /// Call only after the run is terminal and that event has been emitted
    /// (#1810 N4). `mark_inbound_done_for_run` also accepts `starting` →
    /// `done`, because a completion can land before `mark_inbound_started`;
    /// that edge is not for a run that is still live.
    /// [`Self::release_inbound_after_abandoned_start`] is the one caller that
    /// notes when the terminal transition itself did not apply: the Running
    /// transition had already failed, and leaving the row at `started` would
    /// keep the sender's in-flight slot.
    ///
    /// An await between the status write and the emit lets a waiter observe
    /// the terminal run first. No link is a no-op. A failure leaves the row
    /// for startup reconcile.
    async fn note_inbound_terminal(&self, run_id: &str) {
        if let Err(err) = self.store.mark_inbound_done_for_run(run_id).await {
            tracing::warn!(
                run_id,
                error = %err,
                "inbound run_state was not marked done"
            );
        }
    }

    /// The Running transition failed, so this start or resume never became
    /// live. Cancel when that edge is legal: the cancelled event is what
    /// #1810 N4 requires before the in-flight slot is released, and
    /// [`Self::finish_cancelled`] notes only when the transition lands. A
    /// run that is already terminal, or a status write that does not apply,
    /// still has to leave `started` (#1813 L1).
    ///
    /// This runs for every run, inbound or not. An ordinary run whose move
    /// to Running fails is cancelled here. It used to stay non-terminal until
    /// the next restart swept it. The end state is the same; this lands it now.
    async fn release_inbound_after_abandoned_start(&self, run_id: &str) {
        self.finish_cancelled(run_id).await;
        self.note_inbound_terminal(run_id).await;
    }

    /// Land the completed terminal state + event with the given output text.
    async fn finish_completed(&self, run_id: &str, text: &str, usage: Usage) {
        let ended_at = now_iso8601();
        match self
            .transition_terminal(
                run_id,
                RunStatus::Completed,
                RunPatch {
                    output: Some(agent24_protocol::RunOutput {
                        text: text.to_owned(),
                    }),
                    usage: Some(usage.clone()),
                    ended_at: Some(ended_at.clone()),
                    ..Default::default()
                },
                &ended_at,
            )
            .await
        {
            Ok(_) => {
                self.sink.emit(EventBody::RunCompleted(RunCompletedPayload {
                    run_id: run_id.to_owned(),
                    output: RunOutputPayload {
                        text: text.to_owned(),
                    },
                    usage,
                    // This path never attempts a memory write, so none failed.
                    memory_persisted: Some(true),
                }));
                self.note_inbound_terminal(run_id).await;
            }
            Err(err) => tracing::error!("run completion persist failed: {err}"),
        }
    }

    /// Land the failed terminal state + event.
    async fn finish_failed(&self, run_id: &str, code: &str, message: &str) {
        let body = ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            hint: None,
            details: None,
        };
        let ended_at = now_iso8601();
        match self
            .transition_terminal(
                run_id,
                RunStatus::Failed,
                RunPatch {
                    error: Some(body.clone()),
                    ended_at: Some(ended_at.clone()),
                    ..Default::default()
                },
                &ended_at,
            )
            .await
        {
            Ok(_) => {
                self.sink.emit(EventBody::RunFailed(RunFailedPayload {
                    run_id: run_id.to_owned(),
                    error: body,
                }));
                self.note_inbound_terminal(run_id).await;
            }
            Err(err) => tracing::error!("run failure persist failed: {err}"),
        }
    }
    /// The single helper that lands the cancelled terminal state + event.
    /// Both paths use it (executor on token cancel; cancel_run for token-less
    /// runs) — a raced double-write loses in the store's IMMEDIATE tx and is
    /// logged, never duplicated.
    async fn finish_cancelled(&self, run_id: &str) {
        let ended_at = now_iso8601();
        match self
            .transition_terminal(
                run_id,
                RunStatus::Cancelled,
                RunPatch {
                    ended_at: Some(ended_at.clone()),
                    ..Default::default()
                },
                &ended_at,
            )
            .await
        {
            Ok(_) => {
                self.sink.emit(EventBody::RunCancelled(RunCancelledPayload {
                    run_id: run_id.to_owned(),
                }));
                self.note_inbound_terminal(run_id).await;
            }
            Err(err) => tracing::debug!("run cancel persist skipped: {err}"),
        }
    }
}

fn restored_decision_is_consistent(approval: &Approval, decision: &Decision) -> bool {
    if !approval
        .available_decisions
        .iter()
        .any(|offered| offered == &decision.kind)
    {
        return false;
    }
    matches!(
        (approval.status, decision.kind.as_str()),
        (
            ApprovalStatus::Approved,
            "approve" | "approve_for_session" | "approve_for_target"
        ) | (ApprovalStatus::Denied, "deny")
            | (ApprovalStatus::Aborted, "abort")
    )
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod approval_tests;
