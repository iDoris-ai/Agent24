// M1-T04 acceptance tests for the SessionLog-backed agent loop.
use super::*;
use agent24_memory::KvStore;
use agent24_memory::event::{EventQuery, EventStore, MemEvent, Origin, Scope, Trust};
use agent24_memory::session::CanonicalSession;
use agent24_memory::writer::{Candidate, MemoryWriter};
use agent24_models::router::Tier;
use async_trait::async_trait;
use std::sync::{
    Mutex as StdMutex,
    atomic::{AtomicUsize, Ordering},
};

const OWNER: &str = "m1-test-owner";

struct CaptureSummarizer(StdMutex<Vec<(Option<String>, Vec<Msg>)>>);

#[async_trait]
impl Summarizer for CaptureSummarizer {
    async fn summarize(&self, prior: Option<&str>, messages: &[Msg]) -> Result<String, String> {
        let mut calls = self.0.lock().unwrap();
        calls.push((prior.map(str::to_owned), messages.to_vec()));
        let mut folded: Vec<Msg> = prior
            .map(serde_json::from_str)
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default();
        folded.extend_from_slice(messages);
        serde_json::to_string(&folded).map_err(|error| error.to_string())
    }
}

struct FailingSummarizer;

#[async_trait]
impl Summarizer for FailingSummarizer {
    async fn summarize(&self, _: Option<&str>, _: &[Msg]) -> Result<String, String> {
        Err("persistent test failure".into())
    }
}

async fn memory_manager(
    kv: KvStore,
    summarizer: Arc<dyn Summarizer>,
    policy: CompactionPolicy,
) -> Arc<RunManager> {
    let provider: Arc<dyn ModelProvider> = Arc::new(FixedProvider);
    RunManager::with_memory(
        Store::open_memory().await.unwrap(),
        Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
        Arc::new(ToolRegistry::new()),
        Arc::new(RecordingSink(StdMutex::new(vec![]))),
        CancellationToken::new(),
        Some(
            SessionMemory::new(kv, summarizer)
                .with_owner(OWNER.to_owned())
                .with_policy(policy),
        ),
    )
}

fn policy(max_recent: usize) -> CompactionPolicy {
    CompactionPolicy {
        max_recent,
        keep_recent: 1,
        max_summary_chars: 10_000,
    }
}

#[tokio::test]
async fn recall_returns_persisted_source_ref_as_memory_recall() {
    let kv = KvStore::open_memory().await.unwrap();
    let mut source = agent24_store::SourceRef::user_input("event-123", "t");
    source.revision_digest = Some("sha256:body".into());
    let source_json = serde_json::to_value(&source).unwrap();
    let candidate = Candidate::new(
        "assertion-1",
        Scope::owner(OWNER),
        "favorite color",
        "is",
        serde_json::json!("blue"),
        Origin { source: "user".into(), trust: Trust::UserSaid },
    )
    .with_evidence(vec!["event-123".into()])
    .with_source_ref(source_json)
    .remember();
    kv.write_gate().propose(vec![candidate]).await.unwrap();

    let memory = SessionMemory::new(kv.clone(), Arc::new(CaptureSummarizer(StdMutex::new(vec![]))))
        .with_owner(OWNER.to_owned());
    let (_, ids, sources) = memory.recall("favorite color").await.unwrap().unwrap();
    assert_eq!(ids, vec!["assertion-1"]);
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].source_id, source.source_id);
    assert_eq!(sources[0].revision_digest, source.revision_digest);
    assert_eq!(sources[0].kind, agent24_store::SourceKind::MemoryRecall);
    assert_eq!(sources[0].classification, agent24_store::SourceClassification::Ordinary);
    assert_eq!(sources[0].mode, agent24_store::SourceMode::LocalOnly);

    let manager = memory_manager(
        kv,
        Arc::new(CaptureSummarizer(StdMutex::new(vec![]))),
        policy(100),
    )
    .await;
    let store = manager.store.clone();
    seed_session(&store, "memory-recall").await;
    let run_id = run_completed(&manager, &store, "memory-recall", "What is my favorite color?").await;
    let tags = store.list_run_source_tags(&run_id).await.unwrap();
    assert!(tags.iter().any(|tag| {
        tag.source.kind == agent24_store::SourceKind::MemoryRecall
            && tag.source.source_id == source.source_id
            && tag.source.mode == agent24_store::SourceMode::LocalOnly
    }));
    assert_eq!(
        store.run_policy_snapshot(&run_id).await.unwrap().effective_mode,
        agent24_store::SourceMode::LocalOnly
    );
}

