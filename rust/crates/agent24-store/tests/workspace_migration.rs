#![allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)]

use agent24_store::{Store, test_hooks};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-20T00:00:00.000Z";
const DEVICE: &[u8] = &[0; 8];
const INODE: &[u8] = &[1; 8];
const WINDOWS_VOLUME: &[u8] = &[2; 8];
const WINDOWS_FILE: &[u8] = &[3; 16];

async fn workspace(
    store: &Store,
    id: &str,
    generation: &str,
    identity: (
        &str,
        Option<&[u8]>,
        Option<&[u8]>,
        Option<&[u8]>,
        Option<&[u8]>,
    ),
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, provenance_project_ref,
          provenance_base_revision, writeback_policy, lifecycle_owner_kind,
          lifecycle_owner_ref, concurrency_policy, created_at, expires_at,
          revision, canonical_root, root_generation, root_identity_kind,
          unix_device, unix_inode, windows_volume_serial, windows_file_id)
         VALUES (?, 'orchestrator_scratch', 'active', 'test', 'project',
                 'base', 'external', 'orchestrator', 'owner', 'serial',
                 ?, ?, 1, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(format!("/tmp/{id}"))
    .bind(generation)
    .bind(identity.0)
    .bind(identity.1)
    .bind(identity.2)
    .bind(identity.3)
    .bind(identity.4)
    .execute(test_hooks::pool(store))
    .await
    .map(|_| ())
}

fn unix_identity() -> (
    &'static str,
    Option<&'static [u8]>,
    Option<&'static [u8]>,
    Option<&'static [u8]>,
    Option<&'static [u8]>,
) {
    ("unix", Some(DEVICE), Some(INODE), None, None)
}

#[tokio::test]
async fn unix_and_windows_identities_are_exclusive_and_unique() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store, ID, "g1", unix_identity()).await.unwrap();
    let duplicate_root = sqlx::query(
        "INSERT INTO workspaces
         (id, kind, state, provenance_source, writeback_policy,
          lifecycle_owner_kind, lifecycle_owner_ref, concurrency_policy,
          created_at, expires_at, revision, canonical_root, root_generation,
          root_identity_kind, windows_volume_serial, windows_file_id)
         VALUES ('ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8', 'orchestrator_scratch',
                 'active', 'test', 'external', 'orchestrator', 'owner',
                 'serial', ?, ?, 1, '/tmp/ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5',
                 'g2', 'windows', ?, ?)",
    )
    .bind(CREATED)
    .bind(EXPIRES)
    .bind(WINDOWS_VOLUME)
    .bind(WINDOWS_FILE)
    .execute(test_hooks::pool(&store))
    .await;
    assert!(
        duplicate_root.is_err(),
        "canonical roots must resolve uniquely"
    );
    assert!(
        workspace(
            &store,
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "g1",
            unix_identity()
        )
        .await
        .is_err()
    );
    assert!(
        workspace(
            &store,
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "g1",
            (
                "unix",
                Some(DEVICE),
                Some(INODE),
                Some(WINDOWS_VOLUME),
                None
            )
        )
        .await
        .is_err()
    );
    workspace(
        &store,
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
        "g1",
        (
            "windows",
            None,
            None,
            Some(WINDOWS_VOLUME),
            Some(WINDOWS_FILE),
        ),
    )
    .await
    .unwrap();
    assert!(
        workspace(
            &store,
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "g1",
            (
                "windows",
                None,
                None,
                Some(WINDOWS_VOLUME),
                Some(WINDOWS_FILE)
            )
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn lease_generation_is_a_composite_foreign_key() {
    let store = Store::open_memory().await.unwrap();
    workspace(&store, ID, "generation-7", unix_identity())
        .await
        .unwrap();
    let bad = sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
         VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X5', ?, 'wrong', 'run-1', 'run', ?)",
    )
    .bind(ID)
    .bind(CREATED)
    .execute(test_hooks::pool(&store))
    .await;
    assert!(bad.is_err());
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
         VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6', ?, 'generation-7', 'run-1', 'run', ?)",
    )
    .bind(ID)
    .bind(CREATED)
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
}
