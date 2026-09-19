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
            "/lifecycle/hardening".into(),
            "generation-hardening".into(),
            RootIdentity::unix(&[5; 8], &[6; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner-hardening".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

async fn create(store: &Store) {
    store
        .create_workspace(
            &input(),
            &LifecycleOwnerRef::parse("owner-hardening".into()).unwrap(),
            &WorkspaceInstant::parse(CREATED).unwrap(),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn legacy_kind_is_rejected_without_mutation() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    sqlx::query("UPDATE workspaces SET kind = 'legacy_compat' WHERE id = ?")
        .bind(ID)
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
    assert_eq!(
        error,
        WorkspaceStoreError::InvalidValue {
            field: "workspace_kind"
        }
    );
    let row = sqlx::query("SELECT state, revision FROM workspaces WHERE id = ?")
        .bind(ID)
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "active");
    assert_eq!(row.get::<i64, _>("revision"), 1);
    assert!(store.list_audit().await.unwrap().is_empty());
}

#[tokio::test]
async fn post_update_tamper_is_database_and_rolls_back() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    sqlx::query(
        "CREATE TRIGGER tamper_lifecycle AFTER UPDATE OF state ON workspaces
         BEGIN UPDATE workspaces SET revision = revision + 1 WHERE id = NEW.id; END",
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
    assert!(store.list_audit().await.unwrap().is_empty());
}

#[tokio::test]
async fn audit_failure_rolls_back_transition_and_leases_are_untouched() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    sqlx::query(
        "INSERT INTO workspace_leases
         (lease_id, workspace_id, root_generation, owner_id, kind, acquired_at)
         VALUES ('wl_01J5M4Q2Y7N8P9R0S1T2V3W4X5', ?, 'generation-hardening',
                 'run-owner', 'run', ?)",
    )
    .bind(ID)
    .bind(CREATED)
    .execute(test_hooks::pool(&store))
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_lifecycle_audit BEFORE INSERT ON audit_log
         BEGIN SELECT RAISE(ABORT, 'secret audit failure'); END",
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
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM audit_log")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap(),
        0
    );
    sqlx::query("DROP TRIGGER reject_lifecycle_audit")
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
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM workspace_leases")
            .fetch_one(test_hooks::pool(&store))
            .await
            .unwrap(),
        1
    );
}
