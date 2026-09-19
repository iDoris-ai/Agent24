#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceConflict, WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError,
    WorkspaceTtl,
};

const NOW: &str = "2026-09-19T00:00:00.000Z";

fn input(id: &str, root: &str, identity: RootIdentity) -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(id).unwrap(),
        TrustedRootRegistration::new(root.into(), "generation-1".into(), identity).unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse("owner-a".into()).unwrap(),
        WorkspaceTtl::new(1_000).unwrap(),
    )
}

async fn race(
    store: Store,
    first: NewScratchWorkspace,
    second: NewScratchWorkspace,
) -> [Result<(), WorkspaceStoreError>; 2] {
    let now = WorkspaceInstant::parse(NOW).unwrap();
    let owner = LifecycleOwnerRef::parse("owner-a".into()).unwrap();
    let (left, right) = tokio::join!(
        store.create_workspace(&first, &owner, &now),
        store.create_workspace(&second, &owner, &now),
    );
    [left.map(|_| ()), right.map(|_| ())]
}

fn one_winner(results: [Result<(), WorkspaceStoreError>; 2], conflict: WorkspaceConflict) {
    assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == Err(WorkspaceStoreError::Conflict(conflict)))
            .count(),
        1
    );
}

#[tokio::test]
async fn file_backed_wal_race_allows_only_one_identifier() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("same-id.sqlite"))
        .await
        .unwrap();
    let results = race(
        store,
        input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/wal/id-left",
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        ),
        input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/wal/id-right",
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        ),
    )
    .await;
    one_winner(results, WorkspaceConflict::Identifier);
}

#[tokio::test]
async fn file_backed_wal_race_reserves_canonical_root_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("same-root.sqlite"))
        .await
        .unwrap();
    let results = race(
        store,
        input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/wal/shared-root",
            RootIdentity::unix(&[5; 8], &[6; 8]).unwrap(),
        ),
        input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "/wal/shared-root",
            RootIdentity::unix(&[7; 8], &[8; 8]).unwrap(),
        ),
    )
    .await;
    one_winner(results, WorkspaceConflict::CanonicalRoot);
}

#[tokio::test]
async fn file_backed_wal_race_reserves_identity_variant_exactly_once() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("same-identity.sqlite"))
        .await
        .unwrap();
    let results = race(
        store,
        input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/wal/identity-left",
            RootIdentity::windows(&[9; 8], &[10; 16]).unwrap(),
        ),
        input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "/wal/identity-right",
            RootIdentity::windows(&[9; 8], &[10; 16]).unwrap(),
        ),
    )
    .await;
    one_winner(results, WorkspaceConflict::RootIdentity);
}
