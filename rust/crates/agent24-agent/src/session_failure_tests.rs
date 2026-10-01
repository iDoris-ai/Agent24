use super::*;
use crate::session_memory::CommitAcknowledgementProbe;

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
async fn committed_turn_waits_for_ack_after_deadline_and_is_available_next_round() {
    let kv = KvStore::open_memory().await.unwrap();
    let sink = Arc::new(FailureRecordingSink(StdMutex::new(Vec::new())));
    let store = Store::open_memory().await.unwrap();
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(Vec::new()),
    });
    let probe = Arc::new(CommitAcknowledgementProbe {
        committed: tokio::sync::Notify::new(),
        deadline_elapsed: tokio::sync::Notify::new(),
        release: tokio::sync::Notify::new(),
        armed: std::sync::atomic::AtomicBool::new(true),
    });
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
                .with_owner("m1-test-owner".to_owned())
                .with_commit_probe(probe.clone()),
        ),
    );
    seed_session(&store, "commit-ack-session").await;

    let first = manager
        .start_run(RunCreate {
            session_id: Some("commit-ack-session".to_owned()),
            prompt: "first committed prompt".to_owned(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();

    // This notification is emitted only after the real append_turn transaction
    // returned success, while remember is still waiting for its wrapper ack.
    tokio::time::timeout(super::MEMORY_WRITE_BUDGET, probe.committed.notified())
        .await
        .expect("append_turn should commit");
    let committed = kv
        .session_log()
        .load_view("m1-test-owner", "commit-ack-session")
        .await
        .unwrap();
    let committed_messages = committed
        .tail
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        committed_messages,
        [
            Msg::user("first committed prompt"),
            Msg::assistant(Some("pong".to_owned()), vec![])
        ],
        "probe must pause after the complete exchange is durable"
    );
    tokio::time::timeout(
        super::MEMORY_WRITE_BUDGET + Duration::from_secs(5),
        probe.deadline_elapsed.notified(),
    )
    .await
    .expect("memory write deadline should expire at the gated ack");

    // Give the awakened run task a turn to expose an erroneous failure event
    // before we release the acknowledgement gate.
    tokio::task::yield_now().await;
    let pending = store.get_run(&first.id).await.unwrap().unwrap();
    assert_eq!(pending.status, RunStatus::Running);
    assert!(
        sink.0
            .lock()
            .unwrap()
            .iter()
            .all(|body| serde_json::to_value(body).unwrap()["type"] != "memory.write_failed"),
        "a committed transaction must not be reported as a failed write"
    );

    probe.release.notify_one();
    assert_eq!(
        wait_terminal(&store, &first.id).await.status,
        RunStatus::Completed
    );
    assert!(
        sink.0
            .lock()
            .unwrap()
            .iter()
            .all(|body| serde_json::to_value(body).unwrap()["type"] != "memory.write_failed")
    );
    let second = manager
        .start_run(RunCreate {
            session_id: Some("commit-ack-session".to_owned()),
            prompt: "second prompt".to_owned(),
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
    let context = calls[1]
        .iter()
        .filter(|message| message.role != "system")
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        context
            == [
                Msg::user("first committed prompt"),
                Msg::assistant(Some("pong".to_owned()), vec![]),
                Msg::user("second prompt")
            ],
        "next round must receive the complete prior exchange before its prompt: {context:?}"
    );
    drop(calls);
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

struct TwoPhaseProvider {
    b_entered: Arc<tokio::sync::Notify>,
    b_release: Arc<tokio::sync::Notify>,
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl ModelProvider for TwoPhaseProvider {
    fn name(&self) -> &str {
        "two-phase"
    }

    async fn complete(
        &self,
        _request: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if call == 0 {
            self.b_entered.notify_one();
            self.b_release.notified().await;
        }
        let answer = if call == 0 { "B answer" } else { "A answer" };
        Ok(CompletionResponse {
            message: Msg::assistant(Some(answer.to_owned()), vec![]),
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

struct CommitHookRelease(std::sync::mpsc::SyncSender<()>);
impl Drop for CommitHookRelease {
    fn drop(&mut self) {
        let _ = self.0.send(());
    }
}

#[tokio::test]
async fn cancel_during_sqlite_commit_does_not_release_session_lock_before_commit_finishes() {
    use agent24_memory::event::EventStore;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    use std::str::FromStr;
    use std::sync::atomic::{AtomicBool, Ordering};

    let directory = tempfile::tempdir().unwrap();
    let db_path = directory.path().join("memory.sqlite");
    let kv = KvStore::open(&db_path).await.unwrap();
    let (commit_tx, commit_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let (release_tx, release_rx) = std::sync::mpsc::sync_channel::<()>(1);
    let release_rx = Arc::new(StdMutex::new(release_rx));
    let armed = Arc::new(AtomicBool::new(false));
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))
        .unwrap()
        .create_if_missing(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(Duration::from_secs(5));
    let hook_rx = release_rx.clone();
    let hook_tx = commit_tx.clone();
    let hook_armed = armed.clone();
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .after_connect(move |conn, _| {
            let rx = hook_rx.clone();
            let tx = hook_tx.clone();
            let armed = hook_armed.clone();
            Box::pin(async move {
                let mut handle = conn.lock_handle().await?;
                handle.set_commit_hook(move || {
                    if armed.swap(false, Ordering::SeqCst) {
                        let _ = tx.send(());
                        let _ = rx.lock().unwrap().recv_timeout(Duration::from_secs(20));
                    }
                    true
                });
                Ok(())
            })
        })
        .connect_with(options)
        .await
        .unwrap();
    let _hook_cleanup = CommitHookRelease(release_tx);
    let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(journal.to_ascii_lowercase(), "wal");

    let sink = Arc::new(FailureRecordingSink(StdMutex::new(Vec::new())));
    let store = Store::open_memory().await.unwrap();
    let b_entered = Arc::new(tokio::sync::Notify::new());
    let b_release = Arc::new(tokio::sync::Notify::new());
    let provider = Arc::new(TwoPhaseProvider {
        b_entered: b_entered.clone(),
        b_release: b_release.clone(),
        calls: std::sync::atomic::AtomicUsize::new(0),
    });
    let memory = SessionMemory::new(kv.clone(), Arc::new(UnusedSummarizer))
        .with_session_log(agent24_memory::session_log::SessionLog::new(pool.clone()))
        .with_owner("m1-test-owner".to_owned());
    let manager = RunManager::with_memory(
        store.clone(),
        Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)])),
        Arc::new(ToolRegistry::new()),
        sink.clone(),
        CancellationToken::new(),
        Some(memory),
    );
    seed_session(&store, "commit-cancel-race").await;

    // B reaches the provider and pauses after obtaining its read-side session lock.
    let b = manager
        .start_run(RunCreate {
            session_id: Some("commit-cancel-race".into()),
            prompt: "B question".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), b_entered.notified())
        .await
        .expect("B provider entered");
    // A writes first and pauses inside SQLite's real worker-thread commit hook.
    armed.store(true, Ordering::SeqCst);
    let a = manager
        .start_run(RunCreate {
            session_id: Some("commit-cancel-race".into()),
            prompt: "A question".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        tokio::task::spawn_blocking(move || commit_rx.recv_timeout(Duration::from_secs(2)))
            .await
            .unwrap()
            .unwrap()
    })
    .await
    .expect("A reached SQLite COMMIT hook");

    let lock = manager
        .memory
        .as_ref()
        .unwrap()
        .session_lock("commit-cancel-race")
        .await;
    assert!(
        lock.try_lock().is_err(),
        "A must retain the session lock while COMMIT is paused"
    );
    let before = kv
        .events()
        .scan(
            &agent24_memory::event::EventQuery::owner("m1-test-owner")
                .session("commit-cancel-race"),
        )
        .await
        .unwrap();
    assert!(
        before.is_empty(),
        "another WAL connection must see the pre-commit snapshot"
    );
    // Queue this probe before B, so B cannot hide an early release by taking
    // the lock itself. Keep the same future queued across the timeout.
    let mut lock_probe = Box::pin(lock.lock());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut lock_probe)
            .await
            .is_err(),
        "A must still own the per-session lock before cancellation"
    );
    b_release.notify_one();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if store
                .list_run_messages(&b.id)
                .await
                .unwrap()
                .iter()
                .any(|m| m.role == "assistant")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("B must have answered before A is cancelled");

    let started = std::time::Instant::now();
    let _ = manager.cancel_run(&a.id).await;
    let a_done = tokio::time::timeout(Duration::from_secs(1), wait_terminal(&store, &a.id))
        .await
        .expect("A cancellation must complete promptly");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "A cancellation took {:?}",
        started.elapsed()
    );
    assert_eq!(a_done.status, RunStatus::Cancelled);
    let still_waiting = tokio::time::timeout(Duration::from_millis(100), &mut lock_probe)
        .await
        .is_err();
    drop(lock_probe);
    // In the broken implementation B can now count the old WAL snapshot and
    // block in BEGIN IMMEDIATE. A correct implementation keeps B on the lock.
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Keep the worker alive until the waiter check completes, including on panic.
    drop(_hook_cleanup);
    let b_done = wait_terminal(&store, &b.id).await;
    assert_eq!(b_done.status, RunStatus::Completed);
    let view = kv
        .session_log()
        .load_view("m1-test-owner", "commit-cancel-race")
        .await
        .unwrap();
    let messages = view
        .tail
        .iter()
        .map(|(_, message)| message.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        messages,
        [
            Msg::user("A question"),
            Msg::assistant(Some("A answer".to_owned()), vec![]),
            Msg::user("B question"),
            Msg::assistant(Some("B answer".to_owned()), vec![]),
        ],
        "both complete exchanges must be durable in order"
    );
    assert!(
        still_waiting,
        "session lock was released before SQLite COMMIT finished"
    );
    let failures = sink
        .0
        .lock()
        .unwrap()
        .iter()
        .map(|body| serde_json::to_value(body).unwrap())
        .filter(|json| json["type"] == "memory.write_failed")
        .collect::<Vec<_>>();
    assert!(failures.is_empty(), "memory write failures: {failures:?}");
    pool.close().await;
}
