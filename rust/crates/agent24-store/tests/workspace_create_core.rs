#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, Store, TrustedRootRegistration,
    WorkspaceInstant, WorkspaceProvenanceInput, WorkspaceTtl, test_hooks,
};
use sqlx::Row;

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const NOW: &str = "2026-09-19T00:00:00.000Z";

fn input() -> NewScratchWorkspace {
    NewScratchWorkspace::new(
        WorkspaceId::parse(ID).expect("id"),
        TrustedRootRegistration::new(
            "/private/workspace".into(),
            "generation-1".into(),
            RootIdentity::unix(&[1; 8], &[2; 8]).expect("identity"),
        )
        .expect("root"),
        WorkspaceProvenanceInput::new("git".into(), Some("project".into()), Some("base".into()))
            .expect("provenance"),
        LifecycleOwnerRef::parse("orchestrator-1".into()).expect("owner"),
        WorkspaceTtl::new(60_000).expect("ttl"),
    )
}

#[tokio::test]
async fn create_persists_one_active_row_and_returns_redacted_projection() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Store::open(&dir.path().join("registry.sqlite"))
        .await
        .expect("store");
    let now = WorkspaceInstant::parse(NOW).expect("now");
    let owner = LifecycleOwnerRef::parse("orchestrator-1".into()).expect("owner");

    let workspace = store
        .create_workspace(&input(), &owner, &now)
        .await
        .expect("create");

    assert_eq!(workspace.id.as_str(), ID);
    assert_eq!(workspace.kind, "orchestrator_scratch");
    assert_eq!(workspace.state, "active");
    assert_eq!(workspace.created_at, NOW);
    assert_eq!(workspace.expires_at, "2026-09-19T00:01:00.000Z");
    assert_eq!(workspace.revision, 1);
    let wire = serde_json::to_string(&workspace).expect("wire");
    for secret in ["/private/workspace", "generation-1"] {
        assert!(!wire.contains(secret), "projection leaked {secret}");
    }

    let row = sqlx::query(
        "SELECT state, writeback_policy, lifecycle_owner_kind, concurrency_policy,
                renewed_at, released_at, cleanup_attempts, cleanup_last_attempt_at,
                root_identity_kind, unix_device, unix_inode,
                windows_volume_serial, windows_file_id
         FROM workspaces WHERE id = ?",
    )
    .bind(ID)
    .fetch_one(test_hooks::pool(&store))
    .await
    .expect("row");
    assert_eq!(row.get::<String, _>("state"), "active");
    assert_eq!(row.get::<String, _>("writeback_policy"), "external");
    assert_eq!(row.get::<String, _>("lifecycle_owner_kind"), "orchestrator");
    assert_eq!(row.get::<String, _>("concurrency_policy"), "serial");
    assert!(row.get::<Option<String>, _>("renewed_at").is_none());
    assert!(row.get::<Option<String>, _>("released_at").is_none());
    assert_eq!(row.get::<i64, _>("cleanup_attempts"), 0);
    assert!(
        row.get::<Option<String>, _>("cleanup_last_attempt_at")
            .is_none()
    );
    assert_eq!(row.get::<String, _>("root_identity_kind"), "unix");
    assert_eq!(row.get::<Vec<u8>, _>("unix_device"), vec![1; 8]);
    assert_eq!(row.get::<Vec<u8>, _>("unix_inode"), vec![2; 8]);
    assert!(
        row.get::<Option<Vec<u8>>, _>("windows_volume_serial")
            .is_none()
    );
    assert!(row.get::<Option<Vec<u8>>, _>("windows_file_id").is_none());
}
