#![allow(clippy::unwrap_used)]

use std::sync::Mutex;

use serde_json::{Value, json};

use super::*;
use crate::provider::state::{LocalReason, Refusal};

/// A pinned fixture.
fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protocol/knowledge/fixtures/v1")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn envelope(name: &str) -> Failure {
    Failure::Envelope(serde_json::from_value(fixture(name)["response"]["body"].clone()).unwrap())
}

fn input(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

/// Answers every call with `answer` and keeps what it was asked.
struct Backend {
    answer: Result<&'static str, Failure>,
    searched: Mutex<Vec<(String, SearchQuery)>>,
    read: Mutex<Vec<(String, ReadQuery)>>,
}

impl Backend {
    fn ok(fixture: &'static str) -> Self {
        Self::new(Ok(fixture))
    }
    fn failing(failure: Failure) -> Self {
        Self::new(Err(failure))
    }
    fn new(answer: Result<&'static str, Failure>) -> Self {
        Self {
            answer,
            searched: Mutex::new(Vec::new()),
            read: Mutex::new(Vec::new()),
        }
    }
}

/// The request and response of a pinned success fixture.
fn pair<Q: for<'de> Deserialize<'de>, S: for<'de> Deserialize<'de>>(name: &str) -> (Q, S) {
    let v = fixture(name);
    let request = serde_json::from_value(v["request"].clone()).unwrap();
    let response = serde_json::from_value(v["response"]["body"].clone()).unwrap();
    (request, response)
}

#[async_trait]
impl KnowledgeBackend for Backend {
    async fn search(
        &self,
        run_id: &str,
        query: SearchQuery,
    ) -> Result<(SearchRequest, SearchResponse), Failure> {
        self.searched.lock().unwrap().push((run_id.into(), query));
        self.answer.clone().map(pair)
    }

    async fn read(
        &self,
        run_id: &str,
        query: ReadQuery,
    ) -> Result<(ReadRequest, ReadResponse), Failure> {
        self.read.lock().unwrap().push((run_id.into(), query));
        self.answer.clone().map(pair)
    }
}

fn block<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(f)
}

/// The refusal the model reads, parsed back.
fn refused(result: Result<String, Refused>) -> Value {
    match result {
        Err(Refused::Failed(text)) => serde_json::from_str(&text).unwrap(),
        other => panic!("{other:?}"),
    }
}

fn search_refusal(failure: Failure) -> Value {
    let backend = Backend::failing(failure);
    refused(block(search(
        &backend,
        "run-1",
        &input(json!({ "query": "差旅" })),
    )))
}

fn read_refusal(failure: Failure) -> Value {
    let backend = Backend::failing(failure);
    refused(block(read(
        &backend,
        "run-1",
        &input(json!({ "knowledge_id": "doc-1" })),
    )))
}

#[test]
fn search_arguments_are_parsed_strictly() {
    let q = parse_search(&input(json!({
        "query": "差旅", "knowledge_base_ids": ["kb-a", "kb-b"], "max_results": 5
    })))
    .unwrap();
    assert_eq!(q.query, "差旅");
    assert_eq!(q.knowledge_base_ids.unwrap().as_slice(), ["kb-a", "kb-b"]);
    assert_eq!(q.max_results.get(), 5);
    // A lone BOM is no whitespace to `str::trim`, nor to the pattern.
    assert_eq!(
        parse_search(&input(json!({ "query": "\u{feff}" })))
            .unwrap()
            .query,
        "\u{feff}"
    );
    let bare = parse_search(&input(json!({ "query": "差旅" }))).unwrap();
    // Naming no limit asks for the tool's default, well under the cap of 50:
    // fifty hits overflowed a 16K-token model's context.
    assert!(bare.knowledge_base_ids.is_none() && bare.max_results.get() == 10);
    for bad in [
        json!({}),
        json!({ "query": "" }),
        json!({ "query": "  " }),
        // Blank as `str::trim` sees it, the set the schema's pattern spells out.
        json!({ "query": "\u{0085}\u{3000}\t" }),
        json!({ "query": 3 }),
        json!({ "query": "差旅", "knowledge_base_ids": [] }),
        json!({ "query": "差旅", "knowledge_base_ids": ["kb-a", ""] }),
        json!({ "query": "差旅", "max_results": 0 }),
        json!({ "query": "差旅", "max_results": -1 }),
        json!({ "query": "差旅", "knowledge_base_ids": null }),
        json!({ "query": "差旅", "max_results": null }),
        json!({ "query": null }),
        json!({ "query": "差旅", "scope": { "knowledge_base_ids": ["kb-a"] } }),
    ] {
        assert!(
            matches!(parse_search(&input(bad.clone())), Err(Refused::Invalid(_))),
            "{bad}"
        );
    }
}

