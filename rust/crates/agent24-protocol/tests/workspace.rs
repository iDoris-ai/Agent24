#![allow(clippy::expect_used, clippy::unwrap_used)]

use agent24_protocol::{LifecycleOwner, Workspace, WorkspaceId, WorkspaceProvenance};

fn valid_workspace() -> Workspace {
    Workspace {
        id: WorkspaceId::parse("ws_01J5M4Q2Y7N8P9R0S1T2V3W4X5").unwrap(),
        kind: "orchestrator_scratch".into(),
        state: "active".into(),
        provenance: WorkspaceProvenance {
            source: "open-design".into(),
            project_ref: Some("project-1".into()),
            base_revision: Some("abc123".into()),
        },
        writeback_policy: "external".into(),
        lifecycle_owner: LifecycleOwner {
            kind: "orchestrator".into(),
            reference: "orchestrator-1".into(),
        },
        concurrency_policy: "serial".into(),
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
        "ws_Z1J5M4Q2Y7N8P9R0S1T2V3W4X5",
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
fn response_accepts_unknown_fields_and_enum_strings() {
    let mut value = serde_json::to_value(valid_workspace()).unwrap();
    value["future_field"] = true.into();
    value["kind"] = "future_kind".into();
    value["state"] = "future_state".into();
    value["writeback_policy"] = "future_policy".into();
    value["provenance"]["future_provenance"] = 1.into();
    let decoded: Workspace = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.kind, "future_kind");
    assert_eq!(decoded.state, "future_state");
    assert_eq!(decoded.writeback_policy, "future_policy");
}

#[test]
fn exact_ttl_and_fractional_overflow_are_checked_without_truncation() {
    let mut value = serde_json::to_value(valid_workspace()).unwrap();
    value["created_at"] = "2026-09-19T00:00:00.100Z".into();
    value["expires_at"] = "2026-09-26T00:00:00.100Z".into();
    assert!(serde_json::from_value::<Workspace>(value.clone()).is_ok());
    value["expires_at"] = "2026-09-26T00:00:00.100000001Z".into();
    assert!(serde_json::from_value::<Workspace>(value).is_err());
}

#[test]
fn renewal_anchors_the_ttl_and_chronology_is_checked() {
    let mut workspace = valid_workspace();
    workspace.expires_at = "2026-09-27T00:00:00Z".into();
    workspace.renewed_at = Some("2026-09-20T00:00:00Z".into());
    assert!(workspace.validate().is_ok());
    workspace.renewed_at = Some("2026-09-18T23:59:59Z".into());
    assert!(workspace.validate().is_err());
    workspace.renewed_at = Some("2026-09-20T00:00:00Z".into());
    workspace.released_at = Some("2026-09-19T23:00:00Z".into());
    assert!(workspace.validate().is_err());
}

#[test]
fn invalid_in_memory_values_cannot_serialize() {
    let mut workspace = valid_workspace();
    workspace.provenance.source = " ".into();
    assert!(workspace.validate().is_err());
    assert!(serde_json::to_value(&workspace).is_err());
    workspace = valid_workspace();
    workspace.lifecycle_owner.reference = "".into();
    assert!(serde_json::to_value(&workspace).is_err());
    workspace = valid_workspace();
    workspace.expires_at = "2026-09-28T00:00:00Z".into();
    assert!(serde_json::to_value(&workspace).is_err());
    workspace = valid_workspace();
    workspace.created_at = "not-a-timestamp".into();
    assert!(serde_json::to_value(&workspace).is_err());
}
