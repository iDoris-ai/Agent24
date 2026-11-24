//! L7-K1: a search whose knowledge bases sit in more than one score space
//! (READONLY R3.6) is answered group by group and merged (R3.7(b)).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};

use super::*;
use crate::host::Endpoint;
use crate::provider::budget::CALL_REQUESTS;
use crate::tool::call::{Failure, KnowledgeBackend, SearchQuery};

const HOST: &str = "host-cap";
const BINDING: &str = "b1";
const RUN: &str = "run-1";

type Handler = Box<dyn Fn(&Value) -> (u16, Value) + Send + Sync>;

struct Script {
    handler: Handler,
    sent: Mutex<Vec<Value>>,
}

#[async_trait]
impl KnowledgeTransport for Script {
    async fn send(
        &self,
        _op: KnowledgeOperation,
        body: Vec<u8>,
        _deadline: Duration,
    ) -> Result<crate::TransportResponse, TransportError> {
        let request: Value = serde_json::from_slice(&body).unwrap();
        self.sent.lock().unwrap().push(request.clone());
        let (status, mut body) = (self.handler)(&request);
        // Status 0: the request never got an answer.
        if status == 0 {
            return Err(TransportError::Unreachable);
        }
        let echoes = if body.get("error").is_some() {
            &mut body["error"]["details"]
        } else {
            &mut body
        };
        echoes["request_id"] = request["request_id"].clone();
        echoes["generation"] = request["generation"].clone();
        Ok(crate::TransportResponse {
            status,
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

struct Runs;

#[async_trait]
impl RunSelector for Runs {
    async fn knowledge_binding_id(&self, _run_id: &str) -> Option<String> {
        Some(BINDING.to_owned())
    }
}

fn hit(kb: &str, chunk: &str) -> Value {
    json!({ "rank": 1, "knowledge_base_id": kb, "knowledge_id": format!("doc-{chunk}"),
        "chunk_id": chunk, "chunk_type": "text",
        "revision": { "source_revision": "s", "index_build_id": "i", "chunk_revision": 0,
                      "content_tag": format!("ct-{chunk}") },
        "locator": { "valid": false }, "index_state": "fresh", "score": 0.5,
        "score_space": format!("space-{kb}"), "snippet": { "text": "x", "truncated": false } })
}

/// A complete answer for `ids`, with these hits (ranks renumbered).
fn ok(ids: &[&str], hits: Vec<Value>) -> (u16, Value) {
    let hits: Vec<Value> = hits
        .into_iter()
        .enumerate()
        .map(|(i, mut h)| {
            h["rank"] = json!(i + 1);
            h
        })
        .collect();
    (
        200,
        json!({ "protocol_version": 1, "completeness": "complete",
            "effective_scope": { "knowledge_base_ids": ids },
            "applied_budget": { "deadline_ms": 9000, "max_results": 10, "max_concurrency": 2 },
            "hits": hits }),
    )
}

fn error(status: u16, code: &str, reason: Option<&str>, retryable: bool) -> (u16, Value) {
    let mut details = json!({ "retryable": retryable });
    if let Some(reason) = reason {
        details["reason"] = json!(reason);
    }
    (
        status,
        json!({ "error": { "code": code, "message": "m", "details": details } }),
    )
}

/// The mixed-score-space answer: one group per knowledge base.
fn mixed(ids: &[&str]) -> (u16, Value) {
    let (status, mut body) = error(400, "invalid_request", Some("mixed_score_space"), false);
    body["error"]["details"]["groups"] = json!(
        ids.iter()
            .map(|id| json!({ "score_space": format!("space-{id}"), "knowledge_base_ids": [id] }))
            .collect::<Vec<_>>()
    );
    (status, body)
}

fn ids_of(request: &Value) -> Vec<String> {
    request["scope"]["knowledge_base_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect()
}

struct Setup {
    script: Arc<Script>,
    backend: DaemonKnowledgeBackend,
}

fn setup(allowed: &[&str], handler: Handler) -> Setup {
    let registry = Arc::new(KnowledgeHostRegistry::default());
    registry
        .activate(
            HOST,
            BINDING,
            5,
            Endpoint {
                base_url: "https://kb.example/api/v1".into(),
                knowledge_base_ids: allowed.iter().map(|s| (*s).to_owned()).collect(),
            },
        )
        .unwrap();
    registry
        .push_credential(
            HOST,
            BINDING,
            6,
            Credential::ApiKey("k".into()),
            unix_now() + 3600,
        )
        .unwrap();
    registry.pin_run(
        RUN,
        RunBinding {
            host: HOST.into(),
            binding: BINDING.into(),
            generation: 5,
        },
    );
    let script = Arc::new(Script {
        handler,
        sent: Mutex::new(Vec::new()),
    });
    let to = Arc::clone(&script);
    let connect: Box<Connect> = Box::new(move |_, _| {
        let transport: Arc<dyn KnowledgeTransport> = to.clone();
        Some(transport)
    });
    let backend = DaemonKnowledgeBackend::with_connect(registry, Arc::new(Runs), connect);
    Setup { script, backend }
}

fn query(max: u64) -> SearchQuery {
    SearchQuery {
        query: "报销".into(),
        knowledge_base_ids: None,
        max_results: positive(max),
    }
}

/// Answers the whole scope with mixed_score_space and each single knowledge
/// base with `per_kb(id)`.
fn by_group(per_kb: impl Fn(&str) -> (u16, Value) + Send + Sync + 'static) -> Handler {
    Box::new(move |request| {
        let ids = ids_of(request);
        if ids.len() > 1 {
            mixed(&ids.iter().map(String::as_str).collect::<Vec<_>>())
        } else {
            per_kb(&ids[0])
        }
    })
}

#[tokio::test]
async fn groups_are_answered_one_by_one_and_merged_by_rank_without_duplicates() {
    let s = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            "kb-a" => ok(
                &["kb-a"],
                vec![hit("kb-a", "a1"), hit("kb-a", "a2"), hit("kb-a", "s")],
            ),
            _ => ok(
                &["kb-b"],
                vec![hit("kb-b", "b1"), hit("kb-b", "s"), hit("kb-b", "b2")],
            ),
        }),
    );
    let (request, response) = s.backend.search(RUN, query(4)).await.unwrap();
    let sent = s.script.sent.lock().unwrap();
    // The whole scope first, then one request per group, each its own id.
    assert_eq!(sent.len(), 3);
    assert_eq!(ids_of(&sent[0]), ["kb-a", "kb-b"]);
    assert_eq!(ids_of(&sent[1]), ["kb-a"]);
    assert_eq!(ids_of(&sent[2]), ["kb-b"]);
    assert!(sent.iter().all(|r| r["generation"] == json!(5)));
    assert_ne!(sent[1]["request_id"], sent[2]["request_id"]);
    // Rank-interleaved, one duplicate chunk kept once (first seen), cut to
    // max_results and renumbered.
    let got: Vec<(String, String, u64)> = response
        .hits
        .iter()
        .map(|h| {
            (
                h.knowledge_base_id.clone(),
                h.chunk_id.clone(),
                h.rank.get(),
            )
        })
        .collect();
    let want = [
        ("kb-a", "a1"),
        ("kb-b", "b1"),
        ("kb-a", "a2"),
        ("kb-b", "s"),
    ];
    assert_eq!(
        got,
        want.iter()
            .enumerate()
            .map(|(i, (k, c))| ((*k).to_owned(), (*c).to_owned(), i as u64 + 1))
            .collect::<Vec<_>>()
    );
    // The answer is for the request asked first.
    assert_eq!(response.request_id, request.request_id);
    assert_eq!(
        request.scope.knowledge_base_ids.as_slice(),
        ["kb-a", "kb-b"]
    );
    assert_eq!(
        response.effective_scope.knowledge_base_ids.as_slice(),
        ["kb-a", "kb-b"]
    );
    assert_eq!(response.completeness, crate::Completeness::Complete);
    assert!(response.partial_failures.is_none());
}

#[tokio::test]
async fn a_group_that_times_out_makes_the_answer_partial() {
    let s = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            "kb-a" => ok(&["kb-a"], vec![hit("kb-a", "a1")]),
            _ => error(504, "timeout", Some("timeout"), true),
        }),
    );
    let (_, response) = s.backend.search(RUN, query(10)).await.unwrap();
    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.completeness, crate::Completeness::Partial);
    let failed = response.partial_failures.unwrap();
    let failed = failed.as_slice();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].knowledge_base_id, "kb-b");
    assert_eq!(failed[0].code, crate::PartialFailureCode::Timeout);
    assert!(failed[0].retryable);
}

