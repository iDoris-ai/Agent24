// R05-5 4b: the provider send copy is shaped by the PERSISTED origin kind of
// each stored row, never by message text or in-memory position.
use super::*;
use agent24_memory::assertion::{AssertionStore, BeliefQuery};
use agent24_memory::event::{Origin, Trust};
use agent24_policy::{ApprovalBroker, BrokerGate};
use agent24_store::{MessageOrigin, OriginatedRunMessage, PendingRunMessage};

fn test_source_ref() -> agent24_store::AssertionSourceRef {
    agent24_store::AssertionSourceRef::from_sources([(
        "test".to_owned(),
        agent24_store::SourceClassification::Ordinary,
    )])
}

const OWNER: &str = "test-owner";

// ---- warn capture (no tracing-subscriber dev-dependency in this crate) ----

#[derive(Clone, Default)]
struct WarnLog(Arc<StdMutex<Vec<String>>>);

struct FieldText(String);

impl tracing::field::Visit for FieldText {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!("{}={:?} ", field.name(), value));
    }
}

struct WarnSubscriber(WarnLog);

impl tracing::Subscriber for WarnSubscriber {
    fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        if *event.metadata().level() == tracing::Level::WARN {
            let mut text = FieldText(String::new());
            event.record(&mut text);
            self.0.0.lock().unwrap().push(text.0);
        }
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

fn capture_warns() -> (WarnLog, tracing::subscriber::DefaultGuard) {
    let log = WarnLog::default();
    let guard = tracing::subscriber::set_default(WarnSubscriber(log.clone()));
    (log, guard)
}

fn host_recall_warned(log: &WarnLog) -> bool {
    log.0.lock().unwrap().iter().any(|w| w.contains("HostRecall"))
}

// ---- fixtures ----

fn recorder() -> Arc<RecordingProvider> {
    Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    })
}

fn manager_over(
    store: &Store,
    provider: Arc<RecordingProvider>,
    memory: Option<SessionMemory>,
) -> Arc<RunManager> {
    RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            provider as Arc<dyn agent24_models::ModelProvider>,
            agent24_models::router::Tier::Local,
        )])),
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingSink(StdMutex::new(vec![]))),
        CancellationToken::new(),
        memory,
    )
}

fn memory_of(kv: &KvStore, owner: Option<&str>) -> SessionMemory {
    let memory = SessionMemory::new(kv.clone(), Arc::new(UnusedSummarizer));
    match owner {
        Some(owner) => memory.with_owner(owner.to_owned()),
        None => memory,
    }
}

/// Saves each text as a live assertion and returns `(id, text)` in input order.
async fn live_assertions(kv: &KvStore, texts: &[&str]) -> Vec<(String, String)> {
    for (i, text) in texts.iter().enumerate() {
        let outcome = retain::persist(
            kv,
            OWNER,
            text,
            format!("event-{i}"),
            Origin {
                source: "test".into(),
                trust: Trust::UserSaid,
            },
            &test_source_ref(),
        )
        .await
        .unwrap();
        assert_eq!(outcome, retain::RetainOutcome::Saved);
    }
    let beliefs = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(OWNER))
        .await
        .unwrap();
    texts
        .iter()
        .map(|text| {
            let found = beliefs
                .iter()
                .find(|b| b.object.as_str() == Some(*text))
                .unwrap();
            (found.id.clone(), (*text).to_owned())
        })
        .collect()
}

async fn revoke(kv: &KvStore, id: &str) {
    kv.forget(OWNER, id, &now_iso8601()).await.unwrap();
}

fn block(entries: &[&(String, String)]) -> String {
    let mut text = String::from(RECALL_PREFIX);
    for (id, fact) in entries {
        text.push_str(&format!(
            "\n- [id={id} recorded_at=2026-01-01T00:00:00.000Z] \"{fact}\""
        ));
    }
    text.push_str(RECALL_END_MARKER);
    text
}

fn row(role: &str, content: Option<&str>, origin: MessageOrigin) -> OriginatedRunMessage {
    OriginatedRunMessage {
        message: PendingRunMessage {
            role: role.into(),
            content: content.map(str::to_owned),
            tool_calls: serde_json::json!([]),
            tool_call_id: None,
        },
        origin,
    }
}

fn host_recall(content: &str) -> OriginatedRunMessage {
    row(
        "user",
        Some(content),
        MessageOrigin::HostRecall {
            recall_ref: Some("[\"a\"]".into()),
        },
    )
}

