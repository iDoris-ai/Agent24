//! KB-CT-02b (#1012): Knowledge v1 fixture lockstep. `protocol/knowledge/fixtures/v1`
//! is registered here explicitly (`fixtures_roundtrip.rs` only reads
//! `protocol/fixtures/events`). The manifest, and through it every fixture's
//! version and sha256, is pinned (CONTRACT-READONLY-v1 r8). Every error envelope
//! and partial failure decodes into strict types (unknown fields rejected,
//! required fields present, `null` only where the contract allows it).
//! `packages/contract-tests/src/knowledge-v1.test.ts` validates the same files
//! against the full `protocol/knowledge/v1` schemas.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde::Deserialize;
use serde::de::{DeserializeOwned, Deserializer};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const KNOWLEDGE_FIXTURES: &str = "protocol/knowledge/fixtures/v1";

/// sha256 of `manifest.json`, which pins every fixture's version and digest.
/// Changing a fixture means bumping its version, re-pinning its sha256 and
/// updating this constant together with `MANIFEST_SHA256` in knowledge-v1.test.ts.
const MANIFEST_SHA256: &str = "e72773edb4d6a5f09d52f1f2c2b0efc0591c97fe629d6294d6d3d4a4484b94df";

/// Cases the contract names (#1012; §11.3, §11.7; §12 #11, #18, #19; r6, r8); never
/// fewer. The TS test carries the same list.
const REQUIRED: &[&str] = &[
    "search.empty-scope.json",
    "search.scope-empty-array.json",
    "search.partial.json",
    "search.partial-overloaded.json",
    "search.cancelled.json",
    "search.timeout.json",
    "search.unauthorized-revoked.json",
    "read.out-of-scope.json",
    "read.stale-chunk-edited.json",
    "read.stale-anchor-missing.json",
    "read.stale-document.json",
    "read.stale-document-building.json",
    "ready.scoped.json",
    "ready.dependency.json",
    "capabilities.integration.json",
    "search.invalid-body.json",
    "search.unauthorized-missing.json",
    "search.operation-not-allowed.json",
    "search.operation-disabled.json",
    "search.unauthorized-before-parse.json",
    "read.policy-denied.json",
    "read.timeout.json",
    "read.index-failed.json",
    "read.expected-revision-partial.json",
    "read.stale-page.json",
    "ready.operation-disabled.json",
    "read.operation-disabled.json",
];

fn dir() -> PathBuf {
    // crate at rust/crates/agent24-protocol → repo root is ../../..
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .join(KNOWLEDGE_FIXTURES)
}

fn sha256(file: &str) -> String {
    hex::encode(Sha256::digest(fs::read(dir().join(file)).unwrap()))
}

fn load<T: DeserializeOwned>(file: &str) -> T {
    let raw = fs::read_to_string(dir().join(file)).unwrap();
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn decode<T: DeserializeOwned>(file: &str, value: &Value) -> T {
    serde_json::from_value(value.clone()).unwrap_or_else(|e| panic!("{file}: {e}"))
}

/// A key that must be present but may be `null`. A bare `Option` field would
/// also accept a missing key.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(d)
}

/// An optional key that, when present, must not be `null`. Use with
/// `#[serde(default)]`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Deserialize)]
struct Manifest {
    contract_revision: String,
    protocol_version: u8,
    fixtures: Vec<Entry>,
}

#[derive(Deserialize)]
struct Entry {
    file: String,
    version: u32,
    class: String,
    request_valid: Option<bool>,
    server_handler: bool,
    sha256: String,
}

#[derive(Deserialize)]
struct Fixture {
    op: String,
    request: Option<Value>,
    response: Wire,
    local: Option<Value>,
}

#[derive(Deserialize)]
struct Wire {
    status: u16,
    body: Value,
}

/// Requests and success bodies carry these; errors echo them in details (R7.2).
#[derive(Deserialize, PartialEq, Debug)]
struct Echo {
    request_id: String,
    generation: u64,
}

// ---- error envelope (contract §6, R6.1, R7.3) ----

#[derive(Deserialize, PartialEq, Debug)]
#[serde(rename_all = "snake_case")]
enum Code {
    InvalidRequest,
    Unauthorized,
    Forbidden,
    NotFound,
    Stale,
    Timeout,
    Unavailable,
    Internal,
    Cancelled,
}

