//! Contract lockstep tests: every fixture in protocol/fixtures/ must
//! round-trip through the Rust types byte-equivalently (as JSON Values),
//! and event wire names must be dotted (B1 acceptance).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_protocol::{Event, EventBody};
use serde_json::Value;
use std::fs;
use std::path::PathBuf;

fn fixtures_dir(sub: &str) -> PathBuf {
    // crate at rust/crates/agent24-protocol → repo root is ../../..
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protocol/fixtures")
        .join(sub)
}

/// Structural equality with numeric-semantic comparison: JSON does not
/// distinguish `0` from `0.0`, but serde_json::Value does — the wire contract
/// cares about the number's value, not its lexical form.
fn json_semantic_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        // Integers compare exactly (as_f64 would lose precision above 2^53);
        // int/float mixing is only tolerated via lossless f64 comparison.
        (Value::Number(x), Value::Number(y)) => match (x.as_i64(), y.as_i64()) {
            (Some(i), Some(j)) => i == j,
            _ => x.as_f64() == y.as_f64(),
        },
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(m, n)| json_semantic_eq(m, n))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_semantic_eq(v, w)))
        }
        _ => a == b,
    }
}

#[test]
fn every_event_fixture_roundtrips_exactly() {
    let dir = fixtures_dir("events");
    let mut count = 0;
    for entry in fs::read_dir(&dir).expect("fixtures/events dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).unwrap();
        let original: Value = serde_json::from_str(&raw).unwrap();

        let event: Event = serde_json::from_str(&raw)
            .unwrap_or_else(|e| panic!("{} failed to decode: {e}", path.display()));
        let reencoded = serde_json::to_value(&event).unwrap();

        assert!(
            json_semantic_eq(&reencoded, &original),
            "{} did not round-trip:\n left: {reencoded}\nright: {original}",
            path.display()
        );
        count += 1;
    }
    assert!(
        count >= REQUIRED_EVENT_FIXTURES.len(),
        "expected >={} event fixtures, saw {count}",
        REQUIRED_EVENT_FIXTURES.len()
    );
}

/// One fixture per event type must exist. Adding an `EventBody` variant without
/// its fixture would otherwise satisfy every count floor silently (the bug
/// #101 review caught) — so this list is the single source the floor derives
/// from, and it must grow with the enum.
const REQUIRED_EVENT_FIXTURES: &[&str] = &[
    "run.started.json",
    "run.started.transient.json",
    "run.completed.json",
    "run.failed.json",
    "run.cancelled.json",
    "model.delta.json",
    "model.call.json",
    "tool.started.json",
    "tool.completed.json",
    "approval.required.json",
    "approval.resolved.json",
    "module-approval.required.json",
    "module-approval.resolved.json",
    "schedule.fired.json",
    "schedule.disabled.json",
    "schedule.delivered.json",
    "module.json",
];

#[test]
fn required_fixture_set_is_present() {
    let dir = fixtures_dir("events");
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for required in REQUIRED_EVENT_FIXTURES {
        assert!(
            names.iter().any(|n| n == required),
            "missing fixture {required}"
        );
    }
}

#[test]
fn event_wire_types_are_dotted_not_snake_case() {
    let dir = fixtures_dir("events");
    for entry in fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read_to_string(&path).unwrap();
        let event: Event = serde_json::from_str(&raw).unwrap();
        let wire = event.body.wire_type();
        // `module` is the ONE declared exemption (SPEC-002 §3): a namespace tag,
        // not an event name — the dotted name lives in payload.kind.
        assert!(
            wire == "module" || (wire.contains('.') && !wire.contains('_')),
            "event type must be dotted (or the `module` exemption), got {wire}"
        );
        // serialize side: the JSON "type" field equals wire_type()
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"].as_str().unwrap(), wire);
    }
}

#[test]
fn run_started_variant_serializes_run_dot_started() {
    // Explicit guard against a rename_all regression (ADR-026 #8)
    let ev = Event {
        v: 1,
        seq: 0,
        ts: "2026-07-23T12:00:00Z".into(),
        body: EventBody::RunStarted(agent24_protocol::RunStartedPayload {
            run_id: "run_x".into(),
            session_id: None,
            schedule_id: None,
        }),
    };
    let json = serde_json::to_value(&ev).unwrap();
    assert_eq!(json["type"], "run.started");
    // nullable wire fields are present with null, not omitted
    assert!(json["payload"].get("session_id").is_some());
    assert_eq!(json["payload"]["session_id"], Value::Null);
}

