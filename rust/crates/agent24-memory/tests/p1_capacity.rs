//! Small synthetic probe of the F7 owner quota boundary.
//! This does not generate large histories or estimate production capacity.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_memory::event::{EventQuery, EventStore, MemEvent, Origin, Scope, Trust};
use agent24_memory::{KvStore, MemoryError};
use serde_json::json;

fn event(id: &str, owner: &str, body: serde_json::Value) -> MemEvent {
    MemEvent::new(
        id,
        Scope::owner(owner),
        "note",
        body,
        Origin {
            source: "p1-f7-synthetic".into(),
            trust: Trust::UserSaid,
        },
    )
}

async fn set_quota(pool: &sqlx::SqlitePool, owner: &str, rows: i64, bytes: i64) {
    sqlx::query(
        "INSERT INTO mem_owner_quota(owner, max_rows, max_bytes) VALUES(?, ?, ?)
         ON CONFLICT(owner) DO UPDATE SET max_rows=excluded.max_rows, max_bytes=excluded.max_bytes",
    )
    .bind(owner)
    .bind(rows)
    .bind(bytes)
    .execute(pool)
    .await
    .unwrap();
}

async fn usage(pool: &sqlx::SqlitePool, owner: &str) -> (i64, i64) {
    sqlx::query_as("SELECT row_count, byte_count FROM mem_owner_usage WHERE owner=?")
        .bind(owner)
        .fetch_optional(pool)
        .await
        .unwrap()
        .unwrap_or((0, 0))
}

#[tokio::test]
async fn f7_small_quotas_are_owner_scoped_atomic_and_count_payload_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("synthetic-memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let admin = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let defaults: (i64, i64) =
        sqlx::query_as("SELECT max_rows, max_bytes FROM mem_owner_quota WHERE owner='*'")
            .fetch_one(&admin)
            .await
            .unwrap();
    assert_eq!(defaults, (200_000, 268_435_456));

    // A tiny per-owner override exercises the same trigger as default owners.
    set_quota(&admin, "rows-capped", 2, 100_000).await;
    let log = kv.events();
    for id in ["r1", "r2"] {
        log.append(&event(id, "rows-capped", json!({"text":"ok"})))
            .await
            .unwrap();
    }
    let before = log.scan(&EventQuery::owner("rows-capped")).await.unwrap();
    let rejected = log
        .append(&event("r3", "rows-capped", json!({"text":"over"})))
        .await
        .unwrap_err();
    assert!(
        matches!(rejected, MemoryError::QuotaExceeded { ref owner, dimension: "rows" } if owner == "rows-capped")
    );
    let after = log.scan(&EventQuery::owner("rows-capped")).await.unwrap();
    assert_eq!(
        after, before,
        "a rejected append must preserve prior events"
    );
    assert_eq!(usage(&admin, "rows-capped").await.0, 2);

    // No owner override means the migration's '*' fallback applies.
    for id in ["d1", "d2", "d3"] {
        log.append(&event(id, "default-owner", json!("fallback")))
            .await
            .unwrap();
    }
    assert_eq!(usage(&admin, "default-owner").await.0, 3);
    assert_eq!(usage(&admin, "rows-capped").await.0, 2);

    // byte_count is serialized event-body bytes, not SQLite file/disk usage.
    let body = json!("x");
    let payload_bytes = serde_json::to_string(&body).unwrap().len() as i64;
    set_quota(&admin, "bytes-capped", 10, payload_bytes + 2).await;
    log.append(&event("b1", "bytes-capped", body))
        .await
        .unwrap();
    let byte_before = log.scan(&EventQuery::owner("bytes-capped")).await.unwrap();
    let rejected = log
        .append(&event("b2", "bytes-capped", json!("too large")))
        .await
        .unwrap_err();
    assert!(
        matches!(rejected, MemoryError::QuotaExceeded { ref owner, dimension: "bytes" } if owner == "bytes-capped")
    );
    assert_eq!(
        log.scan(&EventQuery::owner("bytes-capped")).await.unwrap(),
        byte_before,
        "a byte-rejected append must preserve prior event contents"
    );
    assert_eq!(usage(&admin, "bytes-capped").await, (1, payload_bytes));

    let (pages, page_size): (i64, i64) =
        sqlx::query_as("SELECT page_count, page_size FROM pragma_page_count(), pragma_page_size()")
            .fetch_one(&admin)
            .await
            .unwrap();
    println!(
        "default_quota_rows={} default_quota_payload_bytes={} rows_capped_used={} bytes_capped_payload_bytes={} sqlite_main_pages_bytes={}",
        defaults.0,
        defaults.1,
        usage(&admin, "rows-capped").await.0,
        usage(&admin, "bytes-capped").await.1,
        pages * page_size
    );
}
