#![allow(clippy::expect_used, clippy::unwrap_used)]
use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl,
};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/private".into(),
            "generation".into(),
            RootIdentity::unix(&[21; 8], &[22; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}
async fn store() -> Store {
    let s = Store::open_memory().await.unwrap();
    s.create_workspace(
        &input(),
        &LifecycleOwnerRef::parse("owner".into()).unwrap(),
        &WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
    )
    .await
    .unwrap();
    s
}

#[tokio::test]
async fn renew_core_table_covers_extension_replay_invalid_and_lazy_expiry() {
    let id = WorkspaceId::parse(ID).unwrap();
    let owner = LifecycleOwnerRef::parse("owner".into()).unwrap();
    let now = WorkspaceInstant::parse("2026-09-19T00:00:30.000Z").unwrap();
    let ttl = WorkspaceTtl::new(120_000).unwrap();
    let s = store().await;
    let first = s.renew_workspace(&id, &owner, ttl, &now).await.unwrap();
    assert_eq!(
        (
            first.state.as_str(),
            first.expires_at.as_str(),
            first.revision
        ),
        ("active", "2026-09-19T00:02:30.000Z", 2)
    );
    assert_eq!(
        s.renew_workspace(&id, &owner, ttl, &now).await.unwrap(),
        first
    );
    assert_eq!(s.list_audit().await.unwrap().len(), 1);
    assert_eq!(
        s.renew_workspace(&id, &owner, WorkspaceTtl::new(119_000).unwrap(), &now)
            .await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "expires_at"
        })
    );
    let expired_store = store().await;
    assert_eq!(
        expired_store
            .renew_workspace(
                &id,
                &owner,
                ttl,
                &WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap(),
            )
            .await,
        Err(WorkspaceStoreError::InvalidValue {
            field: "workspace_state"
        })
    );
    let expired = expired_store.get_workspace(&id).await.unwrap();
    assert_eq!((expired.state.as_str(), expired.revision), ("expired", 2));
    assert_eq!(
        expired_store.list_audit().await.unwrap()[0].action,
        "workspace.expired"
    );
}