#[test]
fn read_arguments_are_parsed_strictly() {
    let q = parse_read(&input(json!({
        "knowledge_id": "doc-1", "chunk_id": "chk-11", "content_tag": "ct-chk-11", "cursor": "c1"
    })))
    .unwrap();
    assert_eq!(
        q,
        ReadQuery {
            knowledge_id: "doc-1".into(),
            chunk_id: Some("chk-11".into()),
            content_tag: Some("ct-chk-11".into()),
            cursor: Some("c1".into()),
        }
    );
    let doc = parse_read(&input(json!({ "knowledge_id": "doc-1" }))).unwrap();
    assert!(doc.chunk_id.is_none() && doc.content_tag.is_none() && doc.cursor.is_none());
    for bad in [
        json!({}),
        json!({ "knowledge_id": "" }),
        json!({ "knowledge_id": "doc-1", "chunk_id": "" }),
        json!({ "knowledge_id": "doc-1", "chunk_id": "chk-11", "content_tag": "" }),
        json!({ "knowledge_id": "doc-1", "cursor": " " }),
        json!({ "knowledge_id": "doc-1", "content_tag": "ct-chk-11" }),
        json!({ "knowledge_id": "doc-1", "generation": 7 }),
        json!({ "knowledge_id": "doc-1", "chunk_id": "chk-11", "content_tag": null }),
        json!({ "knowledge_id": "doc-1", "chunk_id": null }),
        json!({ "knowledge_id": "doc-1", "cursor": null }),
    ] {
        assert!(
            matches!(parse_read(&input(bad.clone())), Err(Refused::Invalid(_))),
            "{bad}"
        );
    }
}

#[test]
fn invalid_arguments_never_reach_the_backend() {
    let backend = Backend::ok("search.ok.json");
    let out = block(search(&backend, "run-1", &input(json!({ "query": "" }))));
    assert!(matches!(out, Err(Refused::Invalid(_))));
    let out = block(read(
        &backend,
        "run-1",
        &input(json!({ "knowledge_id": "" })),
    ));
    assert!(matches!(out, Err(Refused::Invalid(_))));
    assert!(backend.searched.lock().unwrap().is_empty());
    assert!(backend.read.lock().unwrap().is_empty());
}

#[test]
fn a_result_is_rendered_as_evidence_for_the_run_it_belongs_to() {
    let backend = Backend::ok("search.ok.json");
    let out = block(search(
        &backend,
        "run-7",
        &input(json!({ "query": "差旅" })),
    ))
    .unwrap();
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["kind"], "knowledge.search");
    assert_eq!(out["evidence"][0]["use"], "clue");
    let searched = backend.searched.lock().unwrap();
    assert_eq!(searched[0].0, "run-7");
    assert_eq!(searched[0].1.query, "差旅");

    let backend = Backend::ok("read.chunk.json");
    let args = json!({ "knowledge_id": "doc-1", "chunk_id": "chk-11" });
    let out = block(read(&backend, "run-7", &input(args))).unwrap();
    let out: Value = serde_json::from_str(&out).unwrap();
    assert_eq!(out["kind"], "knowledge.read");
    let read = backend.read.lock().unwrap();
    assert_eq!(read[0].0, "run-7");
    assert_eq!(read[0].1.chunk_id.as_deref(), Some("chk-11"));
}

