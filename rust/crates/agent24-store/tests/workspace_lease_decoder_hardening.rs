#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, WorkspaceLeaseRow, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const LEASE: &str = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-19T00:00:30.000Z";

async fn seed(store: &Store) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external',
                 'orchestrator', 'owner', 'serial', ?, '2026-09-20T00:00:00.000Z',
                 1, '/tmp/ws', 'g1', 'unix', zeroblob(8), zeroblob(8))",
    )
    .bind(ID)
    .bind(CREATED)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind,
          daemon_generation, host_instance_id, acquired_at, expires_at)
         VALUES (?, ?, 'g1', 'host-1', 'host', 'daemon-1', 'host-1', ?, ?)",
    )
    .bind(LEASE)
    .bind(ID)
    .bind(CREATED)
    .bind(EXPIRES)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn tamper(store: &Store, sql: &str) {
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
    sqlx::query(sql)
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = OFF")
        .execute(test_hooks::pool(store))
        .await
        .unwrap();
}

async fn decodes(store: &Store) -> agent24_store::WorkspaceResult<WorkspaceLeaseRow> {
    let row = sqlx::query("SELECT * FROM workspace_leases WHERE lease_id = ?")
        .bind(LEASE)
        .fetch_one(test_hooks::pool(store))
        .await
        .unwrap();
    WorkspaceLeaseRow::decode(&row)
}

#[tokio::test]
async fn lease_decoder_rejects_blank_nul_and_wrong_storage_fields() {
    let store = Store::open_memory().await.unwrap();
    seed(&store).await;
    tamper(
        &store,
        "UPDATE workspace_leases SET owner_id = '   ' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decodes(&store).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed(&store).await;
    tamper(
        &store,
        "UPDATE workspace_leases SET host_instance_id = char(0) WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decodes(&store).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed(&store).await;
    tamper(
        &store,
        "UPDATE workspace_leases SET owner_id = x'686f7374' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decodes(&store).await.is_err());
}

#[tokio::test]
async fn lease_decoder_rejects_host_ttl_over_ninety_seconds() {
    let store = Store::open_memory().await.unwrap();
    seed(&store).await;
    tamper(
        &store,
        "UPDATE workspace_leases SET expires_at = '2026-09-19T00:01:31.000Z' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9'",
    )
    .await;
    assert!(decodes(&store).await.is_err());
}
