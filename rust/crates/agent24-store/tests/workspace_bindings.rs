#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{StandingGrant, Store, test_hooks};
use sha2::{Digest, Sha256};
use sqlx::migrate::Migrator;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::{path::Path, str::FromStr};

const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const TS: &str = "2026-09-19T00:00:00.000Z";

fn audit_hash(prev: &str, ts: &str, actor: &str, action: &str, detail: &str) -> String {
    let mut h = Sha256::new();
    for (i, part) in [prev, ts, actor, action, detail].iter().enumerate() {
        if i > 0 {
            h.update(b"|");
        }
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

#[tokio::test]
async fn upgrade_keeps_legacy_rows_and_adds_nullable_workspace_foreign_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy.db");
    let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
        .unwrap()
        .create_if_missing(true)
        .foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .unwrap();
    let mut migrator = Migrator::new(Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations"))
        .await
        .unwrap();
    migrator.migrations.to_mut().retain(|m| m.version <= 8);
    migrator.run(&pool).await.unwrap();
    sqlx::query("INSERT INTO sessions VALUES ('s','t','cli',?,?)")
        .bind(TS)
        .bind(TS)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO runs (id,session_id,status,input,usage,created_at) VALUES ('r','s','queued','{}','{}',?)").bind(TS).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO approvals (id,run_id,tool_call_id,kind,summary,payload,available_decisions,status,expires_at,created_at) VALUES ('a','r','tc','exec','x','{}','[]','pending',?,?)").bind(TS).bind(TS).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tool_calls (id,run_id,tool,input,status,started_at) VALUES ('tc','r','x','{}','running',?)").bind(TS).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO standing_grants VALUES ('g','session','s','x','target',?)")
        .bind(TS)
        .execute(&pool)
        .await
        .unwrap();
    let detail = "{}";
    let hash = audit_hash("genesis", TS, "daemon", "legacy", detail);
    sqlx::query(
        "INSERT INTO audit_log (ts,actor,action,detail,prev_hash,hash) VALUES (?,?,?,?,?,?)",
    )
    .bind(TS)
    .bind("daemon")
    .bind("legacy")
    .bind(detail)
    .bind("genesis")
    .bind(hash)
    .execute(&pool)
    .await
    .unwrap();
    pool.close().await;

    let store = Store::open(&path).await.unwrap();
    let pool = test_hooks::pool(&store);
    sqlx::query("INSERT INTO workspaces (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,canonical_root,root_generation,root_identity_kind,unix_device,unix_inode) VALUES (?,'orchestrator_scratch','active','test','external','orchestrator','owner','serial',?, '2026-09-20T00:00:00.000Z',1,'/tmp/ws','g1','unix',?,?)")
        .bind(WS).bind(TS).bind([0u8; 8].as_slice()).bind([1u8; 8].as_slice()).execute(pool).await.unwrap();
    for table in [
        "sessions",
        "runs",
        "approvals",
        "tool_calls",
        "standing_grants",
    ] {
        let nulls: i64 = sqlx::query_scalar(&format!(
            "SELECT count(*) FROM {table} WHERE workspace_id IS NULL"
        ))
        .fetch_one(pool)
        .await
        .unwrap();
        assert_eq!(nulls, 1, "{table}");
        sqlx::query(&format!("UPDATE {table} SET workspace_id = ?"))
            .bind(WS)
            .execute(pool)
            .await
            .unwrap();
        assert!(
            sqlx::query(&format!("UPDATE {table} SET workspace_id = 'missing'"))
                .execute(pool)
                .await
                .is_err(),
            "{table}"
        );
        sqlx::query(&format!("UPDATE {table} SET workspace_id = NULL"))
            .execute(pool)
            .await
            .unwrap();
    }
    for table in [
        "sessions",
        "runs",
        "approvals",
        "tool_calls",
        "standing_grants",
    ] {
        sqlx::query(&format!("UPDATE {table} SET workspace_id = ?"))
            .bind(WS)
            .execute(pool)
            .await
            .unwrap();
    }
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM sessions WHERE id='s'")
            .fetch_one(pool)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM standing_grants WHERE id='g'")
            .fetch_one(pool)
            .await
            .unwrap(),
        1
    );
    store
        .insert_standing_grant(&StandingGrant {
            id: "g2".into(),
            scope_kind: "session".into(),
            scope_id: "s".into(),
            tool: "x".into(),
            target: "another-target".into(),
            created_at: TS.into(),
        })
        .await
        .unwrap();
    store.verify_audit_chain().await.unwrap();
}
