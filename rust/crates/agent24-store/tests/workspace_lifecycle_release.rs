#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl, test_hooks,
};
use sqlx::Row;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/secret/release-root".into(),
            "secret-release-generation".into(),
            RootIdentity::unix(&[9; 8], &[10; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), Some("release-project".into()), None).unwrap(),
        LifecycleOwnerRef::parse("owner-release".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

async fn create(store: &Store) -> agent24_protocol::Workspace {
    store
        .create_workspace(
            &input(),
            &LifecycleOwnerRef::parse("owner-release".into()).unwrap(),
            &WorkspaceInstant::parse(CREATED).unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn release_requires_exact_owner_and_audits_a_redacted_transition() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let now = WorkspaceInstant::parse("2026-09-19T00:00:10.000Z").unwrap();
    let wrong = LifecycleOwnerRef::parse("owner-other".into()).unwrap();
    let error = store
        .release_workspace(&id, &wrong, &now)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WorkspaceStoreError::InvalidValue {
            field: "lifecycle_owner_ref"
        }
    );
    assert_eq!(
        error.to_string(),
        "invalid workspace value: lifecycle_owner_ref"
    );
    assert!(store.list_audit().await.unwrap().is_empty());

    let owner = LifecycleOwnerRef::parse("owner-release".into()).unwrap();
    let releasing = store.release_workspace(&id, &owner, &now).await.unwrap();
    assert_eq!(releasing.state, "releasing");
    assert_eq!(releasing.revision, 2);
    let entries = store.list_audit().await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].action, "workspace.release_requested");
    assert_eq!(
        entries[0].detail,
        serde_json::json!({
            "id": ID,
            "kind": "orchestrator_scratch",
            "result_state": "releasing",
            "owner_ref": "owner-release",
            "reason": "owner_requested"
        })
    );
    let detail = serde_json::to_string(&entries[0].detail).unwrap();
    assert!(!detail.contains("secret-release-generation"));
    assert!(!detail.contains("/secret/release-root"));
    assert!(!detail.contains("release-project"));
}

#[tokio::test]
async fn release_expired_workspace_and_repeat_are_safe() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner-release".into()).unwrap();
    let expiry = WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap();
    let expired = store.expire_workspace(&id, &expiry).await.unwrap();
    assert_eq!(expired.state, "expired");
    let releasing = store.release_workspace(&id, &owner, &expiry).await.unwrap();
    assert_eq!(releasing.state, "releasing");
    assert_eq!(releasing.revision, 3);

    let other = LifecycleOwnerRef::parse("owner-other".into()).unwrap();
    let error = store
        .release_workspace(&id, &other, &expiry)
        .await
        .unwrap_err();
    assert_eq!(
        error,
        WorkspaceStoreError::InvalidValue {
            field: "lifecycle_owner_ref"
        }
    );
    let unchanged = store.release_workspace(&id, &owner, &expiry).await.unwrap();
    assert_eq!(unchanged, releasing);
    assert_eq!(store.list_audit().await.unwrap().len(), 2);
    let row = sqlx::query("SELECT state, revision FROM workspaces WHERE id = ?")
        .bind(ID)
        .fetch_one(test_hooks::pool(&store))
        .await
        .unwrap();
    assert_eq!(row.get::<String, _>("state"), "releasing");
    assert_eq!(row.get::<i64, _>("revision"), 3);
}

#[tokio::test]
async fn release_checks_owner_for_releasing_cleanup_failed_and_released() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let wrong = LifecycleOwnerRef::parse("owner-other".into()).unwrap();
    let now = WorkspaceInstant::parse(CREATED).unwrap();

    for state_update in [
        "UPDATE workspaces SET state = 'releasing' WHERE id = ?",
        "UPDATE workspaces SET state = 'cleanup_failed', cleanup_attempts = 1,
             cleanup_last_attempt_at = ?, cleanup_error = 'busy', cleanup_retry_at = ?
         WHERE id = ?",
        "UPDATE workspaces SET state = 'released', released_at = ?,
             quarantine_root = '/quarantine', quarantined_at = ?,
             cleanup_attempts = 0, cleanup_last_attempt_at = NULL,
             cleanup_error = NULL, cleanup_retry_at = NULL WHERE id = ?",
    ] {
        if state_update.contains("cleanup_failed") {
            sqlx::query(state_update)
                .bind(CREATED)
                .bind("2026-09-19T00:00:01.000Z")
                .bind(ID)
                .execute(test_hooks::pool(&store))
                .await
                .unwrap();
        } else if state_update.contains("released") {
            sqlx::query(state_update)
                .bind(CREATED)
                .bind(CREATED)
                .bind(ID)
                .execute(test_hooks::pool(&store))
                .await
                .unwrap();
        } else {
            sqlx::query(state_update)
                .bind(ID)
                .execute(test_hooks::pool(&store))
                .await
                .unwrap();
        }
        let error = store
            .release_workspace(&id, &wrong, &now)
            .await
            .unwrap_err();
        assert_eq!(
            error,
            WorkspaceStoreError::InvalidValue {
                field: "lifecycle_owner_ref"
            }
        );
    }
    assert!(store.list_audit().await.unwrap().is_empty());
}
