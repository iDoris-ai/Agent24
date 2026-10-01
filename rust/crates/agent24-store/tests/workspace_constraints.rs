#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)]

use agent24_store::{Store, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const HOST_EXPIRES: &str = "2026-09-19T00:01:00.000Z";
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
                 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/ws',
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

async fn lease(
    store: &Store,
    id: &str,
    owner: &str,
    kind: &str,
    daemon: Option<&str>,
    host: Option<&str>,
    expires: Option<&str>,
    renewed: Option<&str>,
) -> sqlx::Result<()> {
    assert_eq!(
        id.len(),
        29,
        "lease fixture ids must pass the id-length check before field-shape assertions"
    );
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind,
          daemon_generation, host_instance_id, acquired_at, expires_at, renewed_at)
         VALUES (?, ?, 'g1', ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(ID)
    .bind(owner)
    .bind(kind)
    .bind(daemon)
    .bind(host)
    .bind(CREATED)
    .bind(expires)
    .bind(renewed)
    .execute(test_hooks::pool(store))
    .await
    .map(|_| ())
}

#[tokio::test]
async fn lease_kind_fields_and_host_time_order_are_checked() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store).await;
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y1AA",
            "run-1",
            "run",
            Some("d1"),
            None,
            None,
            None
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y2AA",
            "host-1",
            "host",
            None,
            Some("host-1"),
            Some(HOST_EXPIRES),
            None
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y7AA",
            "host-1",
            "host",
            Some(" "),
            Some("host-1"),
            Some(HOST_EXPIRES),
            None
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y3AA",
            "not-host",
            "host",
            Some("d1"),
            Some("host-1"),
            Some(HOST_EXPIRES),
            None
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y4AA",
            "host-1",
            "host",
            Some("d1"),
            Some("host-1"),
            Some(CREATED),
            None
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y5AA",
            "host-1",
            "host",
            Some("d1"),
            Some("host-1"),
            Some(HOST_EXPIRES),
            Some(EXPIRES)
        )
        .await
        .is_err()
    );
    assert!(
        lease(
            &store,
            "wl_01J5M4Q2Y7N8P9R0S1T2V3Y6AA",
            "host-1",
            "host",
            Some("d1"),
            Some("host-1"),
            Some(HOST_EXPIRES),
            None
        )
        .await
        .is_ok()
    );
}
