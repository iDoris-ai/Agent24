use super::*;
use agent24_memory::event::{Origin, Trust};
use agent24_store::{MessageOrigin, MessageOriginLookup, OriginatedRunMessage};
use sqlx::Row;

const UNPROVEN_MESSAGE: &str = "无法证明本线程已落盘消息的来源，已停止发送。请新建会话。不要自动复制旧 recall 或待执行工具。";

fn unproven_body() -> ErrorBody {
    ErrorBody {
        code: "source_unproven".to_owned(),
        message: UNPROVEN_MESSAGE.to_owned(),
        hint: None,
        details: None,
    }
}

async fn wait_settled(manager: &RunManager, store: &Store, run_id: &str) -> Run {
    let done = wait_terminal(store, run_id).await;
    for _ in 0..200 {
        if !manager.cancels.lock().await.contains_key(run_id) {
            return store.get_run(run_id).await.unwrap().unwrap_or(done);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("execute still holds the cancel token for {run_id}");
}

async fn insert_grant(store: &Store, run_id: &str) {
    sqlx::query(
        "INSERT INTO approvals (id, run_id, tool_call_id, kind, summary, payload, available_decisions, status, expires_at, created_at) VALUES ('ap_keep', ?, 'tc_pending', 'tool', 'keep this grant', '{}', '[\"approve\",\"deny\"]', 'pending', '2099-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
    )
    .bind(run_id)
    .execute(agent24_store::test_hooks::pool(store))
    .await
    .unwrap();
}

async fn grant_state(store: &Store) -> (String, Option<String>, Option<String>) {
    let row = sqlx::query("SELECT status, decision, decided_at FROM approvals WHERE id = 'ap_keep'")
        .fetch_one(agent24_store::test_hooks::pool(store))
        .await
        .unwrap();
    (
        row.get("status"),
        row.get("decision"),
        row.get("decided_at"),
    )
}

async fn reopen(store: &Store, run_id: &str) -> Run {
    sqlx::query("UPDATE runs SET status = 'running', ended_at = NULL, error = NULL WHERE id = ?")
        .bind(run_id)
        .execute(agent24_store::test_hooks::pool(store))
        .await
        .unwrap();
    let run = store.get_run(run_id).await.unwrap().unwrap();
    assert_eq!(run.status, RunStatus::Running);
    run
}

#[tokio::test]
async fn present_prompt_without_recall_or_pause_still_forms_a_provider_request() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    let run = manager
        .start_run(RunCreate {
            session_id: None,
            workspace_id: None,
            prompt: "plain question".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    let done = wait_terminal(&store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(done.output.unwrap().text, "pong");
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        seen[0],
        vec![Msg::user("plain question")]
    );
    let thread = store.list_run_messages(&run.id).await.unwrap();
    assert_eq!(thread.len(), 2);
    assert_eq!(thread[0].content.as_deref(), Some("plain question"));
    assert_eq!(
        store
            .read_run_message_origin(&run.id, thread[0].seq)
            .await
            .unwrap(),
        MessageOriginLookup::Present(agent24_store::StoredMessageOrigin {
            seq: thread[0].seq,
            schema_version: agent24_store::MESSAGE_ORIGIN_SCHEMA_VERSION,
            origin: MessageOrigin::UserPrompt,
            instance_digest: agent24_store::message_instance_digest(
                &thread[0].role,
                thread[0].content.as_deref(),
                &thread[0].tool_calls,
                thread[0].tool_call_id.as_deref(),
            )
            .unwrap(),
        })
    );
}

#[tokio::test]
async fn null_host_recall_does_not_call_the_provider() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    store
        .append_originated_run_messages(
            &run.id,
            &[OriginatedRunMessage {
                message: agent24_store::PendingRunMessage {
                    role: "system".into(),
                    content: Some("planted null recall".into()),
                    tool_calls: serde_json::json!([]),
                    tool_call_id: None,
                },
                origin: MessageOrigin::HostRecall { recall_ref: None },
            }],
            &now_iso8601(),
        )
        .await
        .unwrap();
    let before = store.list_run_messages(&run.id).await.unwrap();
    let planted = before
        .iter()
        .find(|row| row.content.as_deref() == Some("planted null recall"))
        .unwrap();
    assert_eq!(
        store
            .read_run_message_origin(&run.id, planted.seq)
            .await
            .unwrap(),
        MessageOriginLookup::Present(agent24_store::StoredMessageOrigin {
            seq: planted.seq,
            schema_version: agent24_store::MESSAGE_ORIGIN_SCHEMA_VERSION,
            origin: MessageOrigin::HostRecall { recall_ref: None },
            instance_digest: agent24_store::message_instance_digest(
                &planted.role,
                planted.content.as_deref(),
                &planted.tool_calls,
                planted.tool_call_id.as_deref(),
            )
            .unwrap(),
        })
    );
    insert_grant(&store, &run.id).await;
    provider.seen.lock().unwrap().clear();
    let running = reopen(&store, &run.id).await;
    manager
        .run_loop(
            running,
            thread_to_messages(&before),
            false,
            CancellationToken::new(),
        )
        .await;
    let done = store.get_run(&run.id).await.unwrap().unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error, Some(unproven_body()));
    assert!(provider.seen.lock().unwrap().is_empty());
    assert_eq!(store.list_run_messages(&run.id).await.unwrap(), before);
    assert_eq!(grant_state(&store).await, ("pending".to_owned(), None, None));
}

#[tokio::test]
async fn pause_notice_completes_the_run_and_keeps_snapshot_rows() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "gate-pause").await;
    kv.session_log()
        .append_turn(
            "test-owner",
            "gate-pause",
            0,
            &Msg::user("earlier question"),
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
    kv.set_memory_enabled("test-owner", false).await.unwrap();
    let run = manager
        .start_run(RunCreate {
            session_id: Some("gate-pause".into()),
            workspace_id: None,
            prompt: "记住 我喜欢喝茶".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    insert_grant(&store, &run.id).await;
    let done = wait_settled(&manager, &store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    assert_eq!(done.error, None);
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0][0], Msg::system(retain::PAUSED_WRITE_NOTICE));
    let thread = store.list_run_messages(&run.id).await.unwrap();
    // The first call saw the snapshot in order: notice, history, history, prompt.
    let sent: Vec<_> = seen[0].iter().map(|m| m.content.as_deref()).collect();
    assert_eq!(
        sent,
        vec![
            Some(retain::PAUSED_WRITE_NOTICE),
            Some("earlier question"),
            Some("earlier answer"),
            Some("记住 我喜欢喝茶"),
        ]
    );
    // Once the run is terminal the duplicated history rows are reclaimed
    // (M3 growth control), leaving notice, prompt and the reply.
    assert_eq!(thread.len(), 3);
    assert_eq!(thread[0].content.as_deref(), Some(retain::PAUSED_WRITE_NOTICE));
    assert_eq!(thread[1].content.as_deref(), Some("记住 我喜欢喝茶"));
    assert_eq!(thread[2].content.as_deref(), Some("pong"));
    match store
        .read_run_message_origin(&run.id, thread[0].seq)
        .await
        .unwrap()
    {
        MessageOriginLookup::Present(stored) => {
            assert_eq!(stored.origin, MessageOrigin::HostSystem)
        }
        other => panic!("expected Present HostSystem, got {other:?}"),
    }
    assert_eq!(grant_state(&store).await, ("pending".to_owned(), None, None));
}

#[tokio::test]
async fn paused_remember_run_completes_and_provider_gets_notice() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, store, kv) =
        manager_with_memory(provider.clone(), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "gate-pause-ok").await;
    kv.set_memory_enabled("test-owner", false).await.unwrap();
    let run = manager
        .start_run(RunCreate {
            session_id: Some("gate-pause-ok".into()),
            workspace_id: None,
            prompt: "记住 X".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    let done = wait_settled(&manager, &store, &run.id).await;
    assert_eq!(done.status, RunStatus::Completed);
    let seen = provider.seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    let notice = seen[0]
        .iter()
        .find(|m| m.content.as_deref() == Some(retain::PAUSED_WRITE_NOTICE))
        .expect("provider must receive the pause notice verbatim");
    assert_eq!(*notice, Msg::system(retain::PAUSED_WRITE_NOTICE));
    let thread = store.list_run_messages(&run.id).await.unwrap();
    let row = thread
        .iter()
        .find(|r| r.content.as_deref() == Some(retain::PAUSED_WRITE_NOTICE))
        .unwrap();
    match store.read_run_message_origin(&run.id, row.seq).await.unwrap() {
        MessageOriginLookup::Present(stored) => {
            assert_eq!(stored.origin, MessageOrigin::HostSystem)
        }
        other => panic!("expected Present HostSystem, got {other:?}"),
    }
}

#[tokio::test]
async fn legacy_unsourced_pause_notice_still_refused() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    // Simulate a pre-0023 run: a pause notice row with no origin row.
    sqlx::query(
        "INSERT INTO run_messages (run_id, seq, role, content, tool_calls, tool_call_id, created_at) \
         VALUES (?, (SELECT COALESCE(MAX(seq), -1) + 1 FROM run_messages WHERE run_id = ?), 'system', ?, '[]', NULL, ?)",
    )
    .bind(&run.id)
    .bind(&run.id)
    .bind(retain::PAUSED_WRITE_NOTICE)
    .bind(now_iso8601())
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let before = store.list_run_messages(&run.id).await.unwrap();
    let legacy = before
        .iter()
        .find(|r| r.content.as_deref() == Some(retain::PAUSED_WRITE_NOTICE))
        .unwrap();
    assert!(matches!(
        store
            .read_run_message_origin(&run.id, legacy.seq)
            .await
            .unwrap(),
        MessageOriginLookup::Missing
    ));
    provider.seen.lock().unwrap().clear();
    let running = reopen(&store, &run.id).await;
    manager
        .run_loop(
            running,
            thread_to_messages(&before),
            false,
            CancellationToken::new(),
        )
        .await;
    let done = store.get_run(&run.id).await.unwrap().unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error, Some(unproven_body()));
    assert!(provider.seen.lock().unwrap().is_empty());
    assert_eq!(store.list_run_messages(&run.id).await.unwrap(), before);
}