#[tokio::test]
async fn an_empty_scope_group_leaves_the_answer_and_all_empty_is_forbidden() {
    let one = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            "kb-a" => ok(&["kb-a"], vec![hit("kb-a", "a1")]),
            _ => error(403, "forbidden", Some("empty_scope"), false),
        }),
    );
    let (request, response) = one.backend.search(RUN, query(10)).await.unwrap();
    // Not a failure: the knowledge base is left out of the effective scope.
    assert_eq!(
        response.effective_scope.knowledge_base_ids.as_slice(),
        ["kb-a"]
    );
    assert_eq!(
        request.scope.knowledge_base_ids.as_slice(),
        ["kb-a", "kb-b"]
    );
    assert_eq!(response.completeness, crate::Completeness::Complete);

    let all = setup(
        &["kb-a", "kb-b"],
        by_group(|_| error(403, "forbidden", Some("empty_scope"), false)),
    );
    let failure = all.backend.search(RUN, query(10)).await.unwrap_err();
    assert!(matches!(&failure, Failure::Envelope(e) if e.error.code() == ErrorCode::Forbidden));
}

#[tokio::test]
async fn any_other_group_error_fails_the_whole_call() {
    for (status, code, reason) in [
        (401, "unauthorized", Some("credential_invalid")),
        (403, "forbidden", Some("policy_denied")),
        (400, "invalid_request", None),
        (500, "internal", None),
    ] {
        let s = setup(
            &["kb-a", "kb-b"],
            by_group(move |id| match id {
                "kb-a" => ok(&["kb-a"], vec![hit("kb-a", "a1")]),
                _ => error(status, code, reason, false),
            }),
        );
        let failure = s.backend.search(RUN, query(10)).await.unwrap_err();
        assert!(
            matches!(&failure, Failure::Envelope(e) if serde_json::to_value(&e.error).unwrap()["code"] == code),
            "{code}: {failure:?}"
        );
    }
}

