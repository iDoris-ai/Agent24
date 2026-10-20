//! KB-C1-02a / #1052: the wire types round-trip the pinned fixtures in
//! `protocol/knowledge/fixtures/v1`. Covered here: every `search.*` and `read.*`
//! fixture (request and response) and the response of every other `reject`
//! fixture (error envelope only). The manifest drives the list. Digests are pinned in `agent24-protocol/tests/knowledge_fixtures.rs`.
//!
//! "Equal" means semantically equal JSON: object key order is ignored and numbers
//! compare by value, so a fixture `1` and a re-encoded `f64` `1.0` match, but
//! never through a lossy conversion (`9007199254740993` ≠ `9007199254740992.0`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_knowledge::{ErrorEnvelope, ReadRequest, ReadResponse, SearchRequest, SearchResponse};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value, json};
use std::path::PathBuf;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../protocol/knowledge/fixtures/v1")
}

fn load(file: &str) -> Value {
    let raw = std::fs::read_to_string(dir().join(file)).unwrap();
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{file}: {e}"))
}

#[derive(Deserialize)]
struct Manifest {
    fixtures: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    file: String,
    class: String,
    request_valid: Option<bool>,
    server_handler: bool,
}

fn covered() -> Vec<Entry> {
    let m: Manifest = serde_json::from_value(load("manifest.json")).unwrap();
    m.fixtures
        .into_iter()
        .filter(|e| has_request(&e.file) || e.class == "reject")
        .collect()
}

/// A JSON integer, widened losslessly (covers both `i64` and `u64`).
fn int(n: &Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}

/// Numbers compare by value without a lossy conversion: an integer equals a
/// float only if the float is integral and is exactly that integer.
fn same_number(x: &Number, y: &Number) -> bool {
    // An integral f64 within i128 range converts to i128 exactly; out of range
    // it saturates, which no JSON integer (at most u64::MAX) can equal.
    let int_eq_float = |i: i128, f: f64| f.is_finite() && f.fract() == 0.0 && f as i128 == i;
    match (int(x), int(y)) {
        (Some(i), Some(j)) => i == j,
        (Some(i), None) => y.as_f64().is_some_and(|f| int_eq_float(i, f)),
        (None, Some(j)) => x.as_f64().is_some_and(|f| int_eq_float(j, f)),
        (None, None) => x.as_f64() == y.as_f64(),
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => same_number(x, y),
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, p)| y.get(k).is_some_and(|q| same(p, q)))
        }
        _ => a == b,
    }
}

fn decode<T: DeserializeOwned>(v: &Value) -> Result<T, String> {
    serde_json::from_value(v.clone()).map_err(|e| format!("decode failed: {e}"))
}

fn roundtrip<T: DeserializeOwned + Serialize>(v: &Value) -> Result<(), String> {
    let back = serde_json::to_value(decode::<T>(v)?).map_err(|e| format!("encode failed: {e}"))?;
    if same(v, &back) {
        Ok(())
    } else {
        Err(format!(
            "re-encoded differs\n  fixture: {v}\n  encoded: {back}"
        ))
    }
}

/// Fixtures whose request this crate has types for.
fn has_request(file: &str) -> bool {
    file.starts_with("search.") || file.starts_with("read.")
}

fn is_read(file: &str) -> bool {
    file.starts_with("read.")
}

fn decode_request(file: &str, v: &Value) -> Result<(), String> {
    if is_read(file) {
        decode::<ReadRequest>(v).map(drop)
    } else {
        decode::<SearchRequest>(v).map(drop)
    }
}

fn decode_raw_request(file: &str, raw: &str) -> bool {
    if is_read(file) {
        serde_json::from_str::<ReadRequest>(raw).is_ok()
    } else {
        serde_json::from_str::<SearchRequest>(raw).is_ok()
    }
}

fn roundtrip_request(file: &str, v: &Value) -> Result<(), String> {
    if is_read(file) {
        roundtrip::<ReadRequest>(v)
    } else {
        roundtrip::<SearchRequest>(v)
    }
}

fn roundtrip_success(file: &str, v: &Value) -> Result<(), String> {
    if is_read(file) {
        roundtrip::<ReadResponse>(v)
    } else {
        roundtrip::<SearchResponse>(v)
    }
}

fn decode_body(file: &str, accept: bool, body: &Value) -> Result<(), String> {
    match (accept, is_read(file)) {
        (true, true) => decode::<ReadResponse>(body).map(drop),
        (true, false) => decode::<SearchResponse>(body).map(drop),
        (false, _) => decode::<ErrorEnvelope>(body).map(drop),
    }
}

