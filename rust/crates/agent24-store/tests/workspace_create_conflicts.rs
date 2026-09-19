#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceConflict, WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceStoreError,
    WorkspaceTtl, test_hooks,
};

const NOW: &str = "2026-09-19T00:00:00.000Z";
const OWNER: &str = "owner-a";

fn input(id: &str, root: &str, identity: RootIdentity) -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(id).unwrap(),
        TrustedRootRegistration::new(root.into(), "generation-1".into(), identity).unwrap(),
        WorkspaceProvenanceInput::new("git".into(), None, None).unwrap(),
        LifecycleOwnerRef::parse(OWNER.into()).unwrap(),
        WorkspaceTtl::new(1_000).unwrap(),
    )
}

async fn create(store: &Store, input: &NewScratchWorkspace) -> Result<(), WorkspaceStoreError> {
    let now = WorkspaceInstant::parse(NOW).unwrap();
    let owner = LifecycleOwnerRef::parse(OWNER.into()).unwrap();
    store
        .create_workspace(input, &owner, &now)
        .await
        .map(|_| ())
}

#[tokio::test]
async fn preflights_are_ordered_identifier_then_root_then_identity() {
    let store = Store::open_memory().await.unwrap();
    create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/existing/root",
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        ),
    )
    .await
    .unwrap();

    let same_id = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/other/root",
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        same_id,
        Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))
    );

    let same_root = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "/existing/root",
            RootIdentity::unix(&[3; 8], &[4; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        same_root,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::CanonicalRoot
        ))
    );

    let same_identity = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "/new/root",
            RootIdentity::unix(&[1; 8], &[2; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        same_identity,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::RootIdentity
        ))
    );
}

#[tokio::test]
async fn overlapping_conflicts_keep_identifier_before_root_and_identity() {
    let store = Store::open_memory().await.unwrap();
    let existing = input(
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
        "/overlap/root",
        RootIdentity::unix(&[11; 8], &[12; 8]).unwrap(),
    );
    create(&store, &existing).await.unwrap();

    let all_three = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/overlap/root",
            RootIdentity::unix(&[11; 8], &[12; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        all_three,
        Err(WorkspaceStoreError::Conflict(WorkspaceConflict::Identifier))
    );

    let root_and_identity = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "/overlap/root",
            RootIdentity::unix(&[11; 8], &[12; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        root_and_identity,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::CanonicalRoot
        ))
    );
}

#[tokio::test]
async fn canonical_root_is_exact_binary_and_inactive_rows_reserve_all_keys() {
    let store = Store::open_memory().await.unwrap();
    let existing = input(
        "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
        "/CaseSensitive",
        RootIdentity::unix(&[5; 8], &[6; 8]).unwrap(),
    );
    create(&store, &existing).await.unwrap();
    sqlx::query("UPDATE workspaces SET state = 'expired' WHERE id = ?")
        .bind(existing.id().as_str())
        .execute(test_hooks::pool(&store))
        .await
        .unwrap();

    let root_case = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X6",
            "/casesensitive",
            RootIdentity::unix(&[7; 8], &[8; 8]).unwrap(),
        ),
    )
    .await;
    assert!(root_case.is_ok(), "canonical root matching is BINARY");

    let reserved_root = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X7",
            "/CaseSensitive",
            RootIdentity::unix(&[9; 8], &[10; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        reserved_root,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::CanonicalRoot
        ))
    );

    let reserved_identity = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X8",
            "/unused",
            RootIdentity::unix(&[5; 8], &[6; 8]).unwrap(),
        ),
    )
    .await;
    assert_eq!(
        reserved_identity,
        Err(WorkspaceStoreError::Conflict(
            WorkspaceConflict::RootIdentity
        ))
    );
}

#[tokio::test]
async fn conflict_errors_are_static_and_never_echo_inputs() {
    let store = Store::open_memory().await.unwrap();
    let secret_root = "/sensitive/private/root";
    create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            secret_root,
            RootIdentity::windows(&[1; 8], &[2; 16]).unwrap(),
        ),
    )
    .await
    .unwrap();
    let error = create(
        &store,
        &input(
            "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5",
            "/another/private/root",
            RootIdentity::windows(&[3; 8], &[4; 16]).unwrap(),
        ),
    )
    .await
    .unwrap_err();
    let text = error.to_string();
    assert_eq!(text, "workspace conflict: identifier");
    assert!(!text.contains(secret_root));
    assert!(!text.contains("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5"));
}
