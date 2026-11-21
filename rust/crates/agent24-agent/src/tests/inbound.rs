const PIN_PROMPT: &str = "记住 my bank pin is 1234";

async fn assertion_count(kv: &KvStore) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM mem_assertions")
        .fetch_one(agent24_memory::test_hooks::pool(kv))
        .await
        .unwrap()
}

async fn user_event_origin(kv: &KvStore, content: &str) -> Vec<(String, String)> {
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT origin_source, origin_trust FROM mem_events \
         WHERE kind = 'message' AND json_extract(payload, '$.role') = 'user' \
           AND json_extract(payload, '$.content') = ?",
    )
    .bind(content)
    .fetch_all(agent24_memory::test_hooks::pool(kv))
    .await
    .unwrap();
    rows.iter()
        .map(|row| (row.get("origin_source"), row.get("origin_trust")))
        .collect()
}

fn inbound_event(id: &str, text: &str) -> agent24_store::NewInboundEvent {
    agent24_store::NewInboundEvent {
        id: id.to_owned(),
        source: "hyphae".to_owned(),
        category: "direct".to_owned(),
        recipient_identity: None,
        thread_key: None,
        actor: Some("npub1".to_owned()),
        human: true,
        addressed: "direct".to_owned(),
        at: "2026-11-11T00:00:00Z".to_owned(),
        received_at: "2026-11-11T00:00:01Z".to_owned(),
        text: Some(text.to_owned()),
        meta: None,
        disposition: "explicit".to_owned(),
        disposition_reason: None,
        policy_version: Some("p".to_owned()),
        run_state: Some("starting".to_owned()),
    }
}

async fn start_inbound(
    manager: &Arc<RunManager>,
    store: &Store,
    event_id: &str,
    session_id: &str,
    prompt: &str,
) -> Run {
    assert_eq!(
        store
            .insert_inbound_event(&inbound_event(event_id, prompt))
            .await
            .unwrap(),
        agent24_store::InboundInsert::Inserted
    );
    let started = manager
        .start_run_inbound(
            RunCreate {
                knowledge_query: None,
                idoris_headroom_confirmed: false,
                session_id: Some(session_id.to_owned()),
                workspace_id: None,
                prompt: prompt.to_owned(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
                skill_id: None,
            },
            InboundOrigin {
                event_id: event_id.to_owned(),
                channel: "hyphae".to_owned(),
            },
        )
        .await
        .unwrap();
    let InboundRunStart::Started(run) = started else {
        panic!("event {event_id} was not linked");
    };
    run
}

#[tokio::test]
async fn an_empty_inbound_origin_is_refused() {
    // Mutation: drop the empty channel / event_id guard and both calls
    // link a run. The event rows exist, so the foreign key does not refuse them.
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    assert_eq!(
        store
            .insert_inbound_event(&inbound_event("hyphae:empty-channel", "hello"))
            .await
            .unwrap(),
        agent24_store::InboundInsert::Inserted
    );
    let mut blank = inbound_event("", "hello");
    blank.actor = Some("npub-blank".to_owned());
    assert_eq!(
        store.insert_inbound_event(&blank).await.unwrap(),
        agent24_store::InboundInsert::Inserted
    );

    let err = manager
        .start_run_inbound(
            inbound_create("sess_empty_channel"),
            InboundOrigin {
                event_id: "hyphae:empty-channel".to_owned(),
                channel: String::new(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AgentError::Store(StoreError::Conflict(ref message))
                if message == "inbound origin is empty"
        ),
        "{err:?}"
    );

    let err = manager
        .start_run_inbound(
            inbound_create("sess_empty_event"),
            InboundOrigin {
                event_id: String::new(),
                channel: "hyphae".to_owned(),
            },
        )
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AgentError::Store(StoreError::Conflict(ref message))
                if message == "inbound origin is empty"
        ),
        "{err:?}"
    );

    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(runs, 0);
    let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(sessions, 0);
}

