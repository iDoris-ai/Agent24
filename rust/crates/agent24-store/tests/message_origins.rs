#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::test_hooks;
use agent24_store::{
    MessageOrigin, MessageOriginLookup, OriginatedRunMessage, PendingRunMessage,
    SnapshotRunMessage, Store, message_instance_digest,
};
use serde_json::{Value, json};

const NOW: &str = "2026-10-24T00:00:00Z";

async fn store_with_run(run_id: &str) -> Store {
    let store = Store::open_memory().await.unwrap();
    sqlx::query(
        "INSERT INTO runs (id, status, input, usage, created_at) VALUES (?, 'queued', '{}', '{}', ?)",
    )
    .bind(run_id)
    .bind(NOW)
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
    store
}

fn row(role: &str, content: Option<&str>, origin: MessageOrigin) -> OriginatedRunMessage {
    OriginatedRunMessage {
        message: PendingRunMessage {
            role: role.to_owned(),
            content: content.map(str::to_owned),
            tool_calls: json!([]),
            tool_call_id: None,
        },
        origin,
    }
}

async fn messages(store: &Store, run_id: &str) -> usize {
    store.list_run_messages(run_id).await.unwrap().len()
}

async fn tags(store: &Store, run_id: &str) -> usize {
    store.list_run_source_tags(run_id).await.unwrap().len()
}

async fn trigger(store: &Store, sql: &str) {
    sqlx::query(sql)
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
}

#[tokio::test]
async fn origin_write_failure_leaves_the_message_count_unchanged() {
    let store = store_with_run("run_fail").await;
    let before = messages(&store, "run_fail").await;
    let tags_before = tags(&store, "run_fail").await;
    trigger(
        &store,
        "CREATE TRIGGER reject_origin BEFORE INSERT ON run_message_origins \
         BEGIN SELECT RAISE(FAIL, 'origin rejected'); END",
    )
    .await;
    assert!(
        store
            .append_originated_run_messages(
                "run_fail",
                &[row("user", Some("a"), MessageOrigin::UserPrompt)],
                NOW,
            )
            .await
            .is_err()
    );
    assert_eq!(messages(&store, "run_fail").await, before);
    assert_eq!(tags(&store, "run_fail").await, tags_before);
    assert!(matches!(
        store.read_run_message_origin("run_fail", 0).await.unwrap(),
        MessageOriginLookup::Missing
    ));
}

#[tokio::test]
async fn missing_origin_read_is_not_no_recall_and_leaves_source_tags() {
    let store = store_with_run("run_old").await;
    let tags_before = tags(&store, "run_old").await;
    store
        .append_run_messages_tx(
            "run_old",
            &[PendingRunMessage {
                role: "user".to_owned(),
                content: Some("same text".to_owned()),
                tool_calls: json!([]),
                tool_call_id: None,
            }],
            NOW,
        )
        .await
        .unwrap();
    let lookup = store.read_run_message_origin("run_old", 0).await.unwrap();
    assert!(matches!(lookup, MessageOriginLookup::Missing));
    assert!(!format!("{lookup:?}").contains("NoRecall"));
    assert_eq!(tags(&store, "run_old").await, tags_before);
    assert_eq!(messages(&store, "run_old").await, 1);
}

#[tokio::test]
async fn second_origin_failure_rolls_back_the_first_message() {
    let store = store_with_run("run_batch").await;
    trigger(
        &store,
        "CREATE TRIGGER reject_second_origin BEFORE INSERT ON run_message_origins \
         WHEN NEW.seq = 1 BEGIN SELECT RAISE(FAIL, 'second origin'); END",
    )
    .await;
    let batch = [
        row("user", Some("a"), MessageOrigin::UserPrompt),
        row(
            "user",
            Some("a"),
            MessageOrigin::HostRecall {
                recall_ref: Some("owner:session:ids".to_owned()),
            },
        ),
    ];
    assert!(
        store
            .append_originated_run_messages("run_batch", &batch, NOW)
            .await
            .is_err()
    );
    assert_eq!(messages(&store, "run_batch").await, 0);
    for seq in [0, 1] {
        assert!(matches!(
            store
                .read_run_message_origin("run_batch", seq)
                .await
                .unwrap(),
            MessageOriginLookup::Missing
        ));
    }
    assert_eq!(tags(&store, "run_batch").await, 0);
}

