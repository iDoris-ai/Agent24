//! v1 REST handlers beyond health (B3: chat / models / usage).

use std::sync::Mutex;

use agent24_models::router::TaskProfile;
use agent24_models::{CompletionRequest, ModelError};
use agent24_protocol::{
    ChatRequest, ChatResponse, ErrorBody, EventBody, Model, ModelDeltaPayload, RunCompletedPayload,
    RunFailedPayload, RunOutputPayload, RunStartedPayload, Usage,
};
use agent24_store::ModelUsageRow;
use axum::body::Body;
use axum::extract::{RawQuery, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};
use serde::Serialize;

use crate::server::{AppState, error_response};

// The v1 body cap and reader live in the CONTRACT crate since ME-1b: a domain OS
// in its own crate must produce the same envelope and enforce the same limit, and
// two copies would drift. Re-exported (not redefined) so every existing call site
// keeps working and there is exactly ONE definition.
pub use agent24_domain::http::read_body_or_response;

/// Single guarded value (not three independent atomics): record+snapshot are
/// each atomic as a whole, so a snapshot can never observe a torn update where
/// total != prompt + completion (review finding on B3).
#[derive(Default)]
pub struct UsageCounters {
    inner: Mutex<Usage>,
}

impl UsageCounters {
    fn record(&self, usage: &Usage) {
        if let Ok(mut u) = self.inner.lock() {
            u.prompt_tokens = u.prompt_tokens.saturating_add(usage.prompt_tokens);
            u.completion_tokens = u.completion_tokens.saturating_add(usage.completion_tokens);
            u.total_tokens = u.total_tokens.saturating_add(usage.total_tokens);
        }
    }

    fn snapshot(&self) -> Usage {
        self.inner.lock().map(|u| u.clone()).unwrap_or_default()
    }
}

pub async fn get_models(State(state): State<AppState>) -> Response {
    let cancel = state.shutdown.child_token();
    let models: Vec<Model> = state.router.models(&cancel).await;
    Json(serde_json::json!({ "models": models })).into_response()
}

/// One `(module, day, served_by)` row's six counters, projected into the
/// wire shape (design §6.5): the store's [`ModelUsageRow`] plus a derived
/// `total_tokens`, never a raw `i64`/negative value (the store already
/// guarantees non-negative `u64`s).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
struct UsageCounts {
    calls_ok: u64,
    calls_failed: u64,
    calls_cancelled: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    total_tokens: u64,
}

impl From<&ModelUsageRow> for UsageCounts {
    fn from(row: &ModelUsageRow) -> Self {
        Self {
            calls_ok: row.calls_ok,
            calls_failed: row.calls_failed,
            calls_cancelled: row.calls_cancelled,
            prompt_tokens: row.prompt_tokens,
            completion_tokens: row.completion_tokens,
            total_tokens: row.prompt_tokens.saturating_add(row.completion_tokens),
        }
    }
}

impl UsageCounts {
    /// Only ever adds `by_served`'s (at most three, already individually
    /// `<= i64::MAX`) rows together to build `totals` — nowhere near
    /// `u64::MAX`, so a plain saturating add (not `agent24-store`'s
    /// `i64::MAX`-ceiling one) is the right tool here.
    fn plus(self, other: Self) -> Self {
        Self {
            calls_ok: self.calls_ok.saturating_add(other.calls_ok),
            calls_failed: self.calls_failed.saturating_add(other.calls_failed),
            calls_cancelled: self.calls_cancelled.saturating_add(other.calls_cancelled),
            prompt_tokens: self.prompt_tokens.saturating_add(other.prompt_tokens),
            completion_tokens: self
                .completion_tokens
                .saturating_add(other.completion_tokens),
            total_tokens: self.total_tokens.saturating_add(other.total_tokens),
        }
    }
}

