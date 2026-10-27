//! P03 HTTP observation fixtures (MEMORY-P1-READINESS-PROBES-20261023 §8.5).
//!
//! These are OBSERVATION tests: they pin what the production Axum router does
//! today to a `/api/v1/chat` request when memory is withdrawn / paused /
//! re-qualified WHILE THE PROVIDER HOLDS THE REQUEST. The barrier sits inside
//! `provider.complete()`, so the window pinned is "after the provider has
//! received the request". The window between "prelude built" and "provider
//! called" is NOT covered here (no hook exists there; that is the H04 send
//! barrier question). OpenAPI promises DELETE blocks NEW session recalls and
//! pause affects NEWLY started runs; it does not promise to rewrite a request
//! the provider already holds, and the report does not judge that an H04 bug
//! ("pause" keeps its existing recovery exception). Each withdrawal test pairs
//! the captured-request observation with readbacks proving the withdrawal
//! really took effect (list/settings GET plus a fresh-session chat), so
//! "withdrawn but the in-flight request kept its recall" is distinguishable
//! from "withdrawal never happened". If the sending contract is later
//! tightened, revisit these deliberately, never flip them silently.

use super::*;
use agent24_memory::assertion::{Assertion, AssertionStore};
use agent24_memory::event::Scope;
use agent24_models::router::{ModelRouter, Tier};
use agent24_models::{CompletionResponse, Msg};
use agent24_protocol::Usage;
use axum::body::to_bytes;
use axum::http::StatusCode;
use sqlx::Connection;
use std::sync::Arc;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

/// Every wait that could hang if the chat fails early gets a bound and a name.
const STEP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

async fn within<T>(step: &str, fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(STEP_TIMEOUT, fut)
        .await
        .unwrap_or_else(|_| panic!("timed out after {STEP_TIMEOUT:?} waiting for: {step}"))
}

const OWNER: &str = "p03-http-owner";
const ASSERTION_ID: &str = "p03-http-assertion";
const RECALL_TEXT: &str = "The secret project codename is Cedar.";

#[derive(Clone, Copy)]
enum FirstResult {
    Success,
    Unavailable,
}

struct BarrierProvider {
    provider_name: &'static str,
    entered: Arc<Notify>,
    release: Arc<Notify>,
    messages: Arc<Mutex<Vec<Vec<Msg>>>>,
    result: FirstResult,
}

#[async_trait::async_trait]
impl ModelProvider for BarrierProvider {
    fn name(&self) -> &str {
        self.provider_name
    }

    async fn complete(
        &self,
        req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        self.messages.lock().unwrap().push(req.messages.clone());
        self.entered.notify_one();
        within(
            "test releasing the held first provider",
            self.release.notified(),
        )
        .await;
        if matches!(self.result, FirstResult::Unavailable) {
            return Err(ModelError::Unavailable("probe fallback".into()));
        }
        Ok(ok_response())
    }

    async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
        Ok(vec![])
    }
}

struct SpyProvider {
    messages: Arc<Mutex<Vec<Vec<Msg>>>>,
}

#[async_trait::async_trait]
impl ModelProvider for SpyProvider {
    fn name(&self) -> &str {
        "p03-spy"
    }

    async fn complete(
        &self,
        req: &CompletionRequest,
        _cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        self.messages.lock().unwrap().push(req.messages.clone());
        Ok(ok_response())
    }

    async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
        Ok(vec![])
    }
}

fn ok_response() -> CompletionResponse {
    CompletionResponse {
        message: Msg::assistant(Some("ok".into()), vec![]),
        usage: Usage::default(),
        model_id: Some("p03-probe".into()),
    }
}

struct Harness {
    app: axum::Router,
    first_entered: Arc<Notify>,
    first_release: Arc<Notify>,
    first_messages: Arc<Mutex<Vec<Vec<Msg>>>>,
    fallback_messages: Arc<Mutex<Vec<Vec<Msg>>>>,
    _dir: tempfile::TempDir,
    db_path: std::path::PathBuf,
}