fn filler(text: &str) -> OriginatedRunMessage {
    row("assistant", Some(text), MessageOrigin::Assistant)
}

async fn plant(store: &Store, run_id: &str, rows: Vec<OriginatedRunMessage>) {
    store
        .append_originated_run_messages(run_id, &rows, &now_iso8601())
        .await
        .unwrap();
}

/// A completed run (rows: user "hi", assistant "pong") to plant further rows on.
async fn base_run(manager: &Arc<RunManager>, store: &Store) -> String {
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(store, &run.id).await.status,
        RunStatus::Completed
    );
    run.id
}

/// Reopen the run and send once from the stored thread. Returns the terminal run.
async fn drive(manager: &Arc<RunManager>, store: &Store, run_id: &str) -> Run {
    sqlx::query("UPDATE runs SET status = 'running', ended_at = NULL, error = NULL WHERE id = ?")
        .bind(run_id)
        .execute(agent24_store::test_hooks::pool(store))
        .await
        .unwrap();
    let running = store.get_run(run_id).await.unwrap().unwrap();
    let thread = store.list_run_messages(run_id).await.unwrap();
    manager
        .run_loop(
            running,
            thread_to_messages(&thread),
            false,
            CancellationToken::new(),
        )
        .await;
    store.get_run(run_id).await.unwrap().unwrap()
}

fn first_send(provider: &RecordingProvider) -> Vec<Msg> {
    let seen = provider.seen.lock().unwrap();
    assert!(!seen.is_empty(), "provider was never called");
    seen[0].clone()
}

fn count_eq(messages: &[Msg], text: &str) -> usize {
    messages
        .iter()
        .filter(|m| m.content.as_deref() == Some(text))
        .count()
}

fn mentions(messages: &[Msg], needle: &str) -> bool {
    messages
        .iter()
        .any(|m| m.content.as_deref().is_some_and(|c| c.contains(needle)))
}

// ---- tests ----

#[tokio::test]
async fn send_copy_keeps_user_prompt_shaped_like_recall() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["I am allergic to peanuts"]).await;
    revoke(&kv, &facts[0].0).await;
    let shaped = block(&[&facts[0]]);
    let run = manager
        .start_run(RunCreate {
            session_id: None,
            workspace_id: None,
            prompt: shaped.clone(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    assert_eq!(first_send(&provider), vec![Msg::user(shaped)]);
}

#[tokio::test]
async fn send_copy_keeps_session_history_shaped_like_recall() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["I am allergic to peanuts"]).await;
    revoke(&kv, &facts[0].0).await;
    let shaped = block(&[&facts[0]]);
    seed_session(&store, "trim-hist").await;
    kv.session_log()
        .append_turn(
            OWNER,
            "trim-hist",
            0,
            &Msg::user(shaped.clone()),
            Origin {
                source: "user".into(),
                trust: Trust::UserSaid,
            },
            &Msg::assistant(Some("earlier answer".into()), vec![]),
            Origin {
                source: "agent".into(),
                trust: Trust::Model,
            },
        )
        .await
        .unwrap();
    let run = manager
        .start_run(RunCreate {
            session_id: Some("trim-hist".into()),
            workspace_id: None,
            prompt: "next".into(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    let sent = first_send(&provider);
    assert_eq!(
        sent,
        vec![
            Msg::user(shaped),
            Msg::assistant(Some("earlier answer".into()), vec![]),
            Msg::user("next"),
        ]
    );
}

#[tokio::test]
async fn send_copy_trims_revoked_ids_from_real_host_recall() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["I am allergic to peanuts"]).await;
    let run = manager
        .start_run(RunCreate {
            session_id: None,
            workspace_id: None,
            prompt: "peanuts".into(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    let thread = store.list_run_messages(&run.id).await.unwrap();
    assert!(
        matches!(
            store.read_run_message_origin(&run.id, thread[0].seq).await.unwrap(),
            MessageOriginLookup::Present(ref s) if matches!(s.origin, MessageOrigin::HostRecall { .. })
        ),
        "the fresh run must have produced a real HostRecall at seq 0"
    );
    assert!(mentions(&first_send(&provider), &facts[0].1));
    // Positive control above; now revoke and send again from the stored thread.
    revoke(&kv, &facts[0].0).await;
    provider.seen.lock().unwrap().clear();
    let done = drive(&manager, &store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&provider);
    assert!(!mentions(&sent, &facts[0].1), "revoked line must be gone");
    assert!(!mentions(&sent, RECALL_PREFIX), "whole block dropped");
    assert!(count_eq(&sent, "peanuts") == 1, "the user prompt stays");
}

#[tokio::test]
async fn send_copy_handles_host_recall_not_at_position_zero() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["fact alive", "fact revoked"]).await;
    revoke(&kv, &facts[1].0).await;
    let run_id = base_run(&manager, &store).await;
    plant(
        &store,
        &run_id,
        vec![
            host_recall(&block(&[&facts[0], &facts[1]])),
            filler("f1"),
        ],
    )
    .await;
    provider.seen.lock().unwrap().clear();
    assert_eq!(
        drive(&manager, &store, &run_id).await.status,
        RunStatus::Completed
    );
    let sent = first_send(&provider);
    assert_eq!(count_eq(&sent, &block(&[&facts[0]])), 1);
    assert!(!mentions(&sent, "fact revoked"));
}

#[tokio::test]
async fn send_copy_drops_host_recall_when_all_ids_revoked() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["fact one", "fact two"]).await;
    revoke(&kv, &facts[0].0).await;
    revoke(&kv, &facts[1].0).await;
    let run_id = base_run(&manager, &store).await;
    plant(
        &store,
        &run_id,
        vec![host_recall(&block(&[&facts[0], &facts[1]])), filler("f1")],
    )
    .await;
    provider.seen.lock().unwrap().clear();
    drive(&manager, &store, &run_id).await;
    let sent = first_send(&provider);
    assert!(!mentions(&sent, RECALL_PREFIX));
    assert_eq!(count_eq(&sent, "f1"), 1);
}