/// §6.5: `by_served`'s three keys are always present, even at all zero — a
/// module that only ever used one tier must not make readers guess whether a
/// missing key means "zero" or "unsupported".
#[derive(Debug, Clone, Copy, Default, Serialize)]
struct ByServed {
    local: UsageCounts,
    remote: UsageCounts,
    none: UsageCounts,
}

#[derive(Debug, Clone, Serialize)]
struct DailyUsage {
    day: String,
    served_by: String,
    #[serde(flatten)]
    counts: UsageCounts,
}

/// §6.5's response shape for `GET /api/v1/usage?module=<name>`. `cost_usd` is
/// always JSON `null` here — not `0.0` (§6.4): a per-module remote call's
/// true cost is unknown, not free, and a stored `0.0` would be a false
/// statement of fact rather than an honest "we don't have a price list yet".
#[derive(Debug, Clone, Serialize)]
struct ModuleUsageResponse {
    module: String,
    totals: UsageCounts,
    by_served: ByServed,
    daily: Vec<DailyUsage>,
    cost_usd: Option<f64>,
}

/// §6.5 (v2 L5): at most one `module` key in the raw query string decides
/// everything here — the rest of the query, however malformed
/// (`?%zz`, `?a=1&a=2`, …), never affects the answer. `serde_urlencoded`'s
/// parse is itself lenient about stray `%`/repeated keys; if it fails
/// outright (e.g. invalid UTF-8 after percent-decoding), that is treated the
/// same as "no `module` key at all" — the global counters, not an error —
/// which is what keeps `?%zz` a plain 200.
///
/// - `Ok(None)`: no `module` key — read the global in-memory counters.
/// - `Ok(Some(name))`: exactly one `module` key, value `name` (not yet
///   validated as a legal module name — the caller does that).
/// - `Err(())`: two or more `module` keys — `400 invalid_request`.
fn module_selector(raw: Option<&str>) -> Result<Option<String>, ()> {
    let pairs: Vec<(String, String)> =
        serde_urlencoded::from_str(raw.unwrap_or("")).unwrap_or_default();
    let mut modules = pairs
        .into_iter()
        .filter(|(k, _)| k == "module")
        .map(|(_, v)| v);
    match (modules.next(), modules.next()) {
        (None, _) => Ok(None),
        (Some(name), None) => Ok(Some(name)),
        (Some(_), Some(_)) => Err(()),
    }
}

/// `GET /api/v1/usage?module=<name>` (design §6.5). Without a `module` key
/// this is byte-for-byte the pre-4.2.3b behavior: the global `/api/v1/chat`
/// counters, untouched by module calls. With one, it reads
/// `agent24-store`'s per-module aggregate instead — a module that never
/// called anything (or was uninstalled) gets a legal, all-zero response, not
/// a 404: the table simply has no rows for it yet.
pub async fn get_usage(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let module = match module_selector(raw.as_deref()) {
        Ok(None) => return Json(state.usage.snapshot()).into_response(),
        Ok(Some(name)) => name,
        Err(()) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "at most one module query parameter is allowed",
            );
        }
    };
    if !agent24_domain::is_valid_module_name(&module) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "module is not a valid module name",
        );
    }

    let totals = match state.store.module_model_usage_totals(&module).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("reading module usage totals for {module}: {e}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not read module usage",
            );
        }
    };
    // Newest 30 UTC calendar days, today included (§6.5's "⚖️"): at most 3
    // rows/day (one per served_by), so this is at most 90 rows without any
    // extra LIMIT — the store never materializes empty days, so "only
    // non-empty rows" (§6.5) falls out of the query itself.
    let since_day = (chrono::Utc::now().date_naive() - chrono::Duration::days(29))
        .format("%Y-%m-%d")
        .to_string();
    let daily_rows = match state.store.module_model_usage(&module, &since_day).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::error!("reading module usage detail for {module}: {e}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not read module usage",
            );
        }
    };

    let mut by_served = ByServed::default();
    for row in &totals {
        let counts = UsageCounts::from(row);
        match row.served_by.as_str() {
            "local" => by_served.local = counts,
            "remote" => by_served.remote = counts,
            "none" => by_served.none = counts,
            other => tracing::warn!(
                served_by = other,
                "module_model_usage_totals returned a served_by outside the closed set"
            ),
        }
    }
    let totals_counts = by_served.local.plus(by_served.remote).plus(by_served.none);
    let daily = daily_rows
        .iter()
        .map(|row| DailyUsage {
            day: row.day.clone(),
            served_by: row.served_by.clone(),
            counts: UsageCounts::from(row),
        })
        .collect();

    Json(ModuleUsageResponse {
        module,
        totals: totals_counts,
        by_served,
        daily,
        cost_usd: None,
    })
    .into_response()
}

