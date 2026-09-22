#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_store::{
    AllocationId, AllocationPhase, RootIdentity, Store, WorkspaceStoreError, test_hooks,
};

const NOW: &str = "2026-09-19T00:00:00.000Z";
const AID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

async fn insert(
    store: &Store,
    id: &str,
    phase: &str,
    parent: &str,
    root: Option<&str>,
    reason: Option<&str>,
) {
    let workspace_id = format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4{}", &id[id.len() - 2..]);
    sqlx::query(
        "INSERT INTO workspace_allocations
         (allocation_id, workspace_id, root_generation, relative_name,
          parent_identity_kind, parent_unix_device, parent_unix_inode,
          parent_windows_volume, parent_windows_file_id, root_identity_kind,
          root_unix_device, root_unix_inode, root_windows_volume,
          root_windows_file_id, phase, created_at, failure_reason)
         VALUES (?, ?, 'g1', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(format!("name-{id}"))
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
    .bind(NOW)
    .bind(reason)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn tamper(store: &Store, sql: &str) {
    let pool = test_hooks::pool(store);
    for statement in [
        "PRAGMA ignore_check_constraints = ON",
        sql,
        "PRAGMA ignore_check_constraints = OFF",
    ] {
        sqlx::query(statement).execute(pool).await.unwrap();
    }
}

fn aid(value: &str) -> AllocationId {
    AllocationId::parse(value).unwrap()
}
async fn seed(store: &Store) {
    insert(store, AID, "reserved", "unix", None, None).await;
}

async fn counts(store: &Store) -> (i64, i64, i64) {
    sqlx::query_as("SELECT (SELECT COUNT(*) FROM workspace_allocations), (SELECT COUNT(*) FROM workspaces), (SELECT COUNT(*) FROM audit_log)")
        .fetch_one(test_hooks::pool(store)).await.unwrap()
}

#[tokio::test]
async fn getter_decodes_all_phases_and_both_identity_families_read_only() {
    let store = Store::open_memory().await.unwrap();
    let cases = [
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "reserved",
            "unix",
            None,
            None,
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "materialized",
            "windows",
            Some("unix"),
            None,
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "committed",
            "unix",
            Some("windows"),
            None,
        ),
        (
            "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X8",
            "retained",
            "windows",
            None,
            Some("failed"),
        ),
    ];
    for (id, phase, parent, root, reason) in cases {
        insert(&store, id, phase, parent, root, reason).await;
    }
    let before = counts(&store).await;
    for (id, phase, parent, root, _reason) in cases {
        let record = store.get_workspace_allocation(&aid(id)).await.unwrap();
        assert_eq!(
            record.phase(),
            match phase {
                "reserved" => AllocationPhase::Reserved,
                "materialized" => AllocationPhase::Materialized,
                "committed" => AllocationPhase::Committed,
                _ => AllocationPhase::Retained,
            }
        );
        assert_eq!(
            record.parent_identity(),
            if parent == "unix" {
                RootIdentity::unix(&[1; 8], &[2; 8]).unwrap()
            } else {
                RootIdentity::windows(&[3; 8], &[4; 16]).unwrap()
            }
        );
        assert_eq!(record.root_identity().is_some(), root.is_some());
    }
    assert_eq!(before, counts(&store).await);
}

#[tokio::test]
async fn getter_rejects_representative_sqlite_corruption() {
    let updates = [
        "UPDATE workspace_allocations SET root_generation = CAST(42 AS BLOB)",
        "UPDATE workspace_allocations SET parent_unix_device = 'wrong'",
        "UPDATE workspace_allocations SET parent_unix_inode = NULL",
        "UPDATE workspace_allocations SET root_identity_kind = 'windows', root_windows_volume = X'01', root_windows_file_id = X'02'",
        "UPDATE workspace_allocations SET created_at = '2026-09-19T07:00:00.000+07:00'",
        "UPDATE workspace_allocations SET relative_name = 'a/b'",
        "UPDATE workspace_allocations SET phase = 'materialized'",
        "UPDATE workspace_allocations SET phase = 'retained', failure_reason = NULL",
    ];
    for update in updates {
        let store = Store::open_memory().await.unwrap();
        seed(&store).await;
        tamper(&store, update).await;
        let result = store.get_workspace_allocation(&aid(AID)).await;
        assert!(matches!(
            result,
            Err(WorkspaceStoreError::CorruptRow {
                table: "workspace_allocations",
                ..
            })
        ));
    }
}

#[tokio::test]
async fn getter_has_typed_not_found_and_database_errors() {
    let store = Store::open_memory().await.unwrap();
    assert!(matches!(
        store.get_workspace_allocation(&aid(AID)).await,
        Err(WorkspaceStoreError::NotFound)
    ));
    sqlx::query("DROP TABLE workspace_allocations")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    assert!(matches!(
        store.get_workspace_allocation(&aid(AID)).await,
        Err(WorkspaceStoreError::Database)
    ));
}