async fn seed_started_inbound(store: &Store, run_id: &str, status: &str) {
    let event_id = format!("hyphae:{run_id}");
    let mut event = inbound_event(&event_id, "hello");
    event.run_state = Some("started".to_owned());
    event.actor = Some(format!("actor-{run_id}"));
    assert_eq!(
        store.insert_inbound_event(&event).await.unwrap(),
        agent24_store::InboundInsert::Inserted
    );
    sqlx::query(
        "INSERT INTO runs (id, status, input, usage, created_at) \
         VALUES (?, ?, \
         '{\"prompt\":\"hello\",\"workspace_id\":null,\"knowledge_binding_id\":null}', \
         '{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0,\"cost_usd\":0.0}', \
         ?)",
    )
    .bind(run_id)
    .bind(status)
    .bind(now_iso8601())
    .execute(agent24_store::test_hooks::pool(store))
    .await
    .unwrap();
    store.insert_inbound_run(run_id, &event_id).await.unwrap();
}

async fn freeze_run_status(store: &Store) {
    sqlx::raw_sql(
        "CREATE TRIGGER freeze_run_status BEFORE UPDATE OF status ON runs \
         BEGIN SELECT RAISE(ABORT, 'frozen'); END;",
    )
    .execute(agent24_store::test_hooks::pool(store))
    .await
    .unwrap();
}

async fn inbound_run_state(store: &Store, event_id: &str) -> String {
    store
        .get_inbound_event(event_id)
        .await
        .unwrap()
        .unwrap()
        .run_state
        .unwrap()
}

fn parked_thread(run_id: &str) -> Vec<RunMessage> {
    vec![RunMessage {
        run_id: run_id.to_owned(),
        seq: 0,
        role: "assistant".to_owned(),
        content: None,
        tool_calls: serde_json::json!([{
            "id": "call_1",
            "name": "http_fetch",
            "arguments": "{}"
        }]),
        tool_call_id: None,
        created_at: now_iso8601(),
    }]
}