impl Code {
    /// §6 (r8): HTTP status, fixed `retryable`, `reason` required (true) or
    /// forbidden (false); `None` = it depends. `cancelled` is never on the wire.
    fn wire(&self) -> Option<(u16, Option<bool>, Option<bool>)> {
        Some(match self {
            Code::InvalidRequest => (400, Some(false), None),
            Code::Unauthorized => (401, Some(false), Some(true)),
            Code::Forbidden => (403, Some(false), Some(true)),
            Code::NotFound => (404, Some(false), Some(false)),
            Code::Stale => (409, Some(false), None),
            Code::Timeout => (504, Some(true), None),
            Code::Unavailable => (503, None, None),
            Code::Internal => (500, Some(false), Some(false)),
            Code::Cancelled => return None,
        })
    }

    /// R6.1, R6.4: only these may echo `null` (body unreadable, ids invalid, or
    /// failed before parsing); `mixed_score_space` comes after E, so it echoes.
    fn may_echo_null(&self, reason: Option<&str>) -> bool {
        match self {
            Code::Unauthorized | Code::Internal => true,
            Code::InvalidRequest => reason != Some("mixed_score_space"),
            _ => false,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[allow(dead_code)] // decoded to check the shape; not every field is read
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorBody {
    code: Code,
    message: String,
    #[serde(default, deserialize_with = "present")]
    hint: Option<String>,
    details: Details,
}

#[allow(dead_code)] // decoded to check the shape; not every field is read
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Details {
    retryable: bool,
    #[serde(deserialize_with = "nullable")]
    request_id: Option<String>,
    #[serde(deserialize_with = "nullable")]
    generation: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    reason: Option<String>,
    #[serde(default, deserialize_with = "present")]
    current_revision: Option<CurrentRevision>,
    #[serde(default, deserialize_with = "present")]
    current_chunk_revision: Option<u64>,
    #[serde(default, deserialize_with = "present")]
    groups: Option<Vec<Group>>,
    #[serde(default, deserialize_with = "present")]
    supported: Option<Vec<u32>>,
    #[serde(default, deserialize_with = "present")]
    partial_failures: Option<Vec<PartialFailure>>,
    #[serde(default, deserialize_with = "present")]
    retry_after_ms: Option<u64>,
    /// Provider-local `cancelled` only (R7.3).
    #[serde(default, deserialize_with = "present")]
    current_generation: Option<u64>,
}

#[allow(dead_code)] // decoded to check the shape; not every field is read
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CurrentRevision {
    source_revision: String,
    #[serde(deserialize_with = "nullable")]
    index_build_id: Option<String>,
}

#[allow(dead_code)] // decoded to check the shape; not every field is read
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Group {
    score_space: String,
    knowledge_base_ids: Vec<String>,
}

#[allow(dead_code)] // decoded to check the shape; not every field is read
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PartialFailure {
    knowledge_base_id: String,
    code: FailCode,
    reason: FailReason,
    retryable: bool,
}

/// `partial_failures[].code` is the subset R3.5 allows.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailCode {
    Timeout,
    Unavailable,
}

/// R3.5 (r8).
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum FailReason {
    Timeout,
    Dependency,
    NotReady,
    Overloaded,
    IndexBuilding,
    UnsupportedKbType,
    IndexFailed,
}

// ---- tests ----

#[test]
fn manifest_pins_every_knowledge_fixture() {
    assert_eq!(
        sha256("manifest.json"),
        MANIFEST_SHA256,
        "改夹具要升 version，并更新清单摘要常量 (MANIFEST_SHA256, Rust and TS)"
    );
    let m: Manifest = load("manifest.json");
    assert_eq!(m.contract_revision, "r8");
    assert_eq!(m.protocol_version, 1);
    let listed: BTreeSet<&str> = m.fixtures.iter().map(|e| e.file.as_str()).collect();
    assert_eq!(listed.len(), m.fixtures.len(), "duplicate manifest entry");
    assert!(
        m.fixtures.iter().all(|e| e.version >= 1),
        "versions start at 1"
    );
    let on_disk: BTreeSet<String> = fs::read_dir(dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n != "manifest.json")
        .collect();
    let on_disk: BTreeSet<&str> = on_disk.iter().map(String::as_str).collect();
    assert_eq!(listed, on_disk, "manifest and directory disagree");
    for required in REQUIRED {
        assert!(listed.contains(required), "missing {required}");
    }
    for e in &m.fixtures {
        let file = &e.file;
        assert!(
            matches!(e.class.as_str(), "accept" | "reject"),
            "{file}: class"
        );
        // A malformed request can only be answered by an error.
        assert!(
            e.request_valid != Some(false) || e.class == "reject",
            "{file}"
        );
        // §12 #18: only the cancel fixture stays out of server tests.
        assert_eq!(e.server_handler, file != "search.cancelled.json", "{file}");
    }
    for e in &m.fixtures {
        assert_eq!(
            sha256(&e.file),
            e.sha256,
            "{}: digest changed; bump its version and pin the new sha256",
            e.file
        );
    }
}

#[test]
fn every_knowledge_fixture_decodes_strictly() {
    for e in load::<Manifest>("manifest.json").fixtures {
        let file = e.file.as_str();
        let fx: Fixture = load(file);
        let body = &fx.response.body;
        assert_eq!(fx.request.is_none(), e.request_valid.is_none(), "{file}");
        assert_eq!(fx.local.is_some(), !e.server_handler, "{file}");
        // Malformed requests still carry the ids the error must echo; a raw,
        // unparseable body (a string) has nothing to echo (R6.1).
        let echo = fx
            .request
            .as_ref()
            .filter(|r| r.is_object())
            .map(|r| decode::<Echo>(file, r));
        if let Some(raw) = fx.request.as_ref().and_then(Value::as_str) {
            // R6.1: a raw request stands for a body that does not parse.
            assert!(
                serde_json::from_str::<Value>(raw).is_err(),
                "{file}: raw request parses"
            );
            let details = &body["error"]["details"];
            assert!(
                details["request_id"].is_null() && details["generation"].is_null(),
                "{file}"
            );
        }
        let echoed = match e.class.as_str() {
            "accept" => {
                assert_eq!(fx.response.status, 200, "{file}");
                body
            }
            "reject" => {
                let err = decode::<ErrorEnvelope>(file, body).error;
                let (status, retryable, reason) = err.code.wire().expect("not on the wire");
                assert_eq!(fx.response.status, status, "{file}");
                if let Some(r) = retryable {
                    assert_eq!(err.details.retryable, r, "{file}");
                }
                if let Some(must) = reason {
                    assert_eq!(err.details.reason.is_some(), must, "{file}: reason");
                }
                // §6 (r8): a non-retryable unavailable names the non-retryable cause.
                if err.code == Code::Unavailable && !err.details.retryable {
                    let r = err.details.reason.as_deref();
                    assert!(
                        matches!(r, Some("unsupported_kb_type" | "index_failed")),
                        "{file}"
                    );
                }
                let null_echo =
                    err.details.request_id.is_none() || err.details.generation.is_none();
                let may = err.code.may_echo_null(err.details.reason.as_deref());
                assert!(!null_echo || may, "{file}: null echo");
                assert!(err.details.current_generation.is_none(), "{file}");
                &body["error"]["details"]
            }
            other => panic!("{file}: unknown class {other}"),
        };
        if let Some(echo) = echo {
            // R7.2: echoed as sent; a null allowed above (R6.1) is not compared.
            if !echoed["request_id"].is_null() {
                assert_eq!(echoed["request_id"], echo.request_id.as_str(), "{file}");
            }
            if !echoed["generation"].is_null() {
                assert_eq!(
                    echoed["generation"].as_u64(),
                    Some(echo.generation),
                    "{file}"
                );
            }
        }
    }
}

#[test]
fn late_response_fixture_is_consistent() {
    // §11.4 / R7.3 / §12 #18: the fixture's late success, its stale generation
    // and its local cancelled result agree with each other. Nothing here drops
    // anything: once the Provider lands (KB-C1-02a), a test must drive the
    // Provider with this fixture.
    let file = "search.cancelled.json";
    let fx: Fixture = load(file);
    assert_eq!(
        (fx.op.as_str(), fx.response.status),
        ("knowledge.search", 200)
    );
    let local = fx.local.expect("cancel fixture carries the local part");
    let current_generation = local["current_generation"].as_u64().unwrap();
    let request: Echo = decode(file, &fx.request.unwrap());
    let late: Echo = decode(file, &fx.response.body);
    let result = decode::<ErrorEnvelope>(file, &local["result"]).error;

    assert_eq!(late, request, "the response answers the in-flight request");
    assert_ne!(late.generation, current_generation, "generation moved on");
    assert_eq!(result.code, Code::Cancelled);
    assert!(!result.details.retryable);
    assert_eq!(result.details.current_generation, Some(current_generation));
    let echoed: Echo = decode(file, &local["result"]["error"]["details"]);
    assert_eq!(echoed, late, "the local result names the dropped response");
}
