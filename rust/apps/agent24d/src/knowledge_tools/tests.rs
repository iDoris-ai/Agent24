#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Mutex;

use agent24_knowledge::tool::call::{Failure, ReadQuery, SearchQuery};
use agent24_knowledge::{
    CancelledCode, CancelledDetails, CancelledError, ConstBool, LocalCancelled, ReadRequest,
    ReadResponse, SearchRequest, SearchResponse,
};
use serde_json::json;

use super::*;

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protocol/knowledge/fixtures/v1")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// Answers every search with `search.ok.json`, or fails every call.
struct Backend {
    fail: Option<Failure>,
    runs: Mutex<Vec<String>>,
}

#[async_trait]
impl KnowledgeBackend for Backend {
    async fn search(
        &self,
        run_id: &str,
        _query: SearchQuery,
    ) -> Result<(SearchRequest, SearchResponse), Failure> {
        self.runs.lock().unwrap().push(run_id.to_owned());
        if let Some(failure) = self.fail.clone() {
            return Err(failure);
        }
        let v = fixture("search.ok.json");
        let request = serde_json::from_value(v["request"].clone()).unwrap();
        Ok((
            request,
            serde_json::from_value(v["response"]["body"].clone()).unwrap(),
        ))
    }

    async fn read(
        &self,
        run_id: &str,
        _query: ReadQuery,
    ) -> Result<(ReadRequest, ReadResponse), Failure> {
        self.runs.lock().unwrap().push(run_id.to_owned());
        Err(self.fail.clone().unwrap_or(Failure::Unbound))
    }
}

fn tool(kind: Kind, fail: Option<Failure>) -> (KnowledgeTool, Arc<Backend>) {
    let backend = Arc::new(Backend {
        fail,
        runs: Mutex::new(Vec::new()),
    });
    let dynamic: Arc<dyn KnowledgeBackend> = backend.clone();
    (
        KnowledgeTool {
            kind,
            backend: dynamic,
        },
        backend,
    )
}

fn ctx() -> ToolContext {
    ToolContext::legacy("run-1", None, None, "call-1")
}