#[tokio::test]
async fn send_copy_drops_host_recall_when_active_ids_unreadable() {
    let (log, _guard) = capture_warns();
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["fact alive"]).await;
    let run_id = base_run(&manager, &store).await;
    plant(
        &store,
        &run_id,
        vec![
            host_recall(&block(&[&facts[0]])),
            filler("f1"),
            row("user", Some("keep me"), MessageOrigin::UserPrompt),
        ],
    )
    .await;
    // Same store, memory whose owner is unset: `active_ids()` fails.
    let provider2 = recorder();
    let ownerless = manager_over(&store, provider2.clone(), Some(memory_of(&kv, None)));
    drive(&ownerless, &store, &run_id).await;
    let sent = first_send(&provider2);
    assert!(!mentions(&sent, RECALL_PREFIX));
    assert_eq!(count_eq(&sent, "keep me"), 1);
    assert!(host_recall_warned(&log));
}

#[tokio::test]
async fn send_copy_three_same_text_origins_each_by_kind() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["fact alive", "fact revoked"]).await;
    revoke(&kv, &facts[1].0).await;
    let full = block(&[&facts[0], &facts[1]]);
    let pruned = block(&[&facts[0]]);
    let run_id = base_run(&manager, &store).await;
    plant(
        &store,
        &run_id,
        vec![
            host_recall(&full),
            filler("f1"),
            row("user", Some(&full), MessageOrigin::UserPrompt),
            filler("f2"),
            row("user", Some(&full), MessageOrigin::SessionHistory),
            filler("f3"),
        ],
    )
    .await;
    provider.seen.lock().unwrap().clear();
    drive(&manager, &store, &run_id).await;
    let sent = first_send(&provider);
    assert_eq!(count_eq(&sent, &pruned), 1, "only the HostRecall row is trimmed");
    assert_eq!(count_eq(&sent, &full), 2, "the other two arrive verbatim");
}

