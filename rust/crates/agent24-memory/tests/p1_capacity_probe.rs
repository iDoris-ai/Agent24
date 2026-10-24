//! P1-02a 合成容量探针：只在临时目录建隔离合成库，测行数 / payload 字节 / 磁盘占用。
//! 不读任何用户库，不改配额行为，不启动 purge。用
//! `cargo test -p agent24-memory --test p1_capacity_probe -- --nocapture` 查看数字。
#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_memory::KvStore;
use agent24_memory::event::{EventStore, MemEvent, Origin, Scope, Trust};
use serde_json::json;
use std::os::unix::fs::MetadataExt;

const ROWS: usize = 20_000;
const OWNER: &str = "probe-owner";

#[tokio::test]
async fn synthetic_probe_reports_rows_payload_and_disk() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("probe-memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let log = kv.events();
    let text = "x".repeat(256);
    for i in 0..ROWS {
        let ev = MemEvent::new(
            format!("probe-{i}"),
            Scope::owner(OWNER),
            "note",
            json!({ "text": text, "i": i }),
            Origin {
                source: "p1-02a-synthetic".into(),
                trust: Trust::UserSaid,
            },
        );
        log.append(&ev).await.unwrap();
    }
    let admin = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)")
        .execute(&admin)
        .await
        .unwrap();
    let (rows, payload): (i64, i64) = sqlx::query_as(
        "SELECT COUNT(*), COALESCE(SUM(LENGTH(CAST(payload AS BLOB))),0) FROM mem_events WHERE scope_owner=?",
    )
    .bind(OWNER)
    .fetch_one(&admin)
    .await
    .unwrap();
    let (u_rows, u_bytes): (i64, i64) =
        sqlx::query_as("SELECT row_count, byte_count FROM mem_owner_usage WHERE owner=?")
            .bind(OWNER)
            .fetch_one(&admin)
            .await
            .unwrap();
    let md = std::fs::metadata(&path).unwrap();
    let (len, alloc) = (md.len(), md.blocks() * 512);
    println!(
        "PROBE rows={rows} payload_bytes={payload} file_len={len} allocated_bytes={alloc} bytes_per_row_disk={:.1}",
        len as f64 / rows as f64
    );
    assert_eq!(rows as usize, ROWS);
    assert_eq!((u_rows, u_bytes), (rows, payload));
    assert!(len as i64 > payload, "磁盘占用应大于 payload 字节");
}
