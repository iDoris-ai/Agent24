use super::*;
use agent24_memory::event::{Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter, WriteDecision};
use agent24_store::{MessageOrigin, MessageOriginLookup};

async fn wait_snapshot(store: &Store, run_id: &str) -> Vec<RunMessage> {
    for _ in 0..200 {
        let rows = store.list_run_messages(run_id).await.unwrap();
        if !rows.is_empty() {
            return rows;
        }
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    panic!("snapshot was not persisted");
}

async fn present(store: &Store, run_id: &str, seq: i64) -> MessageOrigin {
    match store.read_run_message_origin(run_id, seq).await.unwrap() {
        MessageOriginLookup::Present(stored) => stored.origin,
        other => panic!("seq {seq} origin {other:?}"),
    }
}

async fn origin_count(store: &Store, run_id: &str) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM run_message_origins WHERE run_id = ?")
        .bind(run_id)
        .fetch_one(agent24_store::test_hooks::pool(store))
        .await
        .unwrap()
}

fn seq_of(thread: &[RunMessage], text: &str) -> i64 {
    thread
        .iter()
        .find(|row| row.content.as_deref() == Some(text))
        .unwrap_or_else(|| panic!("missing {text}"))
        .seq
}

#[tokio::test]
async fn paused_snapshot_notice_is_missing_and_neighbors_are_present() {
    let (manager, store, kv) =
        manager_with_memory(Arc::new(HangingProvider), Arc::new(UnusedSummarizer)).await;
    seed_session(&store, "snap-pause").await;
    let log = kv.session_log();
    log.append_summary("test-owner", "snap-pause", "they talked about tea", 0)
        .await
        .unwrap();
    log.append_turn(
        "test-owner",
        "snap-pause",
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
            session_id: Some("snap-pause".into()),
            workspace_id: None,
            prompt: "记住 我喜欢喝茶".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    let thread = wait_snapshot(&store, &run.id).await;
    let notice = seq_of(&thread, retain::PAUSED_WRITE_NOTICE);
    let summary = thread
        .iter()
        .find(|row| {
            row.content
                .as_deref()
                .is_some_and(|text| text.starts_with("Summary of earlier conversation:"))
        })
        .expect("summary")
        .seq;
    assert_eq!(summary, notice + 1);
    let notice_origin = store.read_run_message_origin(&run.id, notice).await.unwrap();
    assert!(matches!(notice_origin, MessageOriginLookup::Missing));
    assert!(!format!("{notice_origin:?}").contains("NoRecall"));
    assert_eq!(
        present(&store, &run.id, summary).await,
        MessageOrigin::SessionHistory
    );
    assert_eq!(
        present(&store, &run.id, seq_of(&thread, "earlier question")).await,
        MessageOrigin::SessionHistory
    );
    assert_eq!(
        present(&store, &run.id, seq_of(&thread, "earlier answer")).await,
        MessageOrigin::SessionHistory
    );
    let prompt = seq_of(&thread, "记住 我喜欢喝茶");
    assert_eq!(prompt, thread.last().unwrap().seq);
    assert_eq!(
        present(&store, &run.id, prompt).await,
        MessageOrigin::UserPrompt
    );
    assert!(
        store
            .list_run_source_tags(&run.id)
            .await
            .unwrap()
            .iter()
            .all(|tag| tag.seq != notice)
    );
    manager.cancel_run(&run.id).await.unwrap();
}

#[tokio::test]
async fn recall_snapshot_stores_null_recall_ref() {
    let (manager, store, kv) =
        manager_with_memory(Arc::new(HangingProvider), Arc::new(UnusedSummarizer)).await;
    for (id, object) in [("peanut", "我对花生过敏"), ("duck", "我喜欢北京烤鸭")] {
        let decision = kv
            .write_gate()
            .propose(vec![
                Candidate::new(
                    id,
                    Scope::owner("test-owner"),
                    "user",
                    "said_to_remember",
                    serde_json::json!(object),
                    Origin {
                        source: "test".into(),
                        trust: Trust::UserSaid,
                    },
                )
                .with_evidence(vec![format!("evidence-{id}")])
                .remember(),
            ])
            .await
            .unwrap();
        assert!(matches!(decision[0], WriteDecision::Committed(_)));
    }
    let run = manager
        .start_run(RunCreate {
            session_id: None,
            workspace_id: None,
            prompt: "花生 烤鸭".into(),
            model_override: None,
            mode: RunMode::Normal,
        })
        .await
        .unwrap();
    let thread = wait_snapshot(&store, &run.id).await;
    let text = thread[0].content.as_deref().unwrap();
    assert!(text.contains("id=peanut") && text.contains("id=duck"), "{text}");
    assert_eq!(
        present(&store, &run.id, thread[0].seq).await,
        MessageOrigin::HostRecall { recall_ref: None }
    );
    let recall_ref: Option<Option<String>> = sqlx::query_scalar(
        "SELECT recall_ref FROM run_message_origins WHERE run_id = ? AND seq = ?",
    )
    .bind(&run.id)
    .bind(thread[0].seq)
    .fetch_optional(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    assert_eq!(recall_ref, Some(None));
    assert_ne!(recall_ref, Some(Some("peanut".into())));
    manager.cancel_run(&run.id).await.unwrap();
}

async fn stored_recall_ref(store: &Store, run_id: &str, seq: i64) -> Option<Option<String>> {
    sqlx::query_scalar(
        "SELECT recall_ref FROM run_message_origins WHERE run_id = ? AND seq = ?",
    )
    .bind(run_id)
    .bind(seq)
    .fetch_optional(agent24_store::test_hooks::pool(store))
    .await
    .unwrap()
}

#[tokio::test]
async fn persist_message_writes_assistant_and_tool_result_without_recall_ref() {
    let (manager, _, store) = manager_with(Arc::new(FixedProvider)).await;
    let run = manager.start_run(create()).await.unwrap();
    assert_eq!(
        wait_terminal(&store, &run.id).await.status,
        RunStatus::Completed
    );
    let thread = store.list_run_messages(&run.id).await.unwrap();
    let pong = seq_of(&thread, "pong");
    assert_eq!(
        present(&store, &run.id, pong).await,
        MessageOrigin::Assistant
    );
    assert_eq!(stored_recall_ref(&store, &run.id, pong).await, Some(None));

    let before = origin_count(&store, &run.id).await;
    manager
        .persist_message(
            &run.id,
            &Msg::assistant(Some("later answer".into()), vec![]),
        )
        .await
        .unwrap();
    manager
        .persist_message(&run.id, &Msg::tool_result("call_1", "tool body"))
        .await
        .unwrap();
    manager
        .persist_message(&run.id, &Msg::user("plain user"))
        .await
        .unwrap();
    manager
        .persist_message(&run.id, &Msg::system("plain system"))
        .await
        .unwrap();

    let after = store.list_run_messages(&run.id).await.unwrap();
    assert_eq!(after.len(), thread.len() + 4);
    let later = seq_of(&after, "later answer");
    let tool = seq_of(&after, "tool body");
    let user = seq_of(&after, "plain user");
    let system = seq_of(&after, "plain system");
    assert_eq!(
        present(&store, &run.id, later).await,
        MessageOrigin::Assistant
    );
    assert_eq!(stored_recall_ref(&store, &run.id, later).await, Some(None));
    assert_eq!(
        present(&store, &run.id, tool).await,
        MessageOrigin::ToolResult
    );
    assert_eq!(stored_recall_ref(&store, &run.id, tool).await, Some(None));
    assert_eq!(
        after
            .iter()
            .find(|row| row.seq == tool)
            .unwrap()
            .tool_call_id
            .as_deref(),
        Some("call_1")
    );
    assert!(matches!(
        store.read_run_message_origin(&run.id, user).await.unwrap(),
        MessageOriginLookup::Missing
    ));
    assert!(matches!(
        store
            .read_run_message_origin(&run.id, system)
            .await
            .unwrap(),
        MessageOriginLookup::Missing
    ));
    assert_eq!(origin_count(&store, &run.id).await, before + 2);
}