#[test]
fn covered_fixtures_roundtrip() {
    let mut failures = Vec::new();
    let entries = covered();
    for e in &entries {
        let fx = load(&e.file);
        let mut fail = |side: &str, r: Result<(), String>| {
            if let Err(msg) = r {
                failures.push(format!("{} [{side}]: {msg}", e.file));
            }
        };
        if has_request(&e.file) {
            let req = &fx["request"];
            match e.request_valid {
                Some(true) => fail("request", roundtrip_request(&e.file, req)),
                // A string request is a raw body that is not valid JSON.
                Some(false) => match req {
                    Value::String(raw) if decode_raw_request(&e.file, raw) => {
                        fail("request", Err("raw body decoded".into()))
                    }
                    Value::String(_) => {}
                    _ if decode_request(&e.file, req).is_ok() => {
                        fail("request", Err("invalid request decoded".into()))
                    }
                    _ => {}
                },
                None => {}
            }
        }
        let body = &fx["response"]["body"];
        let accept = e.class == "accept";
        // server_handler: false (§12 #18, search.cancelled) is a client-only fixture:
        // its late success body is something a client decodes, never something a
        // server encodes, so it is decoded only and kept out of the encode check.
        // Its request is still round-tripped (the client encodes requests); its
        // `local` part is the local-cancelled result, covered with those types.
        let r = match (e.server_handler, accept) {
            (false, _) => decode_body(&e.file, accept, body),
            (true, true) => roundtrip_success(&e.file, body),
            (true, false) => roundtrip::<ErrorEnvelope>(body),
        };
        fail("response", r);
    }
    let names: Vec<_> = entries.iter().map(|e| e.file.as_str()).collect();
    println!("covered {} fixtures: {names:#?}", names.len());
    assert_eq!(
        names.len(),
        34,
        "manifest changed; review coverage: {names:#?}"
    );
    assert!(
        failures.is_empty(),
        "{} failure(s):\n{}",
        failures.len(),
        failures.join("\n")
    );
}

/// Every JSON-pointer path to an object inside `v`.
fn object_paths(v: &Value, at: String, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            out.push(at.clone());
            m.iter()
                .for_each(|(k, c)| object_paths(c, format!("{at}/{k}"), out));
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .for_each(|(i, c)| object_paths(c, format!("{at}/{i}"), out)),
        _ => {}
    }
}

fn obj<'a>(v: &'a mut Value, ptr: &str) -> &'a mut Map<String, Value> {
    v.pointer_mut(ptr)
        .and_then(Value::as_object_mut)
        .unwrap_or_else(|| panic!("no object at {ptr}"))
}

/// `deny_unknown_fields` on every type: an extra key on any object of a covered
/// request or body must fail to decode. (No `request_valid: false` fixture in
/// this slice carries an unknown field, so the fixtures alone cannot show this.)
#[test]
fn unknown_fields_are_rejected_at_every_level() {
    let mut accepted = Vec::new();
    for e in covered() {
        let fx = load(&e.file);
        let accept = e.class == "accept";
        let mut targets = vec![("response", fx["response"]["body"].clone())];
        if has_request(&e.file) && e.request_valid == Some(true) {
            targets.push(("request", fx["request"].clone()));
        }
        for (side, v) in targets {
            let mut paths = Vec::new();
            object_paths(&v, String::new(), &mut paths);
            for p in paths {
                let mut m = v.clone();
                obj(&mut m, &p).insert("x_unknown".into(), json!(1));
                let ok = match side {
                    "request" => decode_request(&e.file, &m).is_ok(),
                    _ => decode_body(&e.file, accept, &m).is_ok(),
                };
                if ok {
                    accepted.push(format!("{} [{side}] {p:?}", e.file));
                }
            }
        }
    }
    assert!(
        accepted.is_empty(),
        "unknown field accepted at:\n{}",
        accepted.join("\n")
    );
}

/// `"/request"` or `"/response/body"` of a fixture.
fn part(file: &str, side: &str) -> Value {
    load(file).pointer_mut(side).unwrap().take()
}

fn decode_part(file: &str, side: &str, v: &Value) -> Result<(), String> {
    if side == "/request" {
        decode_request(file, v)
    } else {
        let accept = ["search.ok", "search.partial", "read.chunk", "read.document"]
            .iter()
            .any(|p| file.starts_with(p));
        decode_body(file, accept, v)
    }
}

