#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl, test_hooks,
};
use sqlx::Row;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-19T00:01:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/lifecycle/wal".into(),
            "generation-wal".into(),
            RootIdentity::unix(&[11; 8], &[12; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner-wal".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

async fn create(store: &Store) {
    store
        .create_workspace(
            &input(),
            &LifecycleOwnerRef::parse("owner-wal".into()).unwrap(),
            &WorkspaceInstant::parse(CREATED).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn wal_expire_and_release_serialize_without_lost_audit() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("wal.sqlite")).await.unwrap();
    create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner-wal".into()).unwrap();
    let now = WorkspaceInstant::parse(EXPIRES).unwrap();

    let (expired, released) = tokio::join!(
        store.expire_workspace(&id, &now),
        store.release_workspace(&id, &owner, &now),
    );
    assert!(expired.is_ok() || released.is_ok());
    let final_workspace = store.get_workspace(&id).await.unwrap();
    assert_eq!(final_workspace.state, "releasing");
    assert!(matches!(final_workspace.revision, 2 | 3));
    let audit = store.list_audit().await.unwrap();
    assert!(matches!(audit.len(), 1 | 2));
    store.verify_audit_chain().await.unwrap();
    if audit.len() == 2 {
        assert_eq!(audit[0].action, "workspace.expired");
        assert_eq!(audit[1].action, "workspace.release_requested");
    } else {
        assert_eq!(audit[0].action, "workspace.release_requested");
    }
}

#[tokio::test]
async fn deferred_commit_failure_rolls_back_workspace_and_audit() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    sqlx::query("PRAGMA defer_foreign_keys = ON")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER defer_lifecycle_lease AFTER UPDATE OF state ON workspaces
         BEGIN
             INSERT INTO workspace_leases
                 (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
             VALUES
                 ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X6', 'ws_missing_for_lifecycle',
                  NEW.root_generation, 'run-owner', 'run', NEW.created_at);
         END",
    )
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();

    let error = store
        .expire_workspace(
            &WorkspaceId::parse(ID).unwrap(),
            &WorkspaceInstant::parse(EXPIRES).unwrap(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, WorkspaceStoreError::Database);
    let row = sqlx::query("SELECT state, revision FROM workspaces WHERE id = ?")
        .bind(ID)
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "active");
    assert_eq!(row.get::<i64, _>("revision"), 1);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap(),
        0
    );

    sqlx::query("DROP TRIGGER defer_lifecycle_lease")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    sqlx::query("PRAGMA defer_foreign_keys = OFF")
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();
    let expired = store
        .expire_workspace(
            &WorkspaceId::parse(ID).unwrap(),
            &WorkspaceInstant::parse(EXPIRES).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.state, "expired");
    assert_eq!(store.list_audit().await.unwrap().len(), 1);
}
