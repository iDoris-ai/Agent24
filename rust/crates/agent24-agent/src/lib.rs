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

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use agent24_core::util::{now_iso8601, ulid};
use agent24_memory::KvStore;
use agent24_memory::session::{CanonicalSession, CompactionPolicy, Summarizer};
use agent24_models::router::{ModelRouter, TaskProfile};
use agent24_models::{CompletionRequest, ModelError, Msg, ToolSpec};
use agent24_protocol::{
    ErrorBody, EventBody, ModelDeltaPayload, Run, RunCancelledPayload, RunCompletedPayload,
    RunCreate, RunFailedPayload, RunInput, RunOutputPayload, RunStartedPayload, RunStatus,
    ToolCall, ToolCallStatus, ToolCompletedPayload, ToolCompletedStatus, ToolStartedPayload, Usage,
};
use agent24_store::{RunPatch, Store, StoreError};
use agent24_tools::{ToolContext, ToolError, ToolRegistry, summarize_input, truncate};
use tokio_util::sync::CancellationToken;

/// Completion→tools round trips per run before the run is failed. A model
/// stuck asking for tools forever must terminate deterministically.
pub const MAX_ITERATIONS: usize = 10;

/// Cap for the externally-visible `output_summary` (full output goes back to
/// the model; full input is audit-only in the store row).
const SUMMARY_MAX_BYTES: usize = 500;

/// Tool calls executed per assistant turn; the rest are answered with a
/// "skipped" tool result so the wire protocol stays balanced.
pub const MAX_TOOL_CALLS_PER_TURN: usize = 16;

/// Where lifecycle events go (the daemon adapts this onto its WS hub).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, body: EventBody);
}

