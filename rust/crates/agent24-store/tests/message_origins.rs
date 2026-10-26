#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::str::FromStr;

use agent24_store::test_hooks;
use agent24_store::{
    MessageOrigin, MessageOriginLookup, OriginatedRunMessage, PendingRunMessage,
    SnapshotRunMessage, Store, message_instance_digest,
};
use serde_json::{Value, json};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

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

fn tool_row(content: &str, tool_call_id: &str, origin: MessageOrigin) -> OriginatedRunMessage {
    OriginatedRunMessage {
        message: PendingRunMessage {
            role: "tool".to_owned(),
            content: Some(content.to_owned()),
            tool_calls: json!([]),
            tool_call_id: Some(tool_call_id.to_owned()),
        },
        origin,
    }
}

#[tokio::test]
async fn assistant_or_tool_result_with_recall_ref_is_not_present() {
    let store = store_with_run("run_new_kinds").await;
    store
        .append_originated_run_messages(
            "run_new_kinds",
            &[
                row("assistant", Some("answer"), MessageOrigin::Assistant),
                tool_row("out", "call_1", MessageOrigin::ToolResult),
            ],
            NOW,
        )
        .await
        .unwrap();
    assert_eq!(
        present(&store, "run_new_kinds", 0).await.origin,
        MessageOrigin::Assistant
    );
    assert_eq!(
        present(&store, "run_new_kinds", 1).await.origin,
        MessageOrigin::ToolResult
    );
    let thread = store.list_run_messages("run_new_kinds").await.unwrap();
    assert_eq!(thread[1].tool_call_id.as_deref(), Some("call_1"));
    let recall_refs: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT recall_ref FROM run_message_origins WHERE run_id = ? ORDER BY seq",
    )
    .bind("run_new_kinds")
    .fetch_all(test_hooks::pool(&store))
    .await
    .unwrap();
    assert_eq!(recall_refs, vec![None, None]);

    sqlx::query(
        "UPDATE run_message_origins SET recall_ref = 'call_1' WHERE run_id = ? AND seq = 0",
    )
    .bind("run_new_kinds")
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
    sqlx::query("UPDATE run_message_origins SET recall_ref = '' WHERE run_id = ? AND seq = 1")
        .bind("run_new_kinds")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    for seq in [0_i64, 1] {
        let lookup = store
            .read_run_message_origin("run_new_kinds", seq)
            .await
            .unwrap();
        assert!(
            matches!(lookup, MessageOriginLookup::Unreadable { seq: got } if got == seq),
            "{lookup:?}"
        );
    }
}

#[tokio::test]
async fn rebuilt_origin_table_keeps_old_rows_and_does_not_backfill() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.db");
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.migrations = migrator
        .migrations
        .iter()
        .filter(|migration| migration.version < 22)
        .cloned()
        .collect::<Vec<_>>()
        .into();
    migrator.run(&pool).await.unwrap();

    sqlx::query(
        "INSERT INTO runs (id, status, input, usage, created_at) VALUES ('run_old', 'queued', '{}', '{}', ?)",
    )
    .bind(NOW)
    .execute(&pool)
    .await
    .unwrap();
    for (seq, role, content) in [(0_i64, "user", "kept"), (1_i64, "assistant", "unmapped")] {
        sqlx::query(
            "INSERT INTO run_messages (run_id, seq, role, content, tool_calls, tool_call_id, created_at) \
             VALUES ('run_old', ?, ?, ?, '[]', NULL, ?)",
        )
        .bind(seq)
        .bind(role)
        .bind(content)
        .bind(NOW)
        .execute(&pool)
        .await
        .unwrap();
    }
    let digest = message_instance_digest("user", Some("kept"), &json!([]), None).unwrap();
    sqlx::query(
        "INSERT INTO run_message_origins (run_id, seq, schema_version, kind, instance_digest, recall_ref) \
         VALUES ('run_old', 0, 1, 'UserPrompt', ?, NULL)",
    )
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap();

    sqlx::migrate!("./migrations").run(&pool).await.unwrap();

    let kinds: Vec<(i64, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT seq, kind, recall_ref, schema_version FROM run_message_origins WHERE run_id = 'run_old' ORDER BY seq",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(kinds, vec![(0, "UserPrompt".to_owned(), None, 1)]);
    let message_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM run_messages WHERE run_id = 'run_old'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(message_count, 2);

    let unknown = sqlx::query(
        "INSERT INTO run_message_origins (run_id, seq, schema_version, kind, instance_digest, recall_ref) \
         VALUES ('run_old', 1, 1, 'Bogus', ?, NULL)",
    )
    .bind(&digest)
    .execute(&pool)
    .await;
    assert!(unknown.is_err(), "{unknown:?}");
    sqlx::query(
        "INSERT INTO run_message_origins (run_id, seq, schema_version, kind, instance_digest, recall_ref) \
         VALUES ('run_old', 1, 1, 'Assistant', ?, NULL)",
    )
    .bind(&digest)
    .execute(&pool)
    .await
    .unwrap();
    let assistant: String = sqlx::query_scalar(
        "SELECT kind FROM run_message_origins WHERE run_id = 'run_old' AND seq = 1",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(assistant, "Assistant");
}

#[tokio::test]
async fn snapshot_pause_notice_is_written_as_present_host_system() {
    let store = store_with_run("run_host_system").await;
    store
        .append_snapshot_run_messages(
            "run_host_system",
            &[
                notice("paused"),
                SnapshotRunMessage::Originated(row(
                    "user",
                    Some("prompt"),
                    MessageOrigin::UserPrompt,
                )),
            ],
            NOW,
        )
        .await
        .unwrap();
    match store
        .read_run_message_origin("run_host_system", 0)
        .await
        .unwrap()
    {
        MessageOriginLookup::Present(stored) => {
            assert_eq!(stored.origin, MessageOrigin::HostSystem);
        }
        other => panic!("expected Present HostSystem, got {other:?}"),
    }
}

#[tokio::test]
async fn origin_kind_check_rejects_unknown_kind() {
    let store = store_with_run("run_check").await;
    store
        .append_originated_run_messages(
            "run_check",
            &[
                row("user", Some("a"), MessageOrigin::UserPrompt),
                row("user", Some("b"), MessageOrigin::UserPrompt),
            ],
            NOW,
        )
        .await
        .unwrap();
    let digest = message_instance_digest("user", Some("a"), &json!([]), None).unwrap();
    let insert = |kind: &'static str, seq: i64| {
        let digest = digest.clone();
        let pool = test_hooks::pool(&store).clone();
        async move {
            sqlx::query(
                "INSERT OR REPLACE INTO run_message_origins \
                 (run_id, seq, schema_version, kind, instance_digest, recall_ref) \
                 VALUES ('run_check', ?, 1, ?, ?, NULL)",
            )
            .bind(seq)
            .bind(kind)
            .bind(digest)
            .execute(&pool)
            .await
        }
    };
    assert!(insert("Bogus", 0).await.is_err());
    assert!(insert("HostSystem", 1).await.is_ok());
}

#[tokio::test]
async fn host_system_with_recall_ref_is_unreadable() {
    let store = store_with_run("run_hs_ref").await;
    store
        .append_snapshot_run_messages("run_hs_ref", &[notice("paused")], NOW)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE run_message_origins SET recall_ref = '[\"x\"]' WHERE run_id = 'run_hs_ref'",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
    assert!(matches!(
        store
            .read_run_message_origin("run_hs_ref", 0)
            .await
            .unwrap(),
        MessageOriginLookup::Unreadable { seq: 0 }
    ));
}