#[tokio::test]
async fn send_copy_drops_malformed_host_recall() {
    let (log, _guard) = capture_warns();
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["fact alive"]).await;
    let no_end = {
        let mut text = block(&[&facts[0]]);
        text.truncate(text.len() - RECALL_END_MARKER.len());
        text
    };
    let well_formed = block(&[&facts[0]]);
    let constructions: Vec<(&str, OriginatedRunMessage)> = vec![
        (
            "missing end marker",
            host_recall(&no_end),
        ),
        (
            "role is not user",
            row(
                "assistant",
                Some(&well_formed),
                MessageOrigin::HostRecall {
                    recall_ref: Some("[\"a\"]".into()),
                },
            ),
        ),
        (
            "content is NULL",
            row(
                "user",
                None,
                MessageOrigin::HostRecall {
                    recall_ref: Some("[\"a\"]".into()),
                },
            ),
        ),
    ];
    for (label, malformed) in constructions {
        let run_id = base_run(&manager, &store).await;
        plant(
            &store,
            &run_id,
            vec![
                malformed,
                filler("f1"),
                row("user", Some("keep me"), MessageOrigin::UserPrompt),
            ],
        )
        .await;
        provider.seen.lock().unwrap().clear();
        log.0.lock().unwrap().clear();
        let done = drive(&manager, &store, &run_id).await;
        assert_eq!(done.status, RunStatus::Completed, "{label}");
        let sent = first_send(&provider);
        assert!(!mentions(&sent, RECALL_PREFIX), "{label}: malformed row sent");
        assert!(!mentions(&sent, "fact alive"), "{label}: malformed row sent");
        assert_eq!(count_eq(&sent, "keep me"), 1, "{label}");
        assert!(host_recall_warned(&log), "{label}: no warn");
    }
    // Reverse control: the very same malformed text on a UserPrompt arrives verbatim.
    let run_id = base_run(&manager, &store).await;
    plant(
        &store,
        &run_id,
        vec![
            row("user", Some(&no_end), MessageOrigin::UserPrompt),
            filler("f1"),
        ],
    )
    .await;
    provider.seen.lock().unwrap().clear();
    drive(&manager, &store, &run_id).await;
    assert_eq!(count_eq(&first_send(&provider), &no_end), 1);
}

#[tokio::test]
async fn send_copy_trim_leaves_snapshot_bytes_unchanged() {
    let provider = recorder();
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let facts = live_assertions(&kv, &["fact alive", "fact revoked"]).await;
    revoke(&kv, &facts[1].0).await;
    let run_id = base_run(&manager, &store).await;
    plant(
        &store,
        &run_id,
        vec![host_recall(&block(&[&facts[0], &facts[1]])), filler("f1")],
    )
    .await;
    let snapshot = |store: Store, run_id: String| async move {
        let rows = store.list_run_messages(&run_id).await.unwrap();
        let mut origins = vec![];
        for r in &rows {
            origins.push(store.read_run_message_origin(&run_id, r.seq).await.unwrap());
        }
        (rows, origins)
    };
    let before = snapshot(store.clone(), run_id.clone()).await;
    provider.seen.lock().unwrap().clear();
    drive(&manager, &store, &run_id).await;
    assert!(!mentions(&first_send(&provider), "fact revoked"), "trim happened");
    let after = snapshot(store.clone(), run_id.clone()).await;
    // The completed driver appends the final assistant; the pre-existing rows
    // and origins must be byte-identical.
    assert_eq!(after.0[..before.0.len()], before.0[..]);
    assert_eq!(after.1[..before.1.len()], before.1[..]);
}

#[tokio::test]
async fn send_copy_fails_closed_when_memory_and_store_disagree() {
    let provider = recorder();
    let (manager, store, _kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    let run_id = base_run(&manager, &store).await;
    provider.seen.lock().unwrap().clear();
    sqlx::query("UPDATE runs SET status = 'running', ended_at = NULL, error = NULL WHERE id = ?")
        .bind(&run_id)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    let running = store.get_run(&run_id).await.unwrap().unwrap();
    let thread = store.list_run_messages(&run_id).await.unwrap();
    let mut messages = thread_to_messages(&thread);
    messages[0] = Msg::user("not what the store holds");
    manager
        .run_loop(running, messages, false, CancellationToken::new())
        .await;
    let done = store.get_run(&run_id).await.unwrap().unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error.unwrap().code, "source_unproven");
    assert!(provider.seen.lock().unwrap().is_empty());
}

// ---- resume paths (parked run + approval) ----

struct ResumeEnv {
    manager: Arc<RunManager>,
    broker: Arc<ApprovalBroker>,
    provider: Arc<RecordingProvider>,
    store: Store,
}

fn resume_env(
    store: Store,
    memory: Option<SessionMemory>,
    workdir: std::path::PathBuf,
) -> ResumeEnv {
    struct NullSink;
    impl EventSink for NullSink {
        fn emit(&self, _: EventBody) {}
    }
    let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(|_| {});
    let broker = ApprovalBroker::new(store.clone(), emit, Duration::from_secs(30));
    let tools = ToolRegistry::builtin(workdir).with_gate(Arc::new(BrokerGate::new(broker.clone())));
    let provider = recorder();
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            provider.clone() as Arc<dyn agent24_models::ModelProvider>,
            agent24_models::router::Tier::Local,
        )])),
        Arc::new(tools),
        Arc::new(NullSink),
        CancellationToken::new(),
        memory,
    );
    ResumeEnv {
        manager,
        broker,
        provider,
        store,
    }
}