/// Per-session conversation memory (D1 made live): a KV-backed
/// [`CanonicalSession`] plus the summarizer that compacts it.
///
/// Without this a run starts from the bare prompt, so a "session" carries no
/// conversation memory at all. With it, each run in a session is preceded by the
/// session's context, and the exchange is appended back — with threshold
/// compaction keeping an unbounded conversation a bounded prompt.
pub struct SessionMemory {
    kv: KvStore,
    summarizer: Arc<dyn Summarizer>,
    policy: CompactionPolicy,
    /// Per-session write locks. D1 requires a single writer per session, but
    /// runs execute concurrently (schedules fire them in background tasks), so
    /// `load → append → save` MUST be serialized per session or a later save
    /// silently clobbers an earlier run's turn (review D5b).
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// Absolute ceiling on the verbatim tail, as a multiple of `max_recent`. If
/// compaction keeps failing (e.g. the summarizer's provider is down) the tail
/// would otherwise grow forever and blow the context window; past this the
/// OLDEST messages are dropped. Trimming rather than refusing to record keeps
/// the session live, so a recovered summarizer can compact it again.
const RECENT_HARD_CEILING_FACTOR: usize = 4;

/// Ceiling on the post-completion memory write. Ordering demands it happen
/// before `run.completed`, so it must be bounded — a stuck summarizer must never
/// hang a finished run.
const MEMORY_WRITE_BUDGET: std::time::Duration = std::time::Duration::from_secs(30);

impl SessionMemory {
    pub fn new(kv: KvStore, summarizer: Arc<dyn Summarizer>) -> Self {
        Self {
            kv,
            summarizer,
            policy: CompactionPolicy::default(),
            locks: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub fn with_policy(mut self, policy: CompactionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// The per-session write lock, created on first use. Unreferenced entries
    /// are swept so the map can't grow without bound; sweeping only removes
    /// locks nobody holds (`strong_count == 1`), so mutual exclusion is safe.
    async fn session_lock(&self, session_id: &str) -> Arc<Mutex<()>> {
        let mut locks = self.locks.lock().await;
        if locks.len() > 1024 {
            locks.retain(|_, l| Arc::strong_count(l) > 1);
        }
        Arc::clone(locks.entry(session_id.to_owned()).or_default())
    }
}

/// A [`Summarizer`] backed by the model router.
///
/// Uses the SAME [`TaskProfile::default()`] as chat/runs deliberately: the
/// conversation being summarized already went to whichever provider the router
/// picked for those calls, so summarizing it there is no new exposure — whereas
/// forcing LocalOnly would silently stop compacting on remote-only setups.
pub struct RouterSummarizer {
    router: Arc<ModelRouter>,
    /// Daemon shutdown token — compaction can call a slow provider, and a stuck
    /// summarizer must not outlive shutdown (review D5b).
    shutdown: CancellationToken,
}

/// Per-message budget in the summarization transcript. Generous, because
/// whatever the summarizer doesn't SEE is dropped for good once the fold
/// commits; elision is marked so the summarizer knows content was cut.
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
            // Mark elision explicitly: anything the summarizer can't see is lost
            // once the fold commits, so it must at least know it was cut.
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
        };
        let (_provider, res) = self
            .router
            .complete(TaskProfile::default(), &req, &self.shutdown)
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

fn add_usage(mut total: Usage, delta: &Usage) -> Usage {
    total.prompt_tokens = total.prompt_tokens.saturating_add(delta.prompt_tokens);
    total.completion_tokens = total
        .completion_tokens
        .saturating_add(delta.completion_tokens);
    total.total_tokens = total.total_tokens.saturating_add(delta.total_tokens);
    total.cost_usd += delta.cost_usd;
    total
}

pub struct RunManager {
    store: Store,
    router: Arc<ModelRouter>,
    tools: Arc<ToolRegistry>,
    sink: Arc<dyn EventSink>,
    /// Optional per-session conversation memory (D1). `None` = runs start from
    /// the bare prompt, exactly as before.
    memory: Option<SessionMemory>,
    /// Daemon-wide shutdown token; every run token is a child of it
    shutdown: CancellationToken,
    /// Live run cancellation tokens; entries removed when a run reaches a
    /// terminal state. tokio::sync::Mutex — no poisoning, so a panicked task
    /// can never silently disable cancellation (review C2).
    cancels: Mutex<HashMap<String, CancellationToken>>,
}

impl RunManager {
    pub fn new(
        store: Store,
        router: Arc<ModelRouter>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Self::with_memory(store, router, tools, sink, shutdown, None)
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
        Arc::new(Self {
            store,
            router,
            tools,
            sink,
            memory,
            shutdown,
            cancels: Mutex::new(HashMap::new()),
        })
    }

    /// Create a run (202 semantics: persisted queued, executed in background).
    /// The session's prior context, or empty when memory is off / this run has
    /// no session. Best-effort: a memory failure degrades to a fresh context
    /// rather than failing the run.
    /// Returns `None` if the run was cancelled while waiting — the caller must
    /// then finish it cancelled rather than proceed with an empty context.
    ///
    /// CANCEL-AWARE by necessity: this takes the per-session lock, and another
    /// run in the same session can hold that lock for up to MEMORY_WRITE_BUDGET
    /// while it compacts. A run parked here hasn't even reached its model call
    /// yet, so blocking it uncancellably would break the C2 contract that cancel
    /// works in ANY non-terminal state — the same reason the model call and the
    /// memory write are raced against the token (review D5b).
    async fn session_context(
        &self,
        session_id: Option<&str>,
        cancel: &CancellationToken,
    ) -> Option<Vec<Msg>> {
        let (Some(memory), Some(sid)) = (self.memory.as_ref(), session_id) else {
            return Some(Vec::new());
        };
        // Take the same per-session lock as the writer so a read can never
        // observe a half-written session (a concurrent run's load→append→save).
        let load = async {
            let lock = memory.session_lock(sid).await;
            let _guard = lock.lock().await;
            CanonicalSession::load(&memory.kv, sid).await
        };
        let loaded = tokio::select! {
            result = load => result,
            () = cancel.cancelled() => return None,
        };
        match loaded {
            Ok(Some(session)) => Some(session.context()),
            Ok(None) => Some(Vec::new()),
            Err(err) => {
                tracing::warn!("session {sid} memory load failed: {err}");
                Some(Vec::new())
            }
        }
    }

    /// Append this exchange to the session and persist it. Best-effort — an
    /// already-successful run must never fail because memory did.
    ///
    /// Note the save happens even when compaction errors: `append` deliberately
    /// leaves the message in `recent` on summarizer failure (D1's no-loss
    /// guarantee), so persisting keeps the turn and lets the next append retry
    /// the fold. Skipping the save is what would lose it.
    async fn remember_exchange(&self, session_id: Option<&str>, prompt: &str, answer: &str) {
        let (Some(memory), Some(sid)) = (self.memory.as_ref(), session_id) else {
            return;
        };
        // Serialize the read-modify-write per session: concurrent runs in the
        // same session would otherwise both load the old state and the later
        // save would drop the earlier run's turn (review D5b).
        let lock = memory.session_lock(sid).await;
        let _guard = lock.lock().await;

        let mut session = match CanonicalSession::load(&memory.kv, sid).await {
            Ok(Some(session)) => session,
            Ok(None) => CanonicalSession::new(sid),
            Err(err) => {
                tracing::warn!("session {sid} memory load failed, not persisting turn: {err}");
                return;
            }
        };
        // ALWAYS append — `append` is also where compaction is retried, so
        // returning early here would freeze a session forever once it grew
        // (it could never compact again, even after the summarizer recovered).
        for msg in [
            Msg::user(prompt.to_owned()),
            Msg::assistant(Some(answer.to_owned()), vec![]),
        ] {
            if let Err(err) = session
                .append(msg, memory.policy, memory.summarizer.as_ref())
                .await
            {
                // Compaction failed; the message is still in `recent` (D1's
                // no-loss guarantee), so saving keeps the turn and the next
                // append retries the fold.
                tracing::warn!("session {sid} compaction failed (turn kept verbatim): {err}");
            }
        }
        // Boundedness backstop: with compaction persistently failing the tail
        // would grow every run and be fed back in full. Trim the OLDEST verbatim
        // messages back to the policy's keep window — losing the oldest history
        // beats an unusable prompt, and unlike refusing to record it leaves the
        // session live so a recovered summarizer heals it.
        // Normalize max_recent the same way CanonicalSession::append does: a
        // custom max_recent of 0 would make the ceiling 0, which `keep >= 1` can
        // never satisfy — the "hard" ceiling would be unenforceable.
        let ceiling = memory
            .policy
            .max_recent
            .max(1)
            .saturating_mul(RECENT_HARD_CEILING_FACTOR);
        if session.recent.len() > ceiling {
            // Clamp against the ceiling: a degenerate custom policy (e.g.
            // keep_recent > ceiling) would otherwise compute drop_n == 0 and
            // silently fail to enforce the bound at all.
            let keep = memory
                .policy
                .keep_recent
                .min(ceiling.saturating_sub(1))
                .max(1);
            let drop_n = session.recent.len().saturating_sub(keep);
            tracing::error!(
                "session {sid} verbatim tail ({}) exceeded the hard ceiling ({ceiling}); dropping \
                 the {drop_n} oldest messages — compaction is failing, check the summarizer's \
                 provider",
                session.recent.len()
            );
            session.recent.drain(0..drop_n);
        }
        if let Err(err) = session.save(&memory.kv).await {
            tracing::warn!("session {sid} memory save failed: {err}");
        }
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

        let run = Run {
            id: format!("run_{}", ulid()),
            session_id: create.session_id.clone(),
            status: RunStatus::Queued,
            input: RunInput {
                prompt: create.prompt,
                model_override: create.model_override,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id,
            created_at: now_iso8601(),
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
        if let Err(err) = self.store.insert_run(&run).await {
            self.cancels.lock().await.remove(&run.id);
            return Err(err.into());
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
                self.finish_cancelled(id).await;
            }
        }
        self.store
            .get_run(id)
            .await?
            .ok_or_else(|| AgentError::Store(StoreError::NotFound(format!("run {id}"))))
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

        // C3 loop body: completion → tool execution round trips, bounded by
        // MAX_ITERATIONS. Usage accumulates across iterations.
        let tool_specs: Vec<ToolSpec> = self
            .tools
            .adverts()
            .into_iter()
            .map(|a| ToolSpec {
                name: a.name,
                description: a.description,
                parameters: a.parameters,
            })
            .collect();
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
        let mut messages = prior_context;
        messages.push(Msg::user(run.input.prompt.clone()));
        let mut usage_total = zero_usage();

        for _ in 0..MAX_ITERATIONS {
            let request = CompletionRequest {
                messages: messages.clone(),
                model: run.input.model_override.clone(),
                tools: tool_specs.clone(),
            };
            let outcome = tokio::select! {
                r = self.router.complete(TaskProfile::default(), &request, &cancel) => r,
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
                    let (code, message) = match &err {
                        ModelError::Unavailable(msg) => (
                            "provider_unavailable",
                            format!("All LLM providers unavailable. Last error: {msg}"),
                        ),
                        other => ("internal", other.to_string()),
                    };
                    self.finish_failed(&run_id, code, &message).await;
                    return;
                }
            };
            usage_total = add_usage(usage_total, &res.usage);

            if res.message.tool_calls.is_empty() {
                // Final answer
                let text = res.message.content.clone().unwrap_or_default();
                self.sink.emit(EventBody::ModelDelta(ModelDeltaPayload {
                    run_id: run_id.clone(),
                    text: text.clone(),
                }));
                // Persist memory BEFORE the run becomes observable as completed
                // — through the STORE ROW as well as the event. A client polling
                // get_run/list_runs could otherwise see `completed`, start the
                // next run in this session, win the session lock and read stale
                // memory (review D5b). Bounded by MEMORY_WRITE_BUDGET (and the
                // daemon shutdown token inside the summarizer) so a stuck
                // provider can never hang a finished run.
                //
                // This widens the window in which the run is still non-terminal,
                // so it MUST stay cancellable: `cancel works in any non-terminal
                // state` is the C2 contract, and a 30s uncancellable finalization
                // would break it (review D5b).
                let memory_write = tokio::time::timeout(
                    MEMORY_WRITE_BUDGET,
                    self.remember_exchange(run.session_id.as_deref(), &run.input.prompt, &text),
                );
                let timed_out = tokio::select! {
                    r = memory_write => r.is_err(),
                    () = cancel.cancelled() => {
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
                };
                if timed_out {
                    tracing::warn!(
                        "run {run_id} session memory write exceeded {MEMORY_WRITE_BUDGET:?}; completing without recording the turn"
                    );
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
                match self
                    .store
                    .transition_run(
                        &run_id,
                        RunStatus::Completed,
                        RunPatch {
                            output: Some(agent24_protocol::RunOutput { text: text.clone() }),
                            usage: Some(usage_total.clone()),
                            ended_at: Some(now_iso8601()),
                            ..Default::default()
                        },
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
            messages.push(res.message);
            for (idx, call) in calls.iter().enumerate() {
                if cancel.is_cancelled() {
                    self.finish_cancelled(&run_id).await;
                    return;
                }
                if idx >= MAX_TOOL_CALLS_PER_TURN {
                    messages.push(Msg::tool_result(
                        call.id.clone(),
                        format!(
                            "skipped: per-turn tool call limit ({MAX_TOOL_CALLS_PER_TURN}) exceeded"
                        ),
                    ));
                    continue;
                }
                match self
                    .run_tool_call(&run_id, run.session_id.as_deref(), call, &cancel)
                    .await
                {
                    Ok(content) => messages.push(Msg::tool_result(call.id.clone(), content)),
                    Err(()) => {
                        // Cancelled mid-tool, or the user chose abort on an
                        // approval — either way the run lands cancelled
                        self.finish_cancelled(&run_id).await;
                        return;
                    }
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
        run_id: &str,
        session_id: Option<&str>,
        call: &agent24_models::ToolCallRequest,
        cancel: &CancellationToken,
    ) -> Result<String, ()> {
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
            None => {
                let ctx = ToolContext {
                    run_id: run_id.to_owned(),
                    session_id: session_id.map(str::to_owned),
                    tool_call_id: tc.id.clone(),
                };
                self.tools.dispatch(&call.name, &ctx, &input, cancel).await
            }
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

        if cancelled { Err(()) } else { Ok(content) }
    }

    /// Land the failed terminal state + event.
    async fn finish_failed(&self, run_id: &str, code: &str, message: &str) {
        let body = ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
        };
        match self
            .store
            .transition_run(
                run_id,
                RunStatus::Failed,
                RunPatch {
                    error: Some(body.clone()),
                    ended_at: Some(now_iso8601()),
                    ..Default::default()
                },
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
        match self
            .store
            .transition_run(
                run_id,
                RunStatus::Cancelled,
                RunPatch {
                    ended_at: Some(now_iso8601()),
                    ..Default::default()
                },
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

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_models::router::Tier;
    use agent24_models::{CompletionResponse, ModelProvider, ToolCallRequest};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

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
            Some(SessionMemory::new(kv.clone(), summarizer)),
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
                created_at: now.clone(),
                updated_at: now,
            })
            .await
            .unwrap();
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
                prompt: prompt.to_owned(),
                model_override: None,
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
    async fn concurrent_runs_in_one_session_keep_both_turns() {
        // Codex (High): remember_exchange is load→append→save, and runs execute
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
                    prompt: prompt.to_owned(),
                    model_override: None,
                })
                .await
                .unwrap();
            ids.push(run.id);
        }
        for id in &ids {
            wait_terminal(&store, id).await;
        }
        // The memory write happens just after run.completed, so give the
        // best-effort persist a moment to land.
        let mut session = None;
        for _ in 0..200 {
            let loaded = CanonicalSession::load(&kv, "sess_race").await.unwrap();
            if loaded.as_ref().is_some_and(|s| s.recent.len() >= 4) {
                session = loaded;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let session = session.expect("both turns should have been recorded");
        let texts: Vec<&str> = session
            .recent
            .iter()
            .filter_map(|m| m.content.as_deref())
            .collect();
        assert!(texts.contains(&"alpha"), "lost a turn: {texts:?}");
        assert!(texts.contains(&"beta"), "lost a turn: {texts:?}");
        assert_eq!(session.recent.len(), 4, "{texts:?}");
    }

    #[tokio::test]
    async fn many_concurrent_writers_on_one_session_lose_nothing() {
        // Codex (low): the two-run test can pass even unlocked if tokio happens
        // to serialize. Drive remember_exchange directly from many tasks at once
        // — an unlocked load→append→save loses turns here with high probability.
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
                m.remember_exchange(Some("sess_many"), &format!("q{i}"), &format!("a{i}"))
                    .await;
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        let session = CanonicalSession::load(&kv, "sess_many")
            .await
            .unwrap()
            .expect("session should exist");
        let texts: Vec<&str> = session
            .recent
            .iter()
            .filter_map(|m| m.content.as_deref())
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
        assert_eq!(session.recent.len(), WRITERS * 2, "{texts:?}");
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
                .with_policy(policy),
            ),
        );
        seed_session(&store, "sess_cancel").await;
        let run = manager
            .start_run(RunCreate {
                session_id: Some("sess_cancel".to_owned()),
                prompt: "hi".to_owned(),
                model_override: None,
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
                .with_policy(policy),
            ),
        );
        seed_session(&store, "sess_lockwait").await;

        // Run A: proceed until it is inside compaction, holding the lock.
        let run_a = manager
            .start_run(RunCreate {
                session_id: Some("sess_lockwait".to_owned()),
                prompt: "a".to_owned(),
                model_override: None,
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
                prompt: "b".to_owned(),
                model_override: None,
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
            prompt: "hi".to_owned(),
            model_override: None,
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
                prompt: "hi".to_owned(),
                model_override: None,
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
        let tools = ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(true)));
        let provider = ScriptedProvider::new(vec![tool_call_turn(
            "http_fetch",
            serde_json::json!({ "url": url }).to_string(),
        )]);
        let (manager, sink, store) = manager_with_tools(Arc::new(provider), tools).await;
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

    use super::tests::*;
    use super::*;
    use agent24_models::router::Tier;
    use agent24_policy::{ApprovalBroker, BrokerGate};
    use agent24_protocol::{ApprovalStatus, Decision};
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
        let tools = ToolRegistry::builtin(workdir)
            .with_gate(Arc::new(BrokerGate::new(Arc::clone(&broker))));
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
        );
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
