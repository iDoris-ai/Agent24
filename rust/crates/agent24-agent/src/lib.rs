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
pub mod subagent;
pub use session_memory::{RECALL_END_MARKER, RECALL_PREFIX, SessionMemory};

use std::collections::HashMap;
use std::sync::Arc;

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
    RunAdmission, RunAdmissionDenial, RunMessage, RunPatch, RunTerminalTransition, Store,
    StoreError, WorkspaceInstant, WorkspaceLeaseId, WorkspaceStoreError,
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
    /// The write-gate (or the session append it depends on) reported an
    /// error.
    Failed,
    /// There was nothing to write: no configured memory, no session, the
    /// prompt was not an explicit remember, or the origin was untrusted.
    NotApplicable,
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

/// Derive prompt provenance only from durable run metadata. Scheduled runs are
/// conservative: self-wake prompts ultimately originate in model output and
/// cannot authorize qualified personal memory.
fn run_prompt_origin(run: &Run) -> Origin {
    if run.schedule_id.is_some() {
        Origin {
            source: "scheduler".into(),
            trust: Trust::Model,
        }
    } else {
        Origin {
            source: "agent_loop".into(),
            trust: Trust::UserSaid,
        }
    }
}

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
}

enum ParkedCallStop {
    CancelRun,
    RecoveryStopped,
}