fn parked_call() -> (serde_json::Value, serde_json::Map<String, serde_json::Value>) {
    let args = serde_json::json!({ "argv": ["/bin/echo", "resumed-output"] });
    let call = serde_json::json!([{
        "id": "call_provider_1", "name": "shell_exec", "arguments": args.to_string()
    }]);
    (call, args.as_object().unwrap().clone())
}

async fn seed_parked_run(store: &Store, rows: Vec<OriginatedRunMessage>) {
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
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: zero_usage(),
        schedule_id: None,
        created_at: now_iso8601(),
        started_at: Some(now_iso8601()),
        ended_at: None,
    };
    store.insert_run(&run).await.unwrap();
    plant(store, "run_1", rows).await;
}

fn parked_assistant() -> OriginatedRunMessage {
    let (call, _) = parked_call();
    OriginatedRunMessage {
        message: PendingRunMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: call,
            tool_call_id: None,
        },
        origin: MessageOrigin::Assistant,
    }
}

async fn approve_and_resume(env: &ResumeEnv) -> Run {
    let (_, payload) = parked_call();
    let approval = Approval {
        id: "apr_1".to_owned(),
        run_id: "run_1".to_owned(),
        tool_call_id: "tc_internal_1".to_owned(),
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
    };
    env.store.insert_approval(&approval).await.unwrap();
    env.broker
        .resolve(
            "apr_1",
            agent24_protocol::Decision {
                kind: "approve".to_owned(),
                reason: None,
                extra: serde_json::Map::new(),
            },
        )
        .await
        .unwrap();
    env.manager
        .resume_run("run_1".to_owned(), "apr_1".to_owned())
        .await
        .unwrap();
    wait_terminal(&env.store, "run_1").await
}

#[tokio::test]
async fn resume_after_restart_drops_revoked_recall_line() {
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("agent.db");
    let kv_path = dir.path().join("memory.db");
    let alive;
    let revoked;
    {
        let store = Store::open(&store_path).await.unwrap();
        let kv = KvStore::open(&kv_path).await.unwrap();
        let facts = live_assertions(&kv, &["fact alive", "fact revoked"]).await;
        alive = facts[0].clone();
        revoked = facts[1].clone();
        seed_parked_run(
            &store,
            vec![
                host_recall(&block(&[&alive, &revoked])),
                row("user", Some("earlier question"), MessageOrigin::SessionHistory),
                row(
                    "assistant",
                    Some("earlier answer"),
                    MessageOrigin::SessionHistory,
                ),
                row("user", Some("run echo"), MessageOrigin::UserPrompt),
                parked_assistant(),
            ],
        )
        .await;
        // The owner retracts one recalled fact after the snapshot was taken.
        revoke(&kv, &revoked.0).await;
        // Both handles are dropped here: the "process" is gone.
    }
    let store = Store::open(&store_path).await.unwrap();
    let kv = KvStore::open(&kv_path).await.unwrap();
    let env = resume_env(
        store.clone(),
        Some(memory_of(&kv, Some(OWNER))),
        dir.path().to_path_buf(),
    );
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert!(!mentions(&sent, "fact revoked"), "revoked line was sent");
    // The recall block (pruned) is adjacent to the first history user turn, so
    // normalize merges them; everything else is byte-for-byte.
    assert_eq!(
        sent[0].content.as_deref(),
        Some(format!("{}\n\n{}", block(&[&alive]), "earlier question").as_str())
    );
    assert_eq!(sent[1], Msg::assistant(Some("earlier answer".into()), vec![]));
    assert_eq!(sent[2], Msg::user("run echo"));
    // The stored recall row still holds the revoked id's line.
    let stored = store.list_run_messages("run_1").await.unwrap();
    assert!(
        stored[0]
            .content
            .as_deref()
            .unwrap()
            .contains("fact revoked")
    );
}

