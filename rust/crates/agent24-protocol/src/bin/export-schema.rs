//! Export the WS event JSON Schema from the Rust types (generation source
//! since B4 — SPEC-002 §0.5). Usage:
//!   cargo run -p agent24-protocol --bin export-schema > ../protocol/events.schema.json

use agent24_protocol::Event;

/// Wire rule (SPEC-002 §0): nullable fields are ALWAYS present with value null.
/// serde enforces this on decode (no #[serde(default)] on these Options), but
/// schemars marks Option fields non-required — so force them required here,
/// keeping the null branch intact. Keep in lockstep with the type definitions;
/// the ajv fixture + negative checks in CI guard drift.
// Only defs reachable from Event appear here; REST-only types (Run, ToolCall,
// Schedule, …) get the same treatment when openapi generation switches to
// utoipa.
const FORCE_REQUIRED: &[(&str, &[&str])] = &[
    ("RunStartedPayload", &["session_id", "schedule_id"]),
    ("ToolCompletedPayload", &["output_summary"]),
    ("Approval", &["decision", "decided_at"]),
];

fn main() {
    let mut schema = schemars::schema_for!(Event);
    if let Some(map) = schema.as_object_mut() {
        map.insert(
            "$id".to_owned(),
            serde_json::json!("https://github.com/iDoris-ai/Agent24/protocol/events.schema.json"),
        );
        map.insert(
            "title".to_owned(),
            serde_json::json!("Agent24 v1 WebSocket Event Protocol"),
        );
        map.insert(
            "description".to_owned(),
            serde_json::json!("GENERATED from the agent24-protocol Rust crate (task B4) — do not edit by hand; regenerate with `cargo run -p agent24-protocol --bin export-schema`. Wire contract details: docs/specs/SPEC-002-protocol.md §3 (message classes: approval.required is the only REQUEST-class event, answered via POST /api/v1/approvals/{id}; everything else is NOTIFICATION). Conventions: snake_case; ULID ids; ISO 8601 UTC ts; nullable fields always present as null; per-connection monotonic seq; clients ignore unknown types/fields."),
        );
        // v is the protocol major version — always exactly 1 on the wire
        if let Some(v_schema) = map
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
            .and_then(|p| p.get_mut("v"))
            .and_then(|v| v.as_object_mut())
        {
            v_schema.retain(|k, _| k == "description");
            v_schema.insert("const".to_owned(), serde_json::json!(1));
        }
        // ts carries an ISO 8601 instant — keep format: date-time in the schema
        if let Some(ts_schema) = map
            .get_mut("properties")
            .and_then(|p| p.as_object_mut())
            .and_then(|p| p.get_mut("ts"))
            .and_then(|v| v.as_object_mut())
        {
            ts_schema.insert("format".to_owned(), serde_json::json!("date-time"));
        }
        if let Some(defs) = map.get_mut("$defs").and_then(|d| d.as_object_mut()) {
            // Timestamp-valued fields keep format: date-time in the schema
            const DATE_TIME_FIELDS: &[(&str, &[&str])] =
                &[("Approval", &["expires_at", "created_at", "decided_at"])];
            for (def_name, fields) in DATE_TIME_FIELDS {
                if let Some(props) = defs
                    .get_mut(*def_name)
                    .and_then(|d| d.as_object_mut())
                    .and_then(|d| d.get_mut("properties"))
                    .and_then(|p| p.as_object_mut())
                {
                    for f in *fields {
                        if let Some(field) = props.get_mut(*f).and_then(|v| v.as_object_mut()) {
                            field.insert("format".to_owned(), serde_json::json!("date-time"));
                        }
                    }
                }
            }
            for (def_name, fields) in FORCE_REQUIRED {
                let Some(def) = defs.get_mut(*def_name).and_then(|d| d.as_object_mut()) else {
                    eprintln!("FORCE_REQUIRED def not found: {def_name}");
                    std::process::exit(1);
                };
                let required = def
                    .entry("required")
                    .or_insert_with(|| serde_json::json!([]));
                let Some(arr) = required.as_array_mut() else {
                    eprintln!("required is not an array on {def_name}");
                    std::process::exit(1);
                };
                for f in *fields {
                    let v = serde_json::json!(f);
                    if !arr.contains(&v) {
                        arr.push(v);
                    }
                }
            }
        }
    }
    match serde_json::to_string_pretty(&schema) {
        Ok(s) => println!("{s}"),
        Err(err) => {
            eprintln!("failed to serialize schema: {err}");
            std::process::exit(1);
        }
    }
}