#[tokio::test]
async fn groups_naming_a_knowledge_base_not_asked_send_nothing_more() {
    // The server's groups reach beyond the scope that was asked.
    let s = setup(&["kb-a", "kb-b"], Box::new(|_| mixed(&["kb-a", "kb-z"])));
    let failure = s.backend.search(RUN, query(10)).await.unwrap_err();
    assert!(matches!(failure, Failure::Local(_)));
    assert_eq!(s.script.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_group_answer_outside_its_group_is_refused() {
    let s = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            // Asked for kb-a alone; the answer reaches into kb-b.
            "kb-a" => ok(&["kb-a", "kb-b"], vec![hit("kb-b", "b1")]),
            _ => ok(&["kb-b"], vec![hit("kb-b", "b2")]),
        }),
    );
    assert!(matches!(
        s.backend.search(RUN, query(10)).await.unwrap_err(),
        Failure::Local(_)
    ));
}

#[tokio::test]
async fn the_second_round_never_goes_beyond_the_binding_or_what_was_named() {
    // Binding has three; the model names two. Groups list the two and a third.
    let s = setup(
        &["kb-a", "kb-b", "kb-c"],
        Box::new(|request| {
            let ids = ids_of(request);
            if ids.len() > 1 {
                mixed(&["kb-a", "kb-b", "kb-c"])
            } else {
                ok(&[ids[0].as_str()], vec![hit(&ids[0], "x")])
            }
        }),
    );
    let mut q = query(10);
    q.knowledge_base_ids =
        Some(NonEmpty::try_from(vec!["kb-a".to_owned(), "kb-b".to_owned()]).unwrap());
    assert!(s.backend.search(RUN, q).await.is_err());
    let sent = s.script.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(ids_of(&sent[0]), ["kb-a", "kb-b"]);
}

#[tokio::test]
async fn groups_are_capped_by_the_calls_request_budget() {
    let ids: Vec<String> = (1..=10).map(|i| format!("kb-{i}")).collect();
    let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
    let s = setup(
        &refs,
        by_group(|id| ok(&[id], vec![hit(id, &format!("c-{id}"))])),
    );
    let (_, response) = s.backend.search(RUN, query(20)).await.unwrap();
    let sent = s.script.sent.lock().unwrap();
    // The first request, then the budget's remaining ones: no more.
    assert_eq!(sent.len(), CALL_REQUESTS as usize);
    assert_eq!(response.hits.len(), CALL_REQUESTS as usize - 1);
    // The groups that were not sent are said so, retryable.
    assert_eq!(response.completeness, crate::Completeness::Partial);
    let failed = response.partial_failures.unwrap();
    let failed = failed.as_slice();
    assert_eq!(failed.len(), 10 - (CALL_REQUESTS as usize - 1));
    assert!(failed.iter().all(|f| f.retryable
        && f.code == crate::PartialFailureCode::Unavailable
        && f.reason == crate::PartialFailureReason::Overloaded));
    // Every group request asks within what is left of the call's time.
    let mut last = u64::MAX;
    for request in &sent[1..] {
        let deadline = request["budget"]["deadline_ms"].as_u64().unwrap();
        assert!(deadline <= 15_000 && deadline <= last);
        last = deadline;
    }
}

