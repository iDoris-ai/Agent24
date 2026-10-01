use super::*;

struct FailureRecordingSink(StdMutex<Vec<EventBody>>);

impl EventSink for FailureRecordingSink {
    fn emit(&self, body: EventBody) {
        self.0.lock().unwrap().push(body);
    }
}

struct WaitingProvider {
    entered: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl ModelProvider for WaitingProvider {
    fn name(&self) -> &str {
        "waiting"
    }

    async fn complete(
        &self,
        _request: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        self.entered.notify_one();
        self.release.notified().await;
        Ok(CompletionResponse {
            message: Msg::assistant(Some("answer".to_owned()), vec![]),
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

struct NeverFinishingSummarizer {
    entered: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl Summarizer for NeverFinishingSummarizer {
    async fn summarize(
        &self,
        _prior: Option<&str>,
        _messages: &[Msg],
    ) -> std::result::Result<String, String> {
        self.entered.notify_one();
        std::future::pending().await
    }
}

async fn set_row_quota(path: &std::path::Path, rows: i64) {
    let url = format!("sqlite://{}", path.display());
    let pool = sqlx::SqlitePool::connect(&url).await.unwrap();
    let result = sqlx::query("UPDATE mem_owner_quota SET max_rows=? WHERE owner='*'")
        .bind(rows)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(result.rows_affected(), 1, "wildcard quota row must exist");
    pool.close().await;
}

async fn assert_quota_rejection(rows: i64) {
    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("memory.sqlite");
    let kv = KvStore::open(&db_path).await.unwrap();
    set_row_quota(&db_path, rows).await;

    let sink = Arc::new(FailureRecordingSink(StdMutex::new(Vec::new())));
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(Vec::new()),
    });
    let store = Store::open_memory().await.unwrap();
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            provider.clone(),
            Tier::Local,
        )])),
        Arc::new(ToolRegistry::new()),
        sink.clone(),
        CancellationToken::new(),
        Some(
            SessionMemory::new(kv.clone(), Arc::new(UnusedSummarizer))
                .with_owner("m1-test-owner".to_owned()),
        ),
    );
    seed_session(&store, "quota-session").await;

    let first = manager
        .start_run(RunCreate {
            session_id: Some("quota-session".to_owned()),
            prompt: "this turn cannot be stored".to_owned(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    let finished = wait_terminal(&store, &first.id).await;
    assert_eq!(finished.status, RunStatus::Completed, "quota={rows}");

    let failure = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .find_map(|body| {
            let json = serde_json::to_value(body).unwrap();
            (json["type"] == "memory.write_failed").then_some(json["payload"].clone())
        })
        .unwrap_or_else(|| panic!("quota={rows}: missing memory.write_failed"));
    assert_eq!(failure["session_id"], "quota-session");
    assert!(
        failure["reason"]
            .as_str()
            .is_some_and(|s| s.contains("quota"))
    );

    let view = kv
        .session_log()
        .load_view("m1-test-owner", "quota-session")
        .await
        .unwrap();
    assert!(
        view.tail.is_empty(),
        "quota={rows}: partial message pair persisted"
    );

    let second = manager
        .start_run(RunCreate {
            session_id: Some("quota-session".to_owned()),
            prompt: "next turn".to_owned(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&store, &second.id).await.status,
        RunStatus::Completed
    );
    let calls = provider.seen.lock().unwrap();
    assert_eq!(calls.len(), 2);
    let second_context = &calls[1];
    assert!(
        second_context.iter().all(|message| message
            .content
            .as_deref()
            .is_none_or(|text| text != "this turn cannot be stored" && text != "pong")),
        "failed turn leaked into the next run's context: {second_context:?}"
    );
    drop(calls);
    let failures = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::to_value(body).unwrap())
        .filter(|json| json["type"] == "memory.write_failed")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 2, "each failed write is reported");
}

async fn wait_terminal_with_memory_deadline(store: &Store, id: &str) -> Run {
    for _ in 0..2_100 {
        let run = store.get_run(id).await.unwrap().unwrap();
        if agent24_core::run_is_terminal(run.status) {
            return run;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("run {id} did not finish within the memory write deadline");
}

#[tokio::test]
async fn zero_quota_rejection_keeps_run_completed_and_reports_failure_without_partial_turn() {
    assert_quota_rejection(0).await;
}

#[tokio::test]
async fn one_row_quota_rejection_keeps_run_completed_and_rolls_back_partial_turn() {
    assert_quota_rejection(1).await;
}

#[tokio::test]
async fn lock_timeout_keeps_run_completed_and_reports_failed_memory_write() {
    let kv = KvStore::open_memory().await.unwrap();
    let sink = Arc::new(FailureRecordingSink(StdMutex::new(Vec::new())));
    let store = Store::open_memory().await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(WaitingProvider {
        entered: entered.clone(),
        release: release.clone(),
    });
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
        Arc::new(ToolRegistry::new()),
        sink.clone(),
        CancellationToken::new(),
        Some(
            SessionMemory::new(kv, Arc::new(UnusedSummarizer))
                .with_owner("m1-test-owner".to_owned()),
        ),
    );
    seed_session(&store, "timeout-lock-session").await;

    let run = manager
        .start_run(RunCreate {
            session_id: Some("timeout-lock-session".to_owned()),
            prompt: "lock timeout".to_owned(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    entered.notified().await;
    let memory = manager.memory.as_ref().unwrap();
    let lock = memory.session_lock("timeout-lock-session").await;
    let guard = lock.lock().await;
    release.notify_one();

    let done = wait_terminal_with_memory_deadline(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    let failures = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::to_value(body).unwrap())
        .filter(|json| json["type"] == "memory.write_failed")
        .collect::<Vec<_>>();
    assert_eq!(failures.len(), 1, "lock wait exceeded the write deadline");
    assert_eq!(failures[0]["payload"]["session_id"], "timeout-lock-session");
    assert!(
        failures[0]["payload"]["reason"]
            .as_str()
            .unwrap()
            .contains("timed out")
    );
    drop(guard);
}

#[tokio::test]
async fn summarizer_timeout_keeps_durable_originals_without_write_failed_event() {
    let kv = KvStore::open_memory().await.unwrap();
    let sink = Arc::new(FailureRecordingSink(StdMutex::new(Vec::new())));
    let store = Store::open_memory().await.unwrap();
    let entered = Arc::new(tokio::sync::Notify::new());
    let policy = CompactionPolicy {
        max_recent: 1,
        keep_recent: 1,
        max_summary_chars: 500,
    };
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(FixedProvider),
            Tier::Local,
        )])),
        Arc::new(ToolRegistry::new()),
        sink.clone(),
        CancellationToken::new(),
        Some(
            SessionMemory::new(
                kv.clone(),
                Arc::new(NeverFinishingSummarizer {
                    entered: entered.clone(),
                }),
            )
            .with_owner("m1-test-owner".to_owned())
            .with_policy(policy),
        ),
    );
    seed_session(&store, "timeout-summary-session").await;

    let run = manager
        .start_run(RunCreate {
            session_id: Some("timeout-summary-session".to_owned()),
            prompt: "original survives summary timeout".to_owned(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    entered.notified().await;
    let done = wait_terminal_with_memory_deadline(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);

    let view = kv
        .session_log()
        .load_view("m1-test-owner", "timeout-summary-session")
        .await
        .unwrap();
    let messages = view
        .tail
        .iter()
        .filter_map(|(_, message)| message.content.as_deref())
        .collect::<Vec<_>>();
    assert!(
        messages.contains(&"original survives summary timeout"),
        "{messages:?}"
    );
    assert!(messages.contains(&"pong"), "{messages:?}");
    assert!(
        sink.0
            .lock()
            .unwrap()
            .iter()
            .all(|body| { serde_json::to_value(body).unwrap()["type"] != "memory.write_failed" }),
        "summary timeout must not report failure after original messages were committed"
    );
}