#[tokio::test]
async fn committed_batch_keeps_bytes_and_binds_each_seq() {
    let store = store_with_run("run_ok").await;
    let nfd = "e\u{0301}";
    store
        .append_originated_run_messages(
            "run_ok",
            &[
                row("user", Some(" a "), MessageOrigin::UserPrompt),
                row("user", Some(nfd), MessageOrigin::SessionHistory),
                row(
                    "assistant",
                    None,
                    MessageOrigin::HostRecall {
                        recall_ref: Some("mem:1".to_owned()),
                    },
                ),
            ],
            NOW,
        )
        .await
        .unwrap();
    let thread = store.list_run_messages("run_ok").await.unwrap();
    assert_eq!(
        (
            thread[0].content.as_deref(),
            thread[1].content.as_deref(),
            thread[2].content.as_deref()
        ),
        (Some(" a "), Some(nfd), None)
    );
    let first = present(&store, "run_ok", 0).await;
    assert_eq!(first.origin, MessageOrigin::UserPrompt);
    assert_eq!(
        first.instance_digest,
        message_instance_digest("user", Some(" a "), &json!([]), None).unwrap()
    );
    let second = present(&store, "run_ok", 1).await;
    assert_eq!(second.origin, MessageOrigin::SessionHistory);
    assert_ne!(
        second.instance_digest,
        message_instance_digest("user", Some("é"), &json!([]), None).unwrap()
    );
    assert_eq!(
        present(&store, "run_ok", 2).await.origin,
        MessageOrigin::HostRecall {
            recall_ref: Some("mem:1".to_owned()),
        }
    );
    assert_eq!(tags(&store, "run_ok").await, 0);
}

#[test]
fn digest_keeps_null_whitespace_unicode_and_tool_fields() {
    let empty = json!([]);
    let digest = |content: Option<&str>, calls: &Value, tool_call_id: Option<&str>| {
        message_instance_digest("user", content, calls, tool_call_id).unwrap()
    };
    assert_ne!(digest(None, &empty, None), digest(Some(""), &empty, None));
    assert_ne!(
        digest(Some("a"), &empty, None),
        digest(Some(" a"), &empty, None)
    );
    assert_ne!(
        digest(Some("é"), &empty, None),
        digest(Some("e\u{0301}"), &empty, None)
    );
    assert_ne!(
        message_instance_digest("tool", Some("x"), &empty, None).unwrap(),
        message_instance_digest("tool", Some("x"), &empty, Some("tc")).unwrap()
    );
    assert_ne!(
        digest(Some("x"), &empty, None),
        digest(Some("x"), &json!([{"id":"1"}]), None)
    );
}