#[tokio::test]
async fn resume_after_append_binds_kind_by_store_seq() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_memory().await.unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let facts = live_assertions(&kv, &["fact alive", "fact revoked"]).await;
    revoke(&kv, &facts[1].0).await;
    let full = block(&[&facts[0], &facts[1]]);
    let pruned = block(&[&facts[0]]);
    // Position 0 is a UserPrompt that looks exactly like a recall block; the
    // real HostRecall sits later. Index-based inference would get both wrong.
    seed_parked_run(
        &store,
        vec![
            filler("gap"),
            row("user", Some(&full), MessageOrigin::UserPrompt),
            filler("f1"),
            host_recall(&full),
            filler("f2"),
            row("user", Some(&full), MessageOrigin::SessionHistory),
            parked_assistant(),
        ],
    )
    .await;
    // Open a hole at the front: stored seqs now start at 1, so a row's seq
    // never equals its in-memory index and index-based lookup must fail.
    store.delete_run_message_range("run_1", 0, 0).await.unwrap();
    let seqs: Vec<i64> = store
        .list_run_messages("run_1")
        .await
        .unwrap()
        .iter()
        .map(|m| m.seq)
        .collect();
    assert_eq!(seqs[0], 1);
    let env = resume_env(
        store.clone(),
        Some(memory_of(&kv, Some(OWNER))),
        dir.path().to_path_buf(),
    );
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    // The tool result was appended before this send (second iteration shape).
    assert!(sent.iter().any(|m| m.role == "tool"));
    assert_eq!(count_eq(&sent, &pruned), 1, "HostRecall trimmed");
    assert_eq!(count_eq(&sent, &full), 2, "UserPrompt + SessionHistory verbatim");
    assert_eq!(sent[0], Msg::user(full));
}

#[tokio::test]
async fn send_copy_drops_host_recall_when_memory_is_none() {
    let (log, _guard) = capture_warns();
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open_memory().await.unwrap();
    let kv = KvStore::open_memory().await.unwrap();
    let facts = live_assertions(&kv, &["fact alive"]).await;
    let shaped = block(&[&facts[0]]);
    seed_parked_run(
        &store,
        vec![
            host_recall(&shaped),
            filler("f1"),
            row("user", Some(&shaped), MessageOrigin::SessionHistory),
            filler("f2"),
            row("user", Some("run echo"), MessageOrigin::UserPrompt),
            parked_assistant(),
        ],
    )
    .await;
    let env = resume_env(store.clone(), None, dir.path().to_path_buf());
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert_eq!(
        count_eq(&sent, &shaped),
        1,
        "only the SessionHistory copy remains"
    );
    assert_eq!(count_eq(&sent, "run echo"), 1);
    assert!(host_recall_warned(&log));
}

// ---- cold-restart matrix (R02/R05 b, R06-H03 c) ----
// Every test below seeds file-backed stores, drops every handle, reopens the
// SAME files and goes through `resume_run`. No in-memory store is used.

struct ColdFiles {
    dir: tempfile::TempDir,
    store_path: std::path::PathBuf,
    kv_path: std::path::PathBuf,
}

/// Seeds a parked run on file-backed stores, optionally revokes one fact, then
/// drops both handles ("the process is gone"). `rows` receives the fact list.
async fn cold_seed(
    texts: &[&str],
    revoke_idx: Option<usize>,
    rows: impl FnOnce(&[(String, String)]) -> Vec<OriginatedRunMessage>,
) -> (ColdFiles, Vec<(String, String)>) {
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("agent.db");
    let kv_path = dir.path().join("memory.db");
    let facts;
    {
        let store = Store::open(&store_path).await.unwrap();
        let kv = KvStore::open(&kv_path).await.unwrap();
        facts = live_assertions(&kv, texts).await;
        seed_parked_run(&store, rows(&facts)).await;
        if let Some(i) = revoke_idx {
            revoke(&kv, &facts[i].0).await;
        }
    }
    (
        ColdFiles {
            dir,
            store_path,
            kv_path,
        },
        facts,
    )
}

/// Reopens the same files; `memory` decides how the memory handle is built.
async fn cold_reopen(
    files: &ColdFiles,
    memory: impl FnOnce(&KvStore) -> Option<SessionMemory>,
) -> ResumeEnv {
    let store = Store::open(&files.store_path).await.unwrap();
    let kv = KvStore::open(&files.kv_path).await.unwrap();
    let memory = memory(&kv);
    resume_env(store, memory, files.dir.path().to_path_buf())
}

