#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_protocol::WorkspaceId;
use agent24_store::{
    LifecycleOwnerRef, NewScratchWorkspace, RootIdentity, TrustedRootRegistration,
    WorkspaceConflict, WorkspaceInstant, WorkspaceListCursor, WorkspaceListLimit,
    WorkspaceListQuery, WorkspacePage, WorkspaceProvenanceInput, WorkspaceState,
    WorkspaceStoreError, WorkspaceTtl,
};

const ID: &str = "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5";
const CREATED: &str = "2026-09-19T00:00:00.000Z";

fn root() -> TrustedRootRegistration {
    TrustedRootRegistration::new(
        "/tmp/workspace".to_owned(),
        "generation-1".to_owned(),
        RootIdentity::unix(&[0; 8], &[1; 8]).unwrap(),
    )
    .unwrap()
}

#[test]
fn instant_add_is_checked_and_canonical() {
    let created = WorkspaceInstant::parse(CREATED).unwrap();
    let ttl = WorkspaceTtl::new(1_234).unwrap();
    assert_eq!(
        created.checked_add_workspace_ttl(ttl).unwrap().as_str(),
        "2026-09-19T00:00:01.234Z"
    );
    let max = WorkspaceInstant::parse("9999-12-31T23:59:59.999Z").unwrap();
    assert!(max.checked_add_workspace_ttl(ttl).is_err());
}

#[test]
fn input_wrappers_reject_invalid_values_and_scratch_is_fixed_policy() {
    assert!(WorkspaceProvenanceInput::new(" ".into(), None, None).is_err());
    assert!(WorkspaceProvenanceInput::new("source\0bad".into(), None, None).is_err());
    assert!(WorkspaceProvenanceInput::new("source".into(), Some("x\0".into()), None).is_err());
    assert!(LifecycleOwnerRef::parse(" ".into()).is_err());
    assert!(LifecycleOwnerRef::parse("owner\0bad".into()).is_err());

    let provenance =
        WorkspaceProvenanceInput::new("git".into(), Some("project".into()), Some("base".into()))
            .unwrap();
    let scratch = NewScratchWorkspace::new(
        WorkspaceId::parse(ID).unwrap(),
        root(),
        provenance,
        LifecycleOwnerRef::parse("orchestrator-1".into()).unwrap(),
        WorkspaceTtl::new(60_000).unwrap(),
    );
    assert_eq!(scratch.kind().as_str(), "orchestrator_scratch");
    assert_eq!(scratch.state().as_str(), "active");
    assert_eq!(scratch.revision(), 1);
    assert_eq!(scratch.writeback_policy(), "external");
    assert_eq!(scratch.lifecycle_owner_kind(), "orchestrator");
    assert_eq!(scratch.concurrency_policy(), "serial");
}

#[test]
fn list_models_have_bounded_limit_and_opaque_cursor() {
    assert!(WorkspaceListLimit::new(0).is_err());
    assert!(WorkspaceListLimit::new(101).is_err());
    let limit = WorkspaceListLimit::new(100).unwrap();
    let cursor = WorkspaceListCursor::new(
        WorkspaceInstant::parse(CREATED).unwrap(),
        WorkspaceId::parse(ID).unwrap(),
    );
    let query = WorkspaceListQuery::new(Some(WorkspaceState::Active), Some(cursor.clone()), limit);
    assert_eq!(query.limit().value(), 100);
    assert_eq!(query.state(), Some(WorkspaceState::Active));
    assert_eq!(query.after(), Some(&cursor));
    let page = WorkspacePage::new(Vec::new(), Some(cursor));
    assert!(page.items.is_empty());
    assert!(page.next_cursor.is_some());
}

#[test]
fn not_found_and_conflicts_are_static_and_redacted() {
    assert_eq!(
        WorkspaceStoreError::NotFound.to_string(),
        "workspace not found"
    );
    for conflict in [
        WorkspaceConflict::Identifier,
        WorkspaceConflict::CanonicalRoot,
        WorkspaceConflict::RootIdentity,
    ] {
        let text = WorkspaceStoreError::Conflict(conflict).to_string();
        assert!(text.starts_with("workspace conflict:"));
        assert!(!text.contains(ID));
        assert!(!text.contains("/tmp/workspace"));
    }
}