#[test]
fn approval_fixture_decodes_decision_open_set() {
    let raw = fs::read_to_string(fixtures_dir("events").join("approval.required.json")).unwrap();
    let event: Event = serde_json::from_str(&raw).unwrap();
    match event.body {
        EventBody::ApprovalRequired(a) => {
            assert_eq!(a.status, agent24_protocol::ApprovalStatus::Pending);
            assert!(a.available_decisions.contains(&"approve".to_owned()));
            assert!(a.decision.is_none());
        }
        other => panic!("wrong variant: {}", other.wire_type()),
    }
}

/// P1-02c transition, NEW reads OLD: a `memory.write_failed` from a daemon that
/// predates the typed fields (`{session_id, reason}` only) still decodes, with the
/// cause UNKNOWN (`None`) rather than a made-up code.
#[test]
fn memory_write_failed_from_an_older_daemon_decodes_with_unknown_cause() {
    let old = serde_json::json!({
        "v": 1, "seq": 3, "ts": "2026-07-23T12:00:01Z", "type": "memory.write_failed",
        "payload": { "session_id": "ses_x", "reason": "quota exceeded for owner ..." }
    });
    let event: Event = serde_json::from_value(old).unwrap();
    let EventBody::MemoryWriteFailed(p) = event.body else {
        panic!("not memory.write_failed")
    };
    assert_eq!(p.session_id, "ses_x");
    assert!(p.code.is_none() && p.owner_kind.is_none());
}

/// P1-02c transition, OLD reads NEW: what a current daemon emits is still valid
/// for a decoder that knows only the old `{session_id, reason}` shape, and its
/// `reason` is the sanitized code string, never free text.
#[test]
fn memory_write_failed_from_a_current_daemon_is_valid_for_an_old_decoder() {
    #[derive(serde::Deserialize)]
    struct OldPayload {
        session_id: String,
        reason: String,
    }
    #[derive(serde::Deserialize)]
    struct OldEvent {
        #[serde(rename = "type")]
        ty: String,
        payload: OldPayload,
    }
    let new = EventBody::MemoryWriteFailed(agent24_protocol::MemoryWriteFailedPayload {
        session_id: "ses_x".into(),
        reason: Some("memory_quota_rows".into()),
        code: Some(agent24_protocol::MemoryFailureCode::QuotaRows),
        dimension: Some("rows".into()),
        owner_kind: Some(agent24_protocol::MemoryOwnerKind::Personal),
        kind: None,
        partial: false,
    });
    let event = Event {
        v: 1,
        seq: 3,
        ts: "2026-07-23T12:00:01Z".into(),
        body: new,
    };
    let json = serde_json::to_value(&event).unwrap();
    let old: OldEvent = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(old.ty, "memory.write_failed");
    assert_eq!(old.payload.session_id, "ses_x");
    assert_eq!(old.payload.reason, "memory_quota_rows");
    // And the new decoder round-trips its own output exactly.
    let again: Event = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(serde_json::to_value(&again).unwrap(), json);
}

/// P1-02c: `run.completed` WITHOUT `memory_persisted` (a daemon from before the
/// field, which may well have failed to write) is UNKNOWN, never `true`.
#[test]
fn run_completed_without_memory_persisted_is_unknown_not_true() {
    let raw = fs::read_to_string(fixtures_dir("events").join("run.completed.json")).unwrap();
    let mut value: Value = serde_json::from_str(&raw).unwrap();
    value["payload"]
        .as_object_mut()
        .unwrap()
        .remove("memory_persisted");
    let event: Event = serde_json::from_value(value.clone()).unwrap();
    let EventBody::RunCompleted(p) = &event.body else {
        panic!("not run.completed")
    };
    assert_eq!(p.memory_persisted, None);
    // Unknown stays unknown on the way back out.
    assert!(
        serde_json::to_value(&event).unwrap()["payload"]
            .get("memory_persisted")
            .is_none()
    );
    // The fixture itself (a current daemon) carries the explicit value.
    let raw = fs::read_to_string(fixtures_dir("events").join("run.completed.json")).unwrap();
    let event: Event = serde_json::from_str(&raw).unwrap();
    let EventBody::RunCompleted(p) = event.body else {
        panic!("not run.completed")
    };
    assert_eq!(p.memory_persisted, Some(true));
}
