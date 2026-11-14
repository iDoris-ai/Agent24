use agent24_core::util::{now_iso8601, ulid};
use agent24_models::{ToolCallRequest, ToolSpec};
use agent24_protocol::{
    ErrorBody, EventBody, Run, RunCancelledPayload, RunCompletedPayload, RunFailedPayload,
    RunOutputPayload, RunStatus, ToolCall, ToolCallStatus, ToolCompletedPayload,
    ToolCompletedStatus, ToolStartedPayload, Usage,
};
use agent24_store::{RunPatch, RunTerminalTransition, StoreError, WorkspaceInstant};
use agent24_tools::{GateDecision, ToolError, summarize_input, truncate};
use tokio_util::sync::CancellationToken;

use super::{PROPOSE_PLAN, ParkedCallStop, PlanOutcome, RunManager, ToolAnswer};
use crate::{AgentError, workspace_now};

/// Cap for the externally-visible `output_summary` (full output goes back to
/// the model; full input is audit-only in the store row).
const SUMMARY_MAX_BYTES: usize = 500;

impl RunManager {
    /// Execute one model-requested tool call through the registry pipeline:
    /// persist running → dispatch → persist terminal + event (+ audit on
    /// policy denial). Returns the content handed back to the model, or
    /// `Err(())` when the run was cancelled mid-call.
    pub(crate) async fn run_tool_call(
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
    pub(crate) async fn tool_specs_for(
        &self,
        plan_mode: bool,
        knowledge_bound: bool,
    ) -> Vec<ToolSpec> {
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
    pub(crate) async fn run_plan_proposal(
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

    pub(crate) async fn transition_terminal(
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
    pub(crate) async fn note_inbound_terminal(&self, run_id: &str) {
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
    pub(crate) async fn release_inbound_after_abandoned_start(&self, run_id: &str) {
        self.finish_cancelled(run_id).await;
        self.note_inbound_terminal(run_id).await;
    }

    /// Land the completed terminal state + event with the given output text.
    pub(crate) async fn finish_completed(&self, run_id: &str, text: &str, usage: Usage) {
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
    pub(crate) async fn finish_failed(&self, run_id: &str, code: &str, message: &str) {
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
    pub(crate) async fn finish_cancelled(&self, run_id: &str) {
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