#[test]
fn a_server_refusal_keeps_code_reason_and_retryable_but_never_the_servers_words() {
    let out = search_refusal(envelope("search.unauthorized-revoked.json"));
    assert_eq!(
        out,
        json!({
            "kind": "knowledge.search",
            "outcome": "refused",
            "code": "unauthorized",
            "reason": "credential_revoked",
            "retryable": false,
            "advice": "知识库凭据无效或已失效，本次无法读取。"
        })
    );
    let text = serde_json::to_string(&out).unwrap();
    assert!(!text.contains("Credential is not valid") && !text.contains("issuing authority"));

    let out = search_refusal(envelope("search.mixed-score-space.json"));
    assert_eq!(out["code"], "invalid_request");
    assert_eq!(out["reason"], "mixed_score_space");
    assert_eq!(out["advice"], "所选知识库不能放在一次检索里，请分开检索。");
    // The groups name knowledge bases; they stay behind.
    assert!(out.get("groups").is_none() && !out.to_string().contains("kb-a"));

    let out = search_refusal(envelope("search.timeout.json"));
    assert_eq!(
        (out["code"].as_str(), out["retryable"].as_bool()),
        (Some("timeout"), Some(true))
    );
    assert_eq!(out["advice"], "知识库没有在时限内完成，可以稍后重试。");
}

#[test]
fn unavailable_advice_follows_retryable() {
    let out = search_refusal(envelope("search.unavailable-mixed.json"));
    assert_eq!(out["code"], "unavailable");
    assert_eq!(out["reason"], "index_failed");
    assert_eq!(out["retryable"], false);
    assert_eq!(out["advice"], "知识库当前无法提供这部分内容。");
    // No per-base failures are passed on from an error envelope.
    assert!(out.get("partial_failures").is_none() && !out.to_string().contains("kb-c"));

    let out = read_refusal(envelope("read.index-pending.json"));
    assert_eq!(out["code"], "unavailable");
    assert_eq!(out["retryable"], true);
    assert_eq!(out["advice"], "知识库暂时不可用，可以稍后重试。");
}

#[test]
fn a_missing_document_reads_the_same_as_one_out_of_scope() {
    let gone = read_refusal(envelope("read.not-found.json"));
    let hidden = read_refusal(envelope("read.out-of-scope.json"));
    assert_eq!(gone, hidden);
    assert_eq!(gone["code"], "not_found");
    assert_eq!(gone["advice"], "没有找到可以读取的这份文档。");
    assert!(gone.get("reason").is_none());
}

#[test]
fn a_stale_read_asks_for_a_new_search_and_hands_no_revision() {
    let out = read_refusal(envelope("read.stale-chunk-edited.json"));
    assert_eq!(out["code"], "stale");
    assert_eq!(out["advice"], "这段内容已经变化，请重新检索后再读取。");
    let text = out.to_string();
    assert!(!text.contains("ct-chk-70-v2") && !text.contains("src-8"));
}

#[test]
fn a_local_refusal_is_unavailable_with_its_reason() {
    let out = search_refusal(Failure::Local(Refusal {
        reason: LocalReason::CredentialRejected,
        retryable: false,
    }));
    assert_eq!(
        out,
        json!({
            "kind": "knowledge.search",
            "outcome": "refused",
            "code": "unavailable",
            "reason": "credential_rejected",
            "retryable": false,
            "advice": "知识库登录被拒绝，需要用户重新登录。"
        })
    );
    let out = read_refusal(Failure::Local(Refusal {
        reason: LocalReason::Dependency,
        retryable: true,
    }));
    assert_eq!(out["kind"], "knowledge.read");
    assert_eq!(
        (out["reason"].as_str(), out["retryable"].as_bool()),
        (Some("dependency"), Some(true))
    );
    assert_eq!(out["advice"], "知识库依赖的服务暂时不可用，可以稍后重试。");
    // Held by unsaved settings: no retry until the user confirms them.
    let out = read_refusal(Failure::Local(Refusal {
        reason: LocalReason::Dependency,
        retryable: false,
    }));
    assert_eq!(out["retryable"], false);
    assert_eq!(
        out["advice"],
        "知识库依赖的服务不可用，要等用户确认设置后才会重试。"
    );
}

#[test]
fn an_unbound_or_stale_binding_is_refused_without_retry() {
    let out = search_refusal(Failure::Unbound);
    assert_eq!(out["code"], "knowledge_unbound");
    assert_eq!(out["retryable"], false);
    assert!(out.get("reason").is_none());
    let out = read_refusal(Failure::BindingStale);
    assert_eq!(out["code"], "knowledge_binding_stale");
    assert_eq!(out["retryable"], false);
}

