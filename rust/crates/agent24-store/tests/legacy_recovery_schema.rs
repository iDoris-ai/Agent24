#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};
use sqlx::SqlitePool;

const TS: &str = "2026-09-19T00:00:00.000Z";
const WS: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

async fn legacy_workspace(pool: &SqlitePool, id: &str, n: u8) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO workspaces
         (id,kind,state,provenance_source,writeback_policy,lifecycle_owner_kind,
          lifecycle_owner_ref,concurrency_policy,created_at,expires_at,revision,
          canonical_root,root_generation,root_identity_kind,unix_device,unix_inode)
         VALUES (?,'legacy_compat','active','test','external','orchestrator',
                 'owner','serial',?,'2026-09-20T00:00:00.000Z',1,?, 'g1','unix',?,?)",
    )
    .bind(id)
    .bind(TS)
    .bind(format!("/tmp/{id}"))
    .bind([n; 8].as_slice())
    .bind([n + 1; 8].as_slice())
    .execute(pool)
    .await
    .map(|_| ())
}

async fn cohort(
    pool: &SqlitePool,
    id: &str,
    version: i64,
    ws: &str,
    generation: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO legacy_recovery_cohorts
         (cohort_id,migration_version,legacy_workspace_id,root_generation,created_at)
         VALUES (?,?,?,?,?)",
    )
    .bind(id)
    .bind(version)
    .bind(ws)
    .bind(generation)
    .bind(TS)
    .execute(pool)
    .await
    .map(|_| ())
}

#[tokio::test]
async fn cohort_hold_constraints_are_dormant_and_run_scoped() {
    let store = Store::open_memory().await.unwrap();
    let pool = test_hooks::pool(&store);
    legacy_workspace(pool, WS, 1).await.unwrap();
    assert!(
        legacy_workspace(pool, "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7", 5)
            .await
            .is_err()
    );
    sqlx::query(
        "INSERT INTO runs (id,status,input,usage,created_at) VALUES
         ('r1','queued','{}','{}',?), ('r2','running','{}','{}',?)",
    )
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO approvals (id,run_id,tool_call_id,kind,summary,payload,
         available_decisions,status,expires_at,created_at) VALUES
         ('a1','r1','tc','exec','x','{}','[]','pending',?,?),
         ('a2','r2','tc','exec','x','{}','[]','pending',?,?)",
    )
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .bind(TS)
    .execute(pool)
    .await
    .unwrap();
    cohort(pool, "cohort-a", 1, WS, "g1").await.unwrap();
    assert!(cohort(pool, "cohort-b", 1, WS, "g1").await.is_err());
    assert!(cohort(pool, "cohort-b", 2, WS, "wrong").await.is_err());
    sqlx::query(
        "INSERT INTO legacy_recovery_holds (run_id,cohort_id,workspace_id,
         root_generation,original_status,recovery_state,approval_id,ready_at) VALUES
         ('r1','cohort-a',?,'g1','queued','awaiting_decision','a1',NULL),
         ('r2','cohort-a',?,'g1','running','ready','a2',?)",
    )
    .bind(WS)
    .bind(WS)
    .bind(TS)
    .execute(pool)
    .await
    .unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM legacy_recovery_holds WHERE workspace_id=?"
        )
        .bind(WS)
        .fetch_one(pool)
        .await
        .unwrap(),
        2
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM workspace_leases")
            .fetch_one(pool)
            .await
            .unwrap(),
        0
    );
    for sql in [
        "UPDATE legacy_recovery_holds SET approval_id='a2' WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET root_generation='wrong' WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='ready',ready_at=NULL WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='awaiting_decision',approval_id=NULL WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='needs_attention',reason_code=NULL WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='released',released_at=NULL WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='ready',ready_at='2026-09-19T00:00:00.000Z',released_at='2026-09-19T00:00:00.000Z' WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='active',active_resume_approval_id=NULL WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='ready',active_resume_approval_id='a1' WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET original_status='completed' WHERE run_id='r1'",
        "UPDATE legacy_recovery_holds SET recovery_state='active',approval_id=NULL,active_resume_approval_id='a1' WHERE run_id='r1'",
    ] {
        assert!(sqlx::query(sql).execute(pool).await.is_err(), "{sql}");
    }
    sqlx::query("UPDATE legacy_recovery_holds SET recovery_state='active',active_resume_approval_id='a1' WHERE run_id='r1'").execute(pool).await.unwrap();
    sqlx::query("UPDATE legacy_recovery_holds SET recovery_state='needs_attention',reason_code='bad_input',active_resume_approval_id=NULL WHERE run_id='r1'").execute(pool).await.unwrap();
    sqlx::query("UPDATE legacy_recovery_holds SET recovery_state='released',released_at=? WHERE run_id='r1'").bind(TS).execute(pool).await.unwrap();
}