/// Renames the assertion ledger table in the on-disk memory DB, so a query
/// with a VALID owner passes `check_owner()` and then fails inside
/// `beliefs_as_of` (a real read-layer failure). Done after the handles that
/// seeded the file are gone, through a separate connection.
async fn break_assertion_table(files: &ColdFiles) {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(
            sqlx::sqlite::SqliteConnectOptions::new()
                .filename(&files.kv_path)
                .create_if_missing(false),
        )
        .await
        .unwrap();
    sqlx::query("ALTER TABLE mem_assertions RENAME TO mem_assertions_broken")
        .execute(&pool)
        .await
        .unwrap();
    pool.close().await;
}

#[tokio::test]
async fn cold_resume_drops_host_recall_when_memory_is_none() {
    let (log, _guard) = capture_warns();
    let (files, facts) = cold_seed(&["fact alive"], None, |facts| {
        let shaped = block(&[&facts[0]]);
        vec![
            host_recall(&shaped),
            filler("f1"),
            row("user", Some(&shaped), MessageOrigin::SessionHistory),
            filler("f2"),
            row("user", Some("run echo"), MessageOrigin::UserPrompt),
            parked_assistant(),
        ]
    })
    .await;
    let shaped = block(&[&facts[0]]);
    let env = cold_reopen(&files, |_| None).await;
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert_eq!(
        count_eq(&sent, &shaped),
        1,
        "HostRecall gone, only the SessionHistory copy remains"
    );
    assert_eq!(count_eq(&sent, "run echo"), 1);
    assert!(host_recall_warned(&log));
    // Stored HostRecall row is untouched.
    let stored = env.store.list_run_messages("run_1").await.unwrap();
    assert_eq!(stored[0].content.as_deref(), Some(shaped.as_str()));
}

#[tokio::test]
async fn cold_resume_drops_host_recall_when_active_ids_fail() {
    let (log, _guard) = capture_warns();
    let (files, facts) = cold_seed(&["fact alive"], None, |facts| {
        let shaped = block(&[&facts[0]]);
        vec![
            host_recall(&shaped),
            filler("f1"),
            row("user", Some(&shaped), MessageOrigin::SessionHistory),
            filler("f2"),
            row("user", Some("run echo"), MessageOrigin::UserPrompt),
            parked_assistant(),
        ]
    })
    .await;
    let shaped = block(&[&facts[0]]);
    // The owner is valid, so `check_owner()` passes; the ledger table is gone,
    // so `beliefs_as_of` itself fails.
    break_assertion_table(&files).await;
    let env = cold_reopen(&files, |kv| Some(memory_of(kv, Some(OWNER)))).await;
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert_eq!(
        count_eq(&sent, &shaped),
        1,
        "HostRecall gone, only the SessionHistory copy remains"
    );
    assert_eq!(count_eq(&sent, "run echo"), 1);
    assert!(host_recall_warned(&log));
    let warns = log.0.lock().unwrap().join("\n");
    assert!(
        warns.contains("could not re-validate HostRecall"),
        "the read-failure branch must be the one that fired: {warns}"
    );
    assert!(
        warns.contains("mem_assertions"),
        "the error must come from the ledger query: {warns}"
    );
    assert!(
        !warns.contains("owner is not configured"),
        "must not be the check_owner() failure: {warns}"
    );
}

#[tokio::test]
async fn cold_resume_three_same_text_origins_each_by_kind() {
    let (files, facts) = cold_seed(&["fact alive", "fact revoked"], Some(1), |facts| {
        let full = block(&[&facts[0], &facts[1]]);
        vec![
            host_recall(&full),
            filler("f1"),
            row("user", Some(&full), MessageOrigin::UserPrompt),
            filler("f2"),
            row("user", Some(&full), MessageOrigin::SessionHistory),
            parked_assistant(),
        ]
    })
    .await;
    let full = block(&[&facts[0], &facts[1]]);
    let pruned = block(&[&facts[0]]);
    let env = cold_reopen(&files, |kv| Some(memory_of(kv, Some(OWNER)))).await;
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert_eq!(count_eq(&sent, &pruned), 1, "only the HostRecall row is trimmed");
    assert_eq!(count_eq(&sent, &full), 2, "UserPrompt + SessionHistory verbatim");
}