async fn park_approval(store: &Store, run_id: &str) -> Approval {
    let now = now_iso8601();
    sqlx::query(
        "INSERT INTO approvals (id, run_id, tool_call_id, kind, summary, payload, \
         available_decisions, status, expires_at, created_at) \
         VALUES (?, ?, 'call_1', 'exec', 'fetch', '{}', '[\"approve\",\"deny\"]', \
         'pending', '2099-01-01T00:00:00Z', ?)",
    )
    .bind(format!("apr_{run_id}"))
    .bind(run_id)
    .bind(&now)
    .execute(agent24_store::test_hooks::pool(store))
    .await
    .unwrap();
    store
        .get_approval(&format!("apr_{run_id}"))
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn a_failed_start_releases_the_inbound_slot() {
    // Mutation: drop `note_inbound_terminal` from
    // `release_inbound_after_abandoned_start`. The status write is
    // frozen, so `finish_cancelled` does not note, and this stays
    // `started`.
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_started_inbound(&store, "run_stuck", "queued").await;
    freeze_run_status(&store).await;
    manager
        .execute("run_stuck".to_owned(), CancellationToken::new(), false)
        .await;
    assert_eq!(inbound_run_state(&store, "hyphae:run_stuck").await, "done");
    assert_eq!(
        store.get_run("run_stuck").await.unwrap().unwrap().status,
        RunStatus::Queued
    );
}

#[tokio::test]
async fn a_blocked_running_transition_cancels_and_releases_the_slot() {
    // The trigger refuses only `NEW.status = 'running'`, so
    // `finish_cancelled` can still write `cancelled`. Mutation: drop
    // `finish_cancelled` from `release_inbound_after_abandoned_start`
    // and this run stays `queued` with no `run.cancelled`.
    let (manager, sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_started_inbound(&store, "run_abandon", "queued").await;
    sqlx::raw_sql(
        "CREATE TRIGGER block_running BEFORE UPDATE OF status ON runs \
         WHEN NEW.status = 'running' \
         BEGIN SELECT RAISE(ABORT, 'no-running'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    manager
        .execute("run_abandon".to_owned(), CancellationToken::new(), false)
        .await;
    assert_eq!(
        store.get_run("run_abandon").await.unwrap().unwrap().status,
        RunStatus::Cancelled
    );
    let events = sink.0.lock().unwrap().clone();
    assert!(
        events.iter().any(|event| event == "run.cancelled"),
        "{events:?}"
    );
    assert_eq!(
        inbound_run_state(&store, "hyphae:run_abandon").await,
        "done"
    );
}

#[tokio::test]
async fn a_resume_that_cannot_enter_running_releases_the_inbound_slot() {
    // The origins check passes (no stored messages). The Running
    // transition is frozen, so this is the "resume transition failed"
    // return. Mutation: drop the release call there and the row stays
    // `started`.
    let (manager, _sink, store) = manager_with_tools(
        Arc::new(FixedProvider),
        ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(false))),
    )
    .await;
    seed_started_inbound(&store, "run_resume", "awaiting_approval").await;
    let approval = park_approval(&store, "run_resume").await;
    let run = store.get_run("run_resume").await.unwrap().unwrap();
    freeze_run_status(&store).await;
    manager
        .drive_resume(
            run,
            parked_thread("run_resume"),
            approval,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(inbound_run_state(&store, "hyphae:run_resume").await, "done");
    assert_eq!(
        store.get_run("run_resume").await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
}

#[tokio::test]
async fn a_resume_whose_source_refusal_cannot_enter_running_releases_the_slot() {
    // A stored message with no origin makes the source check refuse.
    // The Running transition is frozen, so this is the other early
    // return. Mutation: drop the release call there and the row stays
    // `started`.
    let (manager, _sink, store) = manager_with_tools(
        Arc::new(FixedProvider),
        ToolRegistry::new().with(Arc::new(agent24_tools::HttpFetchTool::new(false))),
    )
    .await;
    seed_started_inbound(&store, "run_unproven", "awaiting_approval").await;
    sqlx::query(
        "INSERT INTO run_messages (run_id, seq, role, content, tool_calls, created_at) \
         VALUES ('run_unproven', 0, 'user', 'hi', '[]', ?)",
    )
    .bind(now_iso8601())
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let approval = park_approval(&store, "run_unproven").await;
    let run = store.get_run("run_unproven").await.unwrap().unwrap();
    freeze_run_status(&store).await;
    manager
        .drive_resume(
            run,
            parked_thread("run_unproven"),
            approval,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        inbound_run_state(&store, "hyphae:run_unproven").await,
        "done"
    );
    assert_eq!(
        store.get_run("run_unproven").await.unwrap().unwrap().status,
        RunStatus::AwaitingApproval
    );
}

fn inbound_create(session_id: &str) -> RunCreate {
    RunCreate {
        knowledge_query: None,
        idoris_headroom_confirmed: false,
        session_id: Some(session_id.to_owned()),
        workspace_id: None,
        prompt: "hello".to_owned(),
        model_override: None,
        mode: agent24_protocol::RunMode::Normal,
        knowledge_binding_id: None,
        skill_id: None,
    }
}

#[tokio::test]
async fn redelivery_with_a_new_session_id_is_already_linked() {
    // Mutation: pass `sess_new_1` again. The session insert fails before
    // the event unique index, so this is an error, not AlreadyLinked.
    let (manager, _sink, store) = manager_with(Arc::new(FixedProvider)).await;
    seed_session(&store, "sess_owner").await;
    let first = start_inbound(&manager, &store, "hyphae:again", "sess_new_1", "hello").await;
    let second = manager
        .start_run_inbound(
            inbound_create("sess_new_2"),
            InboundOrigin {
                event_id: "hyphae:again".to_owned(),
                channel: "hyphae".to_owned(),
            },
        )
        .await
        .unwrap();
    assert!(matches!(second, InboundRunStart::AlreadyLinked));
    assert!(store.get_session("sess_new_2").await.unwrap().is_none());
    let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
        .fetch_one(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(runs, 1);
    let run = store.get_run(&first.id).await.unwrap().unwrap();
    assert_eq!(run.session_id.as_deref(), Some("sess_new_1"));
    assert_ne!(run.session_id.as_deref(), Some("sess_owner"));
}

#[tokio::test]
async fn inbound_remember_is_not_a_user_assertion() {
    let (manager, store, kv) =
        manager_with_memory(Arc::new(FixedProvider), Arc::new(UnusedSummarizer)).await;
    let inbound = start_inbound(
        &manager,
        &store,
        "hyphae:pin",
        "sess_inbound_pin",
        PIN_PROMPT,
    )
    .await;
    let inbound = wait_terminal(&store, &inbound.id).await;
    assert_eq!(inbound.status, RunStatus::Completed, "{:?}", inbound.error);
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("inbound".to_owned(), "model".to_owned())]
    );

    seed_session(&store, "sess_owner").await;
    let ordinary = run_in_session(&manager, &store, "sess_owner", PIN_PROMPT).await;
    assert_eq!(
        ordinary.status,
        RunStatus::Completed,
        "{:?}",
        ordinary.error
    );
    assert_eq!(assertion_count(&kv).await, 1);
    let mut origins = user_event_origin(&kv, PIN_PROMPT).await;
    origins.sort();
    assert_eq!(
        origins,
        vec![
            ("agent_loop".to_owned(), "user_said".to_owned()),
            ("inbound".to_owned(), "model".to_owned()),
        ]
    );
}

/// Which user-input tag a resumed run already has. `Ordinary` is a damaged
/// tag: the row exists, but it is not `Unknown`.
enum PinnedUserTag {
    Unknown,
    Ordinary,
}

/// The manager never called `start_run_inbound`, so its in-process map is
/// empty. Resume still reaches `remember_exchange` through `run_loop`.
/// `link` writes `inbound_runs`; `tag` is the only user-input tag.
async fn resume_pinned_remember(link: bool, tag: PinnedUserTag) -> (Store, KvStore) {
    let tools = ToolRegistry::new().with(Arc::new(ReadingTool(
        agent24_tools::ToolResultSource::ordinary("reader"),
    )));
    let (manager, store, kv) = manager_with_memory_and_tools(
        Arc::new(ScriptedProvider::new(vec![])),
        Arc::new(UnusedSummarizer),
        tools,
    )
    .await;
    let now = now_iso8601();
    seed_session(&store, "sess_resume_pin").await;
    let run = Run {
        id: "run_resume_pin".to_owned(),
        session_id: Some("sess_resume_pin".to_owned()),
        workspace_id: None,
        status: RunStatus::AwaitingApproval,
        input: RunInput {
            prompt: PIN_PROMPT.to_owned(),
            workspace_id: None,
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: now.clone(),
        started_at: Some(now.clone()),
        ended_at: None,
    };
    store.insert_run(&run).await.unwrap();
    let call = serde_json::json!([{
        "id": "call_1",
        "name": "reader",
        "arguments": "{}"
    }]);
    store
        .append_originated_run_messages(
            &run.id,
            &[
                OriginatedRunMessage {
                    message: agent24_store::PendingRunMessage {
                        role: "user".into(),
                        content: Some(PIN_PROMPT.into()),
                        tool_calls: serde_json::json!([]),
                        tool_call_id: None,
                    },
                    origin: MessageOrigin::UserPrompt,
                },
                OriginatedRunMessage {
                    message: agent24_store::PendingRunMessage {
                        role: "assistant".into(),
                        content: None,
                        tool_calls: call,
                        tool_call_id: None,
                    },
                    origin: MessageOrigin::Assistant,
                },
            ],
            &now,
        )
        .await
        .unwrap();
    let source = match tag {
        PinnedUserTag::Unknown => {
            agent24_store::SourceRef::inbound_external(&run.id, now.clone())
        }
        PinnedUserTag::Ordinary => agent24_store::SourceRef::user_input(&run.id, now.clone()),
    };
    store
        .tag_run_source(&run.id, 0, &source, &now)
        .await
        .unwrap();
    if link {
        let mut event = inbound_event("hyphae:resume", PIN_PROMPT);
        event.run_state = Some("started".to_owned());
        assert_eq!(
            store.insert_inbound_event(&event).await.unwrap(),
            agent24_store::InboundInsert::Inserted
        );
        store
            .insert_inbound_run(&run.id, "hyphae:resume")
            .await
            .unwrap();
    }
    store
        .insert_approval(&Approval {
            id: "apr_resume_pin".to_owned(),
            run_id: run.id.clone(),
            tool_call_id: "call_1".to_owned(),
            kind: "exec".to_owned(),
            summary: "read".to_owned(),
            payload: serde_json::Map::new(),
            available_decisions: vec!["approve".into(), "deny".into(), "abort".into()],
            standing_target: None,
            status: ApprovalStatus::Approved,
            decision: Some(Decision {
                kind: "approve".to_owned(),
                reason: None,
                extra: serde_json::Map::new(),
            }),
            expires_at: now.clone(),
            created_at: now.clone(),
            decided_at: Some(now),
        })
        .await
        .unwrap();

    manager
        .resume_run(run.id.clone(), "apr_resume_pin".to_owned())
        .await
        .unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed, "{:?}", done.error);
    (store, kv)
}

#[tokio::test]
async fn resumed_inbound_remember_is_not_a_user_assertion() {
    let (_store, kv) = resume_pinned_remember(true, PinnedUserTag::Unknown).await;
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("inbound".to_owned(), "model".to_owned())]
    );
}