async fn harness(first_result: FirstResult, providers: usize) -> Harness {
    let dir = tempfile::tempdir().unwrap();
    let db_path = dir.path().join("memory.sqlite");
    let kv = agent24_memory::KvStore::open(&db_path).await.unwrap();
    let assertions = kv.assertions();
    assertions
        .assert(&Assertion::new(
            ASSERTION_ID,
            Scope::owner(OWNER),
            "project",
            "codename",
            serde_json::json!(RECALL_TEXT),
            vec![],
        ))
        .await
        .unwrap();
    kv.retriever().rebuild().await.unwrap();

    let first_entered = Arc::new(Notify::new());
    let first_release = Arc::new(Notify::new());
    let first_messages = Arc::new(Mutex::new(Vec::new()));
    let fallback_messages = Arc::new(Mutex::new(Vec::new()));
    let first: Arc<dyn ModelProvider> = Arc::new(BarrierProvider {
        provider_name: "p03-first",
        entered: first_entered.clone(),
        release: first_release.clone(),
        messages: first_messages.clone(),
        result: first_result,
    });
    let mut routed = vec![(first, Tier::Local)];
    if providers > 1 {
        routed.push((
            Arc::new(SpyProvider {
                messages: fallback_messages.clone(),
            }) as Arc<dyn ModelProvider>,
            // /chat is LocalOnly by default, so the fallback must also be
            // tagged local to remain eligible under the router's privacy gate.
            Tier::Local,
        ));
    }
    let router = Arc::new(ModelRouter::with_defaults(routed));
    let cancel = CancellationToken::new();
    let memory = agent24_agent::SessionMemory::new(
        kv,
        Arc::new(agent24_agent::RouterSummarizer::new(
            router.clone(),
            cancel.clone(),
        )),
    )
    .with_owner(OWNER.to_owned());
    let state = AppState::new(crate::server::AppDeps {
        token: "p03-token".into(),
        router,
        tools: agent24_tools::ToolRegistry::new(),
        store: agent24_store::Store::open_memory().await.unwrap(),
        shutdown: crate::server::Shutdown::new(cancel),
        guardian: None,
        memory: Some(memory),
        workspace_service: None,
        mcp_servers: Vec::new(),
        risk_overrides: Arc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
            Vec::new(),
        )),
        packages_root: Arc::new(dir.path().to_path_buf()),
    });
    Harness {
        app: crate::server::build_router_with_modules(state, axum::Router::new()),
        first_entered,
        first_release,
        first_messages,
        fallback_messages,
        _dir: dir,
        db_path,
    }
}

