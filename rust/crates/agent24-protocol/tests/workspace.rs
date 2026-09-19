use agent24_protocol::{
    ConcurrencyPolicy, LifecycleOwner, LifecycleOwnerKind, Workspace, WorkspaceId, WorkspaceKind,
    WorkspaceProvenance, WorkspaceState, WritebackPolicy,
};

fn valid_workspace() -> Workspace {
    Workspace {
        id: WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
        kind: WorkspaceKind::OrchestratorScratch,
        state: WorkspaceState::Active,
        provenance: WorkspaceProvenance {
            source: "open-design".into(),
            project_ref: Some("project-1".into()),
            base_revision: Some("abc123".into()),
        },
        writeback_policy: WritebackPolicy::External,
        lifecycle_owner: LifecycleOwner {
            kind: LifecycleOwnerKind::Orchestrator,
            reference: "orchestrator-1".into(),
        },
        concurrency_policy: ConcurrencyPolicy::Serial,
        created_at: "2026-09-19T00:00:00Z".into(),
        expires_at: "2026-09-20T00:00:00Z".into(),
        renewed_at: None,
        released_at: None,
        revision: 1,
    }
}

#[test]
fn opaque_id_accepts_only_v1_shape() {
    let id = WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap();
    assert_eq!(id.as_str(), "ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5");
    for invalid in [
        "/tmp/workspace",
        "ws_lowercase",
        "ws_01J5M4Q2Y7N8P9R0S1T2V3WXI",
    ] {
        assert!(WorkspaceId::parse(invalid).is_err(), "accepted {invalid}");
    }
}

#[test]
fn valid_workspace_roundtrips_and_validates() {
    let workspace = valid_workspace();
    workspace.validate().unwrap();
    let json = serde_json::to_value(&workspace).unwrap();
    let decoded: Workspace = serde_json::from_value(json).unwrap();
    assert_eq!(decoded, workspace);
}

#[test]
fn deserialization_rejects_invalid_timestamp_ttl_and_extra_fields() {
    let mut value = serde_json::to_value(valid_workspace()).unwrap();
    value["expires_at"] = "2026-09-28T00:00:00Z".into();
    assert!(serde_json::from_value::<Workspace>(value.clone()).is_err());
    value["expires_at"] = "2026-09-20T00:00:00Z".into();
    value["created_at"] = "not-a-timestamp".into();
    assert!(serde_json::from_value::<Workspace>(value.clone()).is_err());
    value["created_at"] = "2026-09-19T00:00:00Z".into();
    value["future_field"] = true.into();
    assert!(serde_json::from_value::<Workspace>(value).is_err());
}

#[test]
fn unknown_v1_enum_values_are_rejected() {
    let mut value = serde_json::to_value(valid_workspace()).unwrap();
    value["writeback_policy"] = "internal".into();
    assert!(serde_json::from_value::<Workspace>(value).is_err());
}
