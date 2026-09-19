#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const TS: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const HOST_EXPIRES: &str = "2026-09-19T00:01:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];

async fn setup(store: &Store) {
    sqlx::query("INSERT INTO workspaces (id, kind, state, provenance_source, writeback_policy, lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy, created_at, expires_at, revision, canonical_root, root_generation, root_identity_kind, unix_device, unix_inode) VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external', 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/ws', 'g1', 'unix', ?, ?)")
        .bind(ID).bind(TS).bind(EXPIRES).bind(DEVICE).bind(INODE).execute(test_hooks::pool(store)).await.unwrap();
}

async fn lease(
    store: &Store,
    id: &str,
    owner: &str,
    kind: &str,
    host: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query("INSERT INTO workspace_leases (lease_id, workspace_id, root_generation, owner_id, kind, daemon_generation, host_instance_id, acquired_at, expires_at) VALUES (?, ?, 'g1', ?, ?, ?, ?, ?, ?)")
        .bind(id).bind(ID).bind(owner).bind(kind)
        .bind((kind == "host").then_some("d1")).bind(host).bind(TS)
        .bind((kind == "host").then_some(HOST_EXPIRES)).execute(test_hooks::pool(store)).await.map(|_| ())
}

#[tokio::test]
async fn active_run_and_host_indexes_have_the_intended_scopes() {
    let store = Store::open_memory().await.unwrap();
    setup(&store).await;
    lease(
        &store,
        "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
        "run-1",
        "run",
        None,
    )
    .await
    .unwrap();
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "run-2",
            "run",
            None
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "run-1",
            "run",
            None
        )
        .await
        .is_err()
    );
    lease(
        &store,
        "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X8",
        "host-a",
        "host",
        Some("host-a"),
    )
    .await
    .unwrap();
    // The ordinary open index must not make different host instances conflict.
    lease(
        &store,
        "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X9",
        "host-b",
        "host",
        Some("host-b"),
    )
    .await
    .unwrap();
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3W4Y0",
            "host-a",
            "host",
            Some("host-a")
        )
        .await
        .is_err()
    );
}
