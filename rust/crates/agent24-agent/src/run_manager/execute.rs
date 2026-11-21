use agent24_core::util::now_iso8601;
use agent24_models::router::TaskProfile;
use agent24_models::{CompletionRequest, ModelError, Msg};
use agent24_protocol::{
    EventBody, MemoryRecalledPayload, MemoryWriteSkippedPayload, ModelDeltaPayload, RiskClass, Run,
    RunCompletedPayload, RunMode, RunOutputPayload, RunStartedPayload, RunStatus,
};
use agent24_store::{
    MessageOrigin, MessageOriginLookup, OriginatedRunMessage, RunPatch, SnapshotRunMessage,
};
use tokio_util::sync::CancellationToken;

use super::{PROPOSE_PLAN, ParkedCallStop, PlanOutcome, RunManager};
use crate::retain;
use crate::skill_inject;
use crate::{
    MAX_ITERATIONS, MAX_TOOL_CALLS_PER_TURN, PROVIDER_ROLE_MESSAGE, SKILL_LABEL_MESSAGE,
    SOURCE_UNPROVEN_CODE, SOURCE_UNPROVEN_MESSAGE, SessionContext, add_usage, canonical_recall_ref,
    normalize_for_provider, origin_allows_send, shape_host_recall, thread_to_messages,
    unproven_source_failure, zero_usage,
};

impl RunManager {
    pub(crate) async fn execute(
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
        let knowledge_query = self.knowledge_queries.lock().await.remove(&run_id);
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
        if !self
            .search_knowledge_first(&run, knowledge_query.as_deref(), &mut snapshot, &cancel)
            .await
        {
            return;
        }
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
    pub(crate) async fn request_messages_for(
        &self,
        messages: &[Msg],
        run_id: &str,
    ) -> Option<Vec<Msg>> {
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
    pub(crate) async fn resume_sources_proven(&self, run: &Run) -> bool {
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
    pub(crate) async fn skill_label_missing(&self, run_id: &str) -> bool {
        skill_inject::label_missing(&self.store, run_id).await
    }

    /// Stored seq order, not message text. A read error is the same refusal
    /// as a missing or unreadable row: the provider is not called.
    pub(crate) async fn persisted_origins_allow_send(&self, run_id: &str) -> bool {
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
    /// M1 retrieval first (David 2026-11-21, a detail of decision 2026-10-30
    /// A): a 4-bit model answered 「烧开水要多久」 from what it knew, 3 searches
    /// in 5 even when told to. A fresh run that names a Knowledge binding and
    /// a `knowledge_query` searches once before the model is asked, through
    /// the same tool and record as a search the model asks for. Hits go in as
    /// that search's call and result; no hit, a failure or a refusal leaves
    /// the thread as it was, so the answer still comes. False only when the
    /// thread could not be persisted (the run has then ended).
    async fn search_knowledge_first(
        &self,
        run: &Run,
        query: Option<&str>,
        messages: &mut Vec<Msg>,
        cancel: &CancellationToken,
    ) -> bool {
        const SEARCH: &str = "knowledge_search";
        let Some(query) = query.map(str::trim).filter(|q| !q.is_empty()) else {
            return true;
        };
        if run.input.knowledge_binding_id.is_none()
            || retain::explicit_remember(query).is_some()
            || self.tools.tool_risk_class(SEARCH).is_none()
        {
            return true;
        }
        let call = agent24_models::ToolCallRequest {
            id: format!("call_kbfirst_{}", agent24_core::util::ulid()),
            name: SEARCH.to_owned(),
            arguments: serde_json::json!({ "query": query }).to_string(),
        };
        let Ok(answer) = self.run_tool_call(run, &call, cancel, false).await else {
            return true;
        };
        if !has_evidence(&answer.content) {
            return true;
        }
        let asked = Msg::assistant(None, vec![call.clone()]);
        if !self.persist_for_next_send(&run.id, &asked).await {
            return false;
        }
        messages.push(asked);
        self.answer_tool_call(run, messages, &call, answer).await
    }

    pub(crate) async fn run_loop(
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
            // M1 (0.6.73): on 「记住：我最喜欢的水果是芒果」 a 4-bit model also
            // called fs_write to save it to a file. Personal memory takes the
            // turn itself (retain.rs) after the answer; the model only says so,
            // so a remember turn is offered no tools.
            let tools = if self.memory.is_some()
                && retain::explicit_remember(&run.input.prompt).is_some()
            {
                Vec::new()
            } else {
                self.tool_specs_for(plan_mode, run.input.knowledge_binding_id.is_some())
                    .await
            };
            let request = CompletionRequest {
                messages: send_messages,
                model: run.input.model_override.clone(),
                tools,
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
}

/// A knowledge search's result with at least one passage (`evidence`, as
/// `agent24_knowledge::tool::evidence::render_search` writes it). Anything
/// else (none, an error text, a refusal) is no evidence.
fn has_evidence(content: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(content)
        .ok()
        .and_then(|v| {
            v.get("evidence")
                .and_then(|e| e.as_array())
                .map(|e| !e.is_empty())
        })
        .unwrap_or(false)
}
