#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceTtl, test_hooks,
};
use tokio::task::yield_now;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/lifecycle/cancel".into(),
            "generation-cancel".into(),
            RootIdentity::unix(&[7; 8], &[8; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner-cancel".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

#[tokio::test]
async fn cancellation_while_waiting_for_begin_immediate_rolls_back_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("cancel.sqlite"))
        .await
        .unwrap();
    let now = WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap();
    let owner = LifecycleOwnerRef::parse("owner-cancel".into()).unwrap();
    store
        .create_workspace(&input(), &owner, &now)
        .await
        .unwrap();

    let blocker = test_hooks::pool(&store)
        .begin_with("BEGIN IMMEDIATE")
        .await
        .unwrap();
    let task = tokio::spawn({
        let store = store.clone();
        async move {
            store
                .expire_workspace(
                    &WorkspaceId::parse(ID).unwrap(),
                    &WorkspaceInstant::parse("2026-09-19T00:01:00.000Z").unwrap(),
                )
                .await
        }
    });
    yield_now().await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    blocker.rollback().await.unwrap();

    let workspace = store
        .get_workspace(&WorkspaceId::parse(ID).unwrap())
        .await
        .unwrap();
    assert_eq!(workspace.state, "active");
    assert_eq!(workspace.revision, 1);
    assert!(store.list_audit().await.unwrap().is_empty());
}
