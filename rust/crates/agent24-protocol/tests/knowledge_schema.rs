//! KB-CT-02b (#1012): every Knowledge v1 fixture is validated against the full
//! `protocol/knowledge/v1` schemas, the same files and the same mapping as
//! `packages/contract-tests/src/knowledge-v1.test.ts` (Ajv). So a fixture edit
//! that breaks a schema rule turns Rust red too, not only TS, even after the
//! manifest digests are re-pinned. Rules the schemas cannot express (a response
//! against its request) are `knowledge_rules.rs`; `knowledge_fixtures.rs` keeps
//! the strict serde decoding as a smoke check; wire types come with KB-C1-02a.

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

/// `file`'s response body (or request) after `edit`, checked against `name`.
fn edited(file: &str, part: &str, edit: impl FnOnce(&mut Value)) -> Value {
    let mut doc = fixture(file);
    let target = if part == "request" {
        &mut doc["request"]
    } else {
        &mut doc["response"]["body"]
    };
    edit(target);
    target.clone()
}

fn drop_key(v: &mut Value, key: &str) {
    v.as_object_mut().unwrap().remove(key);
}

/// The canonical negatives of knowledge-v1.test.ts, run through this
/// validator too, so the two schema engines are held to the same verdicts.
#[test]
fn schema_negatives_match_the_ts_suite() {
    let v = validators();
    let bad = |name: &str, file: &str, part: &str, edit: &dyn Fn(&mut Value)| {
        check(&v, name, &edited(file, part, |d| edit(d)), false, file);
    };
    let good = |name: &str, file: &str, part: &str, edit: &dyn Fn(&mut Value)| {
        check(&v, name, &edited(file, part, |d| edit(d)), true, file);
    };
    let err = |file: &str, edit: &dyn Fn(&mut Value)| {
        bad("error", file, "body", &|b| edit(&mut b["error"]["details"]));
    };
    // search: partial needs partial_failures, complete has none; null chunk_revision means unknown.
    bad("search.response", "search.partial.json", "body", &|b| {
        b["partial_failures"] = json!([])
    });
    bad("search.response", "search.partial.json", "body", &|b| {
        b["completeness"] = json!("complete")
    });
    bad("search.response", "search.partial.json", "body", &|b| {
        b["completeness"] = json!("complete");
        b["partial_failures"] = json!([]);
    });
    bad("search.response", "search.ok.json", "body", &|b| {
        b["hits"][2]["revision"]["chunk_revision"] = Value::Null
    });
    // read: chunk expected_revision needs content_tag; document pair is two parts; page.limit optional.
    good("read.request", "read.chunk.json", "request", &|r| {
        drop_key(&mut r["expected_revision"], "chunk_revision")
    });
    good("read.request", "read.chunk.json", "request", &|r| {
        r["expected_revision"] = json!({ "content_tag": "ct-chk-71", "chunk_revision": 1 });
    });
    bad("read.request", "read.chunk.json", "request", &|r| {
        r["expected_revision"] =
            json!({ "source_revision": "s", "index_build_id": "i", "chunk_revision": 1 });
    });
    // r9: chunk_revision is informational; a hit's null may be sent back as is.
    good("read.request", "read.chunk.json", "request", &|r| {
        r["expected_revision"]["chunk_revision"] = Value::Null
    });
    good("read.request", "read.document.json", "request", &|r| {
        r["expected_revision"] = json!({ "source_revision": "s", "index_build_id": "i" });
    });
    bad("read.request", "read.document.json", "request", &|r| {
        r["expected_revision"]["content_tag"] = json!("ct-doc");
    });
    bad("read.request", "read.document.json", "request", &|r| {
        r["expected_revision"]["chunk_revision"] = json!(0)
    });
    good("read.request", "read.chunk.json", "request", &|r| {
        r["page"] = json!({ "cursor": "cur-71-1" })
    });
    bad("read.request", "read.chunk.json", "request", &|r| {
        r["page"] = json!({ "limit": 5 })
    });
    // errors: retryable, reasons and details are fixed per code (§6, r8).
    err("read.out-of-scope.json", &|d| {
        d["reason"] = json!("out_of_scope")
    });
    err("read.stale-anchor-missing.json", &|d| {
        d["current_chunk_revision"] = json!(3)
    });
    err("read.stale-anchor-missing.json", &|d| {
        d["current_chunk_revision"] = json!(3);
        d["current_content_tag"] = json!("ct-chk-70");
    });
    err("read.stale-chunk-edited.json", &|d| {
        drop_key(d, "current_content_tag");
    });
    err("search.unavailable-mixed.json", &|d| {
        d["retryable"] = json!(true)
    });
    err("search.unavailable-mixed.json", &|d| {
        d["reason"] = json!("dependency");
        d["retryable"] = json!(true);
    });
    err("search.unavailable-mixed.json", &|d| {
        drop_key(d, "reason");
        d["retryable"] = json!(true);
    });
    err("search.unavailable-mixed.json", &|d| drop_key(d, "reason"));
    err("search.unavailable-mixed.json", &|d| {
        drop_key(d, "partial_failures");
        d["reason"] = json!("dependency");
    });
    err("read.out-of-scope.json", &|d| d["request_id"] = Value::Null);
    good("error", "read.internal.json", "body", &|b| {
        b["error"]["details"]["request_id"] = Value::Null;
        b["error"]["details"]["generation"] = Value::Null;
    });
    err("search.mixed-score-space.json", &|d| {
        d["request_id"] = Value::Null;
        d["generation"] = Value::Null;
    });
    err("read.stale-chunk-edited.json", &|d| {
        d["current_revision"]["index_build_id"] = Value::Null
    });
    bad("error", "search.empty-scope.json", "body", &|b| {
        b["error"]["code"] = json!("cancelled")
    });
    err("search.mixed-score-space.json", &|d| drop_key(d, "groups"));
    // R6.4: protocol_version is checked first, so unsupported_protocol_version may echo null.
    good("error", "read.unsupported-version.json", "body", &|b| {
        b["error"]["details"]["request_id"] = Value::Null;
        b["error"]["details"]["generation"] = Value::Null;
    });
    err("search.unauthorized-revoked.json", &|d| {
        drop_key(d, "reason")
    });
    err("search.empty-scope.json", &|d| drop_key(d, "reason"));
    err("search.unavailable-mixed.json", &|d| {
        d["partial_failures"][0]["reason"] = json!("overloaded")
    });
    err("search.unavailable-mixed.json", &|d| {
        d["partial_failures"][1]["reason"] = json!("unsupported_kb_type")
    });
    err("search.unavailable-mixed.json", &|d| {
        d["reason"] = json!("unsupported_kb_type")
    });
    err("search.unavailable-mixed.json", &|d| {
        d["partial_failures"][1] = json!({ "knowledge_base_id": "kb-c", "code": "unavailable", "reason": "dependency", "retryable": true });
    });
    err("read.index-pending.json", &|d| {
        d["partial_failures"] = json!([{ "knowledge_base_id": "kb-c", "code": "unavailable", "reason": "dependency", "retryable": true }]);
    });
    // ready: infrastructure not ready makes every KB not_ready/dependency (r6).
    let dep = |kb: Value| move |b: &mut Value| b["knowledge_bases"][1]["search"] = kb.clone();
    bad(
        "ready.response",
        "ready.dependency.json",
        "body",
        &dep(json!({ "state": "ready" })),
    );
    bad(
        "ready.response",
        "ready.dependency.json",
        "body",
        &dep(json!({ "state": "not_ready", "reason": "index_building" })),
    );
    bad("ready.response", "ready.dependency.json", "body", &|b| {
        b["knowledge_bases"] = json!([])
    });
    // The same rules for read (R8.2, R8.4).
    let read_down = |kb: Option<Value>| {
        move |b: &mut Value| {
            b["operations"]["read"] =
                json!({ "state": "not_ready", "reason": "vector_store_unavailable" });
            for i in 0..2 {
                if let Some(kb) = &kb {
                    b["knowledge_bases"][i]["read"] = kb.clone();
                }
            }
        }
    };
    let ready = "ready.response";
    bad(ready, "ready.dependency.json", "body", &read_down(None));
    let building = json!({ "state": "not_ready", "reason": "index_building" });
    bad(
        ready,
        "ready.dependency.json",
        "body",
        &read_down(Some(building)),
    );
    let dependency = json!({ "state": "not_ready", "reason": "dependency" });
    good(
        ready,
        "ready.dependency.json",
        "body",
        &read_down(Some(dependency)),
    );
    // ready: closed KB reasons; operation_disabled exactly when the operation is disabled (§12 #19).
    for kb in [
        json!({ "state": "banana" }),
        json!({ "state": "not_ready", "reason": "whatever" }),
        json!({ "state": "not_ready" }),
        json!({ "state": "ready", "reason": "dependency" }),
        json!({ "state": "not_ready", "reason": "operation_disabled" }),
        // R8.4 (r8): dependency only when the infrastructure is not_ready.
        json!({ "state": "not_ready", "reason": "dependency" }),
    ] {
        bad("ready.response", "ready.scoped.json", "body", &|b| {
            b["knowledge_bases"][0]["search"] = kb.clone()
        });
    }
    for kb in [
        json!({ "state": "ready" }),
        json!({ "state": "not_ready" }),
        json!({ "state": "not_ready", "reason": "index_building" }),
    ] {
        bad(
            "ready.response",
            "ready.operation-disabled.json",
            "body",
            &|b| b["knowledge_bases"][0]["search"] = kb.clone(),
        );
    }
    bad(
        "ready.response",
        "ready.operation-disabled.json",
        "body",
        &|b| {
            b["knowledge_bases"][0]["read"] =
                json!({ "state": "not_ready", "reason": "operation_disabled" });
        },
    );
}
