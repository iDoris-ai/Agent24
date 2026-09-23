#![allow(clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    AllocationId, AllocationIntent, LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store,
    TrustedRootRegistration, WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceTtl,
};

const CREATED: &str = "2026-09-19T00:00:00.000Z";

fn inputs() -> (
    AllocationIntent,
    NewScratchWorkspace,
    LifecycleOwnerRef,
    RootIdentity,
) {
    let parent = RootIdentity::unix(&[1; 8], &[2; 8]).unwrap();
    let root = RootIdentity::unix(&[3; 8], &[4; 8]).unwrap();
    let intent = AllocationIntent::new(
        AllocationId::parse("wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
        WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
        "generation-1".into(),
        "scratch".into(),
        parent,
        WorkspaceInstant::parse(CREATED).unwrap(),
    )
    .unwrap();
    let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap();
    let input = NewScratchWorkspace::new(
        intent.workspace_id().clone(),
        TrustedRootRegistration::new(
            "/private/registration-root".into(),
            "generation-1".into(),
            root,
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("test".into(), None, None).unwrap(),
        owner.clone(),
        WorkspaceTtl::new(60_000).unwrap(),
    );
    (intent, input, owner, root)
}

#[tokio::test]
async fn public_registration_returns_a_redacted_committed_snapshot_on_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registration.sqlite");
    let store = Store::open(&path).await.unwrap();
    let (intent, input, owner, root) = inputs();
    store.reserve_workspace_allocation(&intent).await.unwrap();
    store
        .materialize_workspace_allocation(&intent, root)
        .await
        .unwrap();

    let first = store
        .register_workspace_allocation(
            &intent,
            &input,
            &owner,
            &WorkspaceInstant::parse("2026-09-19T00:00:01.000Z").unwrap(),
        )
        .await
        .unwrap();
    let wire = serde_json::to_string(&first).unwrap();
    for private in [
        "/private/registration-root",
        "generation-1",
        "root_identity",
        "cleanup_",
    ] {
        assert!(
            !wire.contains(private),
            "public registration leaked {private}"
        );
    }
    drop(store);

    let reopened = Store::open(&path).await.unwrap();
    let replay = reopened
        .register_workspace_allocation(
            &intent,
            &input,
            &owner,
            &WorkspaceInstant::parse("2026-09-20T00:00:00.000Z").unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(replay, first);
    let counts: (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM workspaces),
                (SELECT count(*) FROM audit_log WHERE action='workspace.allocation_committed')",
    )
    .fetch_one(agent24_store::test_hooks::pool(&reopened))
    .await
    .unwrap();
    assert_eq!(counts, (1, 1));
}