pub async fn post_chat(State(state): State<AppState>, req: Request<Body>) -> Response {
    // The third copy of this logic used to live inline here; it is byte-for-byte
    // the shared reader's behavior (413 only on a real length-limit hit, 400 for a
    // disconnect or malformed transfer encoding).
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let parsed: Result<ChatRequest, _> = serde_json::from_slice(&bytes);
    let chat = match parsed {
        Ok(c) if !c.messages.is_empty() => c,
        _ => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "messages must be a non-empty array of {role, content}",
            );
        }
    };

    let request = CompletionRequest {
        // /chat is the plain conversational surface — no tools offered here;
        // tool-using work goes through /runs (the agent loop)
        messages: chat
            .messages
            .iter()
            .map(|m| agent24_models::Msg {
                role: m.role.clone(),
                content: Some(m.content.clone()),
                tool_calls: vec![],
                tool_call_id: None,
            })
            .collect(),
        model: chat.model,
        tools: vec![],
        response_format: None,
        max_tokens: None,
    };
    // Transient run: session_id null, full run lifecycle events (SPEC-002 §2)
    let run_id = format!("run_{}", agent24_core::util::ulid());
    state
        .events
        .broadcast(EventBody::RunStarted(RunStartedPayload {
            run_id: run_id.clone(),
            session_id: None,
            schedule_id: None,
        }));

    // Child of the daemon shutdown token — shutdown cancels in-flight provider
    // calls; run-level cancellation joins this in C2
    let cancel = state.shutdown.child_token();
    // Default profile: shareable + simple → local-first tier order, so everyday
    // chat prefers the on-device model and only falls back outward (D2).
    match state
        .router
        .complete(TaskProfile::default(), &request, &cancel)
        .await
    {
        Ok((provider, res)) => {
            tracing::debug!("chat served by {provider}");
            state.usage.record(&res.usage);
            let text = res.message.content.clone().unwrap_or_default();
            state
                .events
                .broadcast(EventBody::ModelDelta(ModelDeltaPayload {
                    run_id: run_id.clone(),
                    text: text.clone(),
                }));
            state
                .events
                .broadcast(EventBody::RunCompleted(RunCompletedPayload {
                    run_id,
                    output: RunOutputPayload { text: text.clone() },
                    usage: res.usage.clone(),
                }));
            Json(ChatResponse {
                message: agent24_protocol::ChatMessage {
                    role: res.message.role,
                    content: text,
                },
                usage: res.usage,
            })
            .into_response()
        }
        Err(err) => {
            let (status, code, message) = match err {
                ModelError::Unavailable(msg) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "provider_unavailable",
                    format!("All LLM providers unavailable. Last error: {msg}"),
                ),
                ModelError::Cancelled => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "request cancelled".to_owned(),
                ),
                ModelError::Provider(msg) => (StatusCode::INTERNAL_SERVER_ERROR, "internal", msg),
                // ME4-S2 L3: same handling as `Provider` — same `Display`
                // text, so `/api/v1/chat`'s response body is unchanged.
                ModelError::Rejected { message, .. } => {
                    (StatusCode::INTERNAL_SERVER_ERROR, "internal", message)
                }
            };
            state
                .events
                .broadcast(EventBody::RunFailed(RunFailedPayload {
                    run_id,
                    error: ErrorBody {
                        code: code.to_owned(),
                        message: message.clone(),
                        hint: None,
                        details: None,
                    },
                }));
            error_response(status, code, &message)
        }
    }
}

