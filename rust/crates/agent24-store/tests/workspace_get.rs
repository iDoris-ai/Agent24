#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError, WorkspaceTtl,
};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const NOW: &str = "2026-09-19T00:00:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/private/get-root".into(),
            "generation-get".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), Some("project".into()), Some("base".into()))
            .unwrap(),
        LifecycleOwnerRef::parse("owner-get".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

async fn create(store: &Store) -> agent24_protocol::Workspace {
    store
        .create_workspace(
            &input(),
            &LifecycleOwnerRef::parse("owner-get".into()).unwrap(),
            &WorkspaceInstant::parse(NOW).unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn get_returns_existing_workspace_with_redacted_projection() {
    let store = Store::open_memory().await.unwrap();
    let created = create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();

    let got = store.get_workspace(&id).await.unwrap();
    assert_eq!(got, created);
    let wire = serde_json::to_string(&got).unwrap();
    for secret in ["/private/get-root", "generation-get"] {
        assert!(!wire.contains(secret), "GET projection leaked {secret}");
    }
}

#[tokio::test]
async fn get_missing_is_static_not_found() {
    let store = Store::open_memory().await.unwrap();
    let id = WorkspaceId::parse(ID).unwrap();

    assert_eq!(
        store.get_workspace(&id).await,
        Err(WorkspaceStoreError::NotFound)
    );
    assert_eq!(
        WorkspaceStoreError::NotFound.to_string(),
        "workspace not found"
    );
}