#[tokio::test]
async fn unreadable_assistant_recall_ref_fails_before_the_next_send() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    let before = store.list_run_messages(&run.id).await.unwrap();
    let assistant = before
        .iter()
        .find(|row| row.content.as_deref() == Some("pong"))
        .unwrap();
    sqlx::query(
        "UPDATE run_message_origins SET recall_ref = 'not-a-closure' WHERE run_id = ? AND seq = ?",
    )
    .bind(&run.id)
    .bind(assistant.seq)
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    assert!(matches!(
        store
            .read_run_message_origin(&run.id, assistant.seq)
            .await
            .unwrap(),
        MessageOriginLookup::Unreadable { .. }
    ));
    insert_grant(&store, &run.id).await;
    provider.seen.lock().unwrap().clear();
    let running = reopen(&store, &run.id).await;
    let messages = thread_to_messages(&before);
    manager
        .run_loop(running, messages, false, CancellationToken::new())
        .await;
    let done = store.get_run(&run.id).await.unwrap().unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error, Some(unproven_body()));
    assert!(provider.seen.lock().unwrap().is_empty());
    assert_eq!(store.list_run_messages(&run.id).await.unwrap(), before);
    assert_eq!(grant_state(&store).await, ("pending".to_owned(), None, None));
}

#[tokio::test]
async fn origin_read_failure_does_not_call_the_provider() {
    let provider = Arc::new(RecordingProvider {
        seen: StdMutex::new(vec![]),
    });
    let (manager, _, store) = manager_with(provider.clone()).await;
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    let before = store.list_run_messages(&run.id).await.unwrap();
    sqlx::query("DROP TABLE run_message_origins")
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
    insert_grant(&store, &run.id).await;
    provider.seen.lock().unwrap().clear();
    let running = reopen(&store, &run.id).await;
    manager
        .run_loop(
            running,
            thread_to_messages(&before),
            false,
            CancellationToken::new(),
        )
        .await;
    let done = store.get_run(&run.id).await.unwrap().unwrap();
    assert_eq!(done.status, RunStatus::Failed);
    assert_eq!(done.error, Some(unproven_body()));
    assert!(provider.seen.lock().unwrap().is_empty());
    assert_eq!(store.list_run_messages(&run.id).await.unwrap(), before);
    assert_eq!(grant_state(&store).await, ("pending".to_owned(), None, None));
}
