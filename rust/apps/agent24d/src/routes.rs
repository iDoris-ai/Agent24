//! v1 REST handlers beyond health (B3: chat / models / usage).

use std::sync::atomic::{AtomicU64, Ordering};

use agent24_models::{CompletionRequest, ModelError};
use agent24_protocol::{ChatRequest, ChatResponse, Model, Usage};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Json, Response};

use crate::server::{AppState, error_response};

/// 1 MiB body cap — mirrors the node daemon (loopback is not a DoS boundary)
const MAX_BODY_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct UsageCounters {
    pub prompt_tokens: AtomicU64,
    pub completion_tokens: AtomicU64,
    pub total_tokens: AtomicU64,
}

impl UsageCounters {
    fn record(&self, usage: &Usage) {
        self.prompt_tokens
            .fetch_add(usage.prompt_tokens, Ordering::Relaxed);
        self.completion_tokens
            .fetch_add(usage.completion_tokens, Ordering::Relaxed);
        self.total_tokens
            .fetch_add(usage.total_tokens, Ordering::Relaxed);
    }

    fn snapshot(&self) -> Usage {
        Usage {
            prompt_tokens: self.prompt_tokens.load(Ordering::Relaxed),
            completion_tokens: self.completion_tokens.load(Ordering::Relaxed),
            total_tokens: self.total_tokens.load(Ordering::Relaxed),
            cost_usd: 0.0,
        }
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
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "payload_too_large",
                &format!("Request body exceeds {MAX_BODY_BYTES} bytes"),
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
