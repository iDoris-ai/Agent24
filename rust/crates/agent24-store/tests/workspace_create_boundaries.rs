#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl, test_hooks,
};

const NOW: &str = "2026-09-19T00:00:00.000Z";

fn input(identity: RootIdentity, ttl: WorkspaceTtl) -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
        TrustedRootRegistration::new(
            "/workspace/boundary".into(),
            "generation-1".into(),
            identity,
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner-a".into()).unwrap(),
        ttl,
    )
}

#[tokio::test]
async fn owner_is_exact_and_mismatch_is_a_static_invalid_value() {
    let store = Store::open_memory().await.unwrap();
    let input = input(
        RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        WorkspaceTtl::new(1_000).unwrap(),
    );
    let now = WorkspaceInstant::parse(NOW).unwrap();
    let authorized = LifecycleOwnerRef::parse(" owner-a ".into()).unwrap();
    assert_eq!(
        store.create_workspace(&input, &authorized, &now).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "lifecycle_owner_ref"
        })
    );
}

#[tokio::test]
async fn injected_now_controls_expiry_and_checked_overflow_is_rejected() {
    let store = Store::open_memory().await.unwrap();
    let owner = LifecycleOwnerRef::parse("owner-a".into()).unwrap();
    let now = WorkspaceInstant::parse(NOW).unwrap();
    let created = input(
        RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        WorkspaceTtl::new(1).unwrap(),
    );
    let workspace = store
        .create_workspace(&created, &owner, &now)
        .await
        .unwrap();
    assert_eq!(workspace.created_at, NOW);
    assert_eq!(workspace.expires_at, "2026-09-19T00:00:00.001Z");

    let near_max = WorkspaceInstant::parse("9999-12-31T23:59:59.999Z").unwrap();
    let overflow = input(
        RootIdentity::unix(&[5; 8], &[6; 8]).unwrap(),
        WorkspaceTtl::new(1).unwrap(),
    );
    assert_eq!(
        store.create_workspace(&overflow, &owner, &near_max).await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "expires_at"
        })
    );
}

#[tokio::test]
async fn windows_identity_is_persisted_as_one_identity_family() {
    let store = Store::open_memory().await.unwrap();
    let owner = LifecycleOwnerRef::parse("owner-a".into()).unwrap();
    let now = WorkspaceInstant::parse(NOW).unwrap();
    let workspace = input(
        RootIdentity::windows(&[7; 8], &[8; 16]).unwrap(),
        WorkspaceTtl::new(1_000).unwrap(),
    );
    let result = store
        .create_workspace(&workspace, &owner, &now)
        .await
        .unwrap();
    assert_eq!(result.expires_at, "2026-09-19T00:00:01.000Z");
    let row = sqlx::query(
        "SELECT root_identity_kind, unix_device, unix_inode,
                windows_volume_serial, windows_file_id
         FROM workspaces WHERE id = ?",
    )
    .bind(workspace.id().as_str())
    .fetch_one(test_hooks::pool(&store))
    .await
    .unwrap();
    use sqlx::Row;
    assert_eq!(row.get::<String, _>("root_identity_kind"), "windows");
    assert!(row.get::<Option<Vec<u8>>, _>("unix_device").is_none());
    assert!(row.get::<Option<Vec<u8>>, _>("unix_inode").is_none());
    assert_eq!(row.get::<Vec<u8>, _>("windows_volume_serial"), vec![7; 8]);
    assert_eq!(row.get::<Vec<u8>, _>("windows_file_id"), vec![8; 16]);
}