fn chat_request(session: Option<&str>) -> Request<Body> {
    let mut body = serde_json::json!({
        "messages": [{"role": "user", "content": "What is the project codename?"}]
    });
    if let Some(session) = session {
        body["session_id"] = serde_json::json!(session);
    }
    Request::builder()
        .method("POST")
        .uri("/api/v1/chat")
        .header("authorization", "Bearer p03-token")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

async fn call(app: axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
    let res = within("router response", app.oneshot(req)).await.unwrap();
    let status = res.status();
    let bytes = to_bytes(res.into_body(), 64 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

async fn mutate(app: axum::Router, method: &str, uri: &str, body: Option<&str>) -> StatusCode {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", "Bearer p03-token");
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let req = builder
        .body(Body::from(body.unwrap_or("").to_owned()))
        .unwrap();
    within("mutation request response", app.oneshot(req))
        .await
        .unwrap()
        .status()
}

async fn wait_for_first(h: &Harness) {
    within(
        "first provider to receive the chat request (chat failed before sending?)",
        h.first_entered.notified(),
    )
    .await;
}

async fn set_unqualified(h: &Harness) {
    // A separate file-backed SQLite connection is used only while the provider
    // is held at the barrier. No SQL transaction/lock spans a network await.
    let mut conn = sqlx::SqliteConnection::connect(&format!("sqlite://{}", h.db_path.display()))
        .await
        .unwrap();
    sqlx::query("UPDATE mem_assertions SET qualified = 0 WHERE id = ?")
        .bind(ASSERTION_ID)
        .execute(&mut conn)
        .await
        .unwrap();
    conn.close().await.unwrap();
}

/// GET the assertion ids the memory API lists for the daemon's owner.
async fn listed_assertion_ids(h: &Harness) -> Vec<String> {
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/memory/assertions")
        .header("authorization", "Bearer p03-token")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(h.app.clone(), req).await;
    assert_eq!(status, StatusCode::OK);
    body["assertions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["id"].as_str().unwrap().to_owned())
        .collect()
}

/// GET /memory/settings -> `enabled`.
async fn settings_enabled(h: &Harness) -> bool {
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/memory/settings")
        .header("authorization", "Bearer p03-token")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(h.app.clone(), req).await;
    assert_eq!(status, StatusCode::OK);
    body["enabled"].as_bool().unwrap()
}

/// Run a brand-new session's chat to completion after the first one finished
/// and return the messages of the single request any provider received for it
/// (after a fallback the router may skip the failed first provider, so both
/// recorders are checked). The release permit is stored beforehand so the
/// barrier does not block it.
async fn fresh_session_chat_messages(h: &Harness, session: &str) -> Vec<Msg> {
    let count = |h: &Harness| {
        (
            h.first_messages.lock().unwrap().len(),
            h.fallback_messages.lock().unwrap().len(),
        )
    };
    let (f0, b0) = count(h);
    h.first_release.notify_one();
    let (status, _) = call(h.app.clone(), chat_request(Some(session))).await;
    assert_eq!(status, StatusCode::OK);
    let (f1, b1) = count(h);
    assert_eq!(
        (f1 - f0) + (b1 - b0),
        1,
        "the fresh-session chat must reach a provider exactly once"
    );
    if f1 > f0 {
        h.first_messages.lock().unwrap()[f0].clone()
    } else {
        h.fallback_messages.lock().unwrap()[b0].clone()
    }
}

/// The recall messages (role + full content) in a provider request.
fn recall_messages(messages: &[Msg]) -> Vec<(String, String)> {
    messages
        .iter()
        .filter(|m| {
            m.content
                .as_deref()
                .is_some_and(|c| c.contains(RECALL_TEXT))
        })
        .map(|m| (m.role.clone(), m.content.clone().unwrap()))
        .collect()
}

fn has_recall(messages: &[Msg]) -> bool {
    messages.iter().any(|m| {
        m.content
            .as_deref()
            .is_some_and(|content| content.contains(RECALL_TEXT))
    })
}

#[tokio::test]
// Observes (report §8.5 bullet 1): HTTP DELETE of the assertion after the first provider
// already holds the request; the captured recall is still delivered. DELETE only blocks NEW recalls.
async fn post_chat_http_delete_after_prelude_observes_captured_recall() {
    let h = harness(FirstResult::Success, 1).await;
    let app = h.app.clone();
    let chat = tokio::spawn(async move { call(app, chat_request(Some("s-delete"))).await });
    wait_for_first(&h).await;
    assert_eq!(
        mutate(
            h.app.clone(),
            "DELETE",
            "/api/v1/memory/assertions/p03-http-assertion",
            None,
        )
        .await,
        StatusCode::NO_CONTENT
    );
    h.first_release.notify_one();
    assert_eq!(
        within("chat task to finish", chat).await.unwrap().0,
        StatusCode::OK
    );
    assert!(has_recall(&h.first_messages.lock().unwrap()[0]));
    // Readbacks: the DELETE really took effect, and a NEW session gets no recall.
    assert!(
        !listed_assertion_ids(&h)
            .await
            .contains(&ASSERTION_ID.to_owned())
    );
    assert!(!has_recall(
        &fresh_session_chat_messages(&h, "s-delete-new").await
    ));
}

#[tokio::test]
// Observes (report §8.5 bullet 2): PUT /memory/settings {enabled:false} after the prelude is built;
// the in-flight request keeps its recall. Pause affects new runs; not judged a bug.
async fn post_chat_http_pause_after_prelude_observes_captured_recall() {
    let h = harness(FirstResult::Success, 1).await;
    let app = h.app.clone();
    let chat = tokio::spawn(async move { call(app, chat_request(Some("s-pause"))).await });
    wait_for_first(&h).await;
    assert_eq!(
        mutate(
            h.app.clone(),
            "PUT",
            "/api/v1/memory/settings",
            Some(r#"{"enabled":false}"#),
        )
        .await,
        StatusCode::OK
    );
    h.first_release.notify_one();
    assert_eq!(
        within("chat task to finish", chat).await.unwrap().0,
        StatusCode::OK
    );
    assert!(has_recall(&h.first_messages.lock().unwrap()[0]));
    // Readbacks: the pause really took effect, and a NEW session gets no recall.
    assert!(!settings_enabled(&h).await);
    assert!(!has_recall(
        &fresh_session_chat_messages(&h, "s-pause-new").await
    ));
}

#[tokio::test]
// Observes (report §8.5 bullet 3): first Local provider returns Unavailable after a DELETE; the router
// fallback provider receives the SAME captured recall (no re-authorisation at fallback time).
async fn post_chat_http_unavailable_fallback_spy_observes_captured_recall_after_withdrawal() {
    let h = harness(FirstResult::Unavailable, 2).await;
    let app = h.app.clone();
    let chat = tokio::spawn(async move { call(app, chat_request(Some("s-fallback"))).await });
    wait_for_first(&h).await;
    assert_eq!(
        mutate(
            h.app.clone(),
            "DELETE",
            "/api/v1/memory/assertions/p03-http-assertion",
            None,
        )
        .await,
        StatusCode::NO_CONTENT
    );
    h.first_release.notify_one();
    assert_eq!(
        within("chat task to finish", chat).await.unwrap().0,
        StatusCode::OK
    );
    assert_eq!(h.first_messages.lock().unwrap().len(), 1);
    assert_eq!(h.fallback_messages.lock().unwrap().len(), 1);
    let first = recall_messages(&h.first_messages.lock().unwrap()[0]);
    let fallback = recall_messages(&h.fallback_messages.lock().unwrap()[0]);
    assert_eq!(first.len(), 1, "exactly one recall message: {first:?}");
    assert_eq!(
        first, fallback,
        "fallback receives the identical recall message (role + full content)"
    );
    // Readbacks: the DELETE really took effect, and a NEW session gets no recall.
    assert!(
        !listed_assertion_ids(&h)
            .await
            .contains(&ASSERTION_ID.to_owned())
    );
    assert!(!has_recall(
        &fresh_session_chat_messages(&h, "s-fallback-new").await
    ));
}

#[tokio::test]
// Observes (report §8.5 bullet 4): qualified flips 1->0 in SQLite while the provider is held (no SQL lock
// across the await); the captured request is a snapshot and is not rewritten.
async fn post_chat_http_qualification_change_after_prelude_does_not_rewrite_captured_messages() {
    let h = harness(FirstResult::Success, 1).await;
    let app = h.app.clone();
    let chat = tokio::spawn(async move { call(app, chat_request(Some("s-qualified"))).await });
    wait_for_first(&h).await;
    set_unqualified(&h).await;
    h.first_release.notify_one();
    assert_eq!(
        within("chat task to finish", chat).await.unwrap().0,
        StatusCode::OK
    );
    assert!(has_recall(&h.first_messages.lock().unwrap()[0]));
    let mut conn = sqlx::SqliteConnection::connect(&format!("sqlite://{}", h.db_path.display()))
        .await
        .unwrap();
    let qualified: i64 = sqlx::query_scalar("SELECT qualified FROM mem_assertions WHERE id = ?")
        .bind(ASSERTION_ID)
        .fetch_one(&mut conn)
        .await
        .unwrap();
    assert_eq!(qualified, 0);
    conn.close().await.unwrap();
}

#[tokio::test]
// Observes (report §8.5 bullet 5): no session_id => no memory prelude; provider gets exactly the client message.
async fn post_chat_http_without_session_sends_only_client_messages() {
    let h = harness(FirstResult::Success, 1).await;
    let app = h.app.clone();
    let chat = tokio::spawn(async move { call(app, chat_request(None)).await });
    wait_for_first(&h).await;
    h.first_release.notify_one();
    assert_eq!(
        within("chat task to finish", chat).await.unwrap().0,
        StatusCode::OK
    );
    let received = h.first_messages.lock().unwrap();
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0],
        vec![Msg {
            role: "user".into(),
            content: Some("What is the project codename?".into()),
            tool_calls: vec![],
            tool_call_id: None,
        }],
        "without session_id the provider receives exactly the client message"
    );
    assert!(!has_recall(&received[0]));
}
