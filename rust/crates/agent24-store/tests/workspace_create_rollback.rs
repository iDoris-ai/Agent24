#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceConflict, WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError,
    WorkspaceTtl, test_hooks,
};
use sqlx::Row;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const NOW: &str = "2026-09-19T00:00:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/rollback/root".into(),
            "generation-1".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), Some("project".into()), None).unwrap(),
        LifecycleOwnerRef::parse("owner-a".into()).unwrap(),
        WorkspaceTtl::new(1_000).unwrap(),
    )
}

async fn create(store: &Store) -> Result<agent24_protocol::Workspace, WorkspaceStoreError> {
    let now = WorkspaceInstant::parse(NOW).unwrap();
    let owner = LifecycleOwnerRef::parse("owner-a".into()).unwrap();
    store.create_workspace(&input(), &owner, &now).await
}

#[tokio::test]
async fn corrupt_reselect_rolls_back_and_connection_recovers() {
    let store = Store::open_memory().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER tamper_workspace AFTER INSERT ON workspaces
         BEGIN UPDATE workspaces SET provenance_project_ref = CAST('bad' AS BLOB)
         WHERE id = NEW.id; END",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();

    assert_eq!(
        create(&store).await,
        Err(WorkspaceStoreError::CorruptRow {
            table: "workspaces",
            field: "provenance_project_ref"
        })
    );
    let count = sqlx::query("SELECT COUNT(*) AS count FROM workspaces")
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap()
        .get::<i64, _>("count");
    assert_eq!(count, 0, "decode failure must roll back the insert");

    sqlx::query("DROP TRIGGER tamper_workspace")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let workspace = create(&store).await.unwrap();
    assert_eq!(workspace.id.as_str(), ID);
}

#[tokio::test]
async fn sqlite_failures_are_database_and_redacted_then_recover() {
    let store = Store::open_memory().await.unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_workspace BEFORE INSERT ON workspaces
         BEGIN SELECT RAISE(ABORT, 'secret sqlite trigger detail'); END",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();

    let error = create(&store).await.unwrap_err();
    assert_eq!(error, WorkspaceStoreError::Database);
    assert_eq!(error.to_string(), "workspace database error");
    assert!(!error.to_string().contains("secret sqlite trigger detail"));
    assert_eq!(
        sqlx::query("SELECT COUNT(*) AS count FROM workspaces")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap()
            .get::<i64, _>("count"),
        0
    );

    sqlx::query("DROP TRIGGER reject_workspace")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    assert!(create(&store).await.is_ok());
}

#[tokio::test]
async fn deferred_commit_failure_rolls_back_workspace_and_side_row() {
    let store = Store::open_memory().await.unwrap();
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER defer_workspace_lease_fk AFTER INSERT ON workspaces
         BEGIN
             INSERT INTO workspace_leases
                 (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
             VALUES
                 ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6',
                  'ws_missing_for_deferred_fk', NEW.root_generation,
                  'run-owner', 'run', NEW.created_at);
         END",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();

    assert_eq!(create(&store).await, Err(WorkspaceStoreError::Database));
    let counts = sqlx::query(
        "SELECT (SELECT COUNT(*) FROM workspaces) AS workspaces,
                (SELECT COUNT(*) FROM workspace_leases) AS side_rows",
    )
    .fetch_one(test_hooks::pool(&store))
    .await
    .unwrap();
    assert_eq!(counts.get::<i64, _>("workspaces"), 0);
    assert_eq!(counts.get::<i64, _>("side_rows"), 0);

    sqlx::query("DROP TRIGGER defer_workspace_lease_fk")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    sqlx::query("PRAGMA defer_foreign_keys = OFF")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    assert!(create(&store).await.is_ok());
}

#[tokio::test]
async fn preflight_conflict_does_not_depend_on_sqlite_error_text() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await.unwrap();
    let error = create(&store).await.unwrap_err();
    assert_eq!(
        error,
        WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier)
    );
    assert_eq!(error.to_string(), "workspace conflict: identifier");
}
