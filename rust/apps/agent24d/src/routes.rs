//! v1 REST handlers beyond health (B3: chat / models / usage).

use std::sync::Mutex;

use agent24_models::router::TaskProfile;
use agent24_models::{CompletionRequest, ModelError};
use agent24_protocol::{
    ChatRequest, ChatResponse, ErrorBody, EventBody, Model, ModelDeltaPayload, RunCompletedPayload,
    RunFailedPayload, RunOutputPayload, RunStartedPayload, Usage,
};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};

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

pub async fn get_usage(State(state): State<AppState>) -> Response {
    Json(state.usage.snapshot()).into_response()
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
            };
            state
                .events
                .broadcast(EventBody::RunFailed(RunFailedPayload {
                    run_id,
                    error: ErrorBody {
                        code: code.to_owned(),
                        message: message.clone(),
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