async fn run_completed(manager: &Arc<RunManager>, store: &Store, session: &str, prompt: &str) -> String {
    let run = manager
        .start_run(RunCreate {
            workspace_id: None,
            session_id: Some(session.to_owned()),
            prompt: prompt.to_owned(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    for _ in 0..200 {
        let current = store.get_run(&run.id).await.unwrap().unwrap();
        if current.status != RunStatus::Running && current.status != RunStatus::Queued {
            assert_eq!(
                current.status,
                RunStatus::Completed,
                "run must complete successfully"
            );
            return run.id;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("run did not finish");
}

struct UnavailableProvider;

#[async_trait]
impl ModelProvider for UnavailableProvider {
    fn name(&self) -> &str {
        "unavailable-local"
    }

    async fn complete(
        &self,
        _: &CompletionRequest,
        _: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        Err(ModelError::Unavailable("local provider is down".into()))
    }

    async fn models(&self, _: &CancellationToken) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

struct CountingRemoteProvider(AtomicUsize);

#[async_trait]
impl ModelProvider for CountingRemoteProvider {
    fn name(&self) -> &str {
        "remote-stub"
    }

    async fn complete(
        &self,
        _: &CompletionRequest,
        _: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(CompletionResponse {
            message: Msg::assistant(Some("remote summary".into()), vec![]),
            usage: Default::default(),
            model_id: Some("remote-stub".into()),
        })
    }

    async fn models(&self, _: &CancellationToken) -> Result<Vec<agent24_protocol::Model>, ModelError> {
        Ok(vec![])
    }
}

#[tokio::test]
async fn local_only_run_compaction_never_falls_back_to_remote() {
    let remote = Arc::new(CountingRemoteProvider(AtomicUsize::new(0)));
    let summarizer_router = Arc::new(ModelRouter::with_defaults(vec![
        (Arc::new(UnavailableProvider) as Arc<dyn ModelProvider>, Tier::Local),
        (remote.clone() as Arc<dyn ModelProvider>, Tier::Remote),
    ]));
    let summarizer: Arc<dyn Summarizer> = Arc::new(RouterSummarizer::new(
        summarizer_router,
        CancellationToken::new(),
    ));
    let kv = KvStore::open_memory().await.unwrap();
    let manager = memory_manager(kv, summarizer, policy(1)).await;
    let store = manager.store.clone();
    seed_session(&store, "local-only-compaction").await;

    // max_recent=1 causes this completed run's user/assistant messages to be
    // compacted through the production RouterSummarizer path.
    run_completed(&manager, &store, "local-only-compaction", "private conversation").await;

    assert_eq!(remote.0.load(Ordering::SeqCst), 0);
}

async fn message_events(kv: &KvStore, session: &str) -> Vec<Msg> {
    let log = kv.events();
    let mut cursor = 0;
    let mut messages = Vec::new();
    loop {
        let page = log
            .scan(
                &EventQuery::owner(OWNER)
                    .session(session)
                    .after(cursor)
                    .limit(3),
            )
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = page.last().unwrap().seq;
        messages.extend(
            page.into_iter()
                .filter(|e| e.event.kind == "message")
                .map(|e| serde_json::from_value(e.event.body).unwrap()),
        );
    }
    messages
}

#[tokio::test]
async fn legacy_blob_import_survives_restart_and_is_never_rewritten() {
    let kv = KvStore::open_memory().await.unwrap();
    let mut legacy = CanonicalSession::new("legacy");
    legacy.summary = Some("legacy summary".into());
    legacy.recent = vec![
        Msg::user("old user"),
        Msg::assistant(Some("old answer".into()), vec![]),
        Msg {
            role: "assistant".into(),
            content: None,
            tool_calls: vec![ToolCallRequest {
                id: "call-1".into(),
                name: "lookup".into(),
                arguments: "{\"q\":1}".into(),
            }],
            tool_call_id: None,
        },
        Msg {
            role: "tool".into(),
            content: Some("tool result".into()),
            tool_calls: vec![],
            tool_call_id: Some("call-1".into()),
        },
    ];
    kv.put("session", "legacy", &legacy).await.unwrap();
    let blob_before = kv.get("session", "legacy").await.unwrap();
    let summarizer: Arc<dyn Summarizer> = Arc::new(UnusedSummarizer);
    let manager = memory_manager(kv.clone(), summarizer.clone(), policy(20)).await;
    let store = manager.store.clone();
    seed_session(&store, "legacy").await;
    let cancel = CancellationToken::new();
    let context = manager
        .session_context(Some("legacy"), &cancel)
        .await
        .unwrap();
    assert_eq!(
        context,
        legacy.context(),
        "legacy import must preserve tool call and tool result messages exactly"
    );
    run_completed(&manager, &store, "legacy", "new user").await;
    drop(manager);
    let restarted = memory_manager(kv.clone(), summarizer, policy(20)).await;
    let context = restarted
        .session_context(Some("legacy"), &cancel)
        .await
        .unwrap();
    let mut expected = legacy.context();
    expected.extend([
        Msg::user("new user"),
        Msg::assistant(Some("pong".into()), vec![]),
    ]);
    assert_eq!(context, expected);
    assert_eq!(kv.get("session", "legacy").await.unwrap(), blob_before);
    kv.put("session", "legacy", &"unreadable stale blob")
        .await
        .unwrap();
    assert_eq!(
        restarted
            .session_context(Some("legacy"), &cancel)
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn successful_folds_keep_every_original_message_in_paged_event_log() {
    let kv = KvStore::open_memory().await.unwrap();
    let capture = Arc::new(CaptureSummarizer(StdMutex::new(vec![])));
    let manager = memory_manager(kv.clone(), capture.clone(), policy(2)).await;
    let store = manager.store.clone();
    seed_session(&store, "folds").await;
    let mut originals = Vec::new();
    for i in 0..5 {
        if i == 2 {
            kv.events()
                .append(&MemEvent::new(
                    "unrelated-session-message",
                    Scope::owner(OWNER).with_session("unrelated"),
                    "message",
                    serde_json::to_value(Msg::user("interleaved sequence hole")).unwrap(),
                    Origin {
                        source: "test".into(),
                        trust: Trust::UserSaid,
                    },
                ))
                .await
                .unwrap();
        }
        originals.extend([
            Msg::user(format!("question-{i}")),
            Msg::assistant(Some("pong".into()), vec![]),
        ]);
        run_completed(&manager, &store, "folds", &format!("question-{i}")).await;
    }
    let calls = capture.0.lock().unwrap().clone();
    assert!(
        calls.len() >= 2,
        "expected at least two successful folds: {}",
        calls.len()
    );
    let mut latest = Vec::<Msg>::new();
    for (prior, head) in &calls {
        let expected_prior = if latest.is_empty() {
            None
        } else {
            Some(serde_json::to_string(&latest).unwrap())
        };
        assert_eq!(
            prior.as_deref(),
            expected_prior.as_deref(),
            "fold must receive the latest prior summary"
        );
        assert!(!head.is_empty(), "every fold must have a head");
        latest.extend(head.iter().cloned());
        assert_eq!(
            &originals[..latest.len()],
            latest.as_slice(),
            "prior plus fold head must equal an exact original prefix"
        );
    }
    drop(manager);
    let restarted = memory_manager(kv.clone(), Arc::new(UnusedSummarizer), policy(2)).await;
    let got = message_events(&kv, "folds").await;
    assert_eq!(
        got, originals,
        "paged event scan must preserve every original in order"
    );
    let context = restarted
        .session_context(Some("folds"), &CancellationToken::new())
        .await
        .unwrap();
    let latest_summary = serde_json::to_string(&latest).unwrap();
    assert_eq!(
        context.first().and_then(|m| m.content.as_deref()),
        Some(format!("Summary of earlier conversation:\n{latest_summary}").as_str())
    );
    let mut expected = vec![Msg::system(format!(
        "Summary of earlier conversation:\n{latest_summary}"
    ))];
    expected.extend_from_slice(&originals[latest.len()..]);
    assert_eq!(
        context, expected,
        "context must contain only the latest summary and uncovered originals"
    );
}

#[tokio::test]
async fn repeated_summary_failure_keeps_all_events_and_bounds_context_after_restart() {
    let kv = KvStore::open_memory().await.unwrap();
    let manager = memory_manager(kv.clone(), Arc::new(FailingSummarizer), policy(2)).await;
    let store = manager.store.clone();
    seed_session(&store, "failures").await;
    let mut originals = Vec::new();
    for i in 0..12 {
        originals.extend([
            Msg::user(format!("u{i}")),
            Msg::assistant(Some("pong".into()), vec![]),
        ]);
        run_completed(&manager, &store, "failures", &format!("u{i}")).await;
    }
    drop(manager);
    let restarted = memory_manager(kv.clone(), Arc::new(FailingSummarizer), policy(2)).await;
    assert_eq!(
        message_events(&kv, "failures").await,
        originals,
        "failed summaries must never delete originals"
    );
    let context = restarted
        .session_context(Some("failures"), &CancellationToken::new())
        .await
        .unwrap();
    assert!(
        context.len() <= 8,
        "context {} exceeds hard ceiling 4*max_recent",
        context.len()
    );
    assert_eq!(
        context,
        originals[originals.len() - 8..],
        "bounded context must retain the exact newest eight originals"
    );
    let events = kv
        .events()
        .scan(&EventQuery::owner(OWNER).session("failures").limit(100))
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .all(|event| event.event.kind != "session.summary"),
        "failed summaries must never be committed"
    );
}

#[tokio::test]
async fn concurrent_first_context_reads_import_legacy_messages_once() {
    let kv = KvStore::open_memory().await.unwrap();
    let mut legacy = CanonicalSession::new("concurrent-import");
    legacy.summary = Some("before".into());
    legacy.recent = vec![
        Msg::user("legacy-user"),
        Msg::assistant(Some("legacy-answer".into()), vec![]),
    ];
    kv.put("session", "concurrent-import", &legacy)
        .await
        .unwrap();
    let manager = memory_manager(kv.clone(), Arc::new(UnusedSummarizer), policy(10)).await;
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let manager = manager.clone();
        tasks.push(tokio::spawn(async move {
            manager
                .session_context(Some("concurrent-import"), &CancellationToken::new())
                .await
                .unwrap()
        }));
    }
    for task in tasks {
        assert_eq!(task.await.unwrap().len(), 3);
    }
    let events = kv
        .events()
        .scan(
            &EventQuery::owner(OWNER)
                .session("concurrent-import")
                .limit(100),
        )
        .await
        .unwrap();
    assert_eq!(
        events.iter().filter(|e| e.event.kind == "message").count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event.kind == "session.imported")
            .count(),
        1
    );
}

#[tokio::test]
async fn folding_keeps_tool_calls_and_results_together() {
    let kv = KvStore::open_memory().await.unwrap();
    let mut legacy = CanonicalSession::new("tools-fold");
    legacy.recent = vec![
        Msg::user("lookup"),
        Msg::assistant(
            None,
            vec![
                ToolCallRequest {
                    id: "a".into(),
                    name: "lookup".into(),
                    arguments: "{}".into(),
                },
                ToolCallRequest {
                    id: "b".into(),
                    name: "lookup".into(),
                    arguments: "{}".into(),
                },
            ],
        ),
        Msg {
            role: "tool".into(),
            content: Some("first result".into()),
            tool_calls: vec![],
            tool_call_id: Some("a".into()),
        },
        Msg {
            role: "tool".into(),
            content: Some("second result".into()),
            tool_calls: vec![],
            tool_call_id: Some("b".into()),
        },
    ];
    legacy.save(&kv).await.unwrap();
    let capture = Arc::new(CaptureSummarizer(StdMutex::new(vec![])));
    let manager = memory_manager(
        kv.clone(),
        capture.clone(),
        CompactionPolicy {
            keep_recent: 3,
            ..policy(5)
        },
    )
    .await;
    seed_session(&manager.store, "tools-fold").await;
    run_completed(&manager, &manager.store, "tools-fold", "new question").await;
    assert_eq!(capture.0.lock().unwrap()[0].1, legacy.recent);
    let context = manager
        .session_context(Some("tools-fold"), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        &context[1..],
        &[
            Msg::user("new question"),
            Msg::assistant(Some("pong".into()), vec![])
        ]
    );
    assert_eq!(message_events(&kv, "tools-fold").await.len(), 6);
}