#[tokio::test]
async fn inbound_link_without_unknown_tag_is_not_a_user_assertion() {
    // The user-input tag is Ordinary (damaged, or simply not Unknown).
    // `inbound_runs` alone must still refuse the qualified assertion.
    let (_store, kv) = resume_pinned_remember(true, PinnedUserTag::Ordinary).await;
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("inbound".to_owned(), "model".to_owned())]
    );
}

#[tokio::test]
async fn unknown_tag_without_inbound_link_is_not_a_user_assertion() {
    let (_store, kv) = resume_pinned_remember(false, PinnedUserTag::Unknown).await;
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("inbound".to_owned(), "model".to_owned())]
    );
}

#[tokio::test]
async fn inbound_remember_does_not_inject_the_paused_notice() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    kv.set_memory_enabled("test-owner", false).await.unwrap();
    let inbound = start_inbound(
        &manager,
        &store,
        "hyphae:paused",
        "sess_inbound_paused",
        PIN_PROMPT,
    )
    .await;
    let inbound = wait_terminal(&store, &inbound.id).await;
    assert_eq!(inbound.status, RunStatus::Completed, "{:?}", inbound.error);
    let inbound_thread = store.list_run_messages(&inbound.id).await.unwrap();
    assert!(
        inbound_thread
            .iter()
            .all(|message| message.content.as_deref() != Some(retain::PAUSED_WRITE_NOTICE)),
        "{inbound_thread:?}"
    );

    seed_session(&store, "sess_paused_owner").await;
    let ordinary = run_in_session(&manager, &store, "sess_paused_owner", PIN_PROMPT).await;
    assert_eq!(
        ordinary.status,
        RunStatus::Completed,
        "{:?}",
        ordinary.error
    );
    let ordinary_thread = store.list_run_messages(&ordinary.id).await.unwrap();
    assert!(
        ordinary_thread
            .iter()
            .any(|message| message.content.as_deref() == Some(retain::PAUSED_WRITE_NOTICE)),
        "{ordinary_thread:?}"
    );
    assert_eq!(assertion_count(&kv).await, 0);
    let _ = provider;
}

