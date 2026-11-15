use std::sync::Arc;

use agent24_core::util::now_iso8601;
use agent24_models::{Msg, ToolCallRequest};
use agent24_protocol::{
    Approval, ApprovalStatus, Decision, EventBody, Run, RunCancelledPayload, RunStartedPayload,
    RunStatus,
};
use agent24_store::{RunMessage, RunPatch, RunTerminalTransition, StoreError, WorkspaceInstant};
use agent24_tools::{ToolContext, ToolError};
use tokio_util::sync::CancellationToken;

use super::{PROPOSE_PLAN, ParkedCallStop, RunManager, ToolAnswer};
use crate::{
    AgentError, RESUME_TTL, SKILL_LABEL_MESSAGE, SOURCE_UNPROVEN_CODE, SOURCE_UNPROVEN_MESSAGE,
    restore_authority_error_must_propagate, thread_to_messages, unanswered_calls_of_last_turn,
    workspace_now,
};

impl RunManager {
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

    pub(crate) async fn abort_one_approval(&self, approval: &Approval) -> Result<(), AgentError> {
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
    pub(crate) async fn approved_tool(
        &self,
        approval: &Approval,
    ) -> Result<Option<String>, AgentError> {
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
    pub(crate) async fn drive_resume(
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
    pub(crate) async fn settle_parked_call(
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

    pub(crate) async fn reask_parked_call(
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
}

pub(crate) fn restored_decision_is_consistent(approval: &Approval, decision: &Decision) -> bool {
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
