#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];

async fn workspace(store: &Store) {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external',
                 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/temporal',
                 'g1', 'unix', ?, ?)",
    )
    .bind(ID)
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(DEVICE)
    .bind(INODE)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn host_lease(store: &Store, id: &str, host: &str, expires: &str, renewed: Option<&str>) {
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind,
          daemon_generation, host_instance_id, acquired_at, expires_at, renewed_at)
         VALUES (?, ?, 'g1', ?, 'host', 'daemon-1', ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(ID)
    .bind(host)
    .bind(host)
    .bind(CREATED)
    .bind(expires)
    .bind(renewed)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

#[tokio::test]
async fn cleanup_states_and_workspace_release_order_are_temporal() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store).await;
    assert!(
        sqlx::query("UPDATE workspaces SET state = 'expired', quarantine_root = '/q' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    assert!(sqlx::query("UPDATE workspaces SET cleanup_attempts = 1, cleanup_last_attempt_at = NULL WHERE id = ?")
        .bind(ID).execute(test_hooks::pool(&store)).await.is_err());
    assert!(sqlx::query("UPDATE workspaces SET state = 'cleanup_failed', cleanup_error = 'busy', cleanup_retry_at = '2026-09-19T00:00:01.000Z' WHERE id = ?")
        .bind(ID).execute(test_hooks::pool(&store)).await.is_err());
    sqlx::query("UPDATE workspaces SET state = 'releasing', quarantine_root = '/q' WHERE id = ?")
        .bind(ID)
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    assert!(sqlx::query("UPDATE workspaces SET renewed_at = '2026-09-19T00:00:10.000Z', quarantined_at = '2026-09-19T00:00:05.000Z' WHERE id = ?")
        .bind(ID).execute(test_hooks::pool(&store)).await.is_err());
    assert!(
        sqlx::query("UPDATE workspaces SET state = 'active' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    assert!(
        sqlx::query(
            "UPDATE workspaces SET quarantined_at = '2026-09-18T23:59:59.000Z' WHERE id = ?"
        )
        .bind(ID)
        .execute(test_hooks::pool(&store))
        .await
        .is_err()
    );

    assert!(sqlx::query("UPDATE workspaces SET state = 'released', released_at = '2026-09-18T23:59:59.000Z', quarantine_root = '/q2', quarantined_at = '2026-09-19T00:00:00.000Z' WHERE id = ?")
        .bind(ID).execute(test_hooks::pool(&store)).await.is_err());
    assert!(sqlx::query("UPDATE workspaces SET state = 'released', released_at = '2026-09-19T00:00:01.000Z', quarantine_root = '/q2', quarantined_at = '2026-09-19T00:00:02.000Z' WHERE id = ?")
        .bind(ID).execute(test_hooks::pool(&store)).await.is_err());
}

#[tokio::test]
async fn host_leases_enforce_interval_and_release_order() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store).await;
    assert!(sqlx::query("INSERT INTO workspace_leases (lease_id, workspace_id, root_generation, owner_id, kind, daemon_generation, host_instance_id, acquired_at, expires_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X5', ?, 'g1', 'host-long', 'host', 'daemon-1', 'host-long', ?, '2026-09-19T00:02:00.000Z')")
        .bind(ID).bind(CREATED).execute(test_hooks::pool(&store)).await.is_err());
    host_lease(
        &store,
        "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
        "host-a",
        "2026-09-19T00:01:00.000Z",
        None,
    )
    .await;
    assert!(sqlx::query("UPDATE workspace_leases SET released_at = '2026-09-18T23:59:59.000Z' WHERE lease_id = ?")
        .bind("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6").execute(test_hooks::pool(&store)).await.is_err());
    assert!(sqlx::query("UPDATE workspace_leases SET acquired_at = '2026-09-19T24:00:00.000Z' WHERE lease_id = ?")
        .bind("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6").execute(test_hooks::pool(&store)).await.is_err());
    host_lease(
        &store,
        "wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
        "host-b",
        "2026-09-19T00:01:00.000Z",
        Some("2026-09-19T00:00:30.000Z"),
    )
    .await;
    assert!(sqlx::query("UPDATE workspace_leases SET released_at = '2026-09-19T00:00:10.000Z' WHERE lease_id = ?")
        .bind("wl_01J5M4Q2Y7N8P9R0S1T2V3W4X7").execute(test_hooks::pool(&store)).await.is_err());
}

#[tokio::test]
async fn host_exact_ninety_second_bound_accepts_every_millisecond() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store).await;
    for millis in 0..1000 {
        let lease_id = format!("wl_{millis:026X}");
        let host = format!("boundary-{millis}");
        let acquired = format!("2026-09-19T00:00:00.{millis:03}Z");
        let expires = format!("2026-09-19T00:01:30.{millis:03}Z");
        sqlx::query(
            "INSERT INTO workspace_leases
             (lease_id, workspace_id, root_generation, owner_id, kind,
              daemon_generation, host_instance_id, acquired_at, expires_at)
             VALUES (?, ?, 'g1', ?, 'host', 'daemon-1', ?, ?, ?)",
        )
        .bind(lease_id)
        .bind(ID)
        .bind(&host)
        .bind(&host)
        .bind(acquired)
        .bind(expires)
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    }
    assert!(
        sqlx::query(
            "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind,
          daemon_generation, host_instance_id, acquired_at, expires_at)
         VALUES ('wl_00000000000000000000000000', ?, 'g1', 'over', 'host',
                 'daemon-1', 'over', '2026-09-19T00:00:00.000Z',
                 '2026-09-19T00:01:30.001Z')",
        )
        .bind(ID)
        .execute(test_hooks::pool(&store))
        .await
        .is_err()
    );
}