#[tokio::test]
async fn prompt_origin_lookup_failure_is_not_user_said() {
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    struct HoldProvider {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    }
    #[async_trait]
    impl ModelProvider for HoldProvider {
        fn name(&self) -> &str {
            "hold"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            self.entered.notify_one();
            self.release.notified().await;
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
    let (manager, store, kv, log) = manager_with_memory_log(Arc::new(HoldProvider {
        entered: Arc::clone(&entered),
        release: Arc::clone(&release),
    }))
    .await;
    seed_session(&store, "sess_origin_fail").await;
    let run = manager
        .start_run(RunCreate {
            knowledge_query: None,
            idoris_headroom_confirmed: false,
            session_id: Some("sess_origin_fail".to_owned()),
            workspace_id: None,
            prompt: PIN_PROMPT.to_owned(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
            skill_id: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .expect("model call did not start");
    sqlx::raw_sql("DROP TABLE run_source_tags")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    release.notify_one();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed, "{:?}", done.error);
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("agent_loop".to_owned(), "model".to_owned())]
    );
    let skipped = skipped_reasons(&log);
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].reason, "origin_unavailable");
    assert_eq!(skipped[0].run_id, run.id);
    assert_eq!(skipped[0].session_id.as_deref(), Some("sess_origin_fail"));
}

