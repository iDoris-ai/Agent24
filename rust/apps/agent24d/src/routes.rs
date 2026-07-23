//! v1 REST handlers beyond health (B3: chat / models / usage).

use std::sync::Mutex;

use agent24_models::{CompletionRequest, ModelError};
use agent24_protocol::{ChatRequest, ChatResponse, Model, Usage};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::server::{AppState, error_response};

/// 1 MiB body cap — mirrors the node daemon (loopback is not a DoS boundary)
const MAX_BODY_BYTES: usize = 1024 * 1024;

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
    let models: Vec<Model> = state.registry.models(&cancel).await;
    Json(serde_json::json!({ "models": models })).into_response()
}

pub async fn get_usage(State(state): State<AppState>) -> Response {
    Json(state.usage.snapshot()).into_response()
}

pub async fn post_chat(State(state): State<AppState>, req: Request<Body>) -> Response {
    let bytes = match axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(err) => {
            // Only an actual length-limit hit is 413; disconnects / malformed
            // transfer encodings are the client's bad request, not "too large"
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
            let mut is_limit = false;
            while let Some(e) = source {
                if e.is::<http_body_util::LengthLimitError>() {
                    is_limit = true;
                    break;
                }
                source = e.source();
            }
            if is_limit {
                return error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    &format!("Request body exceeds {MAX_BODY_BYTES} bytes"),
                );
            }
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "failed to read request body",
            );
        }
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
        messages: chat.messages,
        model: chat.model,
    };
    // Child of the daemon shutdown token — shutdown cancels in-flight provider
    // calls; run-level cancellation joins this in C2
    let cancel = state.shutdown.child_token();
    match state.registry.complete(&request, &cancel).await {
        Ok((provider, res)) => {
            tracing::debug!("chat served by {provider}");
            state.usage.record(&res.usage);
            Json(ChatResponse {
                message: res.message,
                usage: res.usage,
            })
            .into_response()
        }
        Err(ModelError::Unavailable(msg)) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "provider_unavailable",
            &format!("All LLM providers unavailable. Last error: {msg}"),
        ),
        Err(ModelError::Cancelled) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "request cancelled",
        ),
        Err(ModelError::Provider(msg)) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &msg)
        }
    }
}