/// `GET /api/v1/tools` — the registered tool list (builtin/mcp/module).
///
/// Reports each tool's EFFECTIVE risk class (declared, as adjusted by the
/// user's H2 overrides) rather than the declared one: the endpoint answers
/// "what happens if this is called", and showing a declared `external` for a
/// tool the user relaxed to `read` would misdescribe the next dispatch.
pub async fn get_tools(State(state): State<AppState>) -> Response {
    Json(serde_json::json!({ "tools": state.tools.list() })).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_models::ModelProvider;
    use agent24_models::router::{ModelRouter, Tier};
    use axum::http::Request;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    // ── module_selector (§6.5, unit level) ──────────────────────────────────

    #[test]
    fn no_module_key_selects_the_global_counters() {
        assert_eq!(module_selector(None), Ok(None));
        assert_eq!(module_selector(Some("")), Ok(None));
        assert_eq!(
            module_selector(Some("a=1&b=2")),
            Ok(None),
            "keys other than 'module', however many, never select a module"
        );
    }

    #[test]
    fn malformed_query_falls_back_to_the_global_counters_not_an_error() {
        // `%zz` is not valid percent-encoding; serde_urlencoded's parse of
        // the whole string fails, which module_selector treats the same as
        // "no module key" — never a 400 (v2 L5).
        assert_eq!(module_selector(Some("%zz")), Ok(None));
    }

    #[test]
    fn exactly_one_module_key_selects_it() {
        assert_eq!(
            module_selector(Some("module=sin90")),
            Ok(Some("sin90".to_owned()))
        );
        assert_eq!(
            module_selector(Some("a=1&module=sin90&b=2")),
            Ok(Some("sin90".to_owned())),
            "other keys around it are ignored"
        );
    }

    #[test]
    fn two_or_more_module_keys_is_rejected() {
        assert_eq!(module_selector(Some("module=a&module=b")), Err(()));
    }

    // ── J12: GET /api/v1/usage?module= (HTTP level) ─────────────────────────

    /// Always answers with the golden usage `{prompt_tokens:3,
    /// completion_tokens:2, total_tokens:5}` (v3 L-d) — the same numbers
    /// `model_callback.rs`'s own handler-level `Stub` uses, chosen there for
    /// the same reason: an unmistakable, non-symmetric fingerprint.
    struct Stub;

    #[async_trait::async_trait]
    impl ModelProvider for Stub {
        fn name(&self) -> &str {
            "stub"
        }
        async fn complete(
            &self,
            _req: &agent24_models::CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<agent24_models::CompletionResponse, ModelError> {
            Ok(agent24_models::CompletionResponse {
                message: agent24_models::Msg::assistant(Some("ok".into()), vec![]),
                usage: Usage {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    total_tokens: 5,
                    cost_usd: 0.0,
                },
                model_id: None,
            })
        }
        async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Ok(vec![])
        }
    }

    /// A daemon state with `Stub` as its only (Local) provider, and a real
    /// `Store` — everything `GET /api/v1/usage?module=` and `POST
    /// /api/v1/chat` need, wired the same way `crate::server::tests::state`
    /// wires an empty router (that helper takes no provider list, so this is
    /// its own small builder rather than a parameter added to a
    /// widely-shared one).
    async fn state_with_stub() -> AppState {
        crate::server::AppState::new(crate::server::AppDeps {
            token: "testtoken".to_owned(),
            router: std::sync::Arc::new(ModelRouter::with_defaults(vec![(
                std::sync::Arc::new(Stub),
                Tier::Local,
            )])),
            tools: agent24_tools::ToolRegistry::new(),
            store: agent24_store::Store::open_memory().await.unwrap(),
            shutdown: crate::server::Shutdown::new(CancellationToken::new()),
            guardian: None,
            memory: None,
            mcp_servers: Vec::new(),
            risk_overrides: std::sync::Arc::new(
                agent24_policy::overrides::RiskOverrideStore::from_rows(Vec::new()),
            ),
            packages_root: std::sync::Arc::new(
                tempfile::tempdir().expect("tempdir").path().to_path_buf(),
            ),
        })
    }

    fn router(state: AppState) -> axum::Router {
        crate::server::build_router_with_modules(state, axum::Router::new())
    }

    async fn get(router: axum::Router, token: &str, uri: &str) -> (StatusCode, serde_json::Value) {
        let res = router
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn post_chat_once(router: axum::Router, token: &str) {
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/chat")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(Body::from(
                        r#"{"messages":[{"role":"user","content":"hi"}]}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::OK,
            "the stub /chat call must succeed"
        );
    }

    /// The golden literal (v3 L-d): unchanged by 4.2.3b, byte-for-byte.
    fn golden_global_usage() -> serde_json::Value {
        serde_json::json!({
            "prompt_tokens": 3,
            "completion_tokens": 2,
            "total_tokens": 5,
            "cost_usd": 0.0
        })
    }

    #[tokio::test]
    async fn no_module_key_returns_the_unchanged_global_counters() {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        post_chat_once(router(state.clone()), &token).await;

        let (status, body) = get(router(state.clone()), &token, "/api/v1/usage").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, golden_global_usage());
    }

    #[tokio::test]
    async fn a_malformed_query_still_returns_the_same_golden_global_literal() {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        post_chat_once(router(state.clone()), &token).await;

        let (status, body) = get(router(state.clone()), &token, "/api/v1/usage?%zz").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, golden_global_usage());

        let (status, body) = get(router(state.clone()), &token, "/api/v1/usage?a=1&a=2").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body,
            golden_global_usage(),
            "repeated, non-module keys never affect the global answer either"
        );
    }

    #[tokio::test]
    async fn module_calls_never_add_to_the_global_counter() {
        // §6.5: "模块调用不加进全局计数器" — a module-scoped GET does not itself
        // call /chat, so this asserts the negative the other way round: a
        // module name that was never called stays at all zero even though
        // the global counter (checked above) is non-zero from /chat.
        let state = state_with_stub().await;
        let token = state.token.to_string();
        post_chat_once(router(state.clone()), &token).await;

        let (status, body) = get(
            router(state.clone()),
            &token,
            "/api/v1/usage?module=never_called",
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["module"], "never_called");
        assert_eq!(body["totals"]["calls_ok"], 0);
        assert_eq!(body["totals"]["prompt_tokens"], 0);
        assert_eq!(body["cost_usd"], serde_json::Value::Null);
        for tier in ["local", "remote", "none"] {
            assert!(
                body["by_served"].get(tier).is_some(),
                "by_served must always carry all three keys, tier {tier} missing"
            );
        }
        assert_eq!(body["daily"], serde_json::json!([]));

        // Positive control: the global counter this same daemon serves is
        // NOT all-zero, so an all-zero module response above is really about
        // the module never having called anything — not a daemon that
        // recorded nothing at all.
        let (_, global) = get(router(state), &token, "/api/v1/usage").await;
        assert_eq!(global, golden_global_usage());
    }

    #[tokio::test]
    async fn an_invalid_module_name_is_400_invalid_request() {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        let (status, body) = get(router(state), &token, "/api/v1/usage?module=../x").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn two_module_keys_is_400_invalid_request_json_not_axums_plain_text() {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        let (status, body) = get(router(state), &token, "/api/v1/usage?module=a&module=b").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }
}
