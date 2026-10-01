#![allow(clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceConflict, WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError,
    WorkspaceTtl, test_hooks,
};
use tempfile::TempDir;
use tokio::time::{Duration, timeout};

const NOW: &str = "2026-09-19T00:00:00.000Z";
const OWNER: &str = "orchestrator-1";

fn input(id: &str, root: &str, identity: RootIdentity) -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(id).unwrap(),
        TrustedRootRegistration::new(root.into(), "generation-1".into(), identity).unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse(OWNER.into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

fn owner() -> LifecycleOwnerRef {
    LifecycleOwnerRef::parse(OWNER.into()).unwrap()
}

fn now() -> WorkspaceInstant {
    WorkspaceInstant::parse(NOW).unwrap()
}

async fn insert_allocation(
    store: &Store,
    allocation_id: &str,
    workspace_id: &str,
    phase: &str,
    root: Option<RootIdentity>,
    failure_reason: Option<&str>,
) {
    let (root_kind, root_unix_device, root_unix_inode, root_windows_volume, root_windows_file_id) =
        match root {
            None => (None, None, None, None, None),
            Some(RootIdentity::Unix { device, inode }) => (
                Some("unix"),
                Some(device.to_vec()),
                Some(inode.to_vec()),
                None,
                None,
            ),
            Some(RootIdentity::Windows {
                volume_serial,
                file_id,
            }) => (
                Some("windows"),
                None,
                None,
                Some(volume_serial.to_vec()),
                Some(file_id.to_vec()),
            ),
        };
    sqlx::query(
        "INSERT INTO workspace_allocations
         (allocation_id, workspace_id, root_generation, relative_name,
          parent_identity_kind, parent_unix_device, parent_unix_inode,
          root_identity_kind, root_unix_device, root_unix_inode,
          root_windows_volume, root_windows_file_id, phase, created_at,
          failure_reason)
         VALUES (?, ?, 'generation-1', ?, 'unix', X'0101010101010101',
                 X'0202020202020202', ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(allocation_id)
    .bind(workspace_id)
    .bind(format!("root-{allocation_id}"))
    .bind(root_kind)
    .bind(root_unix_device)
    .bind(root_unix_inode)
    .bind(root_windows_volume)
    .bind(root_windows_file_id)
    .bind(phase)
    .bind(NOW)
    .bind(failure_reason)
    .execute(test_hooks::pool(store))
    .await
    .unwrap();
}

async fn allocation_count(store: &Store) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM workspace_allocations")
        .fetch_one(test_hooks::pool(store))
        .await
        .unwrap()
}

#[tokio::test]
async fn reserved_allocation_with_same_workspace_id_is_compatible() {
    let store = Store::open_memory().await.unwrap();
    let workspace = input(
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
        "/reserved-compatible",
        RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
    );
    insert_allocation(
        &store,
        "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
        workspace.id().as_str(),
        "reserved",
        None,
        None,
    )
    .await;

    assert!(
        store
            .create_workspace(&workspace, &owner(), &now())
            .await
            .is_ok()
    );
    assert_eq!(allocation_count(&store).await, 1);
}

#[tokio::test]
async fn materialized_committed_and_retained_allocations_block_identifier() {
    for (index, phase, failure_reason) in [
        ("6", "materialized", None),
        ("7", "committed", None),
        ("8", "retained", Some("io_error")),
    ] {
        let store = Store::open_memory().await.unwrap();
        let id = format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{index}");
        let allocation_id = format!("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X{index}");
        let workspace = input(
            &id,
            &format!("/identifier-{phase}"),
            RootIdentity::unix(&[11; 8], &[12; 8]).unwrap(),
        );
        insert_allocation(
            &store,
            &allocation_id,
            &id,
            phase,
            Some(RootIdentity::unix(&[11; 8], &[12; 8]).unwrap()),
            failure_reason,
        )
        .await;

        assert_eq!(
            store.create_workspace(&workspace, &owner(), &now()).await,
            Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))
        );
        assert_eq!(allocation_count(&store).await, 1);
    }
}

#[tokio::test]
async fn materialized_and_retained_allocations_block_unix_and_windows_root_identity() {
    for (index, phase, failure_reason, identity) in [
        (
            "9",
            "materialized",
            None,
            RootIdentity::unix(&[21; 8], &[22; 8]).unwrap(),
        ),
        (
            "A",
            "retained",
            Some("io_error"),
            RootIdentity::windows(&[23; 8], &[24; 16]).unwrap(),
        ),
    ] {
        let store = Store::open_memory().await.unwrap();
        let allocation_id = format!("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X{index}");
        let workspace_id = format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{index}");
        let workspace = input(&workspace_id, &format!("/identity-{phase}"), identity);
        insert_allocation(
            &store,
            &allocation_id,
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XY",
            phase,
            Some(identity),
            failure_reason,
        )
        .await;

        assert_eq!(
            store.create_workspace(&workspace, &owner(), &now()).await,
            Err(WorkspaceStoreError::Conflict(
                WorkspaceConflict::RootIdentity
            ))
        );
        assert_eq!(allocation_count(&store).await, 1);
    }
}

