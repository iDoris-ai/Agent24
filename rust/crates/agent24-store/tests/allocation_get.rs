#![allow(clippy::unwrap_used)]

use agent24_store::{
    AllocationId, AllocationPhase, RootIdentity, Store, WorkspaceStoreError, test_hooks,
};

const AID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

fn aid() -> AllocationId {
    AllocationId::parse(AID).unwrap()
}

async fn insert_valid(store: &Store) {
    sqlx::query(
        "INSERT INTO workspace_allocations
         (allocation_id, workspace_id, root_generation, relative_name,
          parent_identity_kind, parent_unix_device, parent_unix_inode,
          parent_windows_volume, parent_windows_file_id, root_identity_kind,
          root_unix_device, root_unix_inode, root_windows_volume,
          root_windows_file_id, phase, created_at, failure_reason)
         VALUES (?, 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5', 'g1', 'root',
                 'windows', NULL, NULL, X'0101010101010101',
                 X'02020202020202020202020202020202', 'unix',
                 X'0303030303030303', X'0404040404040404', NULL, NULL,
                 'committed', '2026-09-19T00:00:00.000Z', NULL)",
    )
    .bind(AID)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

#[tokio::test]
async fn getter_returns_the_exact_strictly_decoded_record() {
    let store = Store::open_memory().await.unwrap();
    insert_valid(&store).await;

    let record = store.get_workspace_allocation(&aid()).await.unwrap();
    assert_eq!(record.id().as_str(), AID);
    assert_eq!(
        record.workspace_id().as_str(),
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"
    );
    assert_eq!(record.root_generation(), "g1");
    assert_eq!(record.relative_name(), "root");
    assert_eq!(
        record.parent_identity(),
        RootIdentity::windows(&[1; 8], &[2; 16]).unwrap()
    );
    assert_eq!(
        record.root_identity(),
        Some(RootIdentity::unix(&[3; 8], &[4; 8]).unwrap())
    );
    assert_eq!(record.phase(), AllocationPhase::Committed);
    assert_eq!(record.created_at().as_str(), "2026-09-19T00:00:00.000Z");
    assert!(record.failure_reason().is_none());
}

#[tokio::test]
async fn getter_has_typed_not_found_and_database_errors() {
    let store = Store::open_memory().await.unwrap();
    assert_eq!(
        store.get_workspace_allocation(&aid()).await.err(),
        Some(WorkspaceStoreError::NotFound)
    );
    sqlx::query("DROP TABLE workspace_allocations")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(
        store.get_workspace_allocation(&aid()).await.err(),
        Some(WorkspaceStoreError::Database)
    );
}
