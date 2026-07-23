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
    assert!(count >= 12, "expected >=12 event fixtures, saw {count}");
}

#[test]
fn required_fixture_set_is_present() {
    // One fixture per event type must exist (deleting one may not fail the
    // count floor above) — extras are welcome.
    let dir = fixtures_dir("events");
    let names: Vec<String> = fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    for required in [
        "run.started.json",
        "run.started.transient.json",
        "run.completed.json",
        "run.failed.json",
        "run.cancelled.json",
        "model.delta.json",
        "tool.started.json",
        "tool.completed.json",
        "approval.required.json",
        "approval.resolved.json",
        "schedule.fired.json",
        "schedule.disabled.json",
    ] {
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
        assert!(
            wire.contains('.') && !wire.contains('_'),
            "event type must be dotted, got {wire}"
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
