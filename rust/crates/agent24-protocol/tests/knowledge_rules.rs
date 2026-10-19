//! KB-CT-02b (#1012): the cross-field rules JSON Schema cannot express — a
//! response against its own request, a body against the contract header. The
//! same checks run in `packages/contract-tests/src/knowledge-v1.test.ts`, so a
//! fixture edit that breaks one of them turns both sides red even after the
//! manifest digests are re-pinned. Schema validation is `knowledge_schema.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::Value;
use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn fixture(file: &str) -> Value {
    let path = root().join("protocol/knowledge/fixtures/v1").join(file);
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

fn ids(v: &Value) -> Vec<&str> {
    v.as_array()
        .map(|a| a.iter().map(|x| x.as_str().unwrap()).collect())
        .unwrap_or_default()
}

fn unique(file: &str, what: &str, items: &[&str]) {
    let set: BTreeSet<&str> = items.iter().copied().collect();
    assert_eq!(set.len(), items.len(), "{file}: duplicate {what} {items:?}");
}

#[test]
fn manifest_pins_the_contract_header_revision() {
    let contract =
        fs::read_to_string(root().join("docs/knowledge/CONTRACT-READONLY-v1.md")).unwrap();
    let header = contract
        .split("文档修订 ")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_alphanumeric()).next())
        .expect("contract header names its revision");
    assert_eq!(fixture("manifest.json")["contract_revision"], header);
}

#[test]
fn every_knowledge_fixture_keeps_the_cross_field_rules() {
    for e in fixture("manifest.json")["fixtures"].as_array().unwrap() {
        let file = e["file"].as_str().unwrap();
        let fx = fixture(file);
        let (op, request, body) = (
            fx["op"].as_str().unwrap(),
            &fx["request"],
            &fx["response"]["body"],
        );
        let accept = e["class"] == "accept";
        let echo = body.get("error").map_or(body, |e| &e["details"]);
        if request.is_object() {
            // R7.2: echoed as sent, except a null R6.1 allows (not after E, R6.4).
            // Raw, unparseable requests are checked in knowledge_fixtures.rs.
            let code = body["error"]["code"].as_str();
            let reason = body["error"]["details"]["reason"].as_str();
            let may_null = matches!(code, Some("unauthorized" | "internal"))
                || (code == Some("invalid_request") && reason != Some("mixed_score_space"));
            for k in ["request_id", "generation"] {
                assert!(
                    echo[k] == request[k] || (echo[k].is_null() && may_null),
                    "{file}: {k}"
                );
            }
        }
        if !accept {
            if request.is_object() {
                reject_pairs_with_request(file, request, &body["error"]);
            }
            continue;
        }
        if !request.is_object() {
            continue;
        }
        let asked = ids(&request["scope"]["knowledge_base_ids"]);
        let e_scope = ids(&body["effective_scope"]["knowledge_base_ids"]);
        // R2.1: E ⊆ Q.
        assert!(
            e_scope.iter().all(|kb| asked.contains(kb)),
            "{file}: effective_scope"
        );
        match op {
            "knowledge.search" => search(file, request, body, &e_scope),
            "knowledge.read" => read(file, request, body),
            "knowledge.ready" => {
                // R8.2/R8.4: per-KB entries only with scope, only for asked KBs, once each.
                let kbs = body.get("knowledge_bases");
                assert_eq!(kbs.is_some(), request.get("scope").is_some(), "{file}");
                let listed: Vec<&str> = kbs
                    .and_then(Value::as_array)
                    .map(|a| {
                        a.iter()
                            .map(|kb| kb["knowledge_base_id"].as_str().unwrap())
                            .collect()
                    })
                    .unwrap_or_default();
                assert!(
                    listed.iter().all(|kb| asked.contains(kb)),
                    "{file}: ready KBs"
                );
                unique(file, "ready KB", &listed);
            }
            _ => {}
        }
    }
}

