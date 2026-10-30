#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

use agent24_knowledge::TransportResponse;
use agent24_knowledge::tool::call::{self, Refused};
use serde_json::{Value, json};

use super::*;
use crate::knowledge_host::{Change, Endpoint};

const HOST: &str = "host-cap";
const BINDING: &str = "b1";
const RUN: &str = "run-1";

fn fixture(name: &str) -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../protocol/knowledge/fixtures/v1")
        .join(name);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// What the fake server does with a request.
#[derive(Clone)]
enum Answer {
    /// The fixture's response, echoing the request's id and generation, and
    /// answering within the scope asked.
    Fixture(&'static str),
    /// The same, but whatever the fixture's scope and hits are.
    Unscoped(&'static str),
    /// An error fixture echoing another generation.
    OtherGeneration(&'static str),
    /// An error fixture echoing `null` for both.
    NullEcho(&'static str),
    /// The fixture's response with another request id.
    WrongEcho(&'static str),
    /// As `Fixture`, with the value at a JSON pointer of the body replaced.
    Altered(&'static str, &'static str, fn() -> Value),
    /// Never answers.
    Hang,
    Fail(TransportError),
}

struct Server {
    answer: Answer,
    /// Run while the request is out, before the answer comes back.
    during: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    sent: Mutex<Vec<(KnowledgeOperation, Value)>>,
}

#[async_trait]
impl KnowledgeTransport for Server {
    async fn send(
        &self,
        op: KnowledgeOperation,
        body: Vec<u8>,
        _deadline: Duration,
    ) -> Result<TransportResponse, TransportError> {
        let request: Value = serde_json::from_slice(&body).unwrap();
        self.sent.lock().unwrap().push((op, request.clone()));
        if let Some(during) = self.during.lock().unwrap().take() {
            during();
        }
        let mut scoped = false;
        let mut altered = None;
        let mut generation = request["generation"].clone();
        let (name, echo) = match self.answer.clone() {
            Answer::Fixture(name) => {
                scoped = true;
                (name, request["request_id"].clone())
            }
            Answer::Altered(name, pointer, value) => {
                scoped = true;
                altered = Some((pointer, value));
                (name, request["request_id"].clone())
            }
            Answer::Unscoped(name) => (name, request["request_id"].clone()),
            Answer::OtherGeneration(name) => {
                generation = json!(999);
                (name, request["request_id"].clone())
            }
            Answer::WrongEcho(name) => (name, json!("another-request")),
            Answer::NullEcho(name) => {
                generation = Value::Null;
                (name, Value::Null)
            }
            Answer::Hang => std::future::pending().await,
            Answer::Fail(e) => return Err(e),
        };
        let response = fixture(name)["response"].clone();
        let mut body = response["body"].clone();
        let echoes = if body.get("error").is_some() {
            &mut body["error"]["details"]
        } else {
            &mut body
        };
        echoes["request_id"] = echo;
        echoes["generation"] = generation;
        if scoped && body.get("effective_scope").is_some() {
            let asked = request["scope"]["knowledge_base_ids"].clone();
            let ids: Vec<Value> = asked.as_array().unwrap().clone();
            body["effective_scope"]["knowledge_base_ids"] = asked;
            if let Some(hits) = body.get_mut("hits").and_then(Value::as_array_mut) {
                hits.retain(|hit| ids.contains(&hit["knowledge_base_id"]));
            }
        }
        if let Some((pointer, value)) = altered {
            *body.pointer_mut(pointer).unwrap() = value();
        }
        Ok(TransportResponse {
            status: u16::try_from(response["status"].as_u64().unwrap()).unwrap(),
            body: serde_json::to_vec(&body).unwrap(),
        })
    }
}

struct Runs(HashMap<String, String>);

#[async_trait]
impl RunSelector for Runs {
    async fn knowledge_binding_id(&self, run_id: &str) -> Option<String> {
        self.0.get(run_id).cloned()
    }
}

struct Setup {
    registry: Arc<KnowledgeHostRegistry>,
    server: Arc<Server>,
    connects: Arc<AtomicUsize>,
    backend: DaemonKnowledgeBackend,
}

fn endpoint() -> Endpoint {
    Endpoint {
        base_url: "https://kb.example/api/v1".into(),
        knowledge_base_ids: vec!["kb-a".into(), "kb-b".into()],
    }
}

fn push(registry: &KnowledgeHostRegistry, seq: u64, value: &str) {
    let expires = unix_now() + 3600;
    registry
        .push_credential(
            HOST,
            BINDING,
            seq,
            Credential::ApiKey(value.into()),
            expires,
        )
        .unwrap();
}

/// A run pinned to an active binding with an endpoint and C.
fn setup(answer: Answer) -> Setup {
    let registry = Arc::new(KnowledgeHostRegistry::default());
    registry.activate(HOST, BINDING, 5, endpoint()).unwrap();
    push(&registry, 6, "key-1");
    registry.pin_run(
        RUN,
        RunBinding {
            host: HOST.into(),
            binding: BINDING.into(),
            generation: 5,
        },
    );
    let server = Arc::new(Server {
        answer,
        during: Mutex::new(None),
        sent: Mutex::new(Vec::new()),
    });
    let connects = Arc::new(AtomicUsize::new(0));
    let (to, count) = (Arc::clone(&server), Arc::clone(&connects));
    let connect: Box<Connect> = Box::new(move |base, _credential| {
        assert_eq!(base, "https://kb.example/api/v1");
        count.fetch_add(1, Ordering::SeqCst);
        let transport: Arc<dyn KnowledgeTransport> = to.clone();
        Some(transport)
    });
    let runs = Runs(HashMap::from([(RUN.to_owned(), BINDING.to_owned())]));
    let backend =
        DaemonKnowledgeBackend::with_connect(Arc::clone(&registry), Arc::new(runs), connect);
    Setup {
        registry,
        server,
        connects,
        backend,
    }
}

fn query(ids: Option<Vec<&str>>) -> SearchQuery {
    SearchQuery {
        query: "报销流程".into(),
        knowledge_base_ids: ids.map(|ids| {
            NonEmpty::try_from(ids.into_iter().map(String::from).collect::<Vec<_>>()).unwrap()
        }),
        max_results: positive(10),
    }
}

fn chunk_read() -> ReadQuery {
    ReadQuery {
        knowledge_id: "doc-7".into(),
        chunk_id: Some("chk-71".into()),
        content_tag: Some("ct-chk-71".into()),
        cursor: None,
    }
}

fn is_cancelled(result: Result<impl std::fmt::Debug, Failure>) -> bool {
    matches!(result, Err(Failure::Cancelled(_)))
}

#[tokio::test]
async fn a_search_goes_to_the_binding_scope_under_its_generation() {
    let s = setup(Answer::Fixture("search.ok.json"));
    let (request, response) = s.backend.search(RUN, query(None)).await.unwrap();
    assert_eq!(request.generation, 5);
    assert_eq!(response.request_id, request.request_id);
    let sent = s.server.sent.lock().unwrap();
    let (op, body) = &sent[0];
    assert_eq!(*op, KnowledgeOperation::Search);
    assert_eq!(body["scope"]["knowledge_base_ids"], json!(["kb-a", "kb-b"]));
    assert_eq!(body["budget"]["max_results"], json!(10));
}

#[tokio::test]
async fn the_knowledge_bases_named_are_cut_to_the_binding_and_none_left_sends_nothing() {
    let s = setup(Answer::Fixture("search.ok.json"));
    s.backend
        .search(RUN, query(Some(vec!["kb-b", "kb-z"])))
        .await
        .unwrap();
    let sent = s.server.sent.lock().unwrap()[0].1.clone();
    assert_eq!(sent["scope"]["knowledge_base_ids"], json!(["kb-b"]));

    let outside = s.backend.search(RUN, query(Some(vec!["kb-z"]))).await;
    let Err(Failure::Envelope(envelope)) = outside else {
        panic!("{outside:?}");
    };
    assert_eq!(
        envelope.error.code(),
        agent24_knowledge::ErrorCode::Forbidden
    );
    assert_eq!(s.server.sent.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn an_answer_outside_the_scope_asked_is_refused() {
    // The fixture answers for kb-a and kb-b; only kb-b was asked.
    let s = setup(Answer::Unscoped("search.ok.json"));
    let result = s.backend.search(RUN, query(Some(vec!["kb-b"]))).await;
    assert!(
        matches!(
            &result,
            Err(Failure::Local(Refusal {
                reason: LocalReason::Incompatible,
                ..
            }))
        ),
        "{result:?}"
    );
    // The same answer to a search of both is inside.
    s.backend.search(RUN, query(None)).await.unwrap();

    // A read answered for a knowledge base the binding does not have.
    let s = setup(Answer::Unscoped("read.chunk.json"));
    let result = s.backend.read(RUN, chunk_read()).await;
    assert!(
        matches!(
            &result,
            Err(Failure::Local(Refusal {
                reason: LocalReason::Incompatible,
                ..
            }))
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn a_read_answered_with_another_document_or_chunk_is_refused() {
    let incompatible = |result: &Result<_, Failure>| {
        matches!(
            result,
            Err(Failure::Local(Refusal {
                reason: LocalReason::Incompatible,
                ..
            }))
        )
    };
    let cases: [(&str, fn() -> Value); 3] = [
        ("/knowledge_id", || json!("doc-other")),
        ("/segments/0/chunk_id", || json!("chk-other")),
        // That chunk alone (R4.3), so not none of it either.
        ("/segments", || json!([])),
    ];
    for (pointer, value) in cases {
        let s = setup(Answer::Altered("read.chunk.json", pointer, value));
        let result = s.backend.read(RUN, chunk_read()).await;
        assert!(incompatible(&result), "{pointer}: {result:?}");
    }
    // A document read may answer with any of its chunks, not another document.
    let document = ReadQuery {
        chunk_id: None,
        content_tag: None,
        ..chunk_read()
    };
    let s = setup(Answer::Altered(
        "read.chunk.json",
        "/segments/0/chunk_id",
        || json!("chk-other"),
    ));
    s.backend.read(RUN, document.clone()).await.unwrap();
    let s = setup(Answer::Altered("read.chunk.json", "/knowledge_id", || {
        json!("doc-other")
    }));
    assert!(incompatible(&s.backend.read(RUN, document).await));
}

#[tokio::test]
async fn an_error_answer_for_another_request_or_generation_is_dropped() {
    let s = setup(Answer::WrongEcho("search.unauthorized-revoked.json"));
    assert!(is_cancelled(s.backend.search(RUN, query(None)).await));
    let s = setup(Answer::OtherGeneration("search.unauthorized-revoked.json"));
    assert!(is_cancelled(s.backend.search(RUN, query(None)).await));

    // `null` echoes: allowed for an `unauthorized` (it may fail before the
    // body is read), not for a `timeout`.
    let s = setup(Answer::NullEcho("search.unauthorized-revoked.json"));
    let result = s.backend.search(RUN, query(None)).await;
    assert!(matches!(result, Err(Failure::Envelope(_))), "{result:?}");
    let s = setup(Answer::NullEcho("search.timeout.json"));
    assert!(is_cancelled(s.backend.search(RUN, query(None)).await));
}

#[tokio::test]
async fn a_chunk_read_carries_the_content_tag_and_the_binding_scope() {
    let s = setup(Answer::Fixture("read.chunk.json"));
    s.backend.read(RUN, chunk_read()).await.unwrap();
    let sent = s.server.sent.lock().unwrap()[0].1.clone();
    assert_eq!(sent["chunk_id"], json!("chk-71"));
    assert_eq!(sent["expected_revision"]["content_tag"], json!("ct-chk-71"));
    assert_eq!(sent["scope"]["knowledge_base_ids"], json!(["kb-a", "kb-b"]));
    assert_eq!(sent["generation"], json!(5));
}

#[tokio::test]
async fn a_run_without_a_usable_binding_sends_nothing() {
    let s = setup(Answer::Fixture("search.ok.json"));
    let unbound = s.backend.search("run-2", query(None)).await;
    assert!(matches!(unbound, Err(Failure::Unbound)), "{unbound:?}");

    s.registry.activate(HOST, BINDING, 7, endpoint()).unwrap();
    let stale = s.backend.search(RUN, query(None)).await;
    assert!(matches!(stale, Err(Failure::BindingStale)), "{stale:?}");
    assert!(s.server.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn without_an_endpoint_or_a_credential_the_call_is_refused_locally() {
    let s = setup(Answer::Fixture("search.ok.json"));
    s.registry
        .apply(HOST, BINDING, 7, Change::RevokeCredential)
        .unwrap();
    let no_c = s.backend.search(RUN, query(None)).await;
    assert!(
        matches!(
            &no_c,
            Err(Failure::Local(Refusal {
                reason: LocalReason::NotReady,
                retryable: true
            }))
        ),
        "{no_c:?}"
    );

    let registry = Arc::new(KnowledgeHostRegistry::default());
    registry.apply(HOST, BINDING, 1, Change::Activate).unwrap();
    registry.pin_run(
        RUN,
        RunBinding {
            host: HOST.into(),
            binding: BINDING.into(),
            generation: 1,
        },
    );
    registry
        .push_credential(
            HOST,
            BINDING,
            2,
            Credential::ApiKey("k".into()),
            unix_now() + 60,
        )
        .unwrap();
    let runs = Runs(HashMap::from([(RUN.to_owned(), BINDING.to_owned())]));
    let backend =
        DaemonKnowledgeBackend::with_connect(registry, Arc::new(runs), Box::new(|_, _| None));
    let no_endpoint = backend.search(RUN, query(None)).await;
    assert!(
        matches!(
            &no_endpoint,
            Err(Failure::Local(Refusal {
                reason: LocalReason::ProfileInactive,
                ..
            }))
        ),
        "{no_endpoint:?}"
    );
    assert!(s.server.sent.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_deactivation_while_the_call_is_out_abandons_it_at_once() {
    let s = setup(Answer::Hang);
    let registry = Arc::clone(&s.registry);
    let call = s.backend.search(RUN, query(None));
    let off = async {
        // Let the request go out first.
        while s.server.sent.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
        registry
            .apply(HOST, BINDING, 9, Change::Deactivate)
            .unwrap();
    };
    let (result, ()) =
        tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(call, off) })
            .await
            .expect("the call must not wait for the server once its binding is gone");
    assert!(is_cancelled(result));
}

#[tokio::test]
async fn an_answer_to_a_credential_since_replaced_or_revoked_is_dropped() {
    for revoke in [false, true] {
        let s = setup(Answer::Fixture("search.ok.json"));
        let registry = Arc::clone(&s.registry);
        *s.server.during.lock().unwrap() = Some(Box::new(move || {
            if revoke {
                registry
                    .apply(HOST, BINDING, 8, Change::RevokeCredential)
                    .unwrap();
            } else {
                push(&registry, 8, "key-2");
            }
        }));
        assert!(
            is_cancelled(s.backend.search(RUN, query(None)).await),
            "revoke={revoke}"
        );
    }
}

#[tokio::test]
async fn an_answer_that_echoes_another_request_is_dropped() {
    let s = setup(Answer::WrongEcho("search.ok.json"));
    assert!(is_cancelled(s.backend.search(RUN, query(None)).await));
}

#[tokio::test]
async fn the_server_error_envelope_reaches_the_tool() {
    let s = setup(Answer::Fixture("search.unauthorized-revoked.json"));
    let result = s.backend.search(RUN, query(None)).await;
    let Err(Failure::Envelope(envelope)) = result else {
        panic!("{result:?}");
    };
    assert_eq!(
        envelope.error.code(),
        agent24_knowledge::ErrorCode::Unauthorized
    );
}

#[tokio::test]
async fn an_unreachable_server_is_a_retryable_dependency_refusal() {
    let s = setup(Answer::Fail(TransportError::Unreachable));
    let result = s.backend.search(RUN, query(None)).await;
    assert!(
        matches!(
            &result,
            Err(Failure::Local(Refusal {
                reason: LocalReason::Dependency,
                retryable: true
            }))
        ),
        "{result:?}"
    );
}

#[tokio::test]
async fn the_transport_is_built_again_only_when_the_credential_moves() {
    let s = setup(Answer::Fixture("search.ok.json"));
    s.backend.search(RUN, query(None)).await.unwrap();
    s.backend.search(RUN, query(None)).await.unwrap();
    assert_eq!(s.connects.load(Ordering::SeqCst), 1);
    push(&s.registry, 10, "key-2");
    s.backend.search(RUN, query(None)).await.unwrap();
    assert_eq!(s.connects.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn the_tool_layer_renders_what_the_backend_returns() {
    let s = setup(Answer::Fixture("search.ok.json"));
    let input = json!({ "query": "报销流程" });
    let out = call::search(&s.backend, RUN, input.as_object().unwrap())
        .await
        .unwrap();
    assert!(out.contains("chk-11"), "{out}");

    let s = setup(Answer::Hang);
    s.registry
        .apply(HOST, BINDING, 9, Change::Deactivate)
        .unwrap();
    let refused = call::search(&s.backend, RUN, input.as_object().unwrap()).await;
    assert!(
        matches!(refused, Err(Refused::Failed(text)) if text.contains("knowledge_binding_stale"))
    );
}

#[tokio::test]
async fn the_real_transport_goes_to_the_activated_endpoint() {
    // A loopback port nothing listens on: the HTTP transport is built for the
    // endpoint and C, and the refused connection is a dependency refusal.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let registry = Arc::new(KnowledgeHostRegistry::default());
    let endpoint = Endpoint {
        base_url: format!("http://127.0.0.1:{port}/api/v1"),
        knowledge_base_ids: vec!["kb-a".into()],
    };
    registry.activate(HOST, BINDING, 1, endpoint).unwrap();
    push(&registry, 2, "key-1");
    registry.pin_run(
        RUN,
        RunBinding {
            host: HOST.into(),
            binding: BINDING.into(),
            generation: 1,
        },
    );
    let runs = Runs(HashMap::from([(RUN.to_owned(), BINDING.to_owned())]));
    let backend = DaemonKnowledgeBackend::new(registry, Arc::new(runs));
    let result = backend.search(RUN, query(None)).await;
    assert!(
        matches!(
            &result,
            Err(Failure::Local(Refusal {
                reason: LocalReason::Dependency,
                retryable: true
            }))
        ),
        "{result:?}"
    );
}
