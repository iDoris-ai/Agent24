// P1-02c c2b (design D2, tests T5/T6): every memory write path reports a
// TYPED failure — a closed code, never the storage error's text, the partition
// key, or anything that reads as "remembered".
use super::*;
use agent24_memory::MemoryErrorCode;
use agent24_memory::session::CanonicalSession;

fn test_source_ref() -> agent24_store::AssertionSourceRef {
    agent24_store::AssertionSourceRef::from_sources([(
        "test".to_owned(),
        agent24_store::SourceClassification::Ordinary,
    )])
}

/// Deliberately shaped like a real partition key (NUL-separated): if it ever
/// reaches an event, `\u0000` shows up in the JSON.
const OWNER: &str = "v2\u{0}5\u{0}org-1\u{0}9\u{0}usr:local";

struct ReceiptSink(StdMutex<Vec<EventBody>>);

impl EventSink for ReceiptSink {
    fn emit(&self, body: EventBody) {
        self.0.lock().unwrap().push(body);
    }
}

impl ReceiptSink {
    fn json(&self) -> Vec<serde_json::Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|b| serde_json::to_value(b).unwrap())
            .collect()
    }

    fn of_type(&self, ty: &str) -> Vec<serde_json::Value> {
        self.json()
            .into_iter()
            .filter(|j| j["type"] == ty)
            .map(|j| j["payload"].clone())
            .collect()
    }
}

struct FixedSummarizer;

#[async_trait]
impl Summarizer for FixedSummarizer {
    async fn summarize(
        &self,
        _prior: Option<&str>,
        _messages: &[Msg],
    ) -> std::result::Result<String, String> {
        Ok("a summary".to_owned())
    }
}

struct FailingSummarizer;

#[async_trait]
impl Summarizer for FailingSummarizer {
    async fn summarize(
        &self,
        _prior: Option<&str>,
        _messages: &[Msg],
    ) -> std::result::Result<String, String> {
        Err("model unavailable".to_owned())
    }
}

struct Rig {
    manager: Arc<RunManager>,
    sink: Arc<ReceiptSink>,
    store: Store,
    kv: KvStore,
    path: std::path::PathBuf,
    provider: Arc<RecordingProvider>,
    _dir: tempfile::TempDir,
}

async fn rig(summarizer: Arc<dyn Summarizer>, policy: CompactionPolicy) -> Rig {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let sink = Arc::new(ReceiptSink(StdMutex::new(Vec::new())));
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
            SessionMemory::new(kv.clone(), summarizer)
                .with_owner(OWNER.to_owned())
                .with_policy(policy),
        ),
    );
    Rig {
        manager,
        sink,
        store,
        kv,
        path,
        provider,
        _dir: dir,
    }
}

fn roomy_policy() -> CompactionPolicy {
    CompactionPolicy {
        max_recent: 40,
        keep_recent: 20,
        max_summary_chars: 4000,
    }
}

async fn set_rows(path: &std::path::Path, rows: i64) {
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let r = sqlx::query("UPDATE mem_owner_quota SET max_rows=? WHERE owner='*'")
        .bind(rows)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(r.rows_affected(), 1);
    pool.close().await;
}