fn search(file: &str, request: &Value, body: &Value, e_scope: &[&str]) {
    let hits = body["hits"].as_array().unwrap();
    let applied = body["applied_budget"].as_object().unwrap();
    // R3.6: authoritative order, rank 1..n, one score_space per response.
    for (i, h) in hits.iter().enumerate() {
        assert_eq!(h["rank"].as_u64(), Some(i as u64 + 1), "{file}: rank");
        assert!(
            e_scope.contains(&h["knowledge_base_id"].as_str().unwrap()),
            "{file}: hit outside E"
        );
        if h["locator"]["valid"] == true {
            assert!(
                h["locator"]["end"].as_u64() > h["locator"]["start"].as_u64(),
                "{file}: locator"
            );
        }
    }
    let spaces: BTreeSet<&str> = hits
        .iter()
        .map(|h| h["score_space"].as_str().unwrap())
        .collect();
    assert!(spaces.len() <= 1, "{file}: score_space {spaces:?}");
    // R3.3: the applied budget is clamped, never raised, and bounds the hits.
    assert!(
        hits.len() as u64 <= applied["max_results"].as_u64().unwrap(),
        "{file}: hits"
    );
    for (k, v) in applied {
        if let Some(asked) = request["budget"].get(k) {
            assert!(v.as_u64() <= asked.as_u64(), "{file}: applied_budget.{k}");
        }
    }
    // R3.4/R3.5: failed KBs are in E, listed once, never hit, and partial means
    // at least one branch in E succeeded.
    let failed: Vec<&str> = body["partial_failures"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|pf| pf["knowledge_base_id"].as_str().unwrap())
                .collect()
        })
        .unwrap_or_default();
    unique(file, "partial failure", &failed);
    assert!(
        failed.iter().all(|kb| e_scope.contains(kb)),
        "{file}: failure outside E"
    );
    for h in hits {
        assert!(
            !failed.contains(&h["knowledge_base_id"].as_str().unwrap()),
            "{file}: hit from a failed KB"
        );
    }
    if body["completeness"] == "partial" {
        assert!(
            e_scope.iter().any(|kb| !failed.contains(kb)),
            "{file}: partial without success"
        );
    }
}

fn read(file: &str, request: &Value, body: &Value) {
    let segments = body["segments"].as_array().unwrap();
    if let Some(chunk) = request.get("chunk_id") {
        // R4.3: a chunk read returns that one chunk.
        assert_eq!(segments.len(), 1, "{file}: segments");
        assert_eq!(&segments[0]["chunk_id"], chunk, "{file}: chunk_id");
    }
    if let Some(want) = request.get("expected_revision") {
        // R4.4: a successful read is at the expected revision.
        assert_eq!(
            body["revision"]["source_revision"], want["source_revision"],
            "{file}"
        );
        assert_eq!(
            body["revision"]["index_build_id"], want["index_build_id"],
            "{file}"
        );
        if request.get("chunk_id").is_some() {
            assert_eq!(
                segments[0]["chunk_revision"], want["chunk_revision"],
                "{file}"
            );
        }
    }
}

/// Rules that tie an error to the request it answers (§6, R3.6, R4.4).
fn reject_pairs_with_request(file: &str, request: &Value, error: &Value) {
    let d = &error["details"];
    if error["code"] == "stale" {
        let want = request.get("expected_revision");
        let same_build = want.is_some_and(|w| {
            d["current_revision"]["source_revision"] == w["source_revision"]
                && d["current_revision"]["index_build_id"] == w["index_build_id"]
        });
        if request.get("chunk_id").is_none() {
            // Document level: no reason, no chunk revision, and the pair really moved.
            assert!(
                d.get("reason").is_none() && d.get("current_chunk_revision").is_none(),
                "{file}"
            );
            assert!(want.is_none() || !same_build, "{file}: stale but unchanged");
        } else if let Some(reason) = d.get("reason") {
            assert_eq!(reason, "anchor_missing", "{file}");
        } else {
            // Chunk edited in the same build: the new chunk revision is reported.
            assert!(same_build, "{file}: chunk stale needs the same build");
            let current = d
                .get("current_chunk_revision")
                .expect("current_chunk_revision");
            assert_ne!(Some(current), want.unwrap().get("chunk_revision"), "{file}");
        }
    }
    if d["reason"] == "mixed_score_space" {
        let groups = d["groups"].as_array().unwrap();
        let spaces: Vec<&str> = groups
            .iter()
            .map(|g| g["score_space"].as_str().unwrap())
            .collect();
        let kbs: Vec<&str> = groups
            .iter()
            .flat_map(|g| ids(&g["knowledge_base_ids"]))
            .collect();
        unique(file, "score_space", &spaces);
        unique(file, "grouped KB", &kbs);
        let asked = ids(&request["scope"]["knowledge_base_ids"]);
        assert!(
            kbs.iter().all(|kb| asked.contains(kb)),
            "{file}: group KB outside Q"
        );
    }
}