async fn seed_channel(store: &Store, id: &str, channel: &str) {
    let now = now_iso8601();
    store
        .insert_session(&agent24_protocol::Session {
            id: id.to_owned(),
            title: "t".to_owned(),
            channel: channel.to_owned(),
            workspace_id: None,
            created_at: now.clone(),
            updated_at: now,
        })
        .await
        .unwrap();
}

fn chat_source() -> agent24_store::AssertionSourceRef {
    agent24_store::AssertionSourceRef::from_sources([(
        "user_input:chat".to_owned(),
        agent24_store::SourceClassification::Ordinary,
    )])
}

#[tokio::test]
async fn chat_remember_on_a_local_channel_stays_user_said() {
    let (manager, store, kv, log) = manager_with_memory_log(Arc::new(FixedProvider)).await;
    for (id, channel) in [("s_desk", "desktop"), ("s_cli", "cli"), ("s_tui", "tui")] {
        seed_channel(&store, id, channel).await;
        let outcome = manager
            .chat_remember_turn("run_local", id, PIN_PROMPT, "ok", chat_source())
            .await;
        assert_eq!(outcome, MemoryWriteOutcome::Saved, "{channel}");
    }
    // Same object, content-addressed: one assertion, three turns.
    assert_eq!(assertion_count(&kv).await, 1);
    let origins = user_event_origin(&kv, PIN_PROMPT).await;
    assert_eq!(origins.len(), 3, "{origins:?}");
    assert!(
        origins
            .iter()
            .all(|origin| origin == &("chat".to_owned(), "user_said".to_owned())),
        "{origins:?}"
    );
    assert!(skipped_reasons(&log).is_empty());
}

#[tokio::test]
async fn chat_remember_without_a_session_row_stays_user_said() {
    let (manager, store, kv, log) = manager_with_memory_log(Arc::new(FixedProvider)).await;
    assert!(store.get_session("s_absent").await.unwrap().is_none());
    let outcome = manager
        .chat_remember_turn("run_absent", "s_absent", PIN_PROMPT, "ok", chat_source())
        .await;
    assert_eq!(outcome, MemoryWriteOutcome::Saved);
    assert_eq!(assertion_count(&kv).await, 1);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("chat".to_owned(), "user_said".to_owned())]
    );
    assert!(skipped_reasons(&log).is_empty());
}