/// Required-but-nullable keys (R6.1 echoes, `chunk_revision`, `index_build_id`,
/// `page.cursor`, `next_cursor`) must be present; optional keys must not be `null`.
#[test]
fn nullable_and_optional_keys_are_distinct() {
    const B: &str = "/response/body";
    const Q: &str = "/request";
    let missing = [
        (
            "search.invalid-body.json",
            B,
            "/error/details",
            "request_id",
        ),
        (
            "search.unauthorized-before-parse.json",
            B,
            "/error/details",
            "generation",
        ),
        (
            "read.stale-anchor-missing.json",
            B,
            "/error/details/current_revision",
            "index_build_id",
        ),
        ("search.ok.json", B, "/hits/2/revision", "chunk_revision"),
        ("read.chunk.json", B, "", "next_cursor"),
        ("read.document.json", Q, "/page", "cursor"),
    ];
    for (file, side, ptr, key) in missing {
        let mut v = part(file, side);
        assert!(
            obj(&mut v, ptr).remove(key).is_some(),
            "{file}: no {side}{ptr}/{key}"
        );
        assert!(
            decode_part(file, side, &v).is_err(),
            "{file}: missing {side}{ptr}/{key} was accepted"
        );
    }
    let null = [
        ("search.unauthorized-revoked.json", B, "/error", "hint"),
        (
            "search.mixed-score-space.json",
            B,
            "/error/details",
            "reason",
        ),
        ("search.ok.json", B, "/hits/0", "display"),
        ("search.partial.json", B, "", "partial_failures"),
        ("read.document.json", B, "", "display"),
        ("read.document.json", Q, "/page", "limit"),
        ("read.document.json", Q, "", "page"),
        ("read.document.json", Q, "", "expected_revision"),
        ("read.chunk.json", Q, "", "chunk_id"),
        ("read.chunk.json", Q, "", "expected_revision"),
        ("read.chunk.json", Q, "", "max_bytes"),
        ("read.chunk.json", Q, "", "deadline_ms"),
    ];
    for (file, side, ptr, key) in null {
        let mut v = part(file, side);
        assert!(
            obj(&mut v, ptr).insert(key.into(), Value::Null).is_some(),
            "{file}: no {side}{ptr}/{key}"
        );
        assert!(
            decode_part(file, side, &v).is_err(),
            "{file}: null {side}{ptr}/{key} was accepted"
        );
    }
}

#[test]
fn numbers_compare_losslessly() {
    let n = |s: &str| serde_json::from_str::<Value>(s).unwrap();
    assert!(same(&n("1"), &n("1.0")));
    assert!(same(&n("1.0"), &n("1")));
    assert!(same(&n("-3"), &n("-3.0")));
    assert!(same(&n("1.5"), &n("1.5")));
    assert!(!same(&n("1"), &n("1.5")));
    assert!(!same(&n("9007199254740993"), &n("9007199254740992.0")));
    assert!(!same(&n("9007199254740992.0"), &n("9007199254740993")));
    assert!(same(&n("9007199254740992"), &n("9007199254740992.0")));
    assert!(!same(
        &n("18446744073709551615"),
        &n("18446744073709551616.0")
    ));
    // The decoded `f64` of a large integer score is not equal to the fixture.
    let score: f64 = serde_json::from_str("9007199254740993").unwrap();
    assert!(!same(
        &n("9007199254740993"),
        &serde_json::to_value(score).unwrap()
    ));
}

fn mutated(file: &str, ptr: &str, key: &str, v: Value) -> Value {
    let mut body = load(file)["response"]["body"].take();
    obj(&mut body, ptr).insert(key.into(), v);
    body
}

/// `invalid_request.groups` has `minItems: 2`.
#[test]
fn groups_need_two_items() {
    let file = "search.mixed-score-space.json";
    let groups = load(file)["response"]["body"]["error"]["details"]["groups"].take();
    let one = json!([groups[0].clone()]);
    for (label, g) in [("empty", json!([])), ("one group", one)] {
        let body = mutated(file, "/error/details", "groups", g);
        assert!(
            decode::<ErrorEnvelope>(&body).is_err(),
            "groups {label} was accepted"
        );
    }
    assert!(decode::<ErrorEnvelope>(&mutated(file, "/error/details", "groups", groups)).is_ok());
}

