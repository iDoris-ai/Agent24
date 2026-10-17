//! KB-CT-02b (#1012): every Knowledge v1 fixture is validated against the full
//! `protocol/knowledge/v1` schemas, the same files and the same mapping as
//! `packages/contract-tests/src/knowledge-v1.test.ts` (Ajv). So a fixture edit
//! that breaks a cross-field rule turns Rust red too, not only TS, even after
//! the manifest digests are re-pinned. `knowledge_fixtures.rs` keeps the strict
//! serde decoding as a smoke check; wire types come with KB-C1-02a.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use jsonschema::{Registry, Validator};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const BASE: &str = "https://github.com/iDoris-ai/Agent24/protocol/knowledge/v1/";

fn root() -> PathBuf {
    // crate at rust/crates/agent24-protocol → repo root is ../../..
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn read(path: PathBuf) -> Value {
    let raw = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixture(file: &str) -> Value {
    read(root().join("protocol/knowledge/fixtures/v1").join(file))
}

/// One validator per schema entry point, keyed like the TS test.
fn validators() -> BTreeMap<&'static str, Validator> {
    let mut schemas = Vec::new();
    for entry in fs::read_dir(root().join("protocol/knowledge/v1")).unwrap() {
        let schema = read(entry.unwrap().path());
        let id = schema["$id"]
            .as_str()
            .expect("every schema has an $id")
            .to_owned();
        assert!(id.starts_with(BASE), "{id}");
        schemas.push((id, schema));
    }
    let registry = Registry::new().extend(schemas).unwrap().prepare().unwrap();
    let compile = |reference: &str| {
        jsonschema::options()
            .offline()
            .with_registry(&registry)
            .build(&json!({ "$ref": format!("{BASE}{reference}") }))
            .unwrap_or_else(|e| panic!("{reference}: {e}"))
    };
    let mut v = BTreeMap::new();
    for (key, reference) in [
        ("search.request", "search.schema.json#/$defs/request"),
        ("search.response", "search.schema.json#/$defs/response"),
        ("read.request", "read.schema.json#/$defs/request"),
        ("read.response", "read.schema.json#/$defs/response"),
        ("ready.request", "ready.schema.json#/$defs/request"),
        ("ready.response", "ready.schema.json#/$defs/response"),
        ("error", "error.schema.json"),
        ("cancelled", "local-cancelled.schema.json"),
        ("capabilities", "capabilities.schema.json"),
    ] {
        v.insert(key, compile(reference));
    }
    v
}

fn check(v: &BTreeMap<&str, Validator>, name: &str, doc: &Value, want: bool, file: &str) {
    let errors: Vec<String> = v[name].iter_errors(doc).map(|e| e.to_string()).collect();
    assert_eq!(errors.is_empty(), want, "{file}: {name}: {errors:?}");
}

fn short(op: &str) -> &str {
    op.strip_prefix("knowledge.").unwrap_or(op)
}

#[test]
fn every_knowledge_fixture_matches_the_schemas() {
    let v = validators();
    let manifest = fixture("manifest.json");
    let entries = manifest["fixtures"].as_array().unwrap();
    assert!(!entries.is_empty());
    for e in entries {
        let file = e["file"].as_str().unwrap();
        let fx = fixture(file);
        let op = fx["op"].as_str().unwrap();
        match &e["request_valid"] {
            Value::Null => assert!(fx.get("request").is_none(), "{file}"),
            Value::Bool(want) => {
                check(
                    &v,
                    &format!("{}.request", short(op)),
                    &fx["request"],
                    *want,
                    file,
                );
            }
            other => panic!("{file}: request_valid {other}"),
        }
        let body = &fx["response"]["body"];
        match e["class"].as_str().unwrap() {
            "accept" if op == "system.capabilities" => check(&v, "capabilities", body, true, file),
            "accept" => check(&v, &format!("{}.response", short(op)), body, true, file),
            "reject" => check(&v, "error", body, true, file),
            other => panic!("{file}: class {other}"),
        }
        if let Some(local) = fx.get("local") {
            // §11.4: the Provider-local cancelled result is never a wire error.
            check(&v, "cancelled", &local["result"], true, file);
            check(&v, "error", &local["result"], false, file);
        }
    }
}

/// Cross-field rules the serde smoke check cannot see; each edit must fail.
#[test]
fn schema_rejects_what_serde_alone_accepts() {
    let v = validators();
    let mut ready = fixture("ready.operation-disabled.json")["response"]["body"].clone();
    ready["knowledge_bases"][0]["search"] =
        json!({ "state": "not_ready", "reason": "index_building" });
    check(
        &v,
        "ready.response",
        &ready,
        false,
        "ready.operation-disabled.json",
    );
    let mut scoped = fixture("ready.scoped.json")["response"]["body"].clone();
    scoped["knowledge_bases"][1]["search"]["reason"] = json!("whatever");
    check(&v, "ready.response", &scoped, false, "ready.scoped.json");
    let mut partial = fixture("search.partial.json")["response"]["body"].clone();
    partial["completeness"] = json!("complete");
    check(
        &v,
        "search.response",
        &partial,
        false,
        "search.partial.json",
    );
    let mut revoked = fixture("search.unauthorized-revoked.json")["response"]["body"].clone();
    revoked["error"]["details"]
        .as_object_mut()
        .unwrap()
        .remove("reason");
    check(
        &v,
        "error",
        &revoked,
        false,
        "search.unauthorized-revoked.json",
    );
}
