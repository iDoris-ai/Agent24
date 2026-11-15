use agent24_core::util::now_iso8601;
use agent24_memory::event::{Origin, Trust};
use agent24_models::{Msg, ToolCallRequest};
use agent24_protocol::{
    EventBody, MemoryRecalledPayload, MemoryWriteSkippedPayload, Run, RunCancelledPayload,
};
use agent24_store::{
    MessageOrigin, OriginatedRunMessage, RunTerminalTransition, StoreError, WorkspaceInstant,
};
use tokio_util::sync::CancellationToken;

use super::{ResultTag, RunManager, ToolAnswer};
use crate::retain;
use crate::{
    AgentError, ExplicitRememberState, MemoryFailure, MemoryWriteOutcome, SessionContext,
    workspace_now,
};

impl RunManager {
    pub(crate) async fn cancel_workspace_resume_recovery(&self, run_id: &str) {
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
    pub(crate) async fn session_context(
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
    pub(crate) async fn remember_exchange(
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
    pub(crate) fn report_summary_failure(
        &self,
        sid: &str,
        err: Option<&agent24_memory::MemoryError>,
    ) {
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
    /// the LIVE [`MemoryWriteOutcome`] — see [`crate::RunManager::remember_exchange`].
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
    pub(crate) async fn chat_session_external(&self, session_id: &str) -> Result<bool, StoreError> {
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

    pub(crate) fn inbound_source_channel(channel: &str) -> bool {
        Self::INBOUND_SOURCE_CHANNELS.contains(&channel)
    }

    /// K1-6b.4b (ADR-K1-02 §2.1.1 item 4): the classification a memory
    /// written from this run carries — the strictest of every source
    /// persisted for the run. A failed read is no evidence, so `Unknown`.
    pub(crate) async fn run_assertion_source_ref(
        &self,
        run_id: &str,
    ) -> agent24_store::AssertionSourceRef {
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
    pub(crate) async fn run_prompt_origin(&self, run: &Run) -> Origin {
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
    pub(crate) fn note_origin_unavailable(
        &self,
        run_id: &str,
        session_id: Option<&str>,
        prompt: &str,
    ) {
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
    pub(crate) async fn external_user_prompt(&self, run_id: &str) -> Result<bool, StoreError> {
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
    pub(crate) async fn persisted_external_prompt(&self, run_id: &str) -> bool {
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
    pub(crate) fn tool_result_tag(&self, tool: &str, tool_call_id: &str) -> ResultTag {
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
    pub(crate) async fn answer_tool_call(
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
    pub(crate) async fn persist_message(&self, run_id: &str, msg: &Msg) -> Result<(), StoreError> {
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
    pub(crate) async fn persist_for_next_send(&self, run_id: &str, msg: &Msg) -> bool {
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
}