impl RunManager {
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
        })
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
    ) -> Option<Vec<Msg>> {
        let (Some(memory), Some(sid)) = (self.memory.as_ref(), session_id) else {
            return Some(Vec::new());
        };
        let load = memory.context(sid);
        let loaded = tokio::select! {
            result = load => result,
            () = cancel.cancelled() => return None,
        };
        match loaded {
            Ok(context) => Some(context),
            Err(err) => {
                tracing::warn!("session {sid} memory load failed: {err}");
                Some(Vec::new())
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
    ) -> MemoryWriteOutcome {
        let (Some(memory), Some(sid)) = (self.memory.as_ref(), session_id) else {
            return MemoryWriteOutcome::NotApplicable;
        };
        match memory.remember(sid, prompt, answer, prompt_origin).await {
            Ok(retain::RetainOutcome::Saved) => MemoryWriteOutcome::Saved,
            Ok(retain::RetainOutcome::SkippedPaused) => MemoryWriteOutcome::SkippedPaused,
            Ok(retain::RetainOutcome::NotApplicable) => MemoryWriteOutcome::NotApplicable,
            Err(err) => {
                let reason = err.to_string();
                tracing::error!(session_id = sid, %reason, "session memory write failed");
                self.sink.emit(EventBody::MemoryWriteFailed(
                    agent24_protocol::MemoryWriteFailedPayload {
                        session_id: sid.to_owned(),
                        reason,
                    },
                ));
                MemoryWriteOutcome::Failed
            }
        }
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
    /// committed.
    pub async fn chat_memory_prelude(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        prompt: &str,
    ) -> (Vec<Msg>, Option<ExplicitRememberState>) {
        let Some(memory) = self.memory.as_ref() else {
            return (Vec::new(), None);
        };
        let mut prelude = Vec::new();
        match memory.recall(prompt).await {
            Ok(Some((msg, ids))) => {
                self.sink
                    .emit(EventBody::MemoryRecalled(MemoryRecalledPayload {
                        run_id: run_id.to_owned(),
                        ids,
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
        (prelude, explicit_state)
    }

    /// M1-T12: the SAME post-answer commit `drive_new`'s run loop performs
    /// (`remember_exchange`, used as-is — not duplicated), exposed for the
    /// `/api/v1/chat` surface. `source: "chat"` distinguishes this turn's
    /// provenance from the agent loop's `"agent_loop"` in the audit trail;
    /// trust is `UserSaid` for the same reason `drive_new`'s non-scheduled
    /// branch uses it — this prompt came directly from the chat caller, not
    /// from model/scheduler-originated text. Retain/log failures emit
    /// `memory.write_failed` and otherwise never propagate — the chat
    /// response the caller already has must still reach its client. Returns
    /// the LIVE [`MemoryWriteOutcome`] — see [`RunManager::remember_exchange`].
    pub async fn chat_remember_turn(
        &self,
        session_id: &str,
        prompt: &str,
        answer: &str,
    ) -> MemoryWriteOutcome {
        self.remember_exchange(
            Some(session_id),
            prompt,
            answer,
            Origin {
                source: "chat".into(),
                trust: Trust::UserSaid,
            },
        )
        .await
    }

    /// Append one message to the run's durable thread (H3/G1 foundation).
    /// Most callers treat this as best-effort bookkeeping; callers that require
    /// a complete replay prefix must check the result and fail closed.
    async fn persist_message(&self, run_id: &str, msg: &Msg) -> Result<(), StoreError> {
        let tool_calls =
            serde_json::to_value(&msg.tool_calls).unwrap_or_else(|_| serde_json::json!([]));
        if let Err(err) = self
            .store
            .append_run_message(
                run_id,
                &msg.role,
                msg.content.as_deref(),
                &tool_calls,
                msg.tool_call_id.as_deref(),
                &now_iso8601(),
            )
            .await
        {
            tracing::warn!("run {run_id}: message thread append failed: {err}");
            return Err(err);
        }
        Ok(())
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
        if let Some(session_id) = &create.session_id
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
        let persist = self.persist_new_run(&run).await;
        if let Err(err) = persist {
            self.cancels.lock().await.remove(&run.id);
            return Err(err);
        }

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
                async move { manager.execute(run_id, token).await }
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

        Ok(run)
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
            // NB: the approval's `tool_call_id` (an internal `tc_` id) is
            // deliberately NOT passed — the suspension point is derived from the
            // thread's provider ids, and the approval is matched by payload.
            match crate::resume::assess_restore(
                &approval.payload,
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
        // approval's internal `tc_` id — a different namespace that never
        // matches); the approval is tied to that call by payload equality inside
        // assess_restore. Done BEFORE any state change, so an unrestorable run
        // never emits a spurious RunStarted→Cancelled pair.
        let cutoff = agent24_core::util::iso8601_before(RESUME_TTL);
        let tool_call_id = match crate::resume::assess_restore(
            &approval.payload,
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
        let Some(unanswered) = unanswered_calls_of_last_turn(&messages) else {
            tracing::warn!("run {run_id}: no unanswered parked turn on resume; aborting");
            self.finish_cancelled(&run_id).await;
            return;
        };
        for call in unanswered {
            if cancel.is_cancelled() {
                self.finish_cancelled(&run_id).await;
                return;
            }
            let content = if call.id == tool_call_id {
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
                    "Plan approved. The full tool set is now available — carry out the plan."
                        .to_owned()
                } else {
                    match self
                        .settle_parked_call(&run, &approval, &call, &cancel)
                        .await
                    {
                        Ok(content) => content,
                        Err(ParkedCallStop::CancelRun) => {
                            self.finish_cancelled(&run_id).await;
                            return;
                        }
                        Err(ParkedCallStop::RecoveryStopped) => return,
                    }
                }
            } else {
                match self.run_tool_call(&run, &call, &cancel, true).await {
                    Ok(content) => content,
                    Err(ParkedCallStop::CancelRun) => {
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
                    Err(ParkedCallStop::RecoveryStopped) => return,
                }
            };
            let result = Msg::tool_result(call.id.clone(), content);
            let _ = self.persist_message(&run_id, &result).await;
            messages.push(result);
        }

        // 5. Continue the shared loop for subsequent model iterations. Always
        // out of plan mode: the only thing a plan-mode run can park on is its
        // plan approval, and reaching here means that was approved (a rejection
        // returned above), so the full tool set is unlocked (H8).
        self.run_loop(run, messages, false, cancel).await;
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
    ) -> Result<String, ParkedCallStop> {
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
                        return Ok(format!(
                            "tool error: workspace authority unavailable: {err}"
                        ));
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
                match self
                    .tools
                    .execute_preapproved(&call.name, &ctx, &approval.payload, cancel)
                    .await
                {
                    Ok(out) => Ok(out),
                    // The whole-run abort choice is honoured even on resume.
                    Err(ToolError::AbortRun(_)) | Err(ToolError::Cancelled) => {
                        Err(ParkedCallStop::CancelRun)
                    }
                    Err(err) => Ok(format!("tool error: {err}")),
                }
            }
            "deny" => Ok(format!(
                "denied by user: {}",
                decision.reason.as_deref().unwrap_or("no reason given")
            )),
            // "abort" or anything unexpected → cancel the run (fail-closed).
            _ => Err(ParkedCallStop::CancelRun),
        }
    }

    async fn execute(&self, run_id: String, cancel: CancellationToken) {
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
        let write_skipped = match self.memory.as_ref() {
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
        };

        // D1: a session's prior (compacted) context precedes this turn, so a
        // session actually remembers. Empty when memory is off or session-less.
        // A cancel while waiting on a concurrent run's session lock ends the run
        // here rather than proceeding without its own context.
        let Some(prior_context) = self
            .session_context(run.session_id.as_deref(), &cancel)
            .await
        else {
            self.finish_cancelled(&run_id).await;
            return;
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
        // (`append_run_messages_tx`), not N independent single-row appends —
        // a mid-batch failure must leave NOTHING behind, not an unpredictable
        // prefix, which is what fail-closed actually requires.
        let prior_context_len = prior_context.len();
        let mut snapshot = Vec::with_capacity(prior_context_len + 3);
        let mut recalled_ids = None;
        if let Some((recalled, ids)) = recall {
            recalled_ids = Some(ids);
            snapshot.push(recalled);
        }
        if write_skipped {
            // M1-T10 review H1/M2: part of the SAME atomic snapshot as the
            // recalled block above, for the same reason — an approval
            // resume (M2) must replay exactly what THIS run's first call
            // saw, not re-decide anything against whatever the pause switch
            // says by the time it resumes.
            snapshot.push(Msg::system(retain::PAUSED_WRITE_NOTICE));
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
        snapshot.extend(prior_context);
        snapshot.push(Msg::user(run.input.prompt.clone()));
        // K1-6b.1 (ADR-K1-02 §6): the seq this run's own user-input message
        // will be assigned by the batch append below. Valid because THIS
        // call is always the run's first `append_run_messages_tx` (a fresh
        // run's `MAX(seq)` starts at -1), so each row's assigned seq equals
        // its index in `snapshot`/`pending` — the user message is pushed
        // last, so its seq is `snapshot.len() - 1`.
        let user_msg_seq = (snapshot.len() - 1) as i64;
        let pending: Vec<agent24_store::PendingRunMessage> = snapshot
            .iter()
            .map(|msg| agent24_store::PendingRunMessage {
                role: msg.role.clone(),
                content: msg.content.clone(),
                tool_calls: serde_json::to_value(&msg.tool_calls)
                    .unwrap_or_else(|_| serde_json::json!([])),
                tool_call_id: msg.tool_call_id.clone(),
            })
            .collect();
        if let Err(err) = self
            .store
            .append_run_messages_tx(&run_id, &pending, &now_iso8601())
            .await
        {
            self.finish_failed(
                &run_id,
                "memory_snapshot_persist_failed",
                &format!("failed to persist run input snapshot: {err}"),
            )
            .await;
            return;
        }
        // K1-6b.1 (ADR-K1-02 §6, §2.1): tag the run's user input at entry.
        let user_source_tag = agent24_store::SourceRef::user_input(&run_id, now_iso8601());
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
        self.run_loop(run, snapshot, plan_mode, cancel).await;
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

    /// M1-T07.1 M4 (review #674 round 2 ③): builds the SAME shape
    /// `normalize_for_provider` always built, but first re-validates the
    /// recall block — if any — against the CURRENT ledger. Called fresh on
    /// EVERY iteration of [`Self::run_loop`], so this is the ONE place that
    /// covers all three ways a recall block can go stale before the model
    /// sees it again: a cold-restart resume (`drive_resume` rebuilding from
    /// `thread_to_messages`), an approval answered while the run is still
    /// parked in the SAME process (the in-memory broker wakes the task
    /// directly — no `drive_resume` involved at all), and — in principle —
    /// a very slow approval wait spanning multiple loop iterations. Never
    /// mutates `messages` itself, only the copy returned for this request.
    ///
    /// Round 2 ②: only `messages[0]` is ever treated as a candidate recall
    /// block — a POSITION check, not a content-prefix scan over the whole
    /// thread. The snapshot (`execute`) always puts the recall message (if
    /// any) first, so this is exactly where it would be; scanning every
    /// message for a `RECALL_PREFIX` match would also treat a bound run's
    /// own prompt as a recall block if an adversarial user typed text that
    /// happened to start with that exact prefix.
    async fn request_messages_for(&self, messages: &[Msg], run_id: &str) -> Vec<Msg> {
        let Some(memory) = self.memory.as_ref() else {
            return normalize_for_provider(messages);
        };
        let Some(first) = messages.first() else {
            return normalize_for_provider(messages);
        };
        // Round 2 ②: require BOTH the header AND the end marker, not just
        // the prefix — a real recall block always has this exact shape, so
        // this is still unambiguous, but it closes the (already-vanishing,
        // position-0-only) case of a context-free run's very first turn
        // happening to be the user's OWN prompt and that prompt merely
        // STARTING WITH the same prefix text without the matching suffix:
        // such a message is left alone rather than risk getting dropped by
        // `prune_stale_recall_message` failing to parse it as a real block.
        if first.role != "user"
            || !first
                .content
                .as_deref()
                .is_some_and(|c| c.starts_with(RECALL_PREFIX) && c.ends_with(RECALL_END_MARKER))
        {
            return normalize_for_provider(messages);
        }
        let mut rebuilt = messages.to_vec();
        match memory.active_ids().await {
            Ok(active) => match prune_stale_recall_message(first.clone(), &active) {
                Some(pruned) => rebuilt[0] = pruned,
                None => {
                    rebuilt.remove(0);
                }
            },
            Err(err) => {
                tracing::warn!(
                    "run {run_id}: could not re-validate recalled memory; dropping the recall block for this request: {err}"
                );
                rebuilt.remove(0);
            }
        }
        normalize_for_provider(&rebuilt)
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
    ) {
        let run_id = run.id.clone();
        let mut usage_total = zero_usage();

        for _ in 0..MAX_ITERATIONS {
            // Recomputed each turn: in plan mode only the read-only subset plus
            // `propose_plan` is offered; the instant a plan is approved
            // `plan_mode` flips false and the full set is advertised (H8).
            // H1/M4: `messages` itself (the persisted/growing thread) is
            // re-validated against the CURRENT ledger and reshaped fresh
            // every iteration via `request_messages_for` — request-only,
            // never applied back to `messages` itself.
            let request = CompletionRequest {
                messages: self.request_messages_for(&messages, &run_id).await,
                model: run.input.model_override.clone(),
                tools: self.tool_specs_for(plan_mode),
                response_format: None,
                max_tokens: None,
                disable_thinking: false,
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
                        ModelError::Cancelled => ("internal", err.to_string()),
                    };
                    self.finish_failed(&run_id, code, &message).await;
                    return;
                }
            };
            usage_total = add_usage(usage_total, &res.usage);

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
                tokio::select! {
                    _ = self.remember_exchange(
                        run.session_id.as_deref(),
                        &run.input.prompt,
                        &text,
                        run_prompt_origin(&run),
                    ) => {},
                    () = cancel.cancelled() => {
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
                }
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
                            run_id,
                            output: RunOutputPayload { text },
                            usage: usage_total,
                        }));
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
            let _ = self.persist_message(&run_id, &res.message).await;
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
                    let _ = self.persist_message(&run_id, &skipped).await;
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
                            let _ = self.persist_message(&run_id, &result).await;
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
                    let _ = self.persist_message(&run_id, &denied).await;
                    messages.push(denied);
                    continue;
                }
                match self.run_tool_call(&run, call, &cancel, false).await {
                    Ok(content) => {
                        let result = Msg::tool_result(call.id.clone(), content);
                        let _ = self.persist_message(&run_id, &result).await;
                        messages.push(result);
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
    ) -> Result<String, ParkedCallStop> {
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
            return Ok("tool error: internal persistence failure".to_owned());
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
        Ok(content)
    }

    /// The tool specs advertised this turn: the full advert set normally, or the
    /// read-only subset plus `propose_plan` in plan mode (H8).
    fn tool_specs_for(&self, plan_mode: bool) -> Vec<ToolSpec> {
        let adverts = if plan_mode {
            self.tools.plan_adverts()
        } else {
            self.tools.adverts()
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
        match self.store.active_workspace_run_lease_id(run_id).await? {
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
        }
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
            Ok(_) => self.sink.emit(EventBody::RunCompleted(RunCompletedPayload {
                run_id: run_id.to_owned(),
                output: RunOutputPayload {
                    text: text.to_owned(),
                },
                usage,
            })),
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
            Ok(_) => self.sink.emit(EventBody::RunFailed(RunFailedPayload {
                run_id: run_id.to_owned(),
                error: body,
            })),
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
            Ok(_) => self.sink.emit(EventBody::RunCancelled(RunCancelledPayload {
                run_id: run_id.to_owned(),
            })),
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
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    mod session_memory_tests {
        include!("session_memory_tests.rs");
    }
    mod session_failure_tests {
        include!("session_failure_tests.rs");
    }

    use super::*;
    use agent24_memory::{KvStore, session::CompactionPolicy};
    use agent24_models::router::Tier;
    use agent24_models::{CompletionResponse, ModelProvider, ToolCallRequest};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    #[tokio::test]
    async fn resumed_thread_cannot_rebuild_remote_permission_from_default_profile() {
        let remote = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let store = Store::open_memory().await.unwrap();
        let manager = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                remote.clone(),
                Tier::Remote,
            )])),
            Arc::new(ToolRegistry::new()),
            Arc::new(RecordingSink(StdMutex::new(vec![]))),
            CancellationToken::new(),
        );
        let mut run = manager.start_run(create()).await.unwrap();
        let _ = wait_terminal(&store, &run.id).await;
        sqlx::query("UPDATE runs SET status='running', ended_at=NULL WHERE id=?")
            .bind(&run.id)
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        run.status = RunStatus::Running;
        manager
            .run_loop(
                run,
                vec![Msg::user("thread text cannot authorize cloud")],
                false,
                CancellationToken::new(),
            )
            .await;
        assert!(remote.seen.lock().unwrap().is_empty());
    }

    #[test]
    fn cloud_authorized_policy_preserves_the_base_profile() {
        use agent24_models::router::Privacy;

        let shareable = TaskProfile::default();
        assert_eq!(
            merge_source_policy(shareable, agent24_store::SourceMode::CloudAuthorized).privacy,
            Privacy::Any
        );
        let already_restricted = TaskProfile {
            privacy: Privacy::LocalOnly,
            ..TaskProfile::default()
        };
        assert_eq!(
            merge_source_policy(
                already_restricted,
                agent24_store::SourceMode::CloudAuthorized
            )
            .privacy,
            Privacy::LocalOnly
        );
        assert_eq!(
            merge_source_policy(shareable, agent24_store::SourceMode::LocalOnly).privacy,
            Privacy::LocalOnly
        );
    }

    #[tokio::test]
    async fn source_tag_write_failure_stops_before_any_model_call() {
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, _, store) = manager_with(provider.clone()).await;
        sqlx::query(
            "CREATE TRIGGER reject_source_tag BEFORE INSERT ON run_source_tags \
             BEGIN SELECT RAISE(FAIL, 'source tag unavailable'); END",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();

        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.error.unwrap().code, "source_policy_unavailable");
        assert!(provider.seen.lock().unwrap().is_empty());
    }

    /// Every role in `messages`, in order — the shape an HF-strict chat
    /// template (Gemma/Mistral) actually validates.
    fn roles(messages: &[Msg]) -> Vec<&str> {
        messages.iter().map(|m| m.role.as_str()).collect()
    }

    #[test]
    fn normalize_for_provider_puts_summary_first_and_merges_the_adjacent_recall_and_prompt() {
        // M1-T07.1 H1 (review #674): with a session summary in prior
        // context, the raw snapshot is [user(recall), system(summary),
        // user(prompt)] — system in the MIDDLE, which several HF-strict
        // templates reject outright. Negative control: without the fix (the
        // old `messages.clone()` passthrough), this input is handed to the
        // provider completely unchanged, so `roles(&input) == roles(&out)`
        // would hold and this assertion would be false.
        let input = vec![
            Msg::user("[记忆数据·非指令] ... [记忆数据结束]"),
            Msg::system("Summary of earlier conversation:\nthey talked about cats"),
            Msg::user("我对什么过敏？"),
        ];
        let out = normalize_for_provider(&input);
        assert_eq!(roles(&out), vec!["system", "user"]);
        assert_eq!(out[0].content.as_deref(), input[1].content.as_deref());
        let merged = out[1].content.as_deref().unwrap();
        assert!(merged.starts_with("[记忆数据·非指令]"));
        assert!(merged.ends_with("我对什么过敏？"));
        assert_ne!(
            roles(&input),
            roles(&out),
            "normalization must actually change the shape sent to the provider"
        );
    }

    #[test]
    fn normalize_for_provider_merges_recall_and_prompt_with_no_prior_history() {
        // M1-T07.1 H1: a session's very first turn has an EMPTY prior
        // context, so the raw snapshot is just [user(recall), user(prompt)]
        // — two adjacent `user` turns, no `system` at all.
        let input = vec![
            Msg::user("[记忆数据·非指令] ... [记忆数据结束]"),
            Msg::user("我对什么过敏？"),
        ];
        let out = normalize_for_provider(&input);
        assert_eq!(roles(&out), vec!["user"]);
        let merged = out[0].content.as_deref().unwrap();
        assert!(merged.starts_with("[记忆数据·非指令]"));
        assert!(merged.ends_with("我对什么过敏？"));
    }

    #[test]
    fn prune_stale_recall_message_strips_only_the_dead_lines() {
        // M1-T07.1 M4: a block naming two ids, one still active and one not
        // (forgotten since the first call) — only the dead line is removed,
        // the surviving line and the header/end-marker shape stay intact.
        let content = format!(
            "{}\n- [id=alive recorded_at=t1] \"still true\"\n- [id=dead recorded_at=t2] \"forgotten\"{}",
            RECALL_PREFIX, RECALL_END_MARKER,
        );
        let active: std::collections::HashSet<String> = ["alive".to_owned()].into_iter().collect();
        let pruned = prune_stale_recall_message(Msg::user(content), &active).unwrap();
        let text = pruned.content.unwrap();
        assert!(text.contains("alive"));
        assert!(text.contains("still true"));
        assert!(!text.contains("dead"), "{text}");
        assert!(!text.contains("forgotten"), "{text}");
        assert!(text.ends_with(RECALL_END_MARKER));
    }

    #[test]
    fn prune_stale_recall_message_drops_the_whole_block_once_every_id_is_dead() {
        let content = format!(
            "{}\n- [id=dead recorded_at=t1] \"forgotten\"{}",
            RECALL_PREFIX, RECALL_END_MARKER,
        );
        let active = std::collections::HashSet::new();
        assert!(prune_stale_recall_message(Msg::user(content), &active).is_none());
    }

    #[test]
    fn prune_stale_recall_message_never_touches_an_ordinary_message() {
        let msg = Msg::user("我对什么过敏？");
        let active = std::collections::HashSet::new();
        assert_eq!(prune_stale_recall_message(msg.clone(), &active), Some(msg));
    }

    #[test]
    fn prune_stale_recall_message_preserves_rather_than_drops_a_malformed_lookalike() {
        // Round 2 ②: a message that starts with the exact recall header but
        // does NOT end with the end marker is not a real recall block — the
        // OLD `?`-propagating code would have silently DELETED such a
        // message (treating "cannot parse" as "fully stale, drop it");
        // fail-safe here means PRESERVE what we cannot confidently parse.
        let msg = Msg::user(format!("{RECALL_PREFIX}... but not a real block"));
        let active = std::collections::HashSet::new();
        assert_eq!(prune_stale_recall_message(msg.clone(), &active), Some(msg));
    }

    #[test]
    fn normalize_for_provider_leaves_an_already_alternating_thread_untouched() {
        // The common tool-calling tail (assistant → tool → assistant) never
        // has two adjacent same-role turns, so normalization must be a
        // structural no-op there — only the snapshot PREFIX ever needs it.
        let input = vec![Msg::user("hi"), Msg::assistant(Some("ok".into()), vec![])];
        assert_eq!(normalize_for_provider(&input), input);
    }

    #[test]
    fn workspace_agent_clock_preserves_fractional_millis() {
        let now = std::time::UNIX_EPOCH + Duration::from_millis(605);
        assert_eq!(workspace_now_at(now).unwrap(), "1970-01-01T00:00:00.605Z");
    }

    struct RecordingSink(StdMutex<Vec<String>>);

    impl EventSink for RecordingSink {
        fn emit(&self, body: EventBody) {
            if let Ok(mut v) = self.0.lock() {
                v.push(body.wire_type().to_owned());
            }
        }
    }

    fn usage_one() -> Usage {
        Usage {
            prompt_tokens: 1,
            completion_tokens: 1,
            total_tokens: 2,
            cost_usd: 0.0,
        }
    }

    struct FixedProvider;

    #[async_trait]
    impl ModelProvider for FixedProvider {
        fn name(&self) -> &str {
            "fixed"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Ok(CompletionResponse {
                message: Msg::assistant(Some("pong".to_owned()), vec![]),
                usage: usage_one(),
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    /// Answers "pong" and records the messages it was handed, so a test can
    /// assert what context the loop actually built.
    struct RecordingProvider {
        seen: StdMutex<Vec<Vec<Msg>>>,
    }

    #[async_trait]
    impl ModelProvider for RecordingProvider {
        fn name(&self) -> &str {
            "recording"
        }
        async fn complete(
            &self,
            req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            self.seen.lock().unwrap().push(req.messages.clone());
            Ok(CompletionResponse {
                message: Msg::assistant(Some("pong".to_owned()), vec![]),
                usage: usage_one(),
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    /// A summarizer that must never be reached in tests that stay under the
    /// compaction threshold — calling it is the failure.
    struct UnusedSummarizer;

    #[async_trait]
    impl Summarizer for UnusedSummarizer {
        async fn summarize(
            &self,
            _prior: Option<&str>,
            _messages: &[Msg],
        ) -> std::result::Result<String, String> {
            Err("summarizer should not be needed in this test".to_owned())
        }
    }

    /// Plays a fixed sequence of assistant turns, then echoes the last tool
    /// result as the final answer.
    pub(crate) struct ScriptedProvider {
        turns: StdMutex<Vec<Msg>>,
    }

    impl ScriptedProvider {
        pub(crate) fn new(turns: Vec<Msg>) -> Self {
            Self {
                turns: StdMutex::new(turns),
            }
        }
    }

    #[async_trait]
    impl ModelProvider for ScriptedProvider {
        fn name(&self) -> &str {
            "scripted"
        }
        async fn complete(
            &self,
            req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            let next = self.turns.lock().unwrap().pop();
            let message = match next {
                Some(turn) => turn,
                None => {
                    // Script exhausted: answer with the last tool result
                    let last_tool = req
                        .messages
                        .iter()
                        .rev()
                        .find(|m| m.role == "tool")
                        .and_then(|m| m.content.clone())
                        .unwrap_or_else(|| "no tool result".to_owned());
                    Msg::assistant(Some(format!("tool said: {last_tool}")), vec![])
                }
            };
            Ok(CompletionResponse {
                message,
                usage: usage_one(),
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct HangingProvider;

    #[async_trait]
    impl ModelProvider for HangingProvider {
        fn name(&self) -> &str {
            "hanging"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            cancel.cancelled().await;
            Err(ModelError::Cancelled)
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    async fn manager_with_tools(
        provider: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
    ) -> (Arc<RunManager>, Arc<RecordingSink>, Store) {
        let store = Store::open_memory().await.unwrap();
        let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
        let manager = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
            Arc::new(tools),
            sink.clone(),
            CancellationToken::new(),
        );
        (manager, sink, store)
    }

    struct FixtureEgressGate;

    #[async_trait]
    impl agent24_domain::EgressGate for FixtureEgressGate {
        async fn check(
            &self,
            request: &agent24_domain::EgressRequest,
        ) -> Result<(), agent24_domain::EgressDecision> {
            let fixture_resource = request.resources.len() == 1
                && request.resources[0].resource_id == "agent-test-fixture"
                && request.resources[0].revision.as_deref() == Some("fixture-rev-1")
                && request.resources[0].authorization_ref.as_deref()
                    == Some("agent-test-fixture-grant")
                && request.resources[0].policy_version == 1
                && !request.resources[0].local_only;
            let fixture_destination = request
                .destination
                .id()
                .is_some_and(|id| id.starts_with("http://127.0.0.1:"));
            if request.purpose == agent24_domain::EgressPurpose::HttpFetch
                && request.remote
                && request.authorization_generation == 1
                && fixture_resource
                && fixture_destination
            {
                Ok(())
            } else {
                Err(agent24_domain::EgressDecision)
            }
        }
    }

    pub(crate) fn fixture_egress_policy() -> (
        Arc<dyn agent24_domain::EgressGate>,
        Vec<agent24_domain::EgressResource>,
        u64,
    ) {
        (
            Arc::new(FixtureEgressGate),
            vec![
                agent24_domain::EgressResource::cloud_authorized(
                    "agent-test-fixture",
                    "fixture-rev-1",
                    1,
                )
                .with_authorization_ref("agent-test-fixture-grant"),
            ],
            1,
        )
    }

    async fn manager_with_fixture_egress(
        provider: Arc<dyn ModelProvider>,
        tools: ToolRegistry,
    ) -> (Arc<RunManager>, Arc<RecordingSink>, Store) {
        let (manager, sink, store) = manager_with_tools(provider, tools).await;
        let (gate, resources, generation) = fixture_egress_policy();
        (
            manager.with_test_egress_policy(gate, resources, generation),
            sink,
            store,
        )
    }

    async fn manager_with(
        provider: Arc<dyn ModelProvider>,
    ) -> (Arc<RunManager>, Arc<RecordingSink>, Store) {
        manager_with_tools(provider, ToolRegistry::new()).await
    }

    /// A manager with D1 session memory attached (in-memory KV).
    async fn manager_with_memory(
        provider: Arc<dyn ModelProvider>,
        summarizer: Arc<dyn Summarizer>,
    ) -> (Arc<RunManager>, Store, KvStore) {
        let store = Store::open_memory().await.unwrap();
        let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
        let kv = KvStore::open_memory().await.unwrap();
        let manager = RunManager::with_memory(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
            Arc::new(ToolRegistry::new()),
            sink,
            CancellationToken::new(),
            Some(SessionMemory::new(kv.clone(), summarizer).with_owner("test-owner".into())),
        );
        (manager, store, kv)
    }

    /// Insert a session row so `start_run` accepts it.
    async fn seed_session(store: &Store, id: &str) {
        let now = now_iso8601();
        store
            .insert_session(&agent24_protocol::Session {
                id: id.to_owned(),
                title: "t".to_owned(),
                channel: "cli".to_owned(),
                workspace_id: None,
                created_at: now.clone(),
                updated_at: now,
            })
            .await
            .unwrap();
    }

    async fn seed_workspace(store: &Store, id: &str) {
        let now = workspace_timestamp(now_iso8601());
        let expires_at = WorkspaceInstant::parse(&now)
            .unwrap()
            .checked_add_workspace_ttl(
                agent24_store::WorkspaceTtl::new(6 * 24 * 60 * 60 * 1000).unwrap(),
            )
            .unwrap();
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/test/workspace','generation-1','unix',X'0101010101010101',X'0202020202020202')")
            .bind(id).bind(&now).bind(expires_at.as_str()).execute(agent24_store::test_hooks::pool(store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5',?,'generation-1','root','unix',X'0303030303030303',X'0404040404040404','unix',X'0101010101010101',X'0202020202020202','committed',?)")
            .bind(id).bind(&now).execute(agent24_store::test_hooks::pool(store)).await.unwrap();
    }

    #[tokio::test]
    async fn workspace_run_admission_and_terminal_release_are_active() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound".into(),
                title: "bound".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: "2026-09-19T00:00:00.000Z".into(),
                updated_at: "2026-09-19T00:00:00.000Z".into(),
            })
            .await
            .unwrap();
        let first = manager
            .start_run(RunCreate {
                session_id: Some("bound".into()),
                workspace_id: Some(workspace_id.clone()),
                prompt: "workspace run".into(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        let done = wait_terminal(&store, &first.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        let first_release: Option<String> = sqlx::query_scalar(
            "SELECT released_at FROM workspace_leases WHERE owner_id=? AND kind='run'",
        )
        .bind(&first.id)
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        assert!(first_release.is_some());

        let second = manager
            .start_run(RunCreate {
                session_id: Some("bound".into()),
                workspace_id: Some(workspace_id),
                prompt: "workspace run two".into(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        assert_eq!(
            wait_terminal(&store, &second.id).await.status,
            RunStatus::Completed
        );

        let error = manager
            .start_run(RunCreate {
                session_id: Some("bound".into()),
                workspace_id: None,
                prompt: "must not downgrade".into(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AgentError::Store(StoreError::Conflict(message))
                if message == "workspace-bound sessions require atomic admission"
        ));
    }

    #[tokio::test]
    async fn workspace_bound_tool_call_never_downgrades_without_authority_service() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let tools = ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(true)));
        let provider = ScriptedProvider::new(vec![tool_call_turn(
            "http_fetch",
            serde_json::json!({ "url": "http://127.0.0.1:9/" }).to_string(),
        )]);
        let (manager, _sink, store) = manager_with_tools(Arc::new(provider), tools).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound-tool".into(),
                title: "bound-tool".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: now_iso8601(),
                updated_at: now_iso8601(),
            })
            .await
            .unwrap();
        let run = manager
            .start_run(RunCreate {
                session_id: Some("bound-tool".into()),
                workspace_id: Some(workspace_id),
                prompt: "try a tool".into(),
                model_override: None,
                mode: RunMode::Normal,
            })
            .await
            .unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        assert!(
            done.output
                .unwrap()
                .text
                .contains("workspace authority unavailable")
        );
        assert_eq!(
            store.list_tool_calls(&run.id).await.unwrap()[0].status,
            ToolCallStatus::Denied
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn workspace_bound_fs_uses_fresh_pinned_root_not_legacy_root() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
        let store = Store::open_memory().await.unwrap();
        let state = tempfile::tempdir().unwrap();
        let service = Arc::new(WorkspaceService::compose(store.clone(), state.path()).unwrap());
        let locator = format!("{WS}.g1");
        let parent = state.path().join("workspace-roots");
        let root = parent.join(&locator);
        std::fs::create_dir(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let parent_meta = std::fs::metadata(&parent).unwrap();
        let root_meta = std::fs::metadata(&root).unwrap();
        let now = workspace_timestamp(now_iso8601());
        let expires = WorkspaceInstant::parse(&now)
            .unwrap()
            .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
            .unwrap();
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,?,'g1','unix',?,?)")
            .bind(WS).bind(&now).bind(expires.as_str()).bind(root.to_str().unwrap())
            .bind(root_meta.dev().to_le_bytes().to_vec()).bind(root_meta.ino().to_le_bytes().to_vec())
            .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5',?,'g1',?,'unix',?,?,'unix',?,?,'committed',?)")
            .bind(WS).bind(&locator).bind(parent_meta.dev().to_le_bytes().to_vec())
            .bind(parent_meta.ino().to_le_bytes().to_vec()).bind(root_meta.dev().to_le_bytes().to_vec())
            .bind(root_meta.ino().to_le_bytes().to_vec()).bind(&now)
            .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
        sqlx::query("INSERT INTO sessions (id,title,channel,workspace_id,created_at,updated_at) VALUES ('s','s','desktop',?,?,?)")
            .bind(WS).bind(&now).bind(&now).execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
        sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at) VALUES ('r','s',?,'running',?,?,?)")
            .bind(WS).bind(format!(r#"{{"prompt":"go","workspace_id":"{WS}","model_override":null,"mode":"normal"}}"#))
            .bind(r#"{"prompt_tokens":0,"completion_tokens":0,"total_tokens":0,"cost_usd":0.0}"#).bind(&now)
            .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','r','run',?)")
            .bind(LEASE).bind(WS).bind(&now).execute(agent24_store::test_hooks::pool(&store)).await.unwrap();
        let authority = service
            .bind_run_authority("r", &WorkspaceLeaseId::parse(LEASE).unwrap())
            .await
            .unwrap();
        let ctx = ToolContext::workspace_bound("r", Some("s".into()), None, "tc", authority);
        let legacy = tempfile::tempdir().unwrap();
        let read = agent24_tools::FsReadTool::new(vec![legacy.path().to_path_buf()]);
        let write = agent24_tools::FsWriteTool::new(vec![legacy.path().to_path_buf()]);
        let path = root.join("bound.txt");
        std::fs::write(&path, "bound").unwrap();
        let mut input = serde_json::Map::new();
        input.insert(
            "path".into(),
            serde_json::Value::String(path.to_string_lossy().into()),
        );
        assert_eq!(
            agent24_tools::Tool::call(&read, &ctx, &input, &CancellationToken::new())
                .await
                .unwrap(),
            "bound"
        );
        input.insert(
            "content".into(),
            serde_json::Value::String("updated".into()),
        );
        agent24_tools::Tool::call(&write, &ctx, &input, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "updated");
        let shell = agent24_tools::ShellExecTool::new(legacy.path().to_path_buf());
        let mut shell_input = serde_json::Map::new();
        shell_input.insert("argv".into(), serde_json::json!(["/bin/pwd"]));
        let shell_out =
            agent24_tools::Tool::call(&shell, &ctx, &shell_input, &CancellationToken::new())
                .await
                .unwrap();
        let shell_json: serde_json::Value = serde_json::from_str(&shell_out).unwrap();
        assert_eq!(shell_json["exit_code"], 0);
        assert_eq!(
            std::path::Path::new(shell_json["stdout"].as_str().unwrap().trim()),
            root.canonicalize().unwrap()
        );
        sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?")
            .bind(workspace_timestamp(now_iso8601()))
            .bind(LEASE)
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(
            agent24_tools::Tool::call(&read, &ctx, &input, &CancellationToken::new())
                .await
                .is_err()
        );
        assert!(
            agent24_tools::Tool::call(&shell, &ctx, &shell_input, &CancellationToken::new(),)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn stale_legacy_tool_context_rejects_current_bound_run() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let created_at = workspace_timestamp(now_iso8601());
        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: None,
            workspace_id: None,
            status: RunStatus::Running,
            input: RunInput {
                prompt: "legacy".into(),
                workspace_id: None,
                model_override: None,
                mode: RunMode::Normal,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: created_at.clone(),
            started_at: Some(created_at.clone()),
            ended_at: None,
        };
        store.insert_run(&run).await.unwrap();
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        let mut bound_input = run.input.clone();
        bound_input.workspace_id = Some(workspace_id.clone());
        sqlx::query("UPDATE runs SET workspace_id=?, input=? WHERE id=?")
            .bind(workspace_id.as_str())
            .bind(serde_json::to_string(&bound_input).unwrap())
            .bind(&run.id)
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,?,?,'run',?)")
            .bind(format!("wl_{}", ulid())).bind(WORKSPACE_ID).bind("generation-1")
            .bind(&run.id).bind(&created_at)
            .execute(agent24_store::test_hooks::pool(&store)).await.unwrap();

        assert!(matches!(
            manager.tool_context_for(&run, "tc").await,
            Err(AgentError::Workspace(WorkspaceStoreError::CorruptRow {
                table: "runs",
                field: "workspace_id"
            }))
        ));
    }

    #[tokio::test]
    async fn workspace_admission_error_cleans_cancel_token() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound".into(),
                title: "bound".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: now_iso8601(),
                updated_at: now_iso8601(),
            })
            .await
            .unwrap();
        sqlx::query("DROP TABLE workspace_allocations")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(matches!(
            manager
                .start_run(RunCreate {
                    session_id: Some("bound".into()),
                    workspace_id: Some(workspace_id),
                    prompt: "fail admission".into(),
                    model_override: None,
                    mode: RunMode::Normal,
                })
                .await,
            Err(AgentError::Workspace(WorkspaceStoreError::Database))
        ));
        assert!(manager.cancels.lock().await.is_empty());
        assert!(store.list_runs(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn workspace_terminal_fails_closed_when_exact_lease_is_missing() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound".into(),
                title: "bound".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: "2026-09-29T00:00:00.000Z".into(),
                updated_at: "2026-09-29T00:00:00.000Z".into(),
            })
            .await
            .unwrap();
        let created_at = workspace_timestamp(now_iso8601());
        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: Some("bound".into()),
            workspace_id: Some(workspace_id.clone()),
            status: RunStatus::Queued,
            input: RunInput {
                prompt: "go".into(),
                workspace_id: Some(workspace_id),
                model_override: None,
                mode: RunMode::Normal,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: created_at.clone(),
            started_at: None,
            ended_at: None,
        };
        let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid())).unwrap();
        assert!(matches!(
            store
                .insert_run_with_workspace_admission(
                    &run,
                    Some(lease_id.clone()),
                    &WorkspaceInstant::parse(&created_at).unwrap(),
                )
                .await
                .unwrap(),
            RunAdmission::Admitted { lease_id: Some(_) }
        ));
        store
            .transition_run(
                &run.id,
                RunStatus::Running,
                RunPatch {
                    started_at: Some(created_at),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?")
            .bind(lease_id.as_str())
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        manager.finish_cancelled(&run.id).await;

        assert_eq!(
            store.get_run(&run.id).await.unwrap().unwrap().status,
            RunStatus::Running
        );
        assert!(
            !sink
                .0
                .lock()
                .unwrap()
                .iter()
                .any(|event| event == "run.cancelled")
        );
    }

    #[tokio::test]
    async fn cancelling_workspace_run_releases_exact_lease() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let (manager, _sink, store) = manager_with(Arc::new(HangingProvider)).await;
        seed_workspace(&store, WORKSPACE_ID).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
        store
            .insert_session(&agent24_protocol::Session {
                id: "bound".into(),
                title: "bound".into(),
                channel: "desktop".into(),
                workspace_id: Some(workspace_id.clone()),
                created_at: "2026-09-29T00:00:00.000Z".into(),
                updated_at: "2026-09-29T00:00:00.000Z".into(),
            })
            .await
            .unwrap();
        let run = manager
            .start_run(RunCreate {
                session_id: Some("bound".into()),
                workspace_id: Some(workspace_id),
                prompt: "hang".into(),
                model_override: None,
                mode: RunMode::Normal,
            })
            .await
            .unwrap();
        manager.cancel_run(&run.id).await.unwrap();
        assert_eq!(
            wait_terminal(&store, &run.id).await.status,
            RunStatus::Cancelled
        );
        let released: Option<String> = sqlx::query_scalar(
            "SELECT released_at FROM workspace_leases WHERE owner_id=? AND kind='run'",
        )
        .bind(&run.id)
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        assert!(released.is_some());
    }

    #[tokio::test]
    async fn tokenless_workspace_cancel_recovers_active_released_and_missing_lease() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        for mode in 0..4 {
            let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
            seed_workspace(&store, WORKSPACE_ID).await;
            let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
            store
                .insert_session(&agent24_protocol::Session {
                    id: "bound".into(),
                    title: "bound".into(),
                    channel: "desktop".into(),
                    workspace_id: Some(workspace_id.clone()),
                    created_at: "2026-09-29T00:00:00.000Z".into(),
                    updated_at: "2026-09-29T00:00:00.000Z".into(),
                })
                .await
                .unwrap();
            let created_at = workspace_timestamp(now_iso8601());
            let run = Run {
                id: format!("run_{}", ulid()),
                session_id: Some("bound".into()),
                workspace_id: Some(workspace_id.clone()),
                status: RunStatus::Queued,
                input: RunInput {
                    prompt: "stale".into(),
                    workspace_id: Some(workspace_id),
                    model_override: None,
                    mode: RunMode::Normal,
                },
                output: None,
                error: None,
                usage: zero_usage(),
                schedule_id: None,
                created_at: created_at.clone(),
                started_at: None,
                ended_at: None,
            };
            let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid())).unwrap();
            assert!(matches!(
                store
                    .insert_run_with_workspace_admission(
                        &run,
                        Some(lease_id.clone()),
                        &WorkspaceInstant::parse(&created_at).unwrap(),
                    )
                    .await
                    .unwrap(),
                RunAdmission::Admitted { lease_id: Some(_) }
            ));
            store
                .transition_run(
                    &run.id,
                    RunStatus::Running,
                    RunPatch {
                        started_at: Some(created_at.clone()),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            if mode == 1 {
                sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?")
                    .bind(&created_at)
                    .bind(lease_id.as_str())
                    .execute(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
            } else if mode == 2 {
                sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?")
                    .bind(lease_id.as_str())
                    .execute(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
            } else if mode == 3 {
                sqlx::query("UPDATE runs SET input=json_set(input,'$.workspace_id',?) WHERE id=?")
                    .bind("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7")
                    .bind(&run.id)
                    .execute(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
                assert!(matches!(
                    manager.cancel_run(&run.id).await,
                    Err(AgentError::Store(StoreError::Conflict(message)))
                        if message == "run workspace identity mismatch"
                ));
                let raw_status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id=?")
                    .bind(&run.id)
                    .fetch_one(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
                assert_eq!(raw_status, "running");
                assert!(
                    !sink
                        .0
                        .lock()
                        .unwrap()
                        .iter()
                        .any(|event| event.as_str() == "run.cancelled")
                );
                continue;
            }

            assert_eq!(
                manager.cancel_run(&run.id).await.unwrap().status,
                RunStatus::Cancelled
            );
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM workspace_leases WHERE owner_id=? AND kind='run' AND released_at IS NULL",
            )
            .bind(&run.id)
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
            assert_eq!(active, 0);
            assert_eq!(
                sink.0
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|event| event.as_str() == "run.cancelled")
                    .count(),
                1
            );
        }
    }

    #[tokio::test]
    async fn timed_out_recovery_handles_released_and_missing_workspace_lease_history() {
        const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        for missing in [false, true] {
            let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
            seed_workspace(&store, WORKSPACE_ID).await;
            let workspace_id = agent24_protocol::WorkspaceId::parse(WORKSPACE_ID).unwrap();
            store
                .insert_session(&agent24_protocol::Session {
                    id: "bound".into(),
                    title: "bound".into(),
                    channel: "desktop".into(),
                    workspace_id: Some(workspace_id.clone()),
                    created_at: "2026-09-29T00:00:00.000Z".into(),
                    updated_at: "2026-09-29T00:00:00.000Z".into(),
                })
                .await
                .unwrap();
            let created_at = workspace_timestamp(now_iso8601());
            let run = Run {
                id: format!("run_{}", ulid()),
                session_id: Some("bound".into()),
                workspace_id: Some(workspace_id.clone()),
                status: RunStatus::Queued,
                input: RunInput {
                    prompt: "stale approval".into(),
                    workspace_id: Some(workspace_id),
                    model_override: None,
                    mode: RunMode::Normal,
                },
                output: None,
                error: None,
                usage: zero_usage(),
                schedule_id: None,
                created_at: created_at.clone(),
                started_at: None,
                ended_at: None,
            };
            let lease_id = WorkspaceLeaseId::parse(&format!("wl_{}", ulid())).unwrap();
            assert!(matches!(
                store
                    .insert_run_with_workspace_admission(
                        &run,
                        Some(lease_id.clone()),
                        &WorkspaceInstant::parse(&created_at).unwrap(),
                    )
                    .await
                    .unwrap(),
                RunAdmission::Admitted { lease_id: Some(_) }
            ));
            sqlx::query("UPDATE runs SET status='awaiting_approval', started_at=? WHERE id=?")
                .bind(&created_at)
                .bind(&run.id)
                .execute(agent24_store::test_hooks::pool(&store))
                .await
                .unwrap();
            let approval = Approval {
                id: format!("apr_{}", ulid()),
                run_id: run.id.clone(),
                tool_call_id: format!("tc_{}", ulid()),
                kind: "exec".into(),
                summary: "expired".into(),
                payload: serde_json::Map::new(),
                available_decisions: vec!["approve".into(), "deny".into(), "abort".into()],
                standing_target: None,
                status: ApprovalStatus::TimedOut,
                decision: None,
                expires_at: created_at.clone(),
                created_at: created_at.clone(),
                decided_at: Some(created_at.clone()),
            };
            store.insert_approval(&approval).await.unwrap();
            if missing {
                sqlx::query("DELETE FROM workspace_leases WHERE lease_id=?")
                    .bind(lease_id.as_str())
                    .execute(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
            } else {
                sqlx::query("UPDATE workspace_leases SET released_at=? WHERE lease_id=?")
                    .bind(&created_at)
                    .bind(lease_id.as_str())
                    .execute(agent24_store::test_hooks::pool(&store))
                    .await
                    .unwrap();
            }

            assert_eq!(manager.recover_timed_out_approval_runs().await.unwrap(), 1);
            assert_eq!(
                store.get_run(&run.id).await.unwrap().unwrap().status,
                RunStatus::Cancelled
            );
            let active: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM workspace_leases WHERE owner_id=? AND kind='run' AND released_at IS NULL",
            )
            .bind(&run.id)
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
            assert_eq!(active, 0);
            let seen = sink.0.lock().unwrap().clone();
            assert_eq!(
                seen.iter()
                    .filter(|event| event.as_str() == "run.cancelled")
                    .count(),
                1
            );
            assert!(!seen.iter().any(|event| event.as_str() == "run.started"));
        }
    }

    /// Run one prompt in a session and wait for it to reach a terminal state.
    async fn run_in_session(
        manager: &Arc<RunManager>,
        store: &Store,
        session_id: &str,
        prompt: &str,
    ) {
        let run = manager
            .start_run(RunCreate {
                session_id: Some(session_id.to_owned()),
                workspace_id: None,
                prompt: prompt.to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        for _ in 0..200 {
            let current = store.get_run(&run.id).await.unwrap().unwrap();
            if current.status != RunStatus::Running && current.status != RunStatus::Queued {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("run did not finish");
    }

    #[tokio::test]
    async fn a_session_remembers_across_runs() {
        // D1 made live: without memory every run starts from the bare prompt.
        // With it, the second run in a session must SEE the first exchange.
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, _kv) =
            manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_mem").await;

        run_in_session(&manager, &store, "sess_mem", "first question").await;
        run_in_session(&manager, &store, "sess_mem", "second question").await;

        let seen = provider.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "expected one completion per run");
        // Run 1 saw only its own prompt.
        assert_eq!(seen[0].len(), 1);
        assert_eq!(seen[0][0].content.as_deref(), Some("first question"));
        // Run 2 saw the remembered exchange BEFORE its own prompt.
        let second = &seen[1];
        assert!(
            second.len() > 1,
            "second run had no prior context: {second:?}"
        );
        let texts: Vec<&str> = second.iter().filter_map(|m| m.content.as_deref()).collect();
        assert!(texts.contains(&"first question"), "{texts:?}");
        assert!(texts.contains(&"pong"), "{texts:?}");
        assert_eq!(texts.last(), Some(&"second question"));
    }

    #[tokio::test]
    async fn a_run_that_never_parks_sheds_its_copy_of_prior_context_once_terminal() {
        // M1-T07.1 M3 (growth control, review #674): the snapshot persists a
        // COPY of the session's prior context into every run's own
        // `run_messages` row set, so a long session would otherwise grow
        // that table ~O(session length) on every single turn. Once a run
        // that never parked for approval reaches a terminal state, its copy
        // of prior context — pure duplication of what `SessionLog` already
        // holds — is reclaimed; its OWN prompt/answer stay.
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, _kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_shed").await;

        let run1 = manager
            .start_run(RunCreate {
                session_id: Some("sess_shed".to_owned()),
                workspace_id: None,
                prompt: "first question".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        let run1 = wait_terminal(&store, &run1.id).await;
        assert_eq!(run1.status, RunStatus::Completed);
        // Run 1 had no prior context of its own (first turn in the
        // session) — nothing to shed, its own thread is untouched.
        let thread1 = store.list_run_messages(&run1.id).await.unwrap();
        assert_eq!(
            thread1.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant"]
        );

        let run2 = manager
            .start_run(RunCreate {
                session_id: Some("sess_shed".to_owned()),
                workspace_id: None,
                prompt: "second question".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        let run2 = wait_terminal(&store, &run2.id).await;
        assert_eq!(run2.status, RunStatus::Completed);
        // The cleanup runs in `execute()`'s OWN task, AFTER `run_loop`
        // returns and AFTER the terminal transition is already visible — it
        // is eventual, not atomic with "status == Completed". Poll instead
        // of reading the thread the instant the status flips.
        let thread2 = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let thread = store.list_run_messages(&run2.id).await.unwrap();
                if thread.len() == 2 {
                    break thread;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("prior-context cleanup should land shortly after completion");
        // Run 2's copy of run 1's exchange ("first question"/"pong") is
        // gone; its OWN prompt and answer remain. Both turns' assistant
        // reply is literally "pong" (same mock provider), so the shape
        // (exactly 2 rows, not 4) is the real proof — "first question" is
        // the one string unique to run 1's copy.
        let texts: Vec<_> = thread2.iter().filter_map(|m| m.content.clone()).collect();
        assert!(!texts.contains(&"first question".to_owned()), "{texts:?}");
        assert_eq!(texts, vec!["second question", "pong"]);
        assert_eq!(
            thread2.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant"],
            "{thread2:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_runs_in_one_session_keep_both_turns() {
        // remember_exchange appends a numbered turn, and runs execute
        // in background tasks. Without a per-session lock two runs finishing
        // together both load the old state and the later save drops the other's
        // turn. Both exchanges must survive.
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_race").await;

        let mut ids = Vec::new();
        for prompt in ["alpha", "beta"] {
            let run = manager
                .start_run(RunCreate {
                    session_id: Some("sess_race".to_owned()),
                    workspace_id: None,
                    prompt: prompt.to_owned(),
                    model_override: None,
                    mode: agent24_protocol::RunMode::Normal,
                })
                .await
                .unwrap();
            ids.push(run.id);
        }
        for id in &ids {
            wait_terminal(&store, id).await;
        }
        // Memory is committed before completion becomes visible.
        let session = kv
            .session_log()
            .load_view("test-owner", "sess_race")
            .await
            .unwrap();
        let texts: Vec<&str> = session
            .tail
            .iter()
            .filter_map(|(_, m)| m.content.as_deref())
            .collect();
        assert!(texts.contains(&"alpha"), "lost a turn: {texts:?}");
        assert!(texts.contains(&"beta"), "lost a turn: {texts:?}");
        assert_eq!(session.tail.len(), 4, "{texts:?}");
    }

    #[tokio::test]
    async fn many_concurrent_writers_on_one_session_lose_nothing() {
        // Codex (low): the two-run test can pass even unlocked if tokio happens
        // to serialize. Drive remember_exchange directly from many tasks at once
        // so turn number allocation and append must hold the same lock.
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, store, kv) = manager_with_memory(provider, Arc::new(UnusedSummarizer)).await;
        seed_session(&store, "sess_many").await;

        const WRITERS: usize = 12;
        let mut tasks = Vec::new();
        for i in 0..WRITERS {
            let m = Arc::clone(&manager);
            tasks.push(tokio::spawn(async move {
                m.remember_exchange(
                    Some("sess_many"),
                    &format!("q{i}"),
                    &format!("a{i}"),
                    Origin {
                        source: "agent_loop".into(),
                        trust: Trust::UserSaid,
                    },
                )
                .await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let session = kv
            .session_log()
            .load_view("test-owner", "sess_many")
            .await
            .unwrap();
        let texts: Vec<&str> = session
            .tail
            .iter()
            .filter_map(|(_, m)| m.content.as_deref())
            .collect();
        // Every writer's prompt AND answer must have survived.
        for i in 0..WRITERS {
            assert!(
                texts.contains(&format!("q{i}").as_str()),
                "lost q{i}: {texts:?}"
            );
            assert!(
                texts.contains(&format!("a{i}").as_str()),
                "lost a{i}: {texts:?}"
            );
        }
        assert_eq!(session.tail.len(), WRITERS * 2, "{texts:?}");
    }

    /// A summarizer that signals when it is entered and then blocks, so a test
    /// can cancel at a deterministic point INSIDE the memory write.
    struct SlowSummarizer {
        entered: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl Summarizer for SlowSummarizer {
        async fn summarize(
            &self,
            _prior: Option<&str>,
            _messages: &[Msg],
        ) -> std::result::Result<String, String> {
            self.entered.notify_one();
            tokio::time::sleep(SLOW_SUMMARIZER_BLOCK).await;
            Ok("summary".to_owned())
        }
    }

    /// Long enough that completing normally is clearly distinguishable from
    /// being interrupted by the cancel.
    const SLOW_SUMMARIZER_BLOCK: std::time::Duration = std::time::Duration::from_secs(10);

    #[tokio::test]
    async fn cancel_during_the_memory_write_still_cancels_the_run() {
        // Codex (regression I introduced): moving the memory write before
        // transition_run leaves the run non-terminal for up to
        // MEMORY_WRITE_BUDGET. Cancel must still win in that window — "cancel
        // works in any non-terminal state" is the C2 contract.
        let store = Store::open_memory().await.unwrap();
        let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
        let kv = KvStore::open_memory().await.unwrap();
        // max_recent 1 → the very first turn overflows, so compaction (and the
        // slow summarizer) runs inside the memory write.
        let policy = CompactionPolicy {
            max_recent: 1,
            keep_recent: 0,
            max_summary_chars: 500,
        };
        let entered = Arc::new(tokio::sync::Notify::new());
        let manager = RunManager::with_memory(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(FixedProvider),
                Tier::Local,
            )])),
            Arc::new(ToolRegistry::new()),
            sink,
            CancellationToken::new(),
            Some(
                SessionMemory::new(
                    kv,
                    Arc::new(SlowSummarizer {
                        entered: Arc::clone(&entered),
                    }),
                )
                .with_owner("test-owner".into())
                .with_policy(policy),
            ),
        );
        seed_session(&store, "sess_cancel").await;
        let run = manager
            .start_run(RunCreate {
                session_id: Some("sess_cancel".to_owned()),
                workspace_id: None,
                prompt: "hi".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        // Deterministic: wait until the summarizer is actually entered, so the
        // cancel provably lands INSIDE the memory write (not before the run
        // started, and not after it finished).
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("summarizer should have been entered");
        let started = std::time::Instant::now();
        let _ = manager.cancel_run(&run.id).await;
        let final_run = wait_terminal(&store, &run.id).await;
        assert_eq!(
            final_run.status,
            RunStatus::Cancelled,
            "cancel was ignored during the memory write"
        );
        // Latency is the real assertion: without the select! on the cancel token
        // the run would sit until the summarizer returned, so finishing far
        // sooner proves the write was actually interrupted.
        assert!(
            started.elapsed() < SLOW_SUMMARIZER_BLOCK / 2,
            "cancel did not interrupt the memory write (took {:?})",
            started.elapsed()
        );
    }

    #[tokio::test]
    async fn cancel_while_waiting_on_another_runs_session_lock_is_prompt() {
        // Reviewer-found (clestons): session_context() takes the per-session
        // lock BEFORE the model is ever contacted. Run A can hold that lock for
        // up to MEMORY_WRITE_BUDGET while compacting, so Run B parked here must
        // still be cancellable — C2: cancel works in any non-terminal state.
        let store = Store::open_memory().await.unwrap();
        let sink = Arc::new(RecordingSink(StdMutex::new(vec![])));
        let kv = KvStore::open_memory().await.unwrap();
        let entered = Arc::new(tokio::sync::Notify::new());
        // max_recent 1 → run A's very first turn compacts, so its slow
        // summarizer runs while holding the session lock.
        let policy = CompactionPolicy {
            max_recent: 1,
            keep_recent: 0,
            max_summary_chars: 500,
        };
        let manager = RunManager::with_memory(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(FixedProvider),
                Tier::Local,
            )])),
            Arc::new(ToolRegistry::new()),
            sink,
            CancellationToken::new(),
            Some(
                SessionMemory::new(
                    kv,
                    Arc::new(SlowSummarizer {
                        entered: Arc::clone(&entered),
                    }),
                )
                .with_owner("test-owner".into())
                .with_policy(policy),
            ),
        );
        seed_session(&store, "sess_lockwait").await;

        // Run A: proceed until it is inside compaction, holding the lock.
        let run_a = manager
            .start_run(RunCreate {
                session_id: Some("sess_lockwait".to_owned()),
                workspace_id: None,
                prompt: "a".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
            .await
            .expect("run A should have entered the summarizer holding the lock");

        // Run B: blocks in session_context() waiting for A's lock.
        let run_b = manager
            .start_run(RunCreate {
                session_id: Some("sess_lockwait".to_owned()),
                workspace_id: None,
                prompt: "b".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let started = std::time::Instant::now();
        let _ = manager.cancel_run(&run_b.id).await;
        let final_b = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            wait_terminal(&store, &run_b.id),
        )
        .await
        .expect("run B stayed stuck in an uncancellable lock wait");
        assert_eq!(final_b.status, RunStatus::Cancelled, "{final_b:?}");
        assert!(
            started.elapsed() < SLOW_SUMMARIZER_BLOCK / 2,
            "cancel waited out the lock holder ({:?})",
            started.elapsed()
        );
        let _ = run_a;
    }

    #[tokio::test]
    async fn without_memory_runs_do_not_accumulate_context() {
        // The default (no SessionMemory) keeps prior behaviour exactly.
        let provider = Arc::new(RecordingProvider {
            seen: StdMutex::new(vec![]),
        });
        let (manager, _sink, store) = manager_with(provider.clone()).await;
        seed_session(&store, "sess_plain").await;
        run_in_session(&manager, &store, "sess_plain", "first question").await;
        run_in_session(&manager, &store, "sess_plain", "second question").await;
        let seen = provider.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(
            seen[1].len(),
            1,
            "context leaked without memory: {:?}",
            seen[1]
        );
    }

    pub(crate) fn create() -> RunCreate {
        RunCreate {
            session_id: None,
            workspace_id: None,
            prompt: "hi".to_owned(),
            model_override: None,
            mode: agent24_protocol::RunMode::Normal,
        }
    }

    pub(crate) async fn wait_terminal(store: &Store, id: &str) -> Run {
        for _ in 0..100 {
            let run = store.get_run(id).await.unwrap().unwrap();
            if agent24_core::run_is_terminal(run.status) {
                return run;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("run {id} never reached a terminal state");
    }

    #[tokio::test]
    async fn run_completes_with_full_event_sequence() {
        let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
        let run = manager.start_run(create()).await.unwrap();
        assert_eq!(run.status, RunStatus::Queued);
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        assert_eq!(done.output.unwrap().text, "pong");
        assert_eq!(done.usage.total_tokens, 2);
        assert!(done.started_at.is_some() && done.ended_at.is_some());
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events, vec!["run.started", "model.delta", "run.completed"]);
    }

    #[tokio::test]
    async fn cancelling_a_hanging_run_lands_cancelled_within_a_second() {
        let (manager, sink, store) = manager_with(Arc::new(HangingProvider)).await;
        let run = manager.start_run(create()).await.unwrap();
        // Let it reach running
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = std::time::Instant::now();
        manager.cancel_run(&run.id).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Cancelled);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancel was not prompt"
        );
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events, vec!["run.started", "run.cancelled"]);
    }

    #[tokio::test]
    async fn cancel_is_idempotent_on_terminal_runs() {
        let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        // cancel after completion: unchanged, no error
        let after = manager.cancel_run(&run.id).await.unwrap();
        assert_eq!(after.status, RunStatus::Completed);
    }

    #[tokio::test]
    async fn unknown_session_is_rejected() {
        let (manager, _sink, _store) = manager_with(Arc::new(FixedProvider)).await;
        let err = manager
            .start_run(RunCreate {
                session_id: Some("sess_nope".to_owned()),
                workspace_id: None,
                prompt: "hi".to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::SessionNotFound(_)));
    }

    #[tokio::test]
    async fn panicking_provider_still_lands_a_terminal_state() {
        // review #36: a panic in the execution path must not wedge the run
        // non-terminal — the supervisor lands Failed and cleans the token map
        struct PanickingProvider;
        #[async_trait]
        impl ModelProvider for PanickingProvider {
            fn name(&self) -> &str {
                "panics"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                panic!("provider blew up");
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let (manager, sink, store) = manager_with(Arc::new(PanickingProvider)).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Failed);
        assert_eq!(done.error.unwrap().code, "internal");
        // the cancels map entry is gone: cancel after the panic is a no-op
        // on an already-terminal run, not a dangling token
        let after = manager.cancel_run(&run.id).await.unwrap();
        assert_eq!(after.status, RunStatus::Failed);
        assert!(manager.cancels.lock().await.is_empty());
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events, vec!["run.started", "run.failed"]);
    }

    #[tokio::test]
    async fn provider_failure_lands_failed_with_error_body() {
        struct DownProvider;
        #[async_trait]
        impl ModelProvider for DownProvider {
            fn name(&self) -> &str {
                "down"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                Err(ModelError::Unavailable("refused".to_owned()))
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let (manager, sink, store) = manager_with(Arc::new(DownProvider)).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Failed);
        assert_eq!(done.error.unwrap().code, "provider_unavailable");
        let events = sink.0.lock().unwrap().clone();
        assert_eq!(events, vec!["run.started", "run.failed"]);
    }

    // ── C3: tool execution in the loop ───────────────────────────────────────

    /// Canned-response HTTP fixture on a real socket.
    async fn http_fixture(body: &'static str) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}/")
    }

    fn tool_call_turn(name: &str, arguments: String) -> Msg {
        Msg::assistant(
            None,
            vec![ToolCallRequest {
                id: "call_1".to_owned(),
                name: name.to_owned(),
                arguments,
            }],
        )
    }

    #[tokio::test]
    async fn model_fetches_a_url_through_http_fetch() {
        let url = http_fixture("fixture payload 42").await;
        // allow_local: the fixture lives on loopback
        let (egress_gate, _, _) = fixture_egress_policy();
        let tools = ToolRegistry::new()
            .with(Arc::new(agent24_tools::HttpFetchTool::new(true)))
            .with_egress_gate(egress_gate);
        let provider = ScriptedProvider::new(vec![tool_call_turn(
            "http_fetch",
            serde_json::json!({ "url": url }).to_string(),
        )]);
        let (manager, sink, store) = manager_with_fixture_egress(Arc::new(provider), tools).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        let text = done.output.unwrap().text;
        assert!(text.contains("fixture payload 42"), "{text}");
        // two completions' usage accumulated
        assert_eq!(done.usage.total_tokens, 4);

        let events = sink.0.lock().unwrap().clone();
        assert_eq!(
            events,
            vec![
                "run.started",
                "tool.started",
                "tool.completed",
                "model.delta",
                "run.completed"
            ]
        );
        let calls = store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "http_fetch");
        assert_eq!(calls[0].status, ToolCallStatus::Completed);
        assert!(calls[0].ended_at.is_some());
    }

    #[tokio::test]
    async fn a_tool_round_trip_persists_the_full_message_thread() {
        // H3 foundation: the durable thread must be a faithful, ordered image of
        // the loop's in-memory `messages` — user prompt → assistant turn bearing
        // the tool_call → the tool result answering it → closing assistant answer.
        // This is what a restarted daemon reconstructs a suspended run from.
        let url = http_fixture("fixture payload 42").await;
        let (egress_gate, _, _) = fixture_egress_policy();
        let tools = ToolRegistry::new()
            .with(Arc::new(agent24_tools::HttpFetchTool::new(true)))
            .with_egress_gate(egress_gate);
        let provider = ScriptedProvider::new(vec![tool_call_turn(
            "http_fetch",
            serde_json::json!({ "url": url }).to_string(),
        )]);
        let (manager, _sink, store) = manager_with_fixture_egress(Arc::new(provider), tools).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);

        let thread = store.list_run_messages(&run.id).await.unwrap();
        assert_eq!(
            thread.iter().map(|m| m.role.as_str()).collect::<Vec<_>>(),
            vec!["user", "assistant", "tool", "assistant"],
            "{thread:?}"
        );
        // The persisted assistant turn carries the very tool_call the loop keys
        // resume off, and the tool row names the call it answers.
        assert_eq!(thread[1].content, None);
        assert_eq!(thread[1].tool_calls[0]["name"], "http_fetch");
        assert_eq!(thread[2].tool_call_id.as_deref(), Some("call_1"));
        // The closing answer is recorded so the thread is self-contained.
        assert!(
            thread[3]
                .content
                .as_deref()
                .unwrap()
                .contains("fixture payload 42"),
            "{:?}",
            thread[3].content
        );
    }

    #[tokio::test]
    async fn approval_stub_denial_is_persisted_audited_and_survivable() {
        let dir = tempfile::tempdir().unwrap();
        let tools = ToolRegistry::builtin(dir.path().to_path_buf());
        let provider = ScriptedProvider::new(vec![tool_call_turn(
            "shell_exec",
            serde_json::json!({ "argv": ["/bin/echo", "hi"] }).to_string(),
        )]);
        let (manager, sink, store) = manager_with_tools(Arc::new(provider), tools).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        // The denial goes back to the model, which still answers → completed
        assert_eq!(done.status, RunStatus::Completed);
        assert!(done.output.unwrap().text.contains("denied by policy"));

        let calls = store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].status, ToolCallStatus::Denied);

        let events = sink.0.lock().unwrap().clone();
        assert!(events.contains(&"tool.started".to_owned()));
        assert!(events.contains(&"tool.completed".to_owned()));

        // audit chain has the denial and still verifies
        store.verify_audit_chain().await.unwrap();
        let entries = store.list_audit().await.unwrap();
        assert!(
            entries
                .iter()
                .any(|e| e.action == "tool.denied" && e.actor == "policy")
        );
    }

    #[tokio::test]
    async fn invalid_tool_arguments_fail_the_call_not_the_run() {
        let tools = ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(true)));
        let provider =
            ScriptedProvider::new(vec![tool_call_turn("http_fetch", "{not json".to_owned())]);
        let (manager, _sink, store) = manager_with_tools(Arc::new(provider), tools).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        let calls = store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Failed);
        assert!(
            calls[0]
                .output_summary
                .as_deref()
                .unwrap()
                .contains("not valid JSON")
        );
    }

    #[tokio::test]
    async fn endless_tool_requests_hit_max_iterations() {
        /// Always asks for another tool call — never a final answer.
        struct GreedyProvider;
        #[async_trait]
        impl ModelProvider for GreedyProvider {
            fn name(&self) -> &str {
                "greedy"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                Ok(CompletionResponse {
                    message: Msg::assistant(
                        None,
                        vec![ToolCallRequest {
                            id: "call_x".to_owned(),
                            name: "nope".to_owned(),
                            arguments: "{}".to_owned(),
                        }],
                    ),
                    usage: usage_one(),
                    model_id: None,
                })
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let (manager, _sink, store) =
            manager_with_tools(Arc::new(GreedyProvider), ToolRegistry::new()).await;
        let run = manager.start_run(create()).await.unwrap();
        let done = wait_terminal(&store, &run.id).await;
        assert_eq!(done.status, RunStatus::Failed);
        assert_eq!(done.error.unwrap().code, "max_iterations");
        let calls = store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls.len(), MAX_ITERATIONS);
        assert!(calls.iter().all(|c| c.status == ToolCallStatus::Failed));
    }
}

#[cfg(test)]
mod approval_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::tests::fixture_egress_policy;
    use super::tests::*;
    use super::*;
    use agent24_models::router::Tier;
    use agent24_policy::{ApprovalBroker, ApprovalRequest, BrokerGate, Verdict};
    use agent24_protocol::{ApprovalStatus, Decision, RiskClass};
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    struct Harness {
        manager: Arc<RunManager>,
        broker: Arc<ApprovalBroker>,
        store: Store,
        events: Arc<StdMutex<Vec<String>>>,
    }

    /// Real broker + gate + registry with a live shell_exec, driven by a
    /// scripted provider asking for one shell_exec call.
    async fn harness(workdir: std::path::PathBuf) -> Harness {
        let store = Store::open_memory().await.unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
            if let Ok(mut v) = ev.lock() {
                v.push(body.wire_type().to_owned());
            }
        });
        let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
        let (egress_gate, resources, generation) = fixture_egress_policy();
        let tools = ToolRegistry::builtin(workdir)
            .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))))
            .with_egress_gate(Arc::clone(&egress_gate));
        struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
        impl EventSink for FnSink {
            fn emit(&self, body: EventBody) {
                (self.0)(body);
            }
        }
        let provider = ScriptedProvider::new(vec![Msg::assistant(
            None,
            vec![agent24_models::ToolCallRequest {
                id: "call_1".to_owned(),
                name: "shell_exec".to_owned(),
                arguments: serde_json::json!({ "argv": ["/bin/echo", "approved-output"] })
                    .to_string(),
            }],
        )]);
        let manager = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(provider),
                Tier::Local,
            )])),
            Arc::new(tools),
            Arc::new(FnSink(emit)),
            CancellationToken::new(),
        )
        .with_test_egress_policy(egress_gate, resources, generation);
        Harness {
            manager,
            broker,
            store,
            events,
        }
    }

    async fn wait_pending(store: &Store) -> String {
        for _ in 0..200 {
            let pending = store
                .list_approvals(Some(ApprovalStatus::Pending))
                .await
                .unwrap();
            if let Some(a) = pending.first() {
                return a.id.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("no pending approval appeared");
    }

    fn decision(kind: &str, reason: Option<&str>) -> Decision {
        Decision {
            kind: kind.to_owned(),
            reason: reason.map(str::to_owned),
            extra: serde_json::Map::new(),
        }
    }

    #[tokio::test]
    async fn approved_shell_exec_actually_executes() {
        let dir = tempfile::tempdir().unwrap();
        let h = harness(dir.path().to_path_buf()).await;
        let run = h.manager.start_run(create()).await.unwrap();
        let id = wait_pending(&h.store).await;
        // While the approval is pending the run is AWAITING_APPROVAL (SPEC
        // §1.2) — the pending row is created inside the gate, which runs
        // strictly after the awaiting transition, so this read is race-free
        let blocked = h.store.get_run(&run.id).await.unwrap().unwrap();
        assert_eq!(blocked.status, RunStatus::AwaitingApproval);
        h.broker
            .resolve(&id, decision("approve", None))
            .await
            .unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        assert!(done.output.unwrap().text.contains("approved-output"));
        let calls = h.store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Completed);
        let seen = h.events.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![
                "run.started",
                "tool.started",
                "approval.required",
                "approval.resolved",
                "tool.completed",
                "model.delta",
                "run.completed"
            ]
        );
    }

    // ── H8 plan mode ─────────────────────────────────────────────────────────

    fn create_plan() -> RunCreate {
        RunCreate {
            session_id: None,
            workspace_id: None,
            prompt: "do the thing".to_owned(),
            model_override: None,
            mode: RunMode::Plan,
        }
    }

    fn propose_plan_call() -> Msg {
        Msg::assistant(
            None,
            vec![agent24_models::ToolCallRequest {
                id: "plan_1".to_owned(),
                name: "propose_plan".to_owned(),
                arguments: serde_json::json!({ "plan": "delete the notes file" }).to_string(),
            }],
        )
    }

    /// A real broker + gate + builtin registry driven by a caller-supplied
    /// script (unlike [`harness`], whose script is fixed).
    async fn scripted_plan_harness(workdir: std::path::PathBuf, turns: Vec<Msg>) -> Harness {
        let store = Store::open_memory().await.unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
            if let Ok(mut v) = ev.lock() {
                v.push(body.wire_type().to_owned());
            }
        });
        let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
        let (egress_gate, resources, generation) = fixture_egress_policy();
        let tools = ToolRegistry::builtin(workdir)
            .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))))
            .with_egress_gate(Arc::clone(&egress_gate));
        struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
        impl EventSink for FnSink {
            fn emit(&self, body: EventBody) {
                (self.0)(body);
            }
        }
        let provider = ScriptedProvider::new(turns);
        let manager = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(provider),
                Tier::Local,
            )])),
            Arc::new(tools),
            Arc::new(FnSink(emit)),
            CancellationToken::new(),
        )
        .with_test_egress_policy(egress_gate, resources, generation);
        Harness {
            manager,
            broker,
            store,
            events,
        }
    }

    #[tokio::test]
    async fn plan_mode_advertises_only_read_tools_and_propose_plan() {
        let dir = tempfile::tempdir().unwrap();
        let h = scripted_plan_harness(dir.path().to_path_buf(), vec![]).await;
        let plan: Vec<String> = h
            .manager
            .tool_specs_for(true)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(plan.contains(&"propose_plan".to_owned()));
        assert!(plan.contains(&"fs_read".to_owned()));
        assert!(
            !plan.contains(&"shell_exec".to_owned()),
            "plan mode must not offer exec"
        );
        assert!(
            !plan.contains(&"fs_write".to_owned()),
            "plan mode must not offer write"
        );
        let full: Vec<String> = h
            .manager
            .tool_specs_for(false)
            .into_iter()
            .map(|s| s.name)
            .collect();
        assert!(full.contains(&"shell_exec".to_owned()));
        assert!(
            !full.contains(&"propose_plan".to_owned()),
            "propose_plan is plan-mode only"
        );
    }

    #[tokio::test]
    async fn plan_approved_unlocks_the_run_to_completion() {
        let dir = tempfile::tempdir().unwrap();
        // ScriptedProvider pops from the end, so turns are listed last-first.
        let h = scripted_plan_harness(
            dir.path().to_path_buf(),
            vec![
                Msg::assistant(Some("done".to_owned()), vec![]),
                propose_plan_call(),
            ],
        )
        .await;
        let run = h.manager.start_run(create_plan()).await.unwrap();
        let id = wait_pending(&h.store).await;
        let ap = h.store.get_approval(&id).await.unwrap().unwrap();
        assert_eq!(ap.kind, "plan");
        assert_eq!(
            ap.available_decisions,
            vec!["approve".to_owned(), "deny".to_owned()],
            "a plan is human-only: no approve_for_session / target grant is offered"
        );
        assert_eq!(
            h.store.get_run(&run.id).await.unwrap().unwrap().status,
            RunStatus::AwaitingApproval
        );
        h.broker
            .resolve(&id, decision("approve", None))
            .await
            .unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        assert_eq!(done.output.unwrap().text, "done");
    }

    #[tokio::test]
    async fn plan_denied_ends_the_run_without_executing() {
        let dir = tempfile::tempdir().unwrap();
        // The shell_exec turn would run only if the plan were approved — a
        // rejected plan must never reach it. (Turns are listed last-first: the
        // ScriptedProvider pops from the end, so propose_plan runs first.)
        let h = scripted_plan_harness(
            dir.path().to_path_buf(),
            vec![
                Msg::assistant(
                    None,
                    vec![agent24_models::ToolCallRequest {
                        id: "x".to_owned(),
                        name: "shell_exec".to_owned(),
                        arguments: serde_json::json!({ "argv": ["/bin/echo", "SHOULD_NOT_RUN"] })
                            .to_string(),
                    }],
                ),
                propose_plan_call(),
            ],
        )
        .await;
        let run = h.manager.start_run(create_plan()).await.unwrap();
        let id = wait_pending(&h.store).await;
        h.broker
            .resolve(&id, decision("deny", Some("no")))
            .await
            .unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        assert!(done.output.unwrap().text.contains("rejected"));
        // Only the propose_plan call exists — shell_exec never ran.
        let calls = h.store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].tool, "propose_plan");
        assert_eq!(calls[0].status, ToolCallStatus::Denied);
    }

    #[tokio::test]
    async fn plan_mode_denies_a_direct_write_call_at_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        // A rogue provider emits shell_exec DIRECTLY in plan mode, without ever
        // proposing a plan. It must be denied at the dispatch boundary — never
        // reaching the gate (where a standing grant / Guardian could otherwise
        // auto-approve it). Turns are last-first: final answer, then the rogue
        // call.
        let h = scripted_plan_harness(
            dir.path().to_path_buf(),
            vec![
                Msg::assistant(Some("stopped".to_owned()), vec![]),
                Msg::assistant(
                    None,
                    vec![agent24_models::ToolCallRequest {
                        id: "rogue".to_owned(),
                        name: "shell_exec".to_owned(),
                        arguments: serde_json::json!({ "argv": ["/bin/echo", "PWNED"] })
                            .to_string(),
                    }],
                ),
            ],
        )
        .await;
        let run = h.manager.start_run(create_plan()).await.unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        assert_eq!(done.status, RunStatus::Completed);
        // The rogue call was refused before it could become a gated tool call,
        // so no shell_exec ever ran and the run finished on the next model turn.
        assert_eq!(done.output.unwrap().text, "stopped");
        let calls = h.store.list_tool_calls(&run.id).await.unwrap();
        assert!(
            calls.iter().all(|c| c.tool != "shell_exec"),
            "a write/exec call in plan mode must be denied at dispatch, never executed"
        );
    }

    /// Like [`harness`] but with an EMPTY script: the first model call after a
    /// resume answers from the last tool result, so the run finishes as soon as
    /// the parked call has been settled and executed.
    async fn resume_harness(workdir: std::path::PathBuf) -> Harness {
        let store = Store::open_memory().await.unwrap();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
            if let Ok(mut v) = ev.lock() {
                v.push(body.wire_type().to_owned());
            }
        });
        let broker = ApprovalBroker::new(store.clone(), Arc::clone(&emit), Duration::from_secs(30));
        let (egress_gate, resources, generation) = fixture_egress_policy();
        let tools = ToolRegistry::builtin(workdir)
            .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))))
            .with_egress_gate(Arc::clone(&egress_gate));
        struct FnSink(Arc<dyn Fn(EventBody) + Send + Sync>);
        impl EventSink for FnSink {
            fn emit(&self, body: EventBody) {
                (self.0)(body);
            }
        }
        let manager = RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(ScriptedProvider::new(vec![])),
                Tier::Local,
            )])),
            Arc::new(tools),
            Arc::new(FnSink(emit)),
            CancellationToken::new(),
        )
        .with_test_egress_policy(egress_gate, resources, generation);
        Harness {
            manager,
            broker,
            store,
            events,
        }
    }

    #[rustfmt::skip]
    async fn assert_bound_recovery_followup_stops(h:&Harness,run:&Run,call:&agent24_models::ToolCallRequest,lease:&str){assert!(matches!(h.manager.run_tool_call(run,call,&CancellationToken::new(),true).await,Err(ParkedCallStop::RecoveryStopped)));let tool_calls:i64=sqlx::query_scalar("SELECT count(*) FROM tool_calls WHERE run_id='run_1'").fetch_one(agent24_store::test_hooks::pool(&h.store)).await.unwrap();assert_eq!(tool_calls,0);sqlx::query("UPDATE runs SET status='running',ended_at=NULL WHERE id='run_1'").execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();sqlx::query("UPDATE workspace_leases SET released_at=NULL WHERE lease_id=?").bind(lease).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();}
    /// End-to-end crash recovery (H3): a run parked awaiting approval, its task
    /// gone, is resumed once a human answers the restored approval — the parked
    /// tool runs and the run completes, all reconstructed from the persisted
    /// thread.
    #[tokio::test]
    async fn a_parked_run_resumes_after_its_approval_is_answered() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;

        // Seed the crash state directly: an awaiting_approval run with NO live
        // task, its thread persisted (user + assistant turn whose trailing
        // tool_call is unanswered), and a matching pending approval. The
        // assistant call's arguments MUST equal the approval payload, or
        // staleness re-validation would (correctly) refuse to run B for A.
        let args = serde_json::json!({ "argv": ["/bin/echo", "resumed-output"] });
        let run = Run {
            id: "run_1".to_owned(),
            session_id: None,
            workspace_id: None,
            status: RunStatus::AwaitingApproval,
            input: RunInput {
                prompt: "run echo".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: now_iso8601(),
            started_at: Some(now_iso8601()),
            ended_at: None,
        };
        h.store.insert_run(&run).await.unwrap();
        h.store
            .append_run_message(
                "run_1",
                "user",
                Some("run echo"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        h.store
            .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
            .await
            .unwrap();
        let approval = Approval {
            id: "apr_1".to_owned(),
            run_id: "run_1".to_owned(),
            tool_call_id: "tc_internal_1".to_owned(),
            kind: "exec".to_owned(),
            summary: "shell_exec".to_owned(),
            payload: args.as_object().unwrap().clone(),
            available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
            standing_target: None,
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
            created_at: now_iso8601(),
            decided_at: None,
        };
        h.store.insert_approval(&approval).await.unwrap();

        // The human answers — no in-memory waiter is woken (the task is gone).
        h.broker
            .resolve("apr_1", decision("approve", None))
            .await
            .unwrap();
        // The daemon resumes the run off the persisted thread.
        h.manager
            .resume_run("run_1".to_owned(), "apr_1".to_owned())
            .await
            .unwrap();

        let done = wait_terminal(&h.store, "run_1").await;
        assert_eq!(done.status, RunStatus::Completed);
        // The parked shell_exec actually ran on resume; its output flowed into
        // the final answer.
        assert!(
            done.output.unwrap().text.contains("resumed-output"),
            "the parked tool did not run on resume"
        );
        // The reconstructed thread gained the tool result for the parked call,
        // keyed on the THREAD's provider id (call_provider_1) — NOT the
        // approval's tc_internal_1. Resume works across the two id namespaces.
        let thread = h.store.list_run_messages("run_1").await.unwrap();
        assert!(
            thread
                .iter()
                .any(|m| m.role == "tool" && m.tool_call_id.as_deref() == Some("call_provider_1")),
            "no tool result was recorded for the resumed call"
        );
    }

    #[tokio::test]
    async fn bound_resume_without_fresh_authority_cancels_before_running() {
        const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WS).unwrap();
        let now = workspace_timestamp(now_iso8601());
        let expires = WorkspaceInstant::parse(&now)
            .unwrap()
            .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
            .unwrap();
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')")
            .bind(WS).bind(&now).bind(expires.as_str())
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store
            .insert_session(&agent24_protocol::Session {
                id: "sess_1".to_owned(),
                title: "session".to_owned(),
                channel: "desktop".to_owned(),
                workspace_id: Some(workspace_id.clone()),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .unwrap();
        let input = RunInput {
            prompt: "run echo".to_owned(),
            workspace_id: Some(workspace_id.clone()),
            model_override: None,
            mode: RunMode::Normal,
        };
        sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'awaiting_approval',?,?,?,?)")
            .bind(WS)
            .bind(serde_json::to_string(&input).unwrap())
            .bind(serde_json::to_string(&zero_usage()).unwrap())
            .bind(&now)
            .bind(&now)
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)")
            .bind(LEASE).bind(WS).bind(&now)
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store
            .append_run_message(
                "run_1",
                "user",
                Some("run echo"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let args = serde_json::json!({ "argv": ["/bin/echo", "must-not-run"] });
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        h.store
            .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
            .await
            .unwrap();
        let approval = seed_approval(
            "apr_1",
            "run_1",
            "tc_internal_1",
            args.as_object().unwrap().clone(),
        );
        h.store.insert_approval(&approval).await.unwrap();
        h.broker
            .resolve("apr_1", decision("approve", None))
            .await
            .unwrap();

        h.manager
            .resume_run("run_1".to_owned(), "apr_1".to_owned())
            .await
            .unwrap();

        assert_eq!(
            wait_terminal(&h.store, "run_1").await.status,
            RunStatus::Cancelled
        );
        assert!(!h.events.lock().unwrap().contains(&"run.started".to_owned()));
        let released: Option<String> =
            sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?")
                .bind(LEASE)
                .fetch_one(agent24_store::test_hooks::pool(&h.store))
                .await
                .unwrap();
        assert!(released.is_some());
    }

    #[tokio::test]
    #[rustfmt::skip]
    async fn bound_settle_authority_loss_after_running_cancels_recovery() {
        const WS:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"; const LEASE:&str="wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
        let dir=tempfile::tempdir().unwrap(); let h=resume_harness(dir.path().to_path_buf()).await; let workspace_id=agent24_protocol::WorkspaceId::parse(WS).unwrap(); let now=workspace_timestamp(now_iso8601()); let expires=WorkspaceInstant::parse(&now).unwrap().checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap()).unwrap();
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')").bind(WS).bind(&now).bind(expires.as_str()).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store.insert_session(&agent24_protocol::Session{id:"sess_1".into(),title:"session".into(),channel:"desktop".into(),workspace_id:Some(workspace_id.clone()),created_at:now.clone(),updated_at:now.clone()}).await.unwrap();
        let input=RunInput{prompt:"run echo".into(),workspace_id:Some(workspace_id),model_override:None,mode:RunMode::Normal};
        sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'running',?,?,?,?)").bind(WS).bind(serde_json::to_string(&input).unwrap()).bind(serde_json::to_string(&zero_usage()).unwrap()).bind(&now).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)").bind(LEASE).bind(WS).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        let run=h.store.get_run("run_1").await.unwrap().unwrap(); let payload=serde_json::json!({"argv":["/bin/echo","must-not-run"]}); let mut approval=seed_approval("apr_1","run_1","tc_internal_1",payload.as_object().unwrap().clone()); approval.status=ApprovalStatus::Approved; approval.decision=Some(decision("approve",None)); approval.decided_at=Some(now_iso8601());
        let call=agent24_models::ToolCallRequest{id:"call_provider_1".into(),name:"shell_exec".into(),arguments:payload.to_string()};
        assert_bound_recovery_followup_stops(&h,&run,&call,LEASE).await;
        assert!(matches!(h.manager.settle_parked_call(&run,&approval,&call,&CancellationToken::new()).await,Err(ParkedCallStop::RecoveryStopped)));
        assert_eq!(h.store.get_run("run_1").await.unwrap().unwrap().status,RunStatus::Cancelled);
        let released:Option<String>=sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?").bind(LEASE).fetch_one(agent24_store::test_hooks::pool(&h.store)).await.unwrap(); assert!(released.is_some()); assert!(h.events.lock().unwrap().contains(&"run.cancelled".to_owned()));
    }

    #[cfg(unix)]
    #[tokio::test]
    #[rustfmt::skip]
    async fn bound_resume_later_call_authority_loss_stops_once_without_persistence() {
        use std::os::unix::fs::{MetadataExt,PermissionsExt}; const WS:&str="ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"; const LEASE:&str="wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
        let dir=tempfile::tempdir().unwrap(); let state=tempfile::tempdir().unwrap(); let mut h=resume_harness(dir.path().to_path_buf()).await; let service=Arc::new(WorkspaceService::compose(h.store.clone(),state.path()).unwrap()); Arc::get_mut(&mut h.manager).unwrap().workspace=Some(service);
        let now=workspace_timestamp(now_iso8601()); let expires=WorkspaceInstant::parse(&now).unwrap().checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap()).unwrap(); let locator=format!("{WS}.g1"); let parent=state.path().join("workspace-roots"); let root=parent.join(&locator); std::fs::create_dir(&root).unwrap(); std::fs::set_permissions(&root,std::fs::Permissions::from_mode(0o700)).unwrap(); let pm=std::fs::metadata(&parent).unwrap(); let rm=std::fs::metadata(&root).unwrap();
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,?,'g1','unix',?,?)").bind(WS).bind(&now).bind(expires.as_str()).bind(root.to_str().unwrap()).bind(rm.dev().to_le_bytes().to_vec()).bind(rm.ino().to_le_bytes().to_vec()).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_allocations (allocation_id,workspace_id,root_generation,relative_name,parent_identity_kind,parent_unix_device,parent_unix_inode,root_identity_kind,root_unix_device,root_unix_inode,phase,created_at) VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5',?,'g1',?,'unix',?,?,'unix',?,?,'committed',?)").bind(WS).bind(&locator).bind(pm.dev().to_le_bytes().to_vec()).bind(pm.ino().to_le_bytes().to_vec()).bind(rm.dev().to_le_bytes().to_vec()).bind(rm.ino().to_le_bytes().to_vec()).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        let wid=agent24_protocol::WorkspaceId::parse(WS).unwrap(); h.store.insert_session(&agent24_protocol::Session{id:"sess_1".into(),title:"s".into(),channel:"desktop".into(),workspace_id:Some(wid.clone()),created_at:now.clone(),updated_at:now.clone()}).await.unwrap(); let input=RunInput{prompt:"two".into(),workspace_id:Some(wid),model_override:None,mode:RunMode::Normal}; sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'awaiting_approval',?,?,?,?)").bind(WS).bind(serde_json::to_string(&input).unwrap()).bind(serde_json::to_string(&zero_usage()).unwrap()).bind(&now).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap(); sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)").bind(LEASE).bind(WS).bind(&now).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store.append_run_message("run_1","user",Some("two"),&serde_json::json!([]),None,&now_iso8601()).await.unwrap(); let a1=serde_json::json!({"argv":["/bin/echo","first"]}); let a2=serde_json::json!({"argv":["/bin/echo","must-not-run"]}); let calls=serde_json::json!([{"id":"call_provider_1","name":"shell_exec","arguments":a1.to_string()},{"id":"call_provider_2","name":"shell_exec","arguments":a2.to_string()}]); h.store.append_run_message("run_1","assistant",None,&calls,None,&now_iso8601()).await.unwrap(); let approval=seed_approval("apr_1","run_1","tc_internal_1",a1.as_object().unwrap().clone()); h.store.insert_approval(&approval).await.unwrap(); h.broker.resolve("apr_1",decision("approve",None)).await.unwrap();
        let run=h.store.get_run("run_1").await.unwrap().unwrap(); assert!(h.manager.tool_context_for(&run,"preflight").await.is_ok()); sqlx::raw_sql(&format!("CREATE TRIGGER lose_resume_authority AFTER INSERT ON run_messages WHEN NEW.run_id='run_1' AND NEW.role='tool' AND NEW.tool_call_id='call_provider_1' BEGIN UPDATE workspace_leases SET released_at='{now}' WHERE lease_id='{LEASE}'; END;")).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap(); h.manager.resume_run("run_1".into(),"apr_1".into()).await.unwrap(); assert_eq!(wait_terminal(&h.store,"run_1").await.status,RunStatus::Cancelled);
        assert_eq!(h.store.list_tool_calls("run_1").await.unwrap().len(),0); let thread=h.store.list_run_messages("run_1").await.unwrap(); assert!(thread.iter().any(|m|m.role=="tool"&&m.tool_call_id.as_deref()==Some("call_provider_1"))); assert!(!thread.iter().any(|m|m.role=="tool"&&m.tool_call_id.as_deref()==Some("call_provider_2"))); assert_eq!(h.events.lock().unwrap().iter().filter(|e|e.as_str()=="run.cancelled").count(),1);
    }

    #[tokio::test]
    async fn restored_session_approval_replays_its_grant() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        let now = now_iso8601();
        h.store
            .insert_session(&agent24_protocol::Session {
                id: "sess_1".to_owned(),
                title: "session".to_owned(),
                channel: "desktop".to_owned(),
                workspace_id: None,
                created_at: now.clone(),
                updated_at: now,
            })
            .await
            .unwrap();
        let args = serde_json::json!({ "argv": ["/bin/echo", "resumed-output"] });
        let mut run = run_in("run_1", RunStatus::AwaitingApproval);
        run.session_id = Some("sess_1".to_owned());
        h.store.insert_run(&run).await.unwrap();
        h.store
            .append_run_message(
                "run_1",
                "user",
                Some("run echo"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        h.store
            .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
            .await
            .unwrap();
        let mut approval = seed_approval(
            "apr_1",
            "run_1",
            "tc_internal_1",
            args.as_object().unwrap().clone(),
        );
        approval
            .available_decisions
            .insert(1, "approve_for_session".to_owned());
        h.store.insert_approval(&approval).await.unwrap();

        h.broker
            .resolve("apr_1", decision("approve_for_session", None))
            .await
            .unwrap();
        h.manager
            .resume_run("run_1".to_owned(), "apr_1".to_owned())
            .await
            .unwrap();
        assert_eq!(
            wait_terminal(&h.store, "run_1").await.status,
            RunStatus::Completed
        );

        let approvals_before = h.store.list_approvals(None).await.unwrap().len();
        let verdict = h
            .broker
            .request(
                ApprovalRequest {
                    run_id: "probe",
                    session_id: Some("sess_1"),
                    schedule_id: None,
                    tool_call_id: "tc_probe",
                    tool: "shell_exec",
                    kind: "exec",
                    risk: RiskClass::Exec,
                    standing_target: None,
                    summary: "probe".to_owned(),
                    payload: serde_json::Map::new(),
                },
                &CancellationToken::new(),
            )
            .await;
        assert_eq!(verdict, Verdict::Approved);
        assert_eq!(
            h.store.list_approvals(None).await.unwrap().len(),
            approvals_before
        );
    }

    /// A denied restored approval does not execute the tool: the reason goes
    /// back to the model and the run still completes (denial is not failure).
    #[tokio::test]
    async fn a_denied_parked_run_resumes_without_executing() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        let args = serde_json::json!({ "argv": ["/bin/echo", "should-not-run"] });
        let run = Run {
            id: "run_1".to_owned(),
            session_id: None,
            workspace_id: None,
            status: RunStatus::AwaitingApproval,
            input: RunInput {
                prompt: "run echo".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: now_iso8601(),
            started_at: Some(now_iso8601()),
            ended_at: None,
        };
        h.store.insert_run(&run).await.unwrap();
        h.store
            .append_run_message(
                "run_1",
                "user",
                Some("run echo"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        h.store
            .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
            .await
            .unwrap();
        let approval = Approval {
            id: "apr_1".to_owned(),
            run_id: "run_1".to_owned(),
            tool_call_id: "tc_internal_1".to_owned(),
            kind: "exec".to_owned(),
            summary: "shell_exec".to_owned(),
            payload: args.as_object().unwrap().clone(),
            available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
            standing_target: None,
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
            created_at: now_iso8601(),
            decided_at: None,
        };
        h.store.insert_approval(&approval).await.unwrap();

        h.broker
            .resolve("apr_1", decision("deny", Some("not this time")))
            .await
            .unwrap();
        h.manager
            .resume_run("run_1".to_owned(), "apr_1".to_owned())
            .await
            .unwrap();

        let done = wait_terminal(&h.store, "run_1").await;
        assert_eq!(done.status, RunStatus::Completed);
        // The denial reason reached the model (echoed by the exhausted script),
        // and the tool never produced its output.
        let text = done.output.unwrap().text;
        assert!(
            text.contains("not this time"),
            "denial reason not fed back: {text}"
        );
        assert!(
            !text.contains("should-not-run"),
            "denied tool ran anyway: {text}"
        );
    }

    /// A run row in a given status, for seeding crash/parked state directly.
    fn run_in(id: &str, status: RunStatus) -> Run {
        Run {
            id: id.to_owned(),
            session_id: None,
            workspace_id: None,
            status,
            input: RunInput {
                prompt: "go".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: now_iso8601(),
            started_at: Some(now_iso8601()),
            ended_at: None,
        }
    }

    /// A pending approval row.
    fn seed_approval(
        id: &str,
        run_id: &str,
        tool_call_id: &str,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> Approval {
        Approval {
            id: id.to_owned(),
            run_id: run_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            kind: "exec".to_owned(),
            summary: "shell_exec".to_owned(),
            payload,
            available_decisions: vec!["approve".to_owned(), "deny".to_owned(), "abort".to_owned()],
            standing_target: None,
            status: ApprovalStatus::Pending,
            decision: None,
            expires_at: agent24_core::util::iso8601_after(Duration::from_secs(30)),
            created_at: now_iso8601(),
            decided_at: None,
        }
    }

    #[test]
    fn restored_decision_requires_offered_kind_and_matching_status() {
        let mut approval = seed_approval("apr", "run", "tc", serde_json::Map::new());
        approval.status = ApprovalStatus::Approved;
        let session = decision("approve_for_session", None);
        assert!(!restored_decision_is_consistent(&approval, &session));

        approval
            .available_decisions
            .push("approve_for_session".to_owned());
        assert!(restored_decision_is_consistent(&approval, &session));

        approval.status = ApprovalStatus::Denied;
        assert!(!restored_decision_is_consistent(&approval, &session));
    }

    #[tokio::test]
    async fn startup_timeout_prevents_expired_approval_rebroadcast() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        h.store
            .insert_run(&run_in("run_expired", RunStatus::AwaitingApproval))
            .await
            .unwrap();
        let args = serde_json::json!({ "argv": ["/bin/echo", "must-not-run"] });
        h.store
            .append_run_message(
                "run_expired",
                "user",
                Some("go"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let calls = serde_json::json!([{
            "id": "call_provider_expired",
            "name": "shell_exec",
            "arguments": args.to_string()
        }]);
        h.store
            .append_run_message(
                "run_expired",
                "assistant",
                None,
                &calls,
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let mut approval = seed_approval(
            "apr_expired",
            "run_expired",
            "tc_expired",
            args.as_object().unwrap().clone(),
        );
        approval.expires_at = "2020-01-01T00:00:00Z".to_owned();
        h.store.insert_approval(&approval).await.unwrap();

        // Prove this fixture would be re-broadcast if startup restore ran
        // before expiry reconciliation; otherwise the regression is vacuous.
        assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (1, 0));
        h.events.lock().unwrap().clear();

        assert_eq!(
            h.broker
                .timeout_expired("2026-10-01T00:00:00Z")
                .await
                .unwrap(),
            1
        );
        assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (0, 0));
        assert_eq!(
            h.store
                .get_approval("apr_expired")
                .await
                .unwrap()
                .unwrap()
                .status,
            ApprovalStatus::TimedOut
        );
        let seen = h.events.lock().unwrap().clone();
        assert!(seen.iter().any(|event| event == "approval.resolved"));
        assert!(!seen.iter().any(|event| event == "approval.required"));
    }

    #[tokio::test]
    async fn resume_rejects_approval_owned_by_another_run() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        h.store
            .insert_run(&run_in("run_a", RunStatus::AwaitingApproval))
            .await
            .unwrap();
        h.store
            .insert_run(&run_in("run_b", RunStatus::AwaitingApproval))
            .await
            .unwrap();
        let approval = seed_approval("apr_a", "run_a", "tc_a", serde_json::Map::new());
        h.store.insert_approval(&approval).await.unwrap();

        assert!(matches!(
            h.manager
                .resume_run("run_b".to_owned(), "apr_a".to_owned())
                .await,
            Err(AgentError::Store(StoreError::Conflict(message)))
                if message == "approval does not belong to run"
        ));
        assert_eq!(
            h.store.get_run("run_b").await.unwrap().unwrap().status,
            RunStatus::AwaitingApproval
        );
        assert!(!h.manager.cancels.lock().await.contains_key("run_b"));
    }

    /// Startup restore sweep (H3): a restorable pending approval is re-broadcast
    /// and kept pending; a non-restorable one (here: its run already completed)
    /// is aborted fail-closed. Replaces the old abort-everything sweep.
    #[tokio::test]
    async fn restore_sweep_keeps_the_restorable_and_aborts_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;

        // Restorable: an awaiting_approval run with a consistent parked thread.
        let args = serde_json::json!({ "argv": ["/bin/echo", "hi"] });
        h.store
            .insert_run(&run_in("run_ok", RunStatus::AwaitingApproval))
            .await
            .unwrap();
        h.store
            .append_run_message(
                "run_ok",
                "user",
                Some("go"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        h.store
            .append_run_message("run_ok", "assistant", None, &call, None, &now_iso8601())
            .await
            .unwrap();
        h.store
            .insert_approval(&seed_approval(
                "apr_ok",
                "run_ok",
                // tc_ namespace — deliberately DIFFERENT from the thread's
                // provider id (call_provider_1); correspondence is by payload.
                "tc_internal_1",
                args.as_object().unwrap().clone(),
            ))
            .await
            .unwrap();

        // Non-restorable: a pending approval whose run already COMPLETED — the
        // status gate in plan_resume refuses it, so the sweep aborts it.
        h.store
            .insert_run(&run_in("run_done", RunStatus::Completed))
            .await
            .unwrap();
        h.store
            .insert_approval(&seed_approval(
                "apr_done",
                "run_done",
                "call_x",
                serde_json::Map::new(),
            ))
            .await
            .unwrap();

        let (restored, aborted) = h.manager.restore_pending_approvals().await.unwrap();
        assert_eq!((restored, aborted), (1, 1));
        // The restorable one is still pending and was re-announced.
        assert_eq!(
            h.store
                .get_approval("apr_ok")
                .await
                .unwrap()
                .unwrap()
                .status,
            ApprovalStatus::Pending
        );
        assert!(
            h.events
                .lock()
                .unwrap()
                .contains(&"approval.required".to_owned())
        );
        // The non-restorable one is aborted.
        assert_eq!(
            h.store
                .get_approval("apr_done")
                .await
                .unwrap()
                .unwrap()
                .status,
            ApprovalStatus::Aborted
        );
    }

    #[tokio::test]
    async fn timed_out_recovery_cancels_only_tokenless_parked_runs_without_start_event() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        for id in ["run_tokenless", "run_live"] {
            h.store
                .insert_run(&run_in(id, RunStatus::AwaitingApproval))
                .await
                .unwrap();
            let mut approval = seed_approval(
                &format!("apr_{id}"),
                id,
                &format!("tc_{id}"),
                serde_json::Map::new(),
            );
            approval.status = ApprovalStatus::TimedOut;
            approval.decided_at = Some(now_iso8601());
            h.store.insert_approval(&approval).await.unwrap();
        }
        h.manager
            .cancels
            .lock()
            .await
            .insert("run_live".to_owned(), CancellationToken::new());

        assert_eq!(
            h.manager.recover_timed_out_approval_runs().await.unwrap(),
            1
        );
        assert_eq!(
            h.store
                .get_run("run_tokenless")
                .await
                .unwrap()
                .unwrap()
                .status,
            RunStatus::Cancelled
        );
        assert_eq!(
            h.store.get_run("run_live").await.unwrap().unwrap().status,
            RunStatus::AwaitingApproval
        );
        let seen = h.events.lock().unwrap().clone();
        assert_eq!(
            seen.iter()
                .filter(|event| event.as_str() == "run.cancelled")
                .count(),
            1
        );
        assert!(!seen.iter().any(|event| event.as_str() == "run.started"));
    }

    #[tokio::test]
    async fn timed_out_recovery_retries_a_failed_cancel_on_the_next_scan() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        h.store
            .insert_run(&run_in("run_retry", RunStatus::AwaitingApproval))
            .await
            .unwrap();
        let mut approval =
            seed_approval("apr_retry", "run_retry", "tc_retry", serde_json::Map::new());
        approval.status = ApprovalStatus::TimedOut;
        approval.decided_at = Some(now_iso8601());
        h.store.insert_approval(&approval).await.unwrap();
        sqlx::raw_sql(
            "CREATE TRIGGER block_retry_cancel BEFORE UPDATE OF status ON runs
             WHEN OLD.id='run_retry' AND NEW.status='cancelled'
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .execute(agent24_store::test_hooks::pool(&h.store))
        .await
        .unwrap();

        assert_eq!(
            h.manager.recover_timed_out_approval_runs().await.unwrap(),
            0
        );
        assert_eq!(
            h.store.get_run("run_retry").await.unwrap().unwrap().status,
            RunStatus::AwaitingApproval
        );
        assert_eq!(
            h.store.timed_out_approval_recovery_run_ids().await.unwrap(),
            vec!["run_retry".to_owned()]
        );

        sqlx::query("DROP TRIGGER block_retry_cancel")
            .execute(agent24_store::test_hooks::pool(&h.store))
            .await
            .unwrap();
        assert_eq!(
            h.manager.recover_timed_out_approval_runs().await.unwrap(),
            1
        );
        assert_eq!(
            h.store.get_run("run_retry").await.unwrap().unwrap().status,
            RunStatus::Cancelled
        );
    }

    #[tokio::test]
    async fn restore_sweep_aborts_bound_approval_without_fresh_authority() {
        const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        let workspace_id = agent24_protocol::WorkspaceId::parse(WS).unwrap();
        let now = workspace_timestamp(now_iso8601());
        let expires = WorkspaceInstant::parse(&now)
            .unwrap()
            .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
            .unwrap();
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')")
            .bind(WS).bind(&now).bind(expires.as_str())
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store
            .insert_session(&agent24_protocol::Session {
                id: "sess_1".to_owned(),
                title: "session".to_owned(),
                channel: "desktop".to_owned(),
                workspace_id: Some(workspace_id.clone()),
                created_at: now.clone(),
                updated_at: now.clone(),
            })
            .await
            .unwrap();
        let input = RunInput {
            prompt: "run echo".to_owned(),
            workspace_id: Some(workspace_id),
            model_override: None,
            mode: RunMode::Normal,
        };
        sqlx::query("INSERT INTO runs (id,session_id,workspace_id,status,input,usage,created_at,started_at) VALUES ('run_1','sess_1',?,'awaiting_approval',?,?,?,?)")
            .bind(WS).bind(serde_json::to_string(&input).unwrap())
            .bind(serde_json::to_string(&zero_usage()).unwrap())
            .bind(&now).bind(&now)
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        sqlx::query("INSERT INTO workspace_leases (lease_id,workspace_id,root_generation,owner_id,kind,acquired_at) VALUES (?,?,'g1','run_1','run',?)")
            .bind(LEASE).bind(WS).bind(&now)
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store
            .append_run_message(
                "run_1",
                "user",
                Some("run echo"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let args = serde_json::json!({ "argv": ["/bin/echo", "must-not-run"] });
        let call = serde_json::json!([{ "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string() }]);
        h.store
            .append_run_message("run_1", "assistant", None, &call, None, &now_iso8601())
            .await
            .unwrap();
        h.store
            .insert_approval(&seed_approval(
                "apr_1",
                "run_1",
                "tc_internal_1",
                args.as_object().unwrap().clone(),
            ))
            .await
            .unwrap();

        assert_eq!(h.manager.restore_pending_approvals().await.unwrap(), (0, 1));
        assert_eq!(
            h.store.get_approval("apr_1").await.unwrap().unwrap().status,
            ApprovalStatus::Aborted
        );
        assert_eq!(
            h.store.get_run("run_1").await.unwrap().unwrap().status,
            RunStatus::AwaitingApproval
        );
        assert!(
            !h.events
                .lock()
                .unwrap()
                .contains(&"approval.required".to_owned())
        );

        let ended_at = WorkspaceInstant::parse(&workspace_timestamp(now_iso8601())).unwrap();
        assert_eq!(
            h.store
                .sweep_workspace_orphan_runs(&ended_at)
                .await
                .unwrap(),
            agent24_store::WorkspaceOrphanSweep { released_leases: 1 }
        );
        assert_eq!(
            h.store.get_run("run_1").await.unwrap().unwrap().status,
            RunStatus::Cancelled
        );
        let released: Option<String> =
            sqlx::query_scalar("SELECT released_at FROM workspace_leases WHERE lease_id=?")
                .bind(LEASE)
                .fetch_one(agent24_store::test_hooks::pool(&h.store))
                .await
                .unwrap();
        assert!(released.is_some());
    }

    #[tokio::test]
    async fn restore_sweep_propagates_storage_failure() {
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        sqlx::query("DROP TABLE approvals")
            .execute(agent24_store::test_hooks::pool(&h.store))
            .await
            .unwrap();
        assert!(matches!(
            h.manager.restore_pending_approvals().await,
            Err(AgentError::Store(_))
        ));

        const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
        let dir = tempfile::tempdir().unwrap();
        let h = resume_harness(dir.path().to_path_buf()).await;
        let now = workspace_timestamp(now_iso8601());
        let expires = WorkspaceInstant::parse(&now)
            .unwrap()
            .checked_add_workspace_ttl(agent24_store::WorkspaceTtl::new(60_000).unwrap())
            .unwrap();
        let input = RunInput {
            prompt: "go".into(),
            workspace_id: Some(agent24_protocol::WorkspaceId::parse(WS).unwrap()),
            model_override: None,
            mode: RunMode::Normal,
        };
        sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?,?,1,'/scratch','g1','unix',X'0101010101010101',X'0202020202020202')")
            .bind(WS).bind(&now).bind(expires.as_str()).execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        sqlx::query("INSERT INTO runs (id,workspace_id,status,input,usage,created_at) VALUES ('run_db',?,'awaiting_approval',?,?,?)")
            .bind(WS).bind(serde_json::to_string(&input).unwrap()).bind(serde_json::to_string(&zero_usage()).unwrap()).bind(&now)
            .execute(agent24_store::test_hooks::pool(&h.store)).await.unwrap();
        h.store
            .append_run_message(
                "run_db",
                "user",
                Some("go"),
                &serde_json::json!([]),
                None,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let args = serde_json::json!({"argv":["/bin/echo","hi"]});
        let calls = serde_json::json!([{"id":"call_provider_1","name":"shell_exec","arguments":args.to_string()}]);
        h.store
            .append_run_message("run_db", "assistant", None, &calls, None, &now_iso8601())
            .await
            .unwrap();
        h.store
            .insert_approval(&seed_approval(
                "apr_db",
                "run_db",
                "tc_internal_1",
                args.as_object().unwrap().clone(),
            ))
            .await
            .unwrap();
        sqlx::query("DROP TABLE workspace_leases")
            .execute(agent24_store::test_hooks::pool(&h.store))
            .await
            .unwrap();
        assert!(matches!(
            h.manager.restore_pending_approvals().await,
            Err(AgentError::Workspace(WorkspaceStoreError::Database))
        ));
        assert_eq!(
            h.store
                .get_approval("apr_db")
                .await
                .unwrap()
                .unwrap()
                .status,
            ApprovalStatus::Pending
        );
    }

    #[tokio::test]
    async fn denied_approval_feeds_the_reason_back_to_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let h = harness(dir.path().to_path_buf()).await;
        let run = h.manager.start_run(create()).await.unwrap();
        let id = wait_pending(&h.store).await;
        h.broker
            .resolve(&id, decision("deny", Some("not on my machine")))
            .await
            .unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        // Run continues: the scripted provider echoes the tool result
        assert_eq!(done.status, RunStatus::Completed);
        assert!(done.output.unwrap().text.contains("not on my machine"));
        let calls = h.store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Denied);
    }

    #[tokio::test]
    async fn abort_decision_cancels_the_whole_run() {
        let dir = tempfile::tempdir().unwrap();
        let h = harness(dir.path().to_path_buf()).await;
        let run = h.manager.start_run(create()).await.unwrap();
        let id = wait_pending(&h.store).await;
        h.broker
            .resolve(&id, decision("abort", None))
            .await
            .unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        assert_eq!(done.status, RunStatus::Cancelled);
        let calls = h.store.list_tool_calls(&run.id).await.unwrap();
        assert_eq!(calls[0].status, ToolCallStatus::Denied);
        let seen = h.events.lock().unwrap().clone();
        assert!(seen.contains(&"run.cancelled".to_owned()));
    }

    #[tokio::test]
    async fn cancelling_the_run_aborts_its_pending_approval() {
        let dir = tempfile::tempdir().unwrap();
        let h = harness(dir.path().to_path_buf()).await;
        let run = h.manager.start_run(create()).await.unwrap();
        let id = wait_pending(&h.store).await;
        h.manager.cancel_run(&run.id).await.unwrap();
        let done = wait_terminal(&h.store, &run.id).await;
        assert_eq!(done.status, RunStatus::Cancelled);
        for _ in 0..100 {
            let a = h.store.get_approval(&id).await.unwrap().unwrap();
            if a.status != ApprovalStatus::Pending {
                assert_eq!(a.status, ApprovalStatus::Aborted);
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("approval never left pending after run cancel");
    }
}
