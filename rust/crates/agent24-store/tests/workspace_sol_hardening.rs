#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_store::{Store, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];

async fn workspace(
    store: &Store,
    id: Option<&str>,
    created: &str,
    expires: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, unix_device, unix_inode)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'external',
                 'orchestrator', 'owner', 'serial', ?, ?, 1, '/tmp/sol',
                 'g1', 'unix', ?, ?)",
    )
    .bind(id)
    .bind(created)
    .bind(expires)
    .bind(DEVICE)
    .bind(INODE)
    .execute(test_hooks::pool(store))
    .await
    .map(|_| ())
}

#[tokio::test]
async fn ids_are_not_nullable_and_identity_values_must_be_blobs() {
    let store = Store::open_memory().await.unwrap();
    assert!(workspace(&store, None, CREATED, EXPIRES).await.is_err());
    workspace(&store, Some(ID), CREATED, EXPIRES).await.unwrap();
    let text_identity = sqlx::query(
        "UPDATE workspaces SET root_identity_kind = 'windows', unix_device = NULL,
         unix_inode = NULL, windows_volume_serial = '12345678',
         windows_file_id = '1234567890123456' WHERE id = ?",
    )
    .bind(ID)
    .execute(test_hooks::pool(&store))
    .await;
    assert!(
        text_identity.is_err(),
        "TEXT bytes must not satisfy BLOB identity shape"
    );
    let null_lease = sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
         VALUES (NULL, ?, 'g1', 'run-1', 'run', ?)",
    )
    .bind(ID)
    .bind(CREATED)
    .execute(test_hooks::pool(&store))
    .await;
    assert!(null_lease.is_err());
}

#[tokio::test]
async fn every_timestamp_round_trips_and_workspace_ttl_is_bounded() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store, Some(ID), CREATED, EXPIRES).await.unwrap();
    assert!(
        sqlx::query("UPDATE workspaces SET created_at = '2026-02-30T00:00:00.000Z' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE workspaces SET created_at = '2026-99-99T00:00:00.000Z' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    assert!(
        sqlx::query("UPDATE workspaces SET expires_at = '2026-09-20T24:00:00.000Z' WHERE id = ?")
            .bind(ID)
            .execute(test_hooks::pool(&store))
            .await
            .is_err()
    );
    assert!(
        workspace(
            &store,
            Some("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6"),
            CREATED,
            "2026-09-27T00:00:00.000Z"
        )
        .await
        .is_err()
    );
    assert!(sqlx::query("INSERT INTO workspace_leases (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at) VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X5', ?, 'g1', 'run-1', 'run', '2026-02-30T00:00:00.000Z')")
        .bind(ID).execute(test_hooks::pool(&store)).await.is_err());
}

#[tokio::test]
async fn counters_require_integer_values_in_the_safe_range() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store, Some(ID), CREATED, EXPIRES).await.unwrap();
    for value in ["abc", "9223372036854775808"] {
        assert!(
            sqlx::query("UPDATE workspaces SET revision = ? WHERE id = ?")
                .bind(value)
                .bind(ID)
                .execute(test_hooks::pool(&store))
                .await
                .is_err()
        );
    }
    for value in ["abc", "-1"] {
        assert!(
            sqlx::query("UPDATE workspaces SET cleanup_attempts = ? WHERE id = ?")
                .bind(value)
                .bind(ID)
                .execute(test_hooks::pool(&store))
                .await
                .is_err()
        );
    }
}
