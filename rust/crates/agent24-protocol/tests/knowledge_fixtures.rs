//! KB-CT-02b (#1012): Knowledge v1 fixture lockstep. `protocol/knowledge/fixtures/v1`
//! is registered here explicitly (`fixtures_roundtrip.rs` only reads
//! `protocol/fixtures/events`). The manifest, and through it every fixture's
//! version and sha256, is pinned (CONTRACT-READONLY-v1 r9). Every body decodes
//! into the formal `agent24-knowledge` wire types (#1052; their round trips are in
//! that crate), and the rules a type cannot carry are checked here: HTTP status
//! and `retryable` per code, which errors may echo `null`, and the late-response
//! fixture. `knowledge_schema.rs` (Rust) and
//! `packages/contract-tests/src/knowledge-v1.test.ts` (Ajv) validate the same
//! files against the full `protocol/knowledge/v1` schemas.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_knowledge::{
    CapabilitiesData, ErrorCode, ErrorEnvelope, LocalCancelled, Locator, ReadResponse,
    ReadyResponse, SearchResponse,
};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

const KNOWLEDGE_FIXTURES: &str = "protocol/knowledge/fixtures/v1";

/// sha256 of `manifest.json`, which pins every fixture's version and digest.
/// Changing a fixture means bumping its version, re-pinning its sha256 and
/// updating this constant together with `MANIFEST_SHA256` in knowledge-v1.test.ts.
const MANIFEST_SHA256: &str = "cabf69d4a7d8c31a6ca6ca3ed21fb9a1ad96a6d398f2a3e7d26e015d7361f039";

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

// ---- rules a type cannot carry (contract §6, R6.1, R6.4, R7.3) ----

/// §6 (r8): HTTP status and the fixed `retryable` of each wire code; `None` =
/// it depends (`unavailable`).
fn wire(code: ErrorCode) -> (u16, Option<bool>) {
    match code {
        ErrorCode::InvalidRequest => (400, Some(false)),
        ErrorCode::Unauthorized => (401, Some(false)),
        ErrorCode::Forbidden => (403, Some(false)),
        ErrorCode::NotFound => (404, Some(false)),
        ErrorCode::Stale => (409, Some(false)),
        ErrorCode::Timeout => (504, Some(true)),
        ErrorCode::Unavailable => (503, None),
        ErrorCode::Internal => (500, Some(false)),
    }
}

/// R6.1, R6.4: only these may echo `null` (body unreadable, ids invalid, or
/// failed before parsing); `mixed_score_space` comes after E, so it echoes.
fn may_echo_null(code: ErrorCode, reason: Option<&str>) -> bool {
    match code {
        ErrorCode::Unauthorized | ErrorCode::Internal => true,
        ErrorCode::InvalidRequest => reason != Some("mixed_score_space"),
        _ => false,
    }
}

/// Decode a success body into its formal type and check what the type cannot:
/// search rank order, valid locators, one `score_space`; ready `knowledge_bases`
/// exactly when the request carried scope (R8.2). `scoped`: the request carried
/// scope.
fn success(file: &str, op: &str, body: &Value, scoped: bool) {
    match op {
        "knowledge.search" => {
            let r: SearchResponse = decode(file, body);
            // R3.6: authoritative order with rank 1..n, one score_space per response.
            for (i, hit) in r.hits.iter().enumerate() {
                assert_eq!(hit.rank.get(), i as u64 + 1, "{file}: rank");
                if let Locator::Valid { start, end } = hit.locator {
                    // R3.10: a valid locator carries a non-empty [start, end).
                    assert!(end.get() > start, "{file}: locator {start}..{end}");
                }
            }
            let spaces: BTreeSet<&str> = r.hits.iter().map(|h| h.score_space.as_str()).collect();
            assert!(spaces.len() <= 1, "{file}: score_space {spaces:?}");
        }
        "knowledge.read" => {
            decode::<ReadResponse>(file, body);
        }
        "knowledge.ready" => {
            let r: ReadyResponse = decode(file, body);
            assert_eq!(
                r.knowledge_bases.is_some(),
                scoped,
                "{file}: knowledge_bases"
            );
        }
        "system.capabilities" => {
            decode::<CapabilitiesData>(file, body);
        }
        op => panic!("{file}: unknown op {op}"),
    }
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
    assert_eq!(m.contract_revision, "r9");
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
        let scoped = fx
            .request
            .as_ref()
            .is_some_and(|r| r.get("scope").is_some());
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
                success(file, &fx.op, body, scoped);
                body
            }
            "reject" => {
                let env: ErrorEnvelope = decode(file, body);
                let code = env.error.code();
                let (status, retryable) = wire(code);
                assert_eq!(fx.response.status, status, "{file}");
                let details = &body["error"]["details"];
                if let Some(r) = retryable {
                    assert_eq!(details["retryable"].as_bool(), Some(r), "{file}");
                }
                // §6 (r8): a non-retryable unavailable names the non-retryable cause.
                if code == ErrorCode::Unavailable && details["retryable"] == false {
                    let r = details["reason"].as_str();
                    assert!(
                        matches!(r, Some("unsupported_kb_type" | "index_failed")),
                        "{file}"
                    );
                }
                let null_echo = details["request_id"].is_null() || details["generation"].is_null();
                assert!(
                    !null_echo || may_echo_null(code, details["reason"].as_str()),
                    "{file}: null echo"
                );
                details
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
    success(file, &fx.op, &fx.response.body, true);
    let result: LocalCancelled = decode(file, &local["result"]);

    assert_eq!(late, request, "the response answers the in-flight request");
    assert_ne!(late.generation, current_generation, "generation moved on");
    assert!(!result.error.details.retryable.get());
    assert_eq!(result.error.details.current_generation, current_generation);
    let echoed: Echo = decode(file, &local["result"]["error"]["details"]);
    assert_eq!(echoed, late, "the local result names the dropped response");
}