fn input(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn both_tools_are_reads_offered_only_to_a_bound_run() {
    let registry = Arc::new(KnowledgeHostRegistry::default());
    let runs: Arc<dyn RunSelector> = Arc::new(Runs);
    let [search, read] = tools(registry, runs);
    for (tool, name, schema) in [
        (&search, "knowledge_search", call::search_parameters()),
        (&read, "knowledge_read", call::read_parameters()),
    ] {
        let info = tool.info();
        assert_eq!(info.name, name);
        assert_eq!(info.risk_class, RiskClass::Read);
        assert!(tool.needs_knowledge_binding());
        assert!(!tool.requires_outbound_policy());
        assert_eq!(tool.parameters(), schema);
    }
}

struct Runs;

#[async_trait]
impl RunSelector for Runs {
    async fn knowledge_binding_id(&self, _run_id: &str) -> Option<String> {
        None
    }
}

#[tokio::test]
async fn the_registry_advertises_them_to_a_bound_run_only() {
    let registry = Arc::new(KnowledgeHostRegistry::default());
    let mut tools_registry = agent24_tools::ToolRegistry::new();
    for tool in tools(registry, Arc::new(Runs)) {
        tools_registry = tools_registry.with(tool);
    }
    let names = |adverts: Vec<agent24_tools::ToolAdvert>| {
        let mut names: Vec<_> = adverts.into_iter().map(|a| a.name).collect();
        names.sort();
        names
    };
    assert_eq!(
        names(tools_registry.live_adverts(true).await),
        ["knowledge_read", "knowledge_search"]
    );
    assert!(tools_registry.live_adverts(false).await.is_empty());
}

#[tokio::test]
async fn a_search_returns_the_rendered_evidence_for_the_calling_run() {
    let (search, backend) = tool(Kind::Search, None);
    let out = search
        .call(
            &ctx(),
            &input(json!({ "query": "报销流程" })),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(out.contains("chk-11"), "{out}");
    assert_eq!(*backend.runs.lock().unwrap(), ["run-1"]);
}

#[tokio::test]
async fn refusals_invalid_arguments_and_cancellation_map_to_tool_errors() {
    let (read, _) = tool(Kind::Read, None);
    let unbound = read
        .call(
            &ctx(),
            &input(json!({ "knowledge_id": "doc-1" })),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(&unbound, Err(ToolError::Failed(text)) if text.contains("knowledge_unbound")));

    let (search, backend) = tool(Kind::Search, None);
    let invalid = search
        .call(
            &ctx(),
            &input(json!({ "query": " " })),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(invalid, Err(ToolError::Invalid(_))));
    assert!(backend.runs.lock().unwrap().is_empty());

    let cancel = CancellationToken::new();
    cancel.cancel();
    let (search, _) = tool(Kind::Search, None);
    let cancelled = search
        .call(&ctx(), &input(json!({ "query": "q" })), &cancel)
        .await;
    assert!(matches!(cancelled, Err(ToolError::Cancelled)));
}

#[tokio::test]
async fn a_binding_that_moves_mid_call_drops_the_answer_not_the_run() {
    let moved = Failure::Cancelled(LocalCancelled {
        error: CancelledError {
            code: CancelledCode::Cancelled,
            message: "The Knowledge binding changed while the call was out".into(),
            hint: None,
            details: CancelledDetails {
                retryable: ConstBool,
                request_id: "req-1".into(),
                generation: 1,
                current_generation: 1,
            },
        },
    });
    for (kind, args, tag) in [
        (Kind::Search, json!({ "query": "q" }), "knowledge.search"),
        (
            Kind::Read,
            json!({ "knowledge_id": "doc-1" }),
            "knowledge.read",
        ),
    ] {
        let (tool, _) = tool(kind, Some(moved.clone()));
        let refused = tool
            .call(&ctx(), &input(args), &CancellationToken::new())
            .await;
        let Err(ToolError::Failed(text)) = refused else {
            panic!("{refused:?}");
        };
        let out: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(out["code"], "knowledge_call_superseded");
        assert_eq!(out["kind"], tag);
        assert_eq!(out["retryable"], true);
    }
}

#[tokio::test]
async fn a_run_whose_source_policy_is_unavailable_sends_nothing() {
    let (search, backend) = tool(Kind::Search, None);
    let ctx = ctx().with_egress_policy_unavailable();
    let denied = search
        .call(
            &ctx,
            &input(json!({ "query": "q" })),
            &CancellationToken::new(),
        )
        .await;
    assert!(matches!(denied, Err(ToolError::Denied(_))), "{denied:?}");
    assert!(backend.runs.lock().unwrap().is_empty());
}

fn result_of(origin: &str, call: &str) -> agent24_domain::EgressResource {
    agent24_domain::EgressResource::local_only(format!("tool_result:{origin}:{call}"), "r")
}

#[tokio::test]
async fn earlier_knowledge_results_do_not_hold_back_another_knowledge_call() {
    // In this run, and from the session's earlier turns.
    let ctx = ctx().with_egress_resources(vec![
        result_of("tool:knowledge_search", "tc_1"),
        result_of("tool:knowledge_read", "session:s1"),
    ]);
    for kind in [Kind::Search, Kind::Read] {
        let (tool, backend) = tool(kind, None);
        let args = match kind {
            Kind::Search => json!({ "query": "q" }),
            Kind::Read => json!({ "knowledge_id": "k" }),
        };
        let got = tool
            .call(&ctx, &input(args), &CancellationToken::new())
            .await;
        // Past the egress check and on to the backend.
        assert!(!matches!(got, Err(ToolError::Denied(_))), "{got:?}");
        assert_eq!(backend.runs.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn any_other_source_still_holds_it_back() {
    for other in [
        "tool:shell_exec",
        "tool:knowledge_search_x",
        "module:knowledge_search",
    ] {
        let ctx = ctx().with_egress_resources(vec![
            result_of("tool:knowledge_search", "tc_1"),
            result_of(other, "tc_2"),
        ]);
        let (search, backend) = tool(Kind::Search, None);
        let denied = search
            .call(
                &ctx,
                &input(json!({ "query": "q" })),
                &CancellationToken::new(),
            )
            .await;
        assert!(
            matches!(denied, Err(ToolError::Denied(_))),
            "{other}: {denied:?}"
        );
        assert!(backend.runs.lock().unwrap().is_empty(), "{other}");
    }
}

fn stored(tool: &str, status: ToolCallStatus, input: Value) -> ToolCall {
    ToolCall {
        id: format!("tc_{tool}"),
        run_id: "run-1".into(),
        tool: tool.into(),
        input: input.as_object().unwrap().clone(),
        status,
        output_summary: None,
        started_at: "2026-10-31T00:00:00Z".into(),
        ended_at: None,
    }
}

#[test]
fn a_run_answered_without_the_tools_read_nothing() {
    // The answer may still name sources: they are not in the run's calls.
    assert_eq!(
        run_knowledge(&[]),
        RunKnowledge {
            searched: false,
            read_chunks: vec![],
            search_refused: false,
            found_passages: 0,
            found_titles: vec![],
            found: vec![],
        }
    );
    // Calls that did not complete, or other tools, read nothing either.
    let calls = [
        stored(
            READ,
            ToolCallStatus::Failed,
            json!({ "knowledge_id": "doc-x", "chunk_id": "chk-x" }),
        ),
        stored(SEARCH, ToolCallStatus::Denied, json!({ "query": "q" })),
        stored(
            "http_fetch",
            ToolCallStatus::Completed,
            json!({ "knowledge_id": "doc-x", "chunk_id": "chk-x" }),
        ),
    ];
    assert_eq!(
        run_knowledge(&calls),
        RunKnowledge {
            searched: false,
            read_chunks: vec![],
            search_refused: true,
            found_passages: 0,
            found_titles: vec![],
            found: vec![],
        }
    );
}

#[test]
fn the_chunks_a_run_read_are_those_its_completed_reads_named() {
    let chunk = |k: &str, c: &str| ReadChunk {
        knowledge_id: k.into(),
        chunk_id: c.into(),
    };
    let read = |k: &str, c: &str| json!({ "knowledge_id": k, "chunk_id": c, "content_tag": "ct" });
    let calls = [
        stored(SEARCH, ToolCallStatus::Completed, json!({ "query": "q" })),
        stored(READ, ToolCallStatus::Completed, read("doc-7", "chk-71")),
        // A page read names no chunk.
        stored(
            READ,
            ToolCallStatus::Completed,
            json!({ "knowledge_id": "doc-8" }),
        ),
        stored(READ, ToolCallStatus::Failed, read("doc-9", "chk-91")),
        stored(READ, ToolCallStatus::Completed, read("doc-7", "chk-72")),
        stored(READ, ToolCallStatus::Completed, read("doc-7", "chk-71")),
    ];
    assert_eq!(
        run_knowledge(&calls),
        RunKnowledge {
            searched: true,
            read_chunks: vec![chunk("doc-7", "chk-71"), chunk("doc-7", "chk-72")],
            search_refused: false,
            found_passages: 0,
            found_titles: vec![],
            found: vec![],
        }
    );
    // A search alone is a search, with nothing read back.
    let knowledge = run_knowledge(&calls[..1]);
    assert!(knowledge.searched && knowledge.read_chunks.is_empty());
}

/// K3: a completed search's hits count as read, so an answer citing them is
/// grounded; a search that failed, or another tool's answer, counts for nothing.
#[test]
fn a_completed_searchs_hits_count_as_read() {
    let chunk = |k: &str, c: &str| ReadChunk {
        knowledge_id: k.into(),
        chunk_id: c.into(),
    };
    let answer = json!({
        "kind": "knowledge.search",
        "evidence": [
            { "use": "clue", "knowledge_id": "doc-1", "chunk_id": "chk-1" },
            { "use": "clue", "knowledge_id": "doc-2", "chunk_id": "chk-2" },
            { "use": "clue", "knowledge_id": "doc-1", "chunk_id": "chk-1" },
        ],
    })
    .to_string();
    // A tool message carries the provider's call id, not the stored call's (11-19, David Mac).
    let answers = [answer.clone()];
    let done = [stored(
        SEARCH,
        ToolCallStatus::Completed,
        json!({ "query": "q" }),
    )];
    assert_eq!(
        run_knowledge_with(&done, &answers),
        RunKnowledge {
            searched: true,
            read_chunks: vec![chunk("doc-1", "chk-1"), chunk("doc-2", "chk-2")],
            search_refused: false,
            found_passages: 2,
            found_titles: vec![],
            found: vec![],
        }
    );
    let failed = [stored(
        SEARCH,
        ToolCallStatus::Failed,
        json!({ "query": "q" }),
    )];
    assert_eq!(run_knowledge_with(&failed, &answers).read_chunks, vec![]);
    let other = [
        json!({ "kind": "knowledge.read", "evidence": [{ "knowledge_id": "doc-9", "chunk_id": "chk-9" }] }).to_string(),
        "tool error: failed: {}".to_owned(),
    ];
    assert_eq!(run_knowledge_with(&done, &other).read_chunks, vec![]);
}

/// PR-Daemon #1976 F1: what the search first puts in (compacted) still counts
/// its hits as the run's, so an answer citing them is grounded (K3).
#[test]
fn the_first_searchs_compacted_hits_count_as_read() {
    let full = json!({
        "notice": "n", "kind": "knowledge.search", "completeness": "complete",
        "scope_narrowed": false, "failures": [],
        "evidence": [
            { "rank": 1, "use": "clue", "knowledge_base_id": "kb", "knowledge_id": "doc-1",
              "chunk_id": "chk-1", "content_tag": "t", "snippet": "水".repeat(600),
              "snippet_truncated": true, "possibly_stale": false },
        ],
    })
    .to_string();
    let compact = agent24_agent::compact_evidence_for_tests(&full).unwrap();
    let done = [stored(
        SEARCH,
        ToolCallStatus::Completed,
        json!({ "query": "q", "max_results": 3 }),
    )];
    let k = run_knowledge_with(&done, &[compact]);
    assert_eq!(
        k.read_chunks,
        vec![ReadChunk {
            knowledge_id: "doc-1".into(),
            chunk_id: "chk-1".into()
        }]
    );
    // L7-K1: and names the library, which the user most often meets through
    // the search first.
    assert_eq!(k.found.len(), 1);
    assert_eq!(k.found[0].knowledge_base_id, "kb");
}

/// L7-K0 (FU-183): a search the policy refused is said as refused, not as
/// 「searched, found nothing」; one that completed with no passage is 「没找到」.
#[test]
fn a_refused_search_is_told_apart_from_one_that_found_nothing() {
    let denied = [stored(
        SEARCH,
        ToolCallStatus::Denied,
        json!({ "query": "q" }),
    )];
    let k = run_knowledge_with(&denied, &[]);
    assert!(k.search_refused && !k.searched && k.found_passages == 0);
    // The model asked again and it went through: the last word is the answer.
    let retried = [
        stored(SEARCH, ToolCallStatus::Denied, json!({ "query": "q" })),
        stored(SEARCH, ToolCallStatus::Completed, json!({ "query": "q" })),
    ];
    assert!(!run_knowledge_with(&retried, &[]).search_refused);
    // Completed with no passage: searched, none found, not refused.
    let empty = [stored(
        SEARCH,
        ToolCallStatus::Completed,
        json!({ "query": "q" }),
    )];
    let k = run_knowledge_with(&empty, &[]);
    assert!(k.searched && !k.search_refused && k.found_passages == 0 && k.found_titles.is_empty());
    // A refused read is not a refused search.
    let read = [stored(
        READ,
        ToolCallStatus::Denied,
        json!({ "knowledge_id": "d" }),
    )];
    assert!(!run_knowledge_with(&read, &[]).search_refused);
}

/// L7-K0: passages counted once each, titles from the compact entry or the
/// hit's display, at most five, never empty.
#[test]
fn found_passages_and_titles_come_from_the_completed_search_answers() {
    let done = [stored(
        SEARCH,
        ToolCallStatus::Completed,
        json!({ "query": "q" }),
    )];
    let answers = [json!({ "kind": "knowledge.search", "evidence": [
            { "knowledge_id": "d1", "chunk_id": "c1", "title": "制度.pdf" },
            { "knowledge_id": "d1", "chunk_id": "c2", "title": "制度.pdf" },
            { "knowledge_id": "d2", "chunk_id": "c3", "display": { "title": "手册" } },
            { "knowledge_id": "d3", "chunk_id": "c4", "display": { "file_name": "a.docx" } },
            { "knowledge_id": "d4", "chunk_id": "c5", "title": "" },
            { "knowledge_id": "d1", "chunk_id": "c1", "title": "制度.pdf" },
        ] })
    .to_string()];
    let k = run_knowledge_with(&done, &answers);
    assert_eq!(k.found_passages, 5);
    assert_eq!(k.found_titles, vec!["制度.pdf", "手册", "a.docx"]);
    // No completed search: nothing found, whatever an answer says.
    let failed = [stored(
        SEARCH,
        ToolCallStatus::Failed,
        json!({ "query": "q" }),
    )];
    assert_eq!(run_knowledge_with(&failed, &answers).found_passages, 0);
}

/// L7-K1: each hit's library, document, chunk and title, from the answers as
/// production makes them: the full answer (`knowledge_base_id`, `display`, no
/// top-level `title`) and the search first's compacted one (`title`, and the
/// library id). Never the passage text; each chunk once; at most eight; a hit
/// with no library id is not listed.
#[test]
fn found_hits_name_the_library_and_carry_no_passage_text() {
    let done = [stored(
        SEARCH,
        ToolCallStatus::Completed,
        json!({ "query": "q" }),
    )];
    let hit = |kb: &str, doc: &str, chunk: &str, display: serde_json::Value| {
        json!({ "rank": 1, "use": "clue", "possibly_stale": false, "knowledge_base_id": kb,
                "knowledge_id": doc, "chunk_id": chunk, "content_tag": "t",
                "snippet": "秘密正文", "snippet_truncated": false, "display": display })
    };
    let mut evidence = vec![
        hit("kb-a", "d1", "c1", json!({ "title": "制度.pdf" })),
        hit("kb-b", "d2", "c2", json!({ "file_name": "项目.docx" })),
        hit("kb-a", "d1", "c1", json!({ "title": "制度.pdf" })),
    ];
    for i in 0..10 {
        evidence.push(hit("kb-a", &format!("x{i}"), "c", json!({ "title": "t" })));
    }
    let full = json!({ "notice": "n", "kind": "knowledge.search", "completeness": "complete",
        "scope_narrowed": false, "evidence": evidence })
    .to_string();
    let expect = [
        ("kb-a", "d1", "c1", "制度.pdf"),
        ("kb-b", "d2", "c2", "项目.docx"),
    ];
    // The compacted answer keeps the first three hits only.
    for (answer, len) in [
        (full.clone(), 8),
        (agent24_agent::compact_evidence_for_tests(&full).unwrap(), 2),
    ] {
        let k = run_knowledge_with(&done, &[answer]);
        assert_eq!(k.found.len(), len);
        for (found, (kb, doc, chunk, title)) in k.found.iter().zip(expect) {
            assert_eq!(
                found,
                &FoundHit {
                    knowledge_base_id: kb.into(),
                    knowledge_id: doc.into(),
                    chunk_id: chunk.into(),
                    title: title.into()
                }
            );
        }
        assert!(
            !serde_json::to_string(&k.found)
                .unwrap()
                .contains("秘密正文")
        );
    }
    // A hit without a library id is not listed; a failed search lists nothing.
    let no_base = json!({ "kind": "knowledge.search", "evidence": [
        { "knowledge_id": "d9", "chunk_id": "c9", "display": { "title": "x" } }] })
    .to_string();
    assert!(run_knowledge_with(&done, &[no_base]).found.is_empty());
    let failed = [stored(
        SEARCH,
        ToolCallStatus::Failed,
        json!({ "query": "q" }),
    )];
    assert!(run_knowledge_with(&failed, &[full]).found.is_empty());
}