#[tokio::test]
async fn chat_remember_on_a_creative_channel_stays_user_said() {
    let (manager, store, kv, log) = manager_with_memory_log(Arc::new(FixedProvider)).await;
    seed_channel(&store, "s_creative", "creative").await;
    let outcome = manager
        .chat_remember_turn(
            "run_creative",
            "s_creative",
            PIN_PROMPT,
            "ok",
            chat_source(),
        )
        .await;
    assert_eq!(outcome, MemoryWriteOutcome::Saved);
    assert_eq!(assertion_count(&kv).await, 1);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("chat".to_owned(), "user_said".to_owned())]
    );
    assert!(skipped_reasons(&log).is_empty());
}

#[tokio::test]
async fn chat_remember_on_an_inbound_source_channel_is_not_user_said() {
    let (manager, store, kv, log) = manager_with_memory_log(Arc::new(FixedProvider)).await;
    for (id, channel) in [("s_hy", "hyphae"), ("s_wx", "wechat"), ("s_rm", "remote")] {
        seed_channel(&store, id, channel).await;
        let outcome = manager
            .chat_remember_turn("run_ext", id, PIN_PROMPT, "ok", chat_source())
            .await;
        assert_eq!(outcome, MemoryWriteOutcome::NotApplicable, "{channel}");
    }
    assert_eq!(assertion_count(&kv).await, 0);
    let mut origins = user_event_origin(&kv, PIN_PROMPT).await;
    origins.sort();
    assert_eq!(
        origins,
        vec![
            ("chat".to_owned(), "model".to_owned()),
            ("chat".to_owned(), "model".to_owned()),
            ("chat".to_owned(), "model".to_owned()),
        ]
    );
    assert!(skipped_reasons(&log).is_empty());
}

#[tokio::test]
async fn chat_remember_in_a_session_with_an_inbound_run_is_not_user_said() {
    let (manager, store, kv, _) = manager_with_memory_log(Arc::new(FixedProvider)).await;
    seed_channel(&store, "s_mix", "desktop").await;
    let now = now_iso8601();
    store
        .insert_run(&Run {
            id: "run_mix".to_owned(),
            session_id: Some("s_mix".to_owned()),
            workspace_id: None,
            status: RunStatus::Queued,
            input: RunInput {
                prompt: "hi".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: zero_usage(),
            schedule_id: None,
            created_at: now.clone(),
            started_at: None,
            ended_at: None,
        })
        .await
        .unwrap();
    assert_eq!(
        store
            .insert_inbound_event(&inbound_event("hyphae:mix", "hi"))
            .await
            .unwrap(),
        agent24_store::InboundInsert::Inserted
    );
    store
        .insert_inbound_run("run_mix", "hyphae:mix")
        .await
        .unwrap();
    let outcome = manager
        .chat_remember_turn("run_mix_chat", "s_mix", PIN_PROMPT, "ok", chat_source())
        .await;
    assert_eq!(outcome, MemoryWriteOutcome::NotApplicable);
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("chat".to_owned(), "model".to_owned())]
    );
}

#[tokio::test]
async fn chat_origin_lookup_failure_skips_the_remember() {
    let (manager, store, kv, log) = manager_with_memory_log(Arc::new(FixedProvider)).await;
    seed_channel(&store, "s_fail", "desktop").await;
    sqlx::raw_sql("DROP TABLE inbound_runs")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    let outcome = manager
        .chat_remember_turn("run_fail", "s_fail", PIN_PROMPT, "ok", chat_source())
        .await;
    assert_eq!(outcome, MemoryWriteOutcome::NotApplicable);
    assert_eq!(assertion_count(&kv).await, 0);
    assert_eq!(
        user_event_origin(&kv, PIN_PROMPT).await,
        vec![("chat".to_owned(), "model".to_owned())]
    );
    let skipped = skipped_reasons(&log);
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert_eq!(skipped[0].reason, "origin_unavailable");
    assert_eq!(skipped[0].run_id, "run_fail");
    assert_eq!(skipped[0].session_id.as_deref(), Some("s_fail"));
}

