//! KB-CT-03③ (#1668): the knowledge entry part of CONTRACT-OPS-v2 (§8). The
//! same fixtures as `packages/contract-tests/src/knowledge-ops-v2.test.ts`
//! (Ajv) run through this validator, so the two schema engines give every case
//! the same verdict, and every hash sample recomputes under scheme 1 (W8.13)
//! with serde_json's serializer as well as `JSON.stringify`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use jsonschema::{Registry, Validator};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

const BASE: &str = "https://github.com/iDoris-ai/Agent24/protocol/knowledge/ops-v2/";
/// Pins both fixture files; re-pin together with knowledge-ops-v2.test.ts.
const FIXTURE_SHA256: [(&str, &str); 2] = [
    (
        "entry-hash.samples.json",
        "c8069d60b6ee2e5789aa2cfa7a4dfb1c08228cfef16ca78bbdf8fedc2cbf5b2e",
    ),
    (
        "schema-cases.json",
        "8ec64a631b6550a3dc1a123a7ea92bcab5f0f581c4f9df09d266b682f96e81f4",
    ),
];
const DEFS: [(&str, &[&str]); 2] = [
    (
        "candidate",
        &[
            "submit_request",
            "submit_response",
            "withdraw_request",
            "withdraw_response",
            "get_request",
            "get_response",
            "return_request",
            "return_response",
            "approve_request",
            "approve_response",
            "view",
        ],
    ),
    ("entry", &["withdraw_request", "withdraw_response"]),
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn fixture_dir() -> PathBuf {
    root().join("protocol/knowledge/fixtures/ops-v2")
}

fn read(path: PathBuf) -> Value {
    let raw = fs::read_to_string(&path).unwrap();
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn validators() -> BTreeMap<String, Validator> {
    let mut schemas = Vec::new();
    for entry in fs::read_dir(root().join("protocol/knowledge/ops-v2")).unwrap() {
        let schema = read(entry.unwrap().path());
        let id = schema["$id"]
            .as_str()
            .expect("every schema has an $id")
            .to_owned();
        assert!(id.starts_with(BASE), "{id}");
        schemas.push((id, schema));
    }
    let registry = Registry::new().extend(schemas).unwrap().prepare().unwrap();
    let mut v = BTreeMap::new();
    for (file, defs) in DEFS {
        for def in defs {
            let reference = format!("{BASE}{file}.schema.json#/$defs/{def}");
            let validator = jsonschema::options()
                .offline()
                .with_registry(&registry)
                .build(&json!({ "$ref": reference }))
                .unwrap_or_else(|e| panic!("{reference}: {e}"));
            v.insert(format!("{file}.{def}"), validator);
        }
    }
    v
}

#[test]
fn the_fixture_directory_is_pinned() {
    let mut on_disk: Vec<String> = fs::read_dir(fixture_dir())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    on_disk.sort();
    let pinned: Vec<String> = FIXTURE_SHA256
        .iter()
        .map(|(f, _)| (*f).to_owned())
        .collect();
    assert_eq!(on_disk, pinned);
    for (file, want) in FIXTURE_SHA256 {
        let bytes = fs::read(fixture_dir().join(file)).unwrap();
        assert_eq!(
            hex(&Sha256::digest(&bytes)),
            want,
            "{file}: re-pin here and in the TS test"
        );
    }
}

#[test]
fn every_schema_case_gets_its_verdict() {
    let v = validators();
    let cases = read(fixture_dir().join("schema-cases.json"));
    let cases = cases["cases"].as_array().unwrap();
    assert!(cases.len() > 40);
    for c in cases {
        let name = c["name"].as_str().unwrap();
        let schema = c["schema"].as_str().unwrap();
        let validator = v
            .get(schema)
            .unwrap_or_else(|| panic!("{name}: unknown schema {schema}"));
        let errors: Vec<String> = validator
            .iter_errors(&c["doc"])
            .map(|e| e.to_string())
            .collect();
        assert_eq!(
            errors.is_empty(),
            c["valid"].as_bool().unwrap(),
            "{name}: {errors:?}"
        );
    }
    for key in v.keys() {
        assert!(
            cases
                .iter()
                .any(|c| c["schema"] == key.as_str() && c["valid"] == true),
            "{key} has no accepted case"
        );
    }
}

#[test]
fn every_entry_hash_sample_recomputes_under_scheme_1() {
    let fx = read(fixture_dir().join("entry-hash.samples.json"));
    assert_eq!(fx["scheme"], 1);
    for s in fx["samples"].as_array().unwrap() {
        let name = s["name"].as_str().unwrap();
        let serialized =
            serde_json::to_string(&json!([1, s["kind"], s["title"], s["body"]])).unwrap();
        assert_eq!(serialized, s["serialized"].as_str().unwrap(), "{name}");
        assert_eq!(
            hex(&Sha256::digest(serialized.as_bytes())),
            s["sha256"].as_str().unwrap(),
            "{name}"
        );
    }
}