#[tokio::test]
async fn identifier_wins_when_allocation_matches_id_and_root_identity() {
    let store = Store::open_memory().await.unwrap();
    let identity = RootIdentity::windows(&[31; 8], &[32; 16]).unwrap();
    let workspace = input(
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XB",
        "/identifier-priority",
        identity,
    );
    insert_allocation(
        &store,
        "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XB",
        workspace.id().as_str(),
        "committed",
        Some(identity),
        None,
    )
    .await;

    assert_eq!(
        store.create_workspace(&workspace, &owner(), &now()).await,
        Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))
    );
}

#[tokio::test]
async fn post_insert_allocation_trigger_conflict_rolls_back() {
    for (name, suffix, expected) in [
        ("id", "5", WorkspaceConflict::Identifier),
        ("root", "6", WorkspaceConflict::RootIdentity),
    ] {
        let store = Store::open_memory().await.unwrap();
        let identity = if name == "id" {
            RootIdentity::unix(&[41; 8], &[42; 8]).unwrap()
        } else {
            RootIdentity::windows(&[43; 8], &[44; 16]).unwrap()
        };
        let workspace_id = format!("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X{suffix}");
        let workspace = input(&workspace_id, &format!("/trigger-{name}"), identity);
        let allocation_workspace = if name == "id" {
            workspace.id().as_str().to_owned()
        } else {
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4XC".to_owned()
        };
        let root_columns = if name == "id" {
            "'unix', X'2929292929292929', X'2A2A2A2A2A2A2A2A', NULL, NULL"
        } else {
            "'windows', NULL, NULL, X'2B2B2B2B2B2B2B2B', X'2C2C2C2C2C2C2C2C2C2C2C2C2C2C2C2C'"
        };
        let trigger = format!(
            "CREATE TRIGGER allocation_guard_{name} AFTER INSERT ON workspaces
             BEGIN INSERT INTO workspace_allocations
               (allocation_id, workspace_id, root_generation, relative_name,
                parent_identity_kind, parent_unix_device, parent_unix_inode,
                root_identity_kind, root_unix_device, root_unix_inode,
                root_windows_volume, root_windows_file_id, phase, created_at)
             VALUES ('wa_01J5M4Q2Y7N8P9R0S1T2V3W4X{suffix}', '{allocation_workspace}', 'generation-1',
                     'trigger-{name}', 'unix', X'0101010101010101',
                     X'0202020202020202', {root_columns}, 'materialized', '{NOW}'); END",
            allocation_workspace = allocation_workspace,
            root_columns = root_columns,
        );
        sqlx::query(&trigger)
            .execute(test_hooks::pool(&store))
            .await
            .unwrap();

        assert_eq!(
            store.create_workspace(&workspace, &owner(), &now()).await,
            Err(WorkspaceStoreError::Conflict(expected))
        );
        assert_eq!(allocation_count(&store).await, 0);
    }
}

#[tokio::test]
async fn wal_cross_table_commit_beats_contending_create_with_identifier_priority() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("workspace-race.sqlite");
    let creator = Store::open(&path).await.unwrap();
    let allocator = Store::open(&path).await.unwrap();
    let workspace = input(
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X9",
        "/wal-cross-table",
        RootIdentity::unix(&[51; 8], &[52; 8]).unwrap(),
    );
    let allocation_id = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4XA";

    // Keep the write lock while the materialized allocation is inserted. The
    // creator must contend on BEGIN IMMEDIATE and only proceed after this
    // connection commits the cross-table owner.
    let mut allocation_connection = test_hooks::pool(&allocator).acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *allocation_connection)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO workspace_allocations
         (allocation_id, workspace_id, root_generation, relative_name,
          parent_identity_kind, parent_unix_device, parent_unix_inode,
          root_identity_kind, root_unix_device, root_unix_inode,
          root_windows_volume, root_windows_file_id, phase, created_at,
          failure_reason)
         VALUES (?, ?, 'generation-1', 'wal-cross-table', 'unix',
                 X'0101010101010101', X'0202020202020202', 'unix',
                 X'3333333333333333', X'3434343434343434', NULL, NULL,
                 'materialized', ?, NULL)",
    )
    .bind(allocation_id)
    .bind(workspace.id().as_str())
    .bind(NOW)
    .execute(&mut *allocation_connection)
    .await
    .unwrap();

    let workspace_id = workspace.id().clone();
    let mut creator_task =
        tokio::spawn(async move { creator.create_workspace(&workspace, &owner(), &now()).await });
    assert!(
        timeout(Duration::from_millis(100), &mut creator_task)
            .await
            .is_err(),
        "create_workspace must wait on the allocator's WAL write lock"
    );

    sqlx::query("COMMIT")
        .execute(&mut *allocation_connection)
        .await
        .unwrap();
    drop(allocation_connection);

    let result = timeout(Duration::from_secs(5), creator_task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result,
        Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))
    );

    let observer = Store::open(&path).await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspaces")
            .fetch_one(test_hooks::pool(&observer))
            .await
            .unwrap(),
        0,
        "the rejected create must not persist a workspace"
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM workspace_allocations
             WHERE allocation_id = ? AND phase = 'materialized'",
        )
        .bind(allocation_id)
        .fetch_one(test_hooks::pool(&observer))
        .await
        .unwrap(),
        1,
        "the committed allocation must remain durable"
    );
    assert_eq!(
        observer.get_workspace(&workspace_id).await,
        Err(WorkspaceStoreError::NotFound)
    );
}
