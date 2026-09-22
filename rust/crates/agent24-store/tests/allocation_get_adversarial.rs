#![allow(clippy::unwrap_used)]

use agent24_store::{
    AllocationId, AllocationPhase, RootIdentity, Store, WorkspaceStoreError, test_hooks,
};

const AID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

async fn insert(
    store: &Store,
    phase: &str,
    parent: &str,
    root: Option<&str>,
    reason: Option<&str>,
) {
    sqlx::query(
        "INSERT INTO workspace_allocations
         (allocation_id, workspace_id, root_generation, relative_name,
          parent_identity_kind, parent_unix_device, parent_unix_inode,
          parent_windows_volume, parent_windows_file_id, root_identity_kind,
          root_unix_device, root_unix_inode, root_windows_volume,
          root_windows_file_id, phase, created_at, failure_reason)
         VALUES (?, 'ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5', 'g1', 'root',
                 ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?,
                 '2026-09-19T00:00:00.000Z', ?)",
    )
    .bind(AID)
    .bind(parent)
    .bind((parent == "unix").then(|| vec![1; 8]))
    .bind((parent == "unix").then(|| vec![2; 8]))
    .bind((parent == "windows").then(|| vec![3; 8]))
    .bind((parent == "windows").then(|| vec![4; 16]))
    .bind(root)
    .bind((root == Some("unix")).then(|| vec![5; 8]))
    .bind((root == Some("unix")).then(|| vec![6; 8]))
    .bind((root == Some("windows")).then(|| vec![7; 8]))
    .bind((root == Some("windows")).then(|| vec![8; 16]))
    .bind(phase)
    .bind(reason)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn tamper(store: &Store, update: &str) {
    let pool = test_hooks::pool(store);
    sqlx::query("PRAGMA ignore_check_constraints = ON")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(update).execute(pool).await.unwrap();
    sqlx::query("PRAGMA ignore_check_constraints = OFF")
        .execute(pool)
        .await
        .unwrap();
}

fn aid() -> AllocationId {
    AllocationId::parse(AID).unwrap()
}

#[tokio::test]
async fn getter_is_read_only_under_abort_triggers() {
    let store = Store::open_memory().await.unwrap();
    insert(&store, "committed", "windows", Some("unix"), None).await;
    let pool = test_hooks::pool(&store);
    for (table_index, table) in ["workspace_allocations", "workspaces", "audit_log"]
        .into_iter()
        .enumerate()
    {
        for (operation_index, operation) in ["INSERT", "UPDATE", "DELETE"].into_iter().enumerate() {
            let sql = format!(
                "CREATE TRIGGER deny_{table_index}_{operation_index} BEFORE {operation} ON {table}
                 BEGIN SELECT RAISE(ABORT, 'mutation'); END"
            );
            sqlx::query(&sql).execute(pool).await.unwrap();
        }
    }
    let record = store.get_workspace_allocation(&aid()).await.unwrap();
    assert_eq!(record.phase(), AllocationPhase::Committed);
    assert_eq!(
        record.parent_identity(),
        RootIdentity::windows(&[3; 8], &[4; 16]).unwrap()
    );
    assert_eq!(
        record.root_identity(),
        Some(RootIdentity::unix(&[5; 8], &[6; 8]).unwrap())
    );
}

#[tokio::test]
async fn getter_rejects_single_invariant_corruption() {
    let cases = [
        (
            "UPDATE workspace_allocations SET root_generation = CAST(42 AS BLOB)",
            "root_generation",
        ),
        (
            "UPDATE workspace_allocations SET parent_unix_device = X'01'",
            "parent_identity_kind",
        ),
        (
            "UPDATE workspace_allocations SET parent_unix_inode = NULL",
            "parent_identity_kind",
        ),
        (
            "UPDATE workspace_allocations SET parent_windows_volume = X'0101010101010101', parent_windows_file_id = X'02020202020202020202020202020202'",
            "parent_identity_kind",
        ),
        (
            "UPDATE workspace_allocations SET created_at = '2026-09-19T07:00:00.000+07:00'",
            "created_at",
        ),
        (
            "UPDATE workspace_allocations SET relative_name = 'a/b'",
            "relative_name",
        ),
        (
            "UPDATE workspace_allocations SET root_identity_kind = 'unix', root_unix_device = X'0505050505050505', root_unix_inode = X'0606060606060606'",
            "phase",
        ),
        (
            "UPDATE workspace_allocations SET failure_reason = 'failed'",
            "phase",
        ),
    ];
    for (update, field) in cases {
        let store = Store::open_memory().await.unwrap();
        insert(&store, "reserved", "unix", None, None).await;
        tamper(&store, update).await;
        assert_eq!(
            store.get_workspace_allocation(&aid()).await.err(),
            Some(WorkspaceStoreError::CorruptRow {
                table: "workspace_allocations",
                field,
            })
        );
    }
}

#[tokio::test]
async fn getter_rejects_phase_reason_corruption_from_valid_rows() {
    for (phase, root, reason, update, field) in [
        (
            "materialized",
            Some("unix"),
            None,
            "UPDATE workspace_allocations SET failure_reason = 'failed'",
            "phase",
        ),
        (
            "retained",
            None,
            Some("failed"),
            "UPDATE workspace_allocations SET failure_reason = NULL",
            "failure_reason",
        ),
    ] {
        let store = Store::open_memory().await.unwrap();
        insert(&store, phase, "windows", root, reason).await;
        tamper(&store, update).await;
        assert_eq!(
            store.get_workspace_allocation(&aid()).await.err(),
            Some(WorkspaceStoreError::CorruptRow {
                table: "workspace_allocations",
                field,
            })
        );
    }
}
