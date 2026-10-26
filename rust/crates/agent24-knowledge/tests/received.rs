//! KB-C1-04 6a: the Provider's decoding (`received`) reads every pinned
//! response and error body, ignores unknown fields at every level and unknown
//! reasons, and applies READONLY R6.2's fallbacks; what v1 does not let a
//! client tolerate is still refused.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_knowledge::received::{
    ErrorDetails, ErrorEnvelope, KbState, PartialFailureCode, ReadResponse, ReadyResponse,
    SearchResponse,
};
use agent24_knowledge::{
    Completeness, ErrorCode, IndexState, KbNotReadyReason, OperationStatus, UnauthorizedReason,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use std::fmt::Debug;
use std::path::PathBuf;

fn load(file: &str) -> Value {
    let dir =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../protocol/knowledge/fixtures/v1");
    let raw = std::fs::read_to_string(dir.join(file)).unwrap();
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn body(file: &str) -> Value {
    load(file)["response"]["body"].clone()
}

fn decode<T: DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| e.to_string())
}

fn paths(v: &Value, at: String, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            out.push(at.clone());
            m.iter()
                .for_each(|(k, c)| paths(c, format!("{at}/{k}"), out));
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, c)| paths(c, format!("{at}/{i}"), out)),
        _ => {}
    }
}

fn obj<'a>(v: &'a mut Value, ptr: &str) -> &'a mut Map<String, Value> {
    v.pointer_mut(ptr).and_then(Value::as_object_mut).unwrap()
}

/// `v` decodes as `T`, and to the same value with an unknown key added at any
/// one object of it. Returns what went wrong.
fn tolerant<T: DeserializeOwned + PartialEq + Debug>(file: &str, v: &Value) -> Vec<String> {
    let whole = match decode::<T>(v) {
        Ok(whole) => whole,
        Err(e) => return vec![format!("{file}: {e}")],
    };
    let mut at = Vec::new();
    paths(v, String::new(), &mut at);
    at.into_iter()
        .filter_map(|p| {
            let mut m = v.clone();
            obj(&mut m, &p).insert("x_unknown".into(), json!({ "nested": [1] }));
            match decode::<T>(&m) {
                Ok(got) if got == whole => None,
                Ok(_) => Some(format!("{file} {p:?}: decoded differently")),
                Err(e) => Some(format!("{file} {p:?}: {e}")),
            }
        })
        .collect()
}

#[derive(Deserialize)]
struct Entry {
    file: String,
    class: String,
}