#[tokio::test]
async fn cold_resume_leaves_origin_rows_and_digests_unchanged() {
    let (files, facts) = cold_seed(&["fact alive", "fact revoked"], Some(1), |facts| {
        let full = block(&[&facts[0], &facts[1]]);
        vec![
            host_recall(&full),
            filler("f1"),
            row("user", Some(&full), MessageOrigin::UserPrompt),
            filler("f2"),
            row("user", Some(&full), MessageOrigin::SessionHistory),
            filler("f3"),
            parked_assistant(),
        ]
    })
    .await;
    let full = block(&[&facts[0], &facts[1]]);
    let pruned = block(&[&facts[0]]);
    let env = cold_reopen(&files, |kv| Some(memory_of(kv, Some(OWNER)))).await;
    let snapshot = |store: Store| async move {
        let rows = store.list_run_messages("run_1").await.unwrap();
        let mut origins = vec![];
        for r in &rows {
            origins.push(store.read_run_message_origin("run_1", r.seq).await.unwrap());
        }
        (rows, origins)
    };
    let before = snapshot(env.store.clone()).await;
    assert_eq!(before.0.len(), 7);
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    // The trim really happened on the send copy: exactly one message is the
    // recall block with only the live fact, and the revoked line survives only
    // in the two verbatim (non-HostRecall) copies.
    let sent = first_send(&env.provider);
    assert_eq!(count_eq(&sent, &pruned), 1, "HostRecall trimmed to live ids");
    assert_eq!(count_eq(&sent, &full), 2, "UserPrompt + SessionHistory verbatim");
    let with_revoked = sent
        .iter()
        .filter(|m| m.content.as_deref().is_some_and(|c| c.contains("fact revoked")))
        .count();
    assert_eq!(with_revoked, 2, "revoked line only in the two verbatim rows");
    // `drive_resume` does no history reclaim (that lives only at the end of
    // `execute`), so ALL seeded rows must still be present and identical:
    // body, kind, recall_ref and digest. Resume only appends after them.
    let after = snapshot(env.store.clone()).await;
    assert!(after.0.len() >= before.0.len(), "seeded rows disappeared");
    assert_eq!(after.0[..before.0.len()], before.0[..], "message rows changed");
    assert_eq!(after.1[..before.1.len()], before.1[..], "origin rows or digests changed");
}

#[tokio::test]
async fn cold_resume_delivers_host_system_notice_verbatim() {
    let (files, _facts) = cold_seed(&["fact alive"], None, |_| {
        vec![
            row(
                "system",
                Some(retain::PAUSED_WRITE_NOTICE),
                MessageOrigin::HostSystem,
            ),
            row("user", Some("run echo"), MessageOrigin::UserPrompt),
            parked_assistant(),
        ]
    })
    .await;
    let env = cold_reopen(&files, |kv| Some(memory_of(kv, Some(OWNER)))).await;
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert_eq!(sent[0], Msg::system(retain::PAUSED_WRITE_NOTICE));
    assert_eq!(count_eq(&sent, "run echo"), 1);
}

#[tokio::test]
async fn cold_resume_binds_kind_by_store_seq_with_seq_hole() {
    // Cold version of `resume_after_append_binds_kind_by_store_seq`: a
    // recall-shaped UserPrompt at position 0 and the real HostRecall later.
    let (files, facts) = cold_seed(&["fact alive", "fact revoked"], Some(1), |facts| {
        let full = block(&[&facts[0], &facts[1]]);
        vec![
            filler("gap"),
            row("user", Some(&full), MessageOrigin::UserPrompt),
            filler("f1"),
            host_recall(&full),
            filler("f2"),
            row("user", Some(&full), MessageOrigin::SessionHistory),
            parked_assistant(),
        ]
    })
    .await;
    let full = block(&[&facts[0], &facts[1]]);
    let pruned = block(&[&facts[0]]);
    let env = cold_reopen(&files, |kv| Some(memory_of(kv, Some(OWNER)))).await;
    // Open a hole at the front of the reopened file: stored seqs now start at
    // 1, so a row's seq never equals its in-memory index.
    env.store
        .delete_run_message_range("run_1", 0, 0)
        .await
        .unwrap();
    let seqs: Vec<i64> = env
        .store
        .list_run_messages("run_1")
        .await
        .unwrap()
        .iter()
        .map(|m| m.seq)
        .collect();
    assert_eq!(seqs[0], 1);
    let done = approve_and_resume(&env).await;
    assert_eq!(done.status, RunStatus::Completed);
    let sent = first_send(&env.provider);
    assert!(sent.iter().any(|m| m.role == "tool"));
    assert_eq!(count_eq(&sent, &pruned), 1, "non-zero-position HostRecall trimmed");
    assert_eq!(count_eq(&sent, &full), 2, "UserPrompt + SessionHistory verbatim");
    assert_eq!(sent[0], Msg::user(full));
}
