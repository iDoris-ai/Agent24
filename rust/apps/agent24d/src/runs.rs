//! v1 runs + sessions endpoints (C2).

use agent24_agent::AgentError;
use agent24_core::util::{now_iso8601, ulid};
use agent24_protocol::{RunCreate, RunStatus, Session, SessionCreate};
use agent24_store::StoreError;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::routes::read_body_or_response;

use crate::server::{AppState, error_response};

fn map_agent_error(err: AgentError) -> Response {
    match err {
        AgentError::SessionNotFound(id) => {
            error_response(StatusCode::NOT_FOUND, "not_found", &format!("session {id}"))
        }
        AgentError::Store(err) => map_store_error(err),
    }
}

fn map_store_error(err: StoreError) -> Response {
    match err {
        StoreError::NotFound(what) => error_response(StatusCode::NOT_FOUND, "not_found", &what),
        StoreError::Conflict(what) => error_response(StatusCode::CONFLICT, "conflict", &what),
        StoreError::Transition(err) => {
            error_response(StatusCode::CONFLICT, "conflict", &err.to_string())
        }
        other => {
            tracing::error!("store error: {other}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
    }
}

// ── sessions ─────────────────────────────────────────────────────────────────

pub async fn create_session(State(state): State<AppState>, req: Request<Body>) -> Response {
    let body = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    // Manual parse: axum's Json rejection is a 422 plain-text response, which
    // would violate the v1 error envelope. Empty body = defaults.
    let create: SessionCreate = if body.is_empty() {
        SessionCreate {
            title: String::new(),
            channel: "desktop".to_owned(),
        }
    } else {
        match serde_json::from_slice(&body) {
            Ok(c) => c,
            Err(err) => {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    &format!("invalid session body: {err}"),
                );
            }
        }
    };
    let now = now_iso8601();
    let session = Session {
        id: format!("sess_{}", ulid()),
        title: create.title,
        channel: create.channel,
        created_at: now.clone(),
        updated_at: now,
    };
    match state.store.insert_session(&session).await {
        Ok(()) => (StatusCode::CREATED, Json(session)).into_response(),
        Err(err) => map_store_error(err),
    }
}

pub async fn list_sessions(State(state): State<AppState>) -> Response {
    match state.store.list_sessions().await {
        Ok(sessions) => Json(serde_json::json!({ "sessions": sessions })).into_response(),
        Err(err) => map_store_error(err),
    }
}

pub async fn get_session(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_session(&id).await {
        Ok(Some(session)) => Json(session).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not_found", &format!("session {id}")),
        Err(err) => map_store_error(err),
    }
}

// ── runs ─────────────────────────────────────────────────────────────────────

pub async fn create_run(State(state): State<AppState>, req: Request<Body>) -> Response {
    let body = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let create: RunCreate = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("body must be {{prompt, session_id?, model_override?}}: {err}"),
            );
        }
    };
    if create.prompt.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "prompt is required",
        );
    }
    match state.runs.start_run(create).await {
        Ok(run) => (StatusCode::ACCEPTED, Json(run)).into_response(),
        Err(err) => map_agent_error(err),
    }
}

pub async fn list_runs(State(state): State<AppState>, req: Request<Body>) -> Response {
    // Manual query parse: an invalid ?status= must be a v1 400 envelope, not
    // axum's plain-text Query rejection
    let mut status: Option<RunStatus> = None;
    if let Some(raw) = req.uri().query() {
        for pair in raw.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("status") {
                let value = it.next().unwrap_or("");
                match serde_json::from_value::<RunStatus>(serde_json::Value::String(
                    value.to_owned(),
                )) {
                    Ok(s) => status = Some(s),
                    Err(_) => {
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            &format!("invalid status filter: {value}"),
                        );
                    }
                }
            }
        }
    }
    match state.store.list_runs(status).await {
        Ok(runs) => Json(serde_json::json!({ "runs": runs })).into_response(),
        Err(err) => map_store_error(err),
    }
}

pub async fn get_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_run(&id).await {
        Ok(Some(run)) => Json(run).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not_found", &format!("run {id}")),
        Err(err) => map_store_error(err),
    }
}

pub async fn cancel_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.runs.cancel_run(&id).await {
        Ok(run) => (StatusCode::ACCEPTED, Json(run)).into_response(),
        Err(err) => map_agent_error(err),
    }
}
