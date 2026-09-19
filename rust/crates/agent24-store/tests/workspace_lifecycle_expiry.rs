#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceTtl,
};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";
const EXPIRES: &str = "2026-09-19T00:01:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        TrustedRootRegistration::new(
            "/lifecycle/expiry".into(),
            "generation-expiry".into(),
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        )
        .unwrap(),
        WorkspaceProvenanceInput::new("git".into(), Some("project".into()), None).unwrap(),
        LifecycleOwnerRef::parse("owner-expiry".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    )
}

async fn create(store: &Store) -> agent24_protocol::Workspace {
    let now = WorkspaceInstant::parse(CREATED).unwrap();
    let owner = LifecycleOwnerRef::parse("owner-expiry".into()).unwrap();
    store
        .create_workspace(&input(), &owner, &now)
        .await
        .unwrap()
}

#[tokio::test]
async fn expiry_requires_the_exact_boundary_and_is_idempotent() {
    let store = Store::open_memory().await.unwrap();
    create(&store).await;

    let before = WorkspaceInstant::parse("2026-09-19T00:00:59.999Z").unwrap();
    let active = store
        .expire_workspace(&WorkspaceId::parse(ID).unwrap(), &before)
        .await
        .unwrap();
    assert_eq!(active.state, "active");
    assert_eq!(active.revision, 1);
    assert!(store.list_audit().await.unwrap().is_empty());

    let at_boundary = WorkspaceInstant::parse(EXPIRES).unwrap();
    let expired = store
        .expire_workspace(&WorkspaceId::parse(ID).unwrap(), &at_boundary)
        .await
        .unwrap();
    assert_eq!(expired.state, "expired");
    assert_eq!(expired.revision, 2);
    let audit = store.list_audit().await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "workspace.expired");
    assert_eq!(audit[0].actor, "workspace_lifecycle");
    assert_eq!(
        audit[0].detail,
        serde_json::json!({
            "id": ID,
            "kind": "orchestrator_scratch",
            "result_state": "expired",
            "reason": "ttl"
        })
    );
    let again = store
        .expire_workspace(&WorkspaceId::parse(ID).unwrap(), &at_boundary)
        .await
        .unwrap();
    assert_eq!(again, expired);
    assert_eq!(store.list_audit().await.unwrap().len(), 1);
    store.verify_audit_chain().await.unwrap();
}

#[tokio::test]
async fn expiry_does_not_mutate_non_active_states() {
    let store = Store::open_memory().await.unwrap();
    let created = create(&store).await;
    let id = WorkspaceId::parse(ID).unwrap();
    let now = WorkspaceInstant::parse(EXPIRES).unwrap();
    let released = store
        .release_workspace(
            &id,
            &LifecycleOwnerRef::parse("owner-expiry".into()).unwrap(),
            &now,
        )
        .await
        .unwrap();
    assert_eq!(released.state, "releasing");
    assert_eq!(released.revision, created.revision + 1);
    let unchanged = store.expire_workspace(&id, &now).await.unwrap();
    assert_eq!(unchanged, released);
    assert_eq!(store.list_audit().await.unwrap().len(), 1);
}