/// R3.7(b) and R3.4: every group failed, so the answer is a `timeout` when all
/// of them timed out and `unavailable` otherwise; it is retryable only if every
/// failure is, and the reason is the one that is not retryable.
#[tokio::test]
async fn when_every_group_fails_the_error_is_the_one_the_contract_names() {
    let wire = |failure: Failure| match failure {
        Failure::Envelope(e) => serde_json::to_value(&e.error).unwrap(),
        other => panic!("{other:?}"),
    };
    let timeouts = setup(
        &["kb-a", "kb-b"],
        by_group(|_| error(504, "timeout", Some("timeout"), true)),
    );
    let got = wire(timeouts.backend.search(RUN, query(10)).await.unwrap_err());
    assert_eq!(got["code"], "timeout");
    assert_eq!(got["details"]["retryable"], json!(true));

    // Both groups hold only FAQ-style content: nothing to retry.
    let unsupported = setup(
        &["kb-a", "kb-b"],
        by_group(|_| error(503, "unavailable", Some("unsupported_kb_type"), false)),
    );
    let got = wire(
        unsupported
            .backend
            .search(RUN, query(10))
            .await
            .unwrap_err(),
    );
    assert_eq!(got["code"], "unavailable");
    assert_eq!(got["details"]["retryable"], json!(false));
    assert_eq!(got["details"]["reason"], "unsupported_kb_type");

    // One retryable, one not: not retryable, and the reason is the latter's.
    let mixed_kinds = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            "kb-a" => error(503, "unavailable", Some("dependency"), true),
            _ => error(503, "unavailable", Some("index_failed"), false),
        }),
    );
    let got = wire(
        mixed_kinds
            .backend
            .search(RUN, query(10))
            .await
            .unwrap_err(),
    );
    assert_eq!(got["details"]["retryable"], json!(false));
    assert_eq!(got["details"]["reason"], "index_failed");
}

/// A group that fails in transport is a partial failure like one that answers
/// `unavailable`; the other groups' answers are kept.
#[tokio::test]
async fn a_group_lost_in_transport_is_partial_not_fatal() {
    let s = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            "kb-a" => ok(&["kb-a"], vec![hit("kb-a", "a1")]),
            _ => (0, Value::Null),
        }),
    );
    let (_, response) = s.backend.search(RUN, query(10)).await.unwrap();
    assert_eq!(response.hits.len(), 1);
    assert_eq!(response.completeness, crate::Completeness::Partial);
    assert_eq!(
        response.partial_failures.unwrap().as_slice()[0].knowledge_base_id,
        "kb-b"
    );
}

/// A knowledge base that failed or was not sent stays in the effective scope,
/// as in a single request, so the answer is not flagged as narrowed for it.
#[tokio::test]
async fn failed_groups_stay_in_the_effective_scope() {
    let s = setup(
        &["kb-a", "kb-b"],
        by_group(|id| match id {
            "kb-a" => ok(&["kb-a"], vec![hit("kb-a", "a1")]),
            _ => error(504, "timeout", Some("timeout"), true),
        }),
    );
    let (_, response) = s.backend.search(RUN, query(10)).await.unwrap();
    assert_eq!(
        response.effective_scope.knowledge_base_ids.as_slice(),
        ["kb-a", "kb-b"]
    );
}

#[tokio::test]
async fn a_single_space_search_is_one_request_as_before() {
    let s = setup(
        &["kb-a", "kb-b"],
        Box::new(|_| {
            ok(
                &["kb-a", "kb-b"],
                vec![hit("kb-a", "a1"), hit("kb-b", "b1")],
            )
        }),
    );
    let (_, response) = s.backend.search(RUN, query(10)).await.unwrap();
    assert_eq!(s.script.sent.lock().unwrap().len(), 1);
    assert_eq!(response.hits.len(), 2);
}
