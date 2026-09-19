#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, WorkspaceLeaseRow, test_hooks};

const WORKSPACE: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];

async fn seed_workspace(store: &Store) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external',
                 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/ws',
                 'g1', 'unix', ?, ?)",
    )
    .bind(WORKSPACE)
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(DEVICE)
    .bind(INODE)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn seed_lease(
    store: &Store,
    id: &str,
    owner: &str,
    kind: &str,
    daemon: Option<&str>,
    host: Option<&str>,
    expires: Option<&str>,
    renewed: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind,
          daemon_generation, host_instance_id, acquired_at, expires_at, renewed_at)
         VALUES (?, ?, 'g1', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(WORKSPACE)
    .bind(owner)
    .bind(kind)
    .bind(daemon)
    .bind(host)
    .bind(CREATED)
    .bind(expires)
    .bind(renewed)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn decode(store: &Store, id: &str) -> agent24_store::WorkspaceResult<WorkspaceLeaseRow> {
    let row = sqlx::query("SELECT * FROM workspace_leases WHERE lease_id = ?")
        .bind(id)
        .fetch_one(test_hooks::pool(store))
        .await
        .unwrap();
    WorkspaceLeaseRow::decode(&row)
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

#[tokio::test]
async fn lease_decoder_accepts_run_and_host_shapes() {
    let store = Store::open_memory().await.unwrap();
    seed_workspace(&store).await;
    let run_id = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
    seed_lease(&store, run_id, "run-1", "run", None, None, None, None).await;
    assert!(decode(&store, run_id).await.is_ok());

    let store = Store::open_memory().await.unwrap();
    seed_workspace(&store).await;
    let host_id = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6";
    seed_lease(
        &store,
        host_id,
        "host-1",
        "host",
        Some("daemon-1"),
        Some("host-1"),
        Some("2026-09-19T00:00:30.000Z"),
        Some("2026-09-19T00:00:10.000Z"),
    )
    .await;
    assert!(decode(&store, host_id).await.is_ok());
}

#[tokio::test]
async fn lease_decoder_rejects_kind_shape_and_owner_corruption() {
    let store = Store::open_memory().await.unwrap();
    seed_workspace(&store).await;
    let id = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7";
    seed_lease(&store, id, "run-1", "run", None, None, None, None).await;
    tamper(
        &store,
        "UPDATE workspace_leases SET daemon_generation = 'd1' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed_workspace(&store).await;
    seed_lease(
        &store,
        id,
        "host-1",
        "host",
        Some("daemon-1"),
        Some("host-1"),
        Some("2026-09-19T00:00:30.000Z"),
        None,
    )
    .await;
    tamper(
        &store,
        "UPDATE workspace_leases SET owner_id = 'other' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());
}

#[tokio::test]
async fn lease_decoder_rejects_bad_id_storage_and_temporal_values() {
    let store = Store::open_memory().await.unwrap();
    seed_workspace(&store).await;
    let id = "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8";
    seed_lease(
        &store,
        id,
        "host-1",
        "host",
        Some("daemon-1"),
        Some("host-1"),
        Some("2026-09-19T00:00:30.000Z"),
        None,
    )
    .await;
    tamper(
        &store,
        "UPDATE workspace_leases SET lease_id = 'bad' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8'",
    )
    .await;
    assert!(decode(&store, "bad").await.is_err());

    let store = Store::open_memory().await.unwrap();
    seed_workspace(&store).await;
    seed_lease(
        &store,
        id,
        "host-1",
        "host",
        Some("daemon-1"),
        Some("host-1"),
        Some("2026-09-19T00:00:30.000Z"),
        None,
    )
    .await;
    tamper(
        &store,
        "UPDATE workspace_leases SET renewed_at = '2026-09-18T23:59:59.000Z' WHERE lease_id = 'wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8'",
    )
    .await;
    assert!(decode(&store, id).await.is_err());
}