async fn run(rig: &Rig, session: &str, prompt: &str) -> Run {
    seed_session(&rig.store, session).await;
    let started = rig
        .manager
        .start_run(RunCreate {
            workspace_id: None,
            session_id: Some(session.to_owned()),
            prompt: prompt.to_owned(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        })
        .await
        .unwrap();
    wait_terminal(&rig.store, &started.id).await
}

/// T6 + the "never says remembered" half of T5, over EVERY event a rig saw.
fn assert_events_are_clean(rig: &Rig) {
    for event in rig.sink.json() {
        let raw = event.to_string();
        assert!(!raw.contains("\\u0000"), "NUL leaked: {raw}");
        assert!(!raw.contains("org-1"), "partition key leaked: {raw}");
        assert!(!raw.contains("usr:local"), "partition key leaked: {raw}");
        assert!(
            !raw.contains("quota exceeded for owner"),
            "Display text leaked: {raw}"
        );
        assert!(
            !raw.contains("database or disk is full"),
            "driver text leaked: {raw}"
        );
        if event["type"] == "memory.write_failed" {
            // Transitional `reason` is the sanitized code string and nothing else.
            assert_eq!(
                event["payload"]["reason"], event["payload"]["code"],
                "reason must be exactly the code: {raw}"
            );
            assert!(!raw.contains("saved"), "{raw}");
            assert!(!raw.contains("已记住"), "{raw}");
        }
    }
}

#[tokio::test]
async fn run_on_a_full_partition_reports_a_typed_failure_and_memory_persisted_false() {
    let r = rig(Arc::new(UnusedSummarizer), roomy_policy()).await;
    set_rows(&r.path, 0).await;
    let finished = run(&r, "s-full", "hello").await;
    assert_eq!(finished.status, RunStatus::Completed);

    let failed = r.sink.of_type("memory.write_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["code"], "memory_quota_rows");
    assert_eq!(failed[0]["dimension"], "rows");
    assert_eq!(failed[0]["owner_kind"], "personal");
    assert_eq!(failed[0]["session_id"], "s-full");
    assert!(failed[0].get("kind").is_none(), "{failed:?}");
    assert!(failed[0].get("partial").is_none(), "{failed:?}");

    let completed = r.sink.of_type("run.completed");
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["memory_persisted"], false);
    assert_events_are_clean(&r);
}

#[tokio::test]
async fn run_with_room_reports_memory_persisted_true_and_no_failure() {
    let r = rig(Arc::new(UnusedSummarizer), roomy_policy()).await;
    let finished = run(&r, "s-ok", "hello").await;
    assert_eq!(finished.status, RunStatus::Completed);
    assert!(r.sink.of_type("memory.write_failed").is_empty());
    assert_eq!(r.sink.of_type("run.completed")[0]["memory_persisted"], true);
}

#[tokio::test]
async fn chat_turn_outcomes_carry_the_typed_code_on_a_full_partition() {
    let r = rig(Arc::new(UnusedSummarizer), roomy_policy()).await;
    set_rows(&r.path, 0).await;
    for prompt in ["今天天气", "记住我喜欢茶"] {
        let outcome = r.manager.chat_remember_turn("s-chat", prompt, "好的", test_source_ref()).await;
        assert_eq!(
            outcome,
            MemoryWriteOutcome::Failed(MemoryFailure {
                code: MemoryErrorCode::QuotaRows
            }),
            "{prompt}"
        );
        assert!(!outcome.persisted());
    }
    // Nothing of either turn is in the history.
    let view = r.kv.session_log().load_view(OWNER, "s-chat").await.unwrap();
    assert!(view.tail.is_empty());
    assert_events_are_clean(&r);
}

/// T5's `partial` combination: the turn commits (2 rows fit), the explicit
/// remember's assertion + audit does not.
#[tokio::test]
async fn explicit_remember_whose_turn_fits_but_assertion_does_not_is_partial() {
    let r = rig(Arc::new(UnusedSummarizer), roomy_policy()).await;
    set_rows(&r.path, 2).await;
    let outcome = r
        .manager
        .chat_remember_turn("s-partial", "记住我喜欢茶", "好的", test_source_ref())
        .await;
    assert_eq!(
        outcome,
        MemoryWriteOutcome::Partial(MemoryFailure {
            code: MemoryErrorCode::QuotaRows
        })
    );
    assert!(!outcome.persisted());

    let failed = r.sink.of_type("memory.write_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["partial"], true);
    assert_eq!(failed[0]["code"], "memory_quota_rows");
    // The conversation IS in the history (that is what makes it partial).
    let view = r
        .kv
        .session_log()
        .load_view(OWNER, "s-partial")
        .await
        .unwrap();
    assert_eq!(view.tail.len(), 2);
    assert_events_are_clean(&r);
}

/// W2: a compaction summary that cannot be WRITTEN is observable, and the turn
/// that triggered it is still reported saved.
#[tokio::test]
async fn a_summary_that_cannot_be_written_emits_a_typed_session_summary_failure() {
    let policy = CompactionPolicy {
        max_recent: 1,
        keep_recent: 0,
        max_summary_chars: 500,
    };
    let r = rig(Arc::new(FixedSummarizer), policy).await;
    // The turn's two rows fit exactly; the summary would be the third.
    set_rows(&r.path, 2).await;
    let outcome = r.manager.chat_remember_turn("s-sum", "你好", "好的", test_source_ref()).await;
    assert_eq!(outcome, MemoryWriteOutcome::NotApplicable);
    assert!(outcome.persisted(), "the turn itself was saved");

    let failed = r.sink.of_type("memory.write_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["kind"], "session.summary");
    assert_eq!(failed[0]["code"], "memory_quota_rows");
    assert!(failed[0].get("partial").is_none());
    assert_events_are_clean(&r);
}

/// W2 + W4 at once: the assertion AND the summary both fail to write. Each is
/// reported on its own; the summary event must not be lost behind the partial.
#[tokio::test]
async fn a_double_failure_reports_both_the_partial_and_the_summary() {
    let policy = CompactionPolicy {
        max_recent: 1,
        keep_recent: 0,
        max_summary_chars: 500,
    };
    let r = rig(Arc::new(FixedSummarizer), policy).await;
    // The turn's two rows fit; the assertion's audit row and the summary do not.
    set_rows(&r.path, 2).await;
    let outcome = r
        .manager
        .chat_remember_turn("s-double", "记住我喜欢茶", "好的", test_source_ref())
        .await;
    assert!(matches!(outcome, MemoryWriteOutcome::Partial(_)), "{outcome:?}");

    let failed = r.sink.of_type("memory.write_failed");
    assert_eq!(failed.len(), 2, "{failed:?}");
    assert!(
        failed.iter().any(|f| f["partial"] == true && f.get("kind").is_none()),
        "{failed:?}"
    );
    assert!(
        failed.iter().any(|f| f["kind"] == "session.summary"),
        "the summary failure must not be dropped: {failed:?}"
    );
    assert_events_are_clean(&r);
}

/// The other half of W2: a summariser (model) failure is not a write failure.
#[tokio::test]
async fn a_summariser_failure_is_not_reported_as_a_write_failure() {
    let policy = CompactionPolicy {
        max_recent: 1,
        keep_recent: 0,
        max_summary_chars: 500,
    };
    let r = rig(Arc::new(FailingSummarizer), policy).await;
    let outcome = r.manager.chat_remember_turn("s-sum2", "你好", "好的", test_source_ref()).await;
    assert!(outcome.persisted());
    assert!(r.sink.of_type("memory.write_failed").is_empty());
}

/// W3: a legacy session whose import cannot be written is a typed, retryable
/// failure — not an empty session the run carries on with.
#[tokio::test]
async fn a_failed_legacy_import_fails_the_run_typed_and_a_retry_recovers_the_history() {
    let r = rig(Arc::new(UnusedSummarizer), roomy_policy()).await;
    let mut legacy = CanonicalSession::new("s-legacy");
    legacy.recent = vec![Msg::user("旧问题"), Msg::assistant(Some("旧回答".into()), vec![])];
    r.kv.put("session", "s-legacy", &legacy).await.unwrap();
    set_rows(&r.path, 0).await;

    let finished = run(&r, "s-legacy", "新问题").await;
    assert_eq!(finished.status, RunStatus::Failed);
    assert_eq!(
        finished.error.as_ref().map(|e| e.code.as_str()),
        Some("memory_quota_rows")
    );
    let failed = r.sink.of_type("memory.write_failed");
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert_eq!(failed[0]["kind"], "session.import");
    assert_eq!(failed[0]["code"], "memory_quota_rows");
    assert!(
        r.provider.seen.lock().unwrap().is_empty(),
        "the model must not be called on an empty stand-in for the history"
    );
    // Nothing was written, so the session is still un-imported.
    let view = r
        .kv
        .session_log()
        .load_view(OWNER, "s-legacy")
        .await
        .unwrap();
    assert!(view.tail.is_empty());
    assert_events_are_clean(&r);

    // Retry once there is room: the legacy history comes through.
    set_rows(&r.path, 200_000).await;
    let second = r
        .manager
        .start_run(RunCreate {
            workspace_id: None,
            session_id: Some("s-legacy".to_owned()),
            prompt: "再问".to_owned(),
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        })
        .await
        .unwrap();
    assert_eq!(
        wait_terminal(&r.store, &second.id).await.status,
        RunStatus::Completed
    );
    let seen = r.provider.seen.lock().unwrap();
    let last = seen.last().expect("provider called after the retry");
    assert!(
        last.iter()
            .any(|m| m.content.as_deref() == Some("旧回答")),
        "the legacy history must be present after a successful retry: {last:?}"
    );
}

#[test]
fn wire_codes_match_the_memory_crate_strings_one_for_one() {
    for code in MemoryErrorCode::ALL {
        let wire = MemoryFailure { code }.wire_code();
        assert_eq!(
            serde_json::to_value(wire).unwrap(),
            serde_json::json!(code.as_str()),
            "{code}"
        );
    }
}