/// Every pinned search, read and ready body, success or error.
#[test]
fn every_fixture_body_decodes_and_unknown_fields_change_nothing() {
    let m: Vec<Entry> = serde_json::from_value(load("manifest.json")["fixtures"].clone()).unwrap();
    let mut failures = Vec::new();
    let mut seen = 0;
    for e in m {
        let v = body(&e.file);
        if v.is_null() {
            continue;
        }
        let op = e.file.split('.').next().unwrap();
        failures.extend(match (op, e.class.as_str()) {
            (_, "reject") => tolerant::<ErrorEnvelope>(&e.file, &v),
            ("search", _) => tolerant::<SearchResponse>(&e.file, &v),
            ("read", _) => tolerant::<ReadResponse>(&e.file, &v),
            ("ready", _) => tolerant::<ReadyResponse>(&e.file, &v),
            _ => continue,
        });
        seen += 1;
    }
    assert_eq!(seen, 40, "bodies");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

fn error(code: &str, details: Value) -> Result<ErrorEnvelope, String> {
    decode(&json!({ "error": { "code": code, "message": "m", "details": details } }))
}

/// R6.2: an unknown code is `internal`, retried only on its own say-so.
#[test]
fn an_unknown_code_branches_as_internal_with_its_own_retryable() {
    for (retryable, expected) in [
        (json!(true), true),
        (json!(false), false),
        (Value::Null, false),
    ] {
        let mut d = json!({ "request_id": "r1", "generation": 3, "x": 1 });
        if !retryable.is_null() {
            d["retryable"] = retryable;
        }
        let e = error("quota_exhausted", d).unwrap().error;
        assert_eq!(e.code(), ErrorCode::Internal);
        assert!(matches!(e.details, ErrorDetails::Other(_)));
        assert_eq!(e.retryable(), expected, "{e:?}");
    }
    // A known code keeps its fixed `retryable` (§6).
    let internal = json!({ "retryable": true, "request_id": null, "generation": null });
    assert!(error("internal", internal).is_err());
    let timeout = json!({ "retryable": false, "request_id": "r", "generation": 1 });
    assert!(error("timeout", timeout).is_err());
}

/// R3.4, R3.5: an `unavailable` envelope is never read as more retryable than
/// its reason and its partial failures allow, whatever its own value says.
#[test]
fn an_error_is_never_more_retryable_than_its_parts() {
    let mixed = body("search.unavailable-mixed.json");
    let retryable = |v: &Value| decode::<ErrorEnvelope>(v).unwrap().error.retryable();
    assert!(!retryable(&mixed));
    // Claims retryable, but its reason is final.
    let mut v = mixed.clone();
    v["error"]["details"]["retryable"] = json!(true);
    assert!(!retryable(&v));
    // Claims retryable with a retryable reason, but one part is final.
    v["error"]["details"]["reason"] = json!("overloaded");
    assert!(!retryable(&v));
    // Every part retryable: its own value stands.
    v["error"]["details"]["partial_failures"] = json!([
        { "knowledge_base_id": "kb-a", "code": "timeout", "reason": "timeout", "retryable": true }
    ]);
    assert!(retryable(&v));
    v["error"]["details"]["retryable"] = json!(false);
    assert!(!retryable(&v));
    // Every part retryable, but the reason is final.
    for reason in ["index_failed", "unsupported_kb_type"] {
        v["error"]["details"]["retryable"] = json!(true);
        v["error"]["details"]["reason"] = json!(reason);
        assert!(!retryable(&v), "{reason}");
    }

    let mut t = body("search.timeout.json");
    assert!(retryable(&t));
    t["error"]["details"]["partial_failures"] = json!([
        { "knowledge_base_id": "kb-a", "code": "unavailable", "reason": "index_failed", "retryable": false }
    ]);
    assert!(!retryable(&t));
}

/// R6.1: both echoes are required; only `unauthorized`, `internal`, an
/// unknown code and an `invalid_request` other than `mixed_score_space` may
/// echo `null`.
#[test]
fn echoes_are_required_and_null_only_where_r6_1_allows() {
    let base = |reason: Option<&str>, id: Value, generation: Value| {
        let mut d = json!({ "retryable": false, "request_id": id, "generation": generation });
        if let Some(reason) = reason {
            d["reason"] = json!(reason);
        }
        d
    };
    let null = || base(None, Value::Null, Value::Null);
    let revoked = Some("credential_revoked");
    let e = error("unauthorized", base(revoked, Value::Null, Value::Null))
        .unwrap()
        .error;
    assert_eq!(e.echo(), (None, None));
    let e = error("unauthorized", base(revoked, json!("r1"), json!(4)))
        .unwrap()
        .error;
    assert_eq!(e.echo(), (Some("r1"), Some(4)));
    assert!(
        error(
            "unauthorized",
            json!({ "retryable": false, "reason": "credential_revoked" })
        )
        .is_err()
    );
    for key in ["request_id", "generation"] {
        let mut d = base(revoked, json!("r1"), json!(4));
        d.as_object_mut().unwrap().remove(key);
        assert!(error("unauthorized", d).is_err(), "missing {key}");
    }
    assert!(error("unauthorized", base(revoked, json!("r1"), Value::Null)).is_ok());
    assert!(error("internal", null()).is_ok());
    assert!(error("quota_exhausted", json!({})).is_ok());
    assert!(
        error(
            "invalid_request",
            base(
                Some("unsupported_protocol_version"),
                Value::Null,
                Value::Null
            )
        )
        .is_ok()
    );

    let mixed = body("search.mixed-score-space.json");
    assert!(decode::<ErrorEnvelope>(&mixed).is_ok());
    for key in ["request_id", "generation"] {
        let mut v = mixed.clone();
        v["error"]["details"][key] = Value::Null;
        assert!(
            decode::<ErrorEnvelope>(&v).is_err(),
            "mixed_score_space with null {key}"
        );
    }
    assert!(error("not_found", base(None, Value::Null, json!(4))).is_err());
    assert!(
        error(
            "forbidden",
            base(Some("policy_denied"), json!("r1"), Value::Null)
        )
        .is_err()
    );
}

/// §0.3: an unknown reason reads as none; an `unauthorized` without a known
/// one is the invalid-credential class (STATE S3.1), never a revocation.
#[test]
fn an_unknown_reason_reads_as_none() {
    let mut v = body("search.unauthorized-revoked.json");
    let ErrorDetails::Unauthorized(d) = decode::<ErrorEnvelope>(&v).unwrap().error.details else {
        panic!("not unauthorized");
    };
    assert_eq!(d.reason, Some(UnauthorizedReason::CredentialRevoked));
    v["error"]["details"]["reason"] = json!("credential_rotated");
    let ErrorDetails::Unauthorized(d) = decode::<ErrorEnvelope>(&v).unwrap().error.details else {
        panic!("not unauthorized");
    };
    assert_eq!(d.reason, None);

    let mut v = body("search.unavailable-mixed.json");
    v["error"]["details"]["reason"] = json!("maintenance");
    let e = decode::<ErrorEnvelope>(&v).unwrap().error;
    let ErrorDetails::Unavailable(d) = &e.details else {
        panic!("not unavailable");
    };
    assert_eq!(d.reason, None);
    assert_eq!(e.retryable(), d.retryable);

    let mut v = body("read.policy-denied.json");
    v["error"]["details"]["reason"] = json!("region_blocked");
    let ErrorDetails::Forbidden(d) = decode::<ErrorEnvelope>(&v).unwrap().error.details else {
        panic!("not forbidden");
    };
    assert_eq!(d.reason, None);

    // `null` is no reason; anything but a string is still malformed.
    v["error"]["details"]["reason"] = Value::Null;
    assert!(decode::<ErrorEnvelope>(&v).is_ok());
    v["error"]["details"]["reason"] = json!(7);
    assert!(decode::<ErrorEnvelope>(&v).is_err());
}

/// R6.2 for `partial_failures[]`: an unknown code is `internal`, an unknown
/// reason none, and without `retryable` the item is not retried.
#[test]
fn a_partial_failure_outside_v1_keeps_its_own_retryable() {
    let mut v = body("search.partial.json");
    let failures = v["partial_failures"].as_array_mut().unwrap();
    let first = failures[0].clone();
    failures.push(json!({ "knowledge_base_id": "kb-x", "code": "throttled",
                          "reason": "quota", "retryable": true }));
    failures.push(json!({ "knowledge_base_id": "kb-y", "code": "unavailable",
                          "reason": "rebalancing" }));
    let r = decode::<SearchResponse>(&v).unwrap();
    let got = r.partial_failures.unwrap();
    let [known, throttled, rebalancing] = got.as_slice() else {
        panic!("{got:?}");
    };
    assert_eq!(known.knowledge_base_id, first["knowledge_base_id"]);
    assert!(known.reason.is_some());
    assert_eq!(
        (throttled.code, throttled.reason, throttled.retryable),
        (PartialFailureCode::Internal, None, true)
    );
    assert_eq!(
        (rebalancing.code, rebalancing.reason, rebalancing.retryable),
        (PartialFailureCode::Unavailable, None, false)
    );
}

/// R3.5: `index_failed` and `unsupported_kb_type` are never retried, even
/// when the item says otherwise; other reasons keep the item's own value.
#[test]
fn a_partial_failure_with_a_final_reason_is_not_retried() {
    let mut v = body("search.partial.json");
    let item = |reason: &str| {
        json!({ "knowledge_base_id": format!("kb-{reason}"), "code": "unavailable",
                "reason": reason, "retryable": true })
    };
    v["partial_failures"] = json!([
        item("index_failed"),
        item("unsupported_kb_type"),
        item("overloaded")
    ]);
    let r = decode::<SearchResponse>(&v).unwrap();
    let got: Vec<bool> = r
        .partial_failures
        .unwrap()
        .as_slice()
        .iter()
        .map(|f| f.retryable)
        .collect();
    assert_eq!(got, [false, false, true]);
}

#[test]
fn unknown_index_state_and_completeness_read_as_the_stricter_value() {
    let mut v = body("search.ok.json");
    v["completeness"] = json!("best_effort");
    v["hits"][0]["index_state"] = json!("reindexing");
    let r = decode::<SearchResponse>(&v).unwrap();
    assert_eq!(r.completeness, Completeness::Partial);
    assert_eq!(r.hits[0].index_state, IndexState::Stale);
    // A known value is kept as it is.
    let ok = decode::<SearchResponse>(&body("search.ok.json")).unwrap();
    assert_eq!(ok.completeness, Completeness::Complete);
}

#[test]
fn an_unknown_readiness_state_is_not_ready() {
    let mut v = body("ready.scoped.json");
    v["operations"]["search"]["state"] = json!("warming");
    v["operations"]["ingest"] = json!({ "state": "enabled" });
    let kb = &mut v["knowledge_bases"][0];
    kb["search"] = json!({ "state": "warming" });
    kb["read"] = json!({ "state": "not_ready", "reason": "reindexing" });
    let r = decode::<ReadyResponse>(&v).unwrap();
    assert_eq!(r.operations.search.state, OperationStatus::NotReady);
    let kbs = r.knowledge_bases.unwrap();
    let kb = &kbs.as_slice()[0];
    assert_eq!(
        (kb.search, kb.read),
        (KbState::NotReady(None), KbState::NotReady(None))
    );

    let mut v = body("ready.scoped.json");
    v["knowledge_bases"][0]["search"] = json!({ "state": "not_ready", "reason": "dependency" });
    v["knowledge_bases"][0]["read"] = json!({ "state": "ready", "reason": "index_building" });
    let r = decode::<ReadyResponse>(&v).unwrap();
    let kbs = r.knowledge_bases.unwrap();
    let kb = &kbs.as_slice()[0];
    assert_eq!(
        kb.search,
        KbState::NotReady(Some(KbNotReadyReason::Dependency))
    );
    assert_eq!(kb.read, KbState::Ready);
}

/// Values §0.3 does not list as compatible additions are semantic changes.
#[test]
fn what_v1_does_not_tolerate_is_still_refused() {
    let mut v = body("search.ok.json");
    v["hits"][0]["chunk_type"] = json!("table");
    assert!(decode::<SearchResponse>(&v).is_err());
    let mut v = body("read.chunk.json");
    v["protocol_version"] = json!(2);
    assert!(decode::<ReadResponse>(&v).is_err());
    let mut v = body("search.ok.json");
    v["hits"][0]["locator"] = json!({ "valid": true });
    assert!(decode::<SearchResponse>(&v).is_err());
    v["hits"][0]["locator"] = json!({ "valid": false, "start": 0, "end": 1 });
    assert!(decode::<SearchResponse>(&v).is_err());
    let mut v = body("search.ok.json");
    v.as_object_mut().unwrap().remove("generation");
    assert!(decode::<SearchResponse>(&v).is_err());
}
