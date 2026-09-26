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
            // Review, L2: `prompt_tokens`/`completion_tokens` are each
            // independently capped at `i64::MAX` by the store (`ModelUsageRow`
            // already reflects that), so their sum could in principle reach
            // `2 * i64::MAX` — `agent24_store::saturating_add_capped` is the
            // same ceiling the store itself uses, not a second one that could
            // disagree.
            total_tokens: agent24_store::saturating_add_capped(
                row.prompt_tokens,
                row.completion_tokens,
            ),
        }
    }
}

impl UsageCounts {
    /// Adds `by_served`'s (at most three) rows together to build `totals`.
    /// Review, L2: reuses `agent24-store`'s own `saturating_add_capped`
    /// (capped at `i64::MAX`, the same ceiling every stored row is already
    /// individually held to) rather than a second, independent
    /// `u64::saturating_add` (capped at `u64::MAX` instead) — one function
    /// decides what "too big to be real" means for this table, not two that
    /// could silently disagree.
    fn plus(self, other: Self) -> Self {
        use agent24_store::saturating_add_capped as add;
        Self {
            calls_ok: add(self.calls_ok, other.calls_ok),
            calls_failed: add(self.calls_failed, other.calls_failed),
            calls_cancelled: add(self.calls_cancelled, other.calls_cancelled),
            prompt_tokens: add(self.prompt_tokens, other.prompt_tokens),
            completion_tokens: add(self.completion_tokens, other.completion_tokens),
            total_tokens: add(self.total_tokens, other.total_tokens),
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
/// (`?%zz`, `?a=1&a=2`, …), never affects the answer.
///
/// Review, L1: this used to say `serde_urlencoded`'s parse "fails outright"
/// on bad input and treated that (hypothetical) failure as "no `module`
/// key". That was wrong on two counts, checked empirically: `%zz` does NOT
/// fail to parse — it comes back as a literal one-pair list
/// `[("%zz", "")]` — and even genuinely invalid UTF-8 after percent-decoding
/// (`%FF`) decodes losslessly to U+FFFD rather than erroring. `form_urlencoded`
/// (the crate `serde_urlencoded` itself delegates to, used here directly
/// instead) makes this a property of the type, not an implementation detail
/// to trust: `Parse` is a plain iterator with no `Result` in sight, so there
/// is no error branch left to (mis-)handle, and — the actual risk L1 named —
/// no way for one malformed key/value pair to make an unrelated, well-formed
/// `module=` key elsewhere in the same query string silently disappear: each
/// `&`-separated pair is decoded independently of every other one.
///
/// - `Ok(None)`: no `module` key — read the global in-memory counters.
/// - `Ok(Some(name))`: exactly one `module` key, value `name` (not yet
///   validated as a legal module name — the caller does that).
/// - `Err(())`: two or more `module` keys — `400 invalid_request`.
fn module_selector(raw: Option<&str>) -> Result<Option<String>, ()> {
    let mut modules = form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .filter(|(k, _)| k == "module")
        .map(|(_, v)| v.into_owned());
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
    fn usage_by_module_api_no_module_key_selects_the_global_counters() {
        assert_eq!(module_selector(None), Ok(None));
        assert_eq!(module_selector(Some("")), Ok(None));
        assert_eq!(
            module_selector(Some("a=1&b=2")),
            Ok(None),
            "keys other than 'module', however many, never select a module"
        );
    }

    #[test]
    fn usage_by_module_api_malformed_query_falls_back_to_the_global_counters_not_an_error() {
        // Review, L1: `%zz` is not valid percent-encoding, but
        // `form_urlencoded::parse` does not error on it — it decodes
        // losslessly to the literal pair `("%zz", "")` (verified
        // empirically). That key is simply not `"module"`, so this falls to
        // the global-counters branch for the same reason any query without a
        // `module` key does — never a 400 (v2 L5), and never because of a
        // parse failure that does not actually occur.
        assert_eq!(module_selector(Some("%zz")), Ok(None));
    }

    #[test]
    fn usage_by_module_api_exactly_one_module_key_selects_it() {
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
    fn usage_by_module_api_two_or_more_module_keys_is_rejected() {
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
    async fn usage_by_module_api_no_module_key_returns_the_unchanged_global_counters() {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        post_chat_once(router(state.clone()), &token).await;

        let (status, body) = get(router(state.clone()), &token, "/api/v1/usage").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, golden_global_usage());
    }

    #[tokio::test]
    async fn usage_by_module_api_a_malformed_query_still_returns_the_same_golden_global_literal() {
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
    async fn usage_by_module_api_module_calls_never_add_to_the_global_counter() {
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
    async fn usage_by_module_api_an_invalid_module_name_is_400_invalid_request() {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        let (status, body) = get(router(state), &token, "/api/v1/usage?module=../x").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn usage_by_module_api_two_module_keys_is_400_invalid_request_json_not_axums_plain_text()
    {
        let state = state_with_stub().await;
        let token = state.token.to_string();
        let (status, body) = get(router(state), &token, "/api/v1/usage?module=a&module=b").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    // ── H1 (review): the actual positive path — real per-module data, ───────
    // ── isolation, windowing and the totals/by_served/daily shapes ──────────

    /// Review, H1: `module_calls_never_add_to_the_global_counter` above only
    /// ever exercised the ALL-ZERO case (`?module=never_called`) — which
    /// passes identically whether `get_usage` reads the module the caller
    /// asked for or a hardcoded, always-empty one. This test seeds REAL,
    /// distinguishable rows for two modules across multiple days and all
    /// three `served_by` tiers — one row deliberately a day outside the
    /// `daily` window — directly through `Store::record_module_model_usage`
    /// (bypassing `/api/v1/chat` entirely, so the numbers are exact and
    /// under this test's own control), then asserts on the full response
    /// shape: module isolation (both directions), `daily`'s exact newest-
    /// first order and its exclusion of the out-of-window row, `totals`/
    /// `by_served`'s ALL-TIME correctness (which must still include that same
    /// out-of-window row — §6.5: "totals/by_served 为全期"), and
    /// `total_tokens` being `prompt_tokens + completion_tokens`.
    ///
    /// Mutation (verified): changing the two `state.store.module_model_usage*`
    /// calls in `get_usage` to query a hardcoded `"zzz"` instead of `&module`
    /// turns this test red (module "a"'s non-zero totals/daily all come back
    /// zero/empty) — the OLD test suite (only ever asserting all-zero
    /// responses) did not catch that mutation at all.
    #[tokio::test]
    async fn usage_by_module_api_totals_and_daily_are_module_scoped_and_windowed() {
        let state = state_with_stub().await;
        let token = state.token.to_string();

        let day = |offset_days: i64| {
            (chrono::Utc::now().date_naive() - chrono::Duration::days(offset_days))
                .format("%Y-%m-%d")
                .to_string()
        };
        let t0 = day(0);
        let t1 = day(1);
        let t29 = day(29); // the OLDEST day still inside the 30-day daily window
        let t31 = day(31); // outside the daily window, but still counted in totals

        async fn record(
            state: &AppState,
            module: &str,
            day: &str,
            served_by: agent24_store::ServedBy,
            delta: agent24_store::ModelUsageDelta,
        ) {
            state
                .store
                .record_module_model_usage(module, day, served_by, delta)
                .await
                .unwrap();
        }

        fn delta(
            calls_ok: u64,
            calls_failed: u64,
            prompt_tokens: u64,
            completion_tokens: u64,
        ) -> agent24_store::ModelUsageDelta {
            agent24_store::ModelUsageDelta {
                calls_ok,
                calls_failed,
                prompt_tokens,
                completion_tokens,
                ..Default::default()
            }
        }

        // Module "a": five rows, three tiers, four distinct days.
        record(
            &state,
            "a",
            &t0,
            agent24_store::ServedBy::Local,
            delta(2, 0, 20, 10),
        )
        .await;
        record(
            &state,
            "a",
            &t1,
            agent24_store::ServedBy::Remote,
            delta(1, 0, 5, 1),
        )
        .await;
        record(
            &state,
            "a",
            &t1,
            agent24_store::ServedBy::None,
            delta(0, 1, 0, 0),
        )
        .await;
        record(
            &state,
            "a",
            &t29,
            agent24_store::ServedBy::Local,
            delta(1, 0, 1, 1),
        )
        .await;
        record(
            &state,
            "a",
            &t31,
            agent24_store::ServedBy::Local,
            delta(100, 0, 1000, 1000),
        )
        .await;

        // Module "b": isolation control — large-looking numbers of its own
        // that must never leak into "a"'s response, and vice versa.
        record(
            &state,
            "b",
            &t0,
            agent24_store::ServedBy::Remote,
            delta(5, 0, 50, 5),
        )
        .await;

        let (status, a) = get(router(state.clone()), &token, "/api/v1/usage?module=a").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(a["module"], "a");
        assert_eq!(a["cost_usd"], serde_json::Value::Null);
        assert_eq!(
            a["totals"],
            serde_json::json!({
                "calls_ok": 104, "calls_failed": 1, "calls_cancelled": 0,
                "prompt_tokens": 1026, "completion_tokens": 1012, "total_tokens": 2038
            }),
            "totals must be ALL-TIME — including the day-31 row outside the daily window"
        );
        assert_eq!(
            a["by_served"],
            serde_json::json!({
                "local": {"calls_ok": 103, "calls_failed": 0, "calls_cancelled": 0,
                           "prompt_tokens": 1021, "completion_tokens": 1011, "total_tokens": 2032},
                "remote": {"calls_ok": 1, "calls_failed": 0, "calls_cancelled": 0,
                            "prompt_tokens": 5, "completion_tokens": 1, "total_tokens": 6},
                "none": {"calls_ok": 0, "calls_failed": 1, "calls_cancelled": 0,
                          "prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0}
            })
        );
        assert_eq!(
            a["daily"],
            serde_json::json!([
                {"day": t0, "served_by": "local", "calls_ok": 2, "calls_failed": 0,
                 "calls_cancelled": 0, "prompt_tokens": 20, "completion_tokens": 10,
                 "total_tokens": 30},
                {"day": t1, "served_by": "none", "calls_ok": 0, "calls_failed": 1,
                 "calls_cancelled": 0, "prompt_tokens": 0, "completion_tokens": 0,
                 "total_tokens": 0},
                {"day": t1, "served_by": "remote", "calls_ok": 1, "calls_failed": 0,
                 "calls_cancelled": 0, "prompt_tokens": 5, "completion_tokens": 1,
                 "total_tokens": 6},
                {"day": t29, "served_by": "local", "calls_ok": 1, "calls_failed": 0,
                 "calls_cancelled": 0, "prompt_tokens": 1, "completion_tokens": 1,
                 "total_tokens": 2}
            ]),
            "daily must be newest-first, at most the last 30 UTC days (today included), \
             and exclude the day-31 row entirely — it must still count in totals/by_served \
             (checked above)"
        );

        // Module "b": isolation the other way, and its own correctness.
        let (status, b) = get(router(state.clone()), &token, "/api/v1/usage?module=b").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(b["module"], "b");
        assert_eq!(
            b["totals"],
            serde_json::json!({
                "calls_ok": 5, "calls_failed": 0, "calls_cancelled": 0,
                "prompt_tokens": 50, "completion_tokens": 5, "total_tokens": 55
            }),
            "module b's totals must not include any of module a's rows"
        );
        assert_eq!(
            b["daily"],
            serde_json::json!([
                {"day": t0, "served_by": "remote", "calls_ok": 5, "calls_failed": 0,
                 "calls_cancelled": 0, "prompt_tokens": 50, "completion_tokens": 5,
                 "total_tokens": 55}
            ])
        );
    }
}
