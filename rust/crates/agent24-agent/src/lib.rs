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
use agent24_models::{CompletionRequest, ModelError, Msg, ProviderRegistry, ToolSpec};
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

/// Where lifecycle events go (the daemon adapts this onto its WS hub).
pub trait EventSink: Send + Sync + 'static {
    fn emit(&self, body: EventBody);
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
    registry: Arc<ProviderRegistry>,
    tools: Arc<ToolRegistry>,
    sink: Arc<dyn EventSink>,
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
        registry: Arc<ProviderRegistry>,
        tools: Arc<ToolRegistry>,
        sink: Arc<dyn EventSink>,
        shutdown: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            registry,
            tools,
            sink,
            shutdown,
            cancels: Mutex::new(HashMap::new()),
        })
    }

    /// Create a run (202 semantics: persisted queued, executed in background).
    pub async fn start_run(self: &Arc<Self>, create: RunCreate) -> Result<Run, AgentError> {
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
            schedule_id: None,
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
        let mut messages = vec![Msg::user(run.input.prompt.clone())];
        let mut usage_total = zero_usage();

        for _ in 0..MAX_ITERATIONS {
            let request = CompletionRequest {
                messages: messages.clone(),
                model: run.input.model_override.clone(),
                tools: tool_specs.clone(),
            };
            let outcome = tokio::select! {
                r = self.registry.complete(&request, &cancel) => r,
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
                    Ok(_) => self.sink.emit(EventBody::RunCompleted(RunCompletedPayload {
                        run_id,
                        output: RunOutputPayload { text },
                        usage: usage_total,
                    })),
                    Err(err) => tracing::error!("run completion persist failed: {err}"),
                }
                return;
            }

            // Tool round trip: echo the assistant turn, then answer every call
            let calls = res.message.tool_calls.clone();
            messages.push(res.message);
            for call in &calls {
                if cancel.is_cancelled() {
                    self.finish_cancelled(&run_id).await;
                    return;
                }
                match self.run_tool_call(&run_id, call, &cancel).await {
                    Ok(content) => messages.push(Msg::tool_result(call.id.clone(), content)),
                    Err(()) => {
                        // Cancelled mid-tool
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

        let outcome = match parse_error {
            Some(msg) => Err(ToolError::Invalid(msg)),
            None => {
                let ctx = ToolContext {
                    run_id: run_id.to_owned(),
                };
                self.tools.dispatch(&call.name, &ctx, &input, cancel).await
            }
        };

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
            Err(err) => {
                let content = format!("tool error: {err}");
                (ToolCallStatus::Failed, content.clone(), content, false)
            }
        };

        if let Err(err) = self
            .store
            .finish_tool_call(&tc.id, status, Some(summary.clone()), now_iso8601())
            .await
        {
            tracing::error!("tool call finish persist failed: {err}");
        }
        self.sink
            .emit(EventBody::ToolCompleted(ToolCompletedPayload {
                run_id: run_id.to_owned(),
                tool_call_id: tc.id.clone(),
                status: match status {
                    ToolCallStatus::Completed => ToolCompletedStatus::Completed,
                    ToolCallStatus::Denied => ToolCompletedStatus::Denied,
                    _ => ToolCompletedStatus::Failed,
                },
                output_summary: Some(summary),
            }));

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
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
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

    /// Plays a fixed sequence of assistant turns, then echoes the last tool
    /// result as the final answer.
    struct ScriptedProvider {
        turns: StdMutex<Vec<Msg>>,
    }

    impl ScriptedProvider {
        fn new(turns: Vec<Msg>) -> Self {
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
            Arc::new(ProviderRegistry::new(vec![provider])),
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

    fn create() -> RunCreate {
        RunCreate {
            session_id: None,
            prompt: "hi".to_owned(),
            model_override: None,
        }
    }

    async fn wait_terminal(store: &Store, id: &str) -> Run {
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