/// `locator.end` has `minimum: 1`.
#[test]
fn locator_end_must_be_positive() {
    let file = "search.ok.json";
    let zero = json!({"valid": true, "start": 0, "end": 0});
    let body = mutated(file, "/hits/0", "locator", zero);
    assert!(
        decode::<SearchResponse>(&body).is_err(),
        "end 0 was accepted"
    );
    let one = json!({"valid": true, "start": 0, "end": 1});
    assert!(decode::<SearchResponse>(&mutated(file, "/hits/0", "locator", one)).is_ok());
}

/// `retryable` is fixed per code (§6) except for `unavailable`.
#[test]
fn retryable_is_fixed_per_code() {
    let wrong = [
        ("read.not-found.json", true),
        ("search.timeout.json", false),
        ("search.mixed-score-space.json", true),
        ("search.unauthorized-revoked.json", true),
        ("search.empty-scope.json", true),
        ("read.stale-anchor-missing.json", true),
        ("read.internal.json", true),
    ];
    for (file, v) in wrong {
        let body = mutated(file, "/error/details", "retryable", json!(v));
        assert!(
            decode::<ErrorEnvelope>(&body).is_err(),
            "{file}: retryable {v} was accepted"
        );
        let back =
            serde_json::to_value(decode::<ErrorEnvelope>(&load(file)["response"]["body"]).unwrap())
                .unwrap();
        assert_eq!(
            back["error"]["details"]["retryable"],
            json!(!v),
            "{file}: encoded"
        );
    }
    for v in [true, false] {
        let mut body = mutated(
            "search.unavailable-mixed.json",
            "/error/details",
            "retryable",
            json!(v),
        );
        // Drop the fields whose pairing with `retryable` is a schema rule.
        let details = obj(&mut body, "/error/details");
        details.remove("partial_failures");
        details.remove("reason");
        let decoded = decode::<ErrorEnvelope>(&body)
            .unwrap_or_else(|e| panic!("unavailable retryable {v}: {e}"));
        assert_eq!(
            serde_json::to_value(decoded).unwrap()["error"]["details"]["retryable"],
            json!(v)
        );
    }
}

fn read_request(file: &str, ptr: &str, key: &str, v: Value) -> Value {
    let mut req = part(file, "/request");
    obj(&mut req, ptr).insert(key.into(), v);
    req
}

/// `positive_int` (`max_bytes`, `deadline_ms`, `page.limit`) and the non-empty
/// `scope` are in the read request types.
#[test]
fn read_request_bounds() {
    let bad = [
        ("read.chunk.json", "", "max_bytes", json!(0)),
        ("read.chunk.json", "", "deadline_ms", json!(0)),
        ("read.document.json", "/page", "limit", json!(0)),
        (
            "read.document.json",
            "/scope",
            "knowledge_base_ids",
            json!([]),
        ),
    ];
    for (file, ptr, key, v) in bad {
        let req = read_request(file, ptr, key, v.clone());
        assert!(
            decode::<ReadRequest>(&req).is_err(),
            "{file}: {ptr}/{key} = {v} was accepted"
        );
    }
    for (file, ptr, key) in [
        ("read.chunk.json", "", "max_bytes"),
        ("read.chunk.json", "", "deadline_ms"),
        ("read.document.json", "/page", "limit"),
    ] {
        assert!(decode::<ReadRequest>(&read_request(file, ptr, key, json!(1))).is_ok());
    }
}

/// R4.3: with `chunk_id`, `expected_revision` has all three parts and a non-null
/// `chunk_revision`; without it, only the document pair.
#[test]
fn read_expected_revision_matches_target() {
    let full = json!({"source_revision": "s", "index_build_id": "b", "chunk_revision": 0});
    let null_rev = json!({"source_revision": "s", "index_build_id": "b", "chunk_revision": null});
    let pair = json!({"source_revision": "s", "index_build_id": "b"});
    let cases = [
        ("read.chunk.json", pair.clone(), false),
        ("read.chunk.json", null_rev.clone(), false),
        ("read.chunk.json", full.clone(), true),
        ("read.document.json", full, false),
        ("read.document.json", null_rev, false),
        ("read.document.json", pair, true),
    ];
    for (file, rev, ok) in cases {
        let req = read_request(file, "", "expected_revision", rev.clone());
        assert_eq!(
            decode::<ReadRequest>(&req).is_ok(),
            ok,
            "{file}: expected_revision {rev}"
        );
    }
}
