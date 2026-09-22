#![allow(clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    AllocationId, AllocationIntent, AllocationPhase, RootIdentity, Store, WorkspaceConflict,
    WorkspaceInstant, WorkspaceStoreError,
};

const ALLOCATION_ID: &str = "wa_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const WORKSPACE_ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";

fn intent() -> AllocationIntent {
    AllocationIntent::new(
        AllocationId::parse(ALLOCATION_ID).unwrap(),
        WorkspaceId::parse(WORKSPACE_ID).unwrap(),
        "generation-1".to_owned(),
        "public-api".to_owned(),
        RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        WorkspaceInstant::parse("2026-09-19T00:00:00.000Z").unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn public_reservation_is_db_only_and_exact_replay_is_idempotently_rejected() {
    let store = Store::open_memory().await.unwrap();
    let input = intent();

    let record = store.reserve_workspace_allocation(&input).await.unwrap();
    assert_eq!(record.id().as_str(), ALLOCATION_ID);
    assert_eq!(record.workspace_id().as_str(), WORKSPACE_ID);
    assert_eq!(record.phase(), AllocationPhase::Reserved);
    assert!(record.root_identity().is_none());
    assert!(record.failure_reason().is_none());
    assert_eq!(
        store
            .get_workspace(&WorkspaceId::parse(WORKSPACE_ID).unwrap())
            .await
            .err(),
        Some(WorkspaceStoreError::NotFound)
    );

    let audit = store.list_audit().await.unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "workspace.allocation_reserved");

    assert_eq!(
        store.reserve_workspace_allocation(&input).await.err(),
        Some(WorkspaceStoreError::Conflict(
            WorkspaceConflict::AllocationIdentifier
        ))
    );
    assert_eq!(store.list_audit().await.unwrap().len(), 1);
}