#[test]
fn a_local_cancellation_never_reaches_the_model() {
    let local = fixture("search.cancelled.json")["local"]["result"].clone();
    let cancelled: LocalCancelled = serde_json::from_value(local).unwrap();
    let backend = Backend::failing(Failure::Cancelled(cancelled.clone()));
    let out = block(search(
        &backend,
        "run-1",
        &input(json!({ "query": "差旅" })),
    ));
    assert_eq!(out, Err(Refused::Cancelled));
    let backend = Backend::failing(Failure::Cancelled(cancelled));
    let out = block(read(
        &backend,
        "run-1",
        &input(json!({ "knowledge_id": "doc-1" })),
    ));
    assert_eq!(out, Err(Refused::Cancelled));
}

#[test]
fn every_local_reason_has_its_own_advice() {
    use LocalReason as R;
    let all = [
        R::KnowledgeOff,
        R::LocalStorage,
        R::ProfileInactive,
        R::Incompatible,
        R::CredentialRejected,
        R::AccessRevoked,
        R::InstanceUnidentified,
        R::NotReady,
        R::Dependency,
    ];
    let advice: std::collections::HashSet<_> = all.iter().map(|r| local_advice(*r, true)).collect();
    assert_eq!(advice.len(), all.len());
}

#[test]
fn the_advertised_schemas_match_the_parser() {
    let s = search_parameters();
    assert_eq!(s["required"], json!(["query"]));
    assert_eq!(s["additionalProperties"], false);
    let keys: Vec<_> = s["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["knowledge_base_ids", "max_results", "query"]);
    let text = |max: usize| json!({ "type": "string", "pattern": "[^\\s\\u0085]|\\uFEFF", "maxLength": max });
    assert_eq!(s["properties"]["query"], text(4096));
    assert_eq!(
        s["properties"]["knowledge_base_ids"],
        json!({ "type": "array", "minItems": 1, "items": text(256) })
    );
    assert_eq!(
        s["properties"]["max_results"],
        json!({ "type": "integer", "minimum": 1, "maximum": 50 })
    );
    let r = read_parameters();
    assert_eq!(r["required"], json!(["knowledge_id"]));
    assert_eq!(r["additionalProperties"], false);
    let keys: Vec<_> = r["properties"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys, ["chunk_id", "content_tag", "cursor", "knowledge_id"]);
    for key in ["chunk_id", "content_tag", "knowledge_id"] {
        assert_eq!(r["properties"][key], text(256), "{key}");
    }
    assert_eq!(r["properties"]["cursor"], text(1024));
    assert_eq!(
        r["dependentRequired"],
        json!({ "content_tag": ["chunk_id"] })
    );
    assert!(s.get("dependentRequired").is_none());
}

#[test]
fn arguments_longer_than_the_wire_allows_are_refused() {
    let long = |n: usize| "长".repeat(n);
    let ok = |v: Value| parse_search(&input(v)).is_ok();
    assert!(ok(json!({ "query": long(4096) })));
    assert!(!ok(json!({ "query": long(4097) })));
    assert!(ok(
        json!({ "query": "q", "knowledge_base_ids": [long(256)] })
    ));
    assert!(!ok(
        json!({ "query": "q", "knowledge_base_ids": [long(257)] })
    ));
    assert!(ok(json!({ "query": "q", "max_results": 50 })));
    assert!(!ok(json!({ "query": "q", "max_results": 51 })));
    let ok = |v: Value| parse_read(&input(v)).is_ok();
    for key in ["chunk_id", "cursor"] {
        let max = if key == "cursor" { 1024 } else { 256 };
        assert!(ok(json!({ "knowledge_id": "d", key: long(max) })), "{key}");
        assert!(
            !ok(json!({ "knowledge_id": "d", key: long(max + 1) })),
            "{key}"
        );
    }
    assert!(ok(json!({ "knowledge_id": long(256) })));
    assert!(!ok(json!({ "knowledge_id": long(257) })));
    let tag = |n| json!({ "knowledge_id": "d", "chunk_id": "c", "content_tag": long(n) });
    assert!(ok(tag(256)) && !ok(tag(257)));
}

mod paging;