#[tokio::test]
async fn unknown_origin_version_is_unreadable_and_not_no_recall() {
    let store = store_with_run("run_bad_ver").await;
    let tags_before = tags(&store, "run_bad_ver").await;
    store
        .append_originated_run_messages(
            "run_bad_ver",
            &[row("user", Some("a"), MessageOrigin::UserPrompt)],
            NOW,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE run_message_origins SET schema_version = 99 WHERE run_id = ?")
        .bind("run_bad_ver")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let lookup = store
        .read_run_message_origin("run_bad_ver", 0)
        .await
        .unwrap();
    assert!(matches!(lookup, MessageOriginLookup::Unreadable { seq: 0 }));
    assert!(!format!("{lookup:?}").contains("NoRecall"));
    assert_eq!(tags(&store, "run_bad_ver").await, tags_before);
    assert_eq!(messages(&store, "run_bad_ver").await, 1);
}

#[tokio::test]
async fn tampered_message_content_is_unreadable_and_not_no_recall() {
    let store = store_with_run("run_bad_body").await;
    let tags_before = tags(&store, "run_bad_body").await;
    store
        .append_originated_run_messages(
            "run_bad_body",
            &[row(
                "user",
                Some("a"),
                MessageOrigin::HostRecall {
                    recall_ref: Some("mem:1".to_owned()),
                },
            )],
            NOW,
        )
        .await
        .unwrap();
    sqlx::query("UPDATE run_messages SET content = 'changed' WHERE run_id = ?")
        .bind("run_bad_body")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let lookup = store
        .read_run_message_origin("run_bad_body", 0)
        .await
        .unwrap();
    assert!(matches!(lookup, MessageOriginLookup::Unreadable { seq: 0 }));
    assert!(!format!("{lookup:?}").contains("NoRecall"));
    assert_eq!(tags(&store, "run_bad_body").await, tags_before);
    assert_eq!(messages(&store, "run_bad_body").await, 1);
    assert_eq!(
        store.list_run_messages("run_bad_body").await.unwrap()[0]
            .content
            .as_deref(),
        Some("changed")
    );
}

#[tokio::test]
async fn tampered_instance_digest_is_unreadable_and_not_no_recall() {
    let store = store_with_run("run_bad_digest").await;
    let tags_before = tags(&store, "run_bad_digest").await;
    store
        .append_originated_run_messages(
            "run_bad_digest",
            &[row("user", Some("a"), MessageOrigin::UserPrompt)],
            NOW,
        )
        .await
        .unwrap();
    let other = message_instance_digest("user", Some("other"), &json!([]), None).unwrap();
    sqlx::query("UPDATE run_message_origins SET instance_digest = ? WHERE run_id = ?")
        .bind(&other)
        .bind("run_bad_digest")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let lookup = store
        .read_run_message_origin("run_bad_digest", 0)
        .await
        .unwrap();
    assert!(matches!(lookup, MessageOriginLookup::Unreadable { seq: 0 }));
    assert!(!format!("{lookup:?}").contains("NoRecall"));
    assert_eq!(tags(&store, "run_bad_digest").await, tags_before);
    assert_eq!(messages(&store, "run_bad_digest").await, 1);
    assert_eq!(
        store.list_run_messages("run_bad_digest").await.unwrap()[0]
            .content
            .as_deref(),
        Some("a")
    );
}

fn notice(text: &str) -> SnapshotRunMessage {
    SnapshotRunMessage::PauseNotice(PendingRunMessage {
        role: "system".to_owned(),
        content: Some(text.to_owned()),
        tool_calls: json!([]),
        tool_call_id: None,
    })
}

#[tokio::test]
async fn snapshot_origin_failure_leaves_the_message_count_unchanged() {
    let store = store_with_run("run_snap_fail").await;
    let before = messages(&store, "run_snap_fail").await;
    trigger(
        &store,
        "CREATE TRIGGER reject_snapshot_origin BEFORE INSERT ON run_message_origins \
         BEGIN SELECT RAISE(FAIL, 'origin rejected'); END",
    )
    .await;
    let batch = [
        notice("notice"),
        SnapshotRunMessage::Originated(row("user", Some("prompt"), MessageOrigin::UserPrompt)),
    ];
    assert!(
        store
            .append_snapshot_run_messages("run_snap_fail", &batch, NOW)
            .await
            .is_err()
    );
    assert_eq!(messages(&store, "run_snap_fail").await, before);
    assert_eq!(tags(&store, "run_snap_fail").await, 0);
    assert!(matches!(
        store
            .read_run_message_origin("run_snap_fail", 0)
            .await
            .unwrap(),
        MessageOriginLookup::Missing
    ));
}

#[tokio::test]
async fn snapshot_empty_recall_ref_rolls_back_the_pause_notice() {
    let store = store_with_run("run_empty_ref").await;
    let batch = [
        notice("notice"),
        SnapshotRunMessage::Originated(row(
            "user",
            Some("block"),
            MessageOrigin::HostRecall {
                recall_ref: Some(String::new()),
            },
        )),
    ];
    assert!(
        store
            .append_snapshot_run_messages("run_empty_ref", &batch, NOW)
            .await
            .is_err()
    );
    assert_eq!(messages(&store, "run_empty_ref").await, 0);
}

async fn present(store: &Store, run_id: &str, seq: i64) -> agent24_store::StoredMessageOrigin {
    match store.read_run_message_origin(run_id, seq).await.unwrap() {
        MessageOriginLookup::Present(origin) => origin,
        other => panic!("expected present origin, got {other:?}"),
    }
}
