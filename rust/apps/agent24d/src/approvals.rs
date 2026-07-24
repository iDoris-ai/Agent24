//! v1 approvals endpoints (C4). Manual body/query parsing throughout — axum
//! extractor rejections would violate the v1 error envelope (SPEC-002).

use agent24_policy::ResolveError;
use agent24_protocol::{ApprovalStatus, Decision};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::routes::read_body_or_response;
use crate::server::{AppState, error_response};

pub async fn list_approvals(State(state): State<AppState>, req: Request<Body>) -> Response {
    let mut status: Option<ApprovalStatus> = None;
    if let Some(raw) = req.uri().query() {
        for pair in raw.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("status") {
                let value = it.next().unwrap_or("");
                match serde_json::from_value::<ApprovalStatus>(serde_json::Value::String(
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
    match state.store.list_approvals(status).await {
        Ok(approvals) => Json(serde_json::json!({ "approvals": approvals })).into_response(),
        Err(err) => {
            tracing::error!("list approvals failed: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
    }
}

pub async fn get_approval(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_approval(&id).await {
        Ok(Some(approval)) => Json(approval).into_response(),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("approval {id}"),
        ),
        Err(err) => {
            tracing::error!("get approval failed: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
    }
}

pub async fn decide_approval(State(state): State<AppState>, req: Request<Body>) -> Response {
    // Path is parsed manually off the URI: the (Path, Request) extractor
    // combination is fine, but keeping Request whole preserves the shared
    // body-limit helper. URI shape is /api/v1/approvals/{id}.
    let id = req
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    let body = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let decision: Decision = match serde_json::from_slice(&body) {
        Ok(d) => d,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("body must be a Decision {{type, reason?}}: {err}"),
            );
        }
    };
    match state.broker.resolve(&id, decision).await {
        Ok(approval) => Json(approval).into_response(),
        Err(ResolveError::NotFound(_)) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("approval {id}"),
        ),
        Err(ResolveError::AlreadyResolved(_)) => error_response(
            StatusCode::CONFLICT,
            "approval_already_resolved",
            &format!("approval {id} was already resolved"),
        ),
        Err(ResolveError::Invalid(msg)) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request", &msg)
        }
        Err(ResolveError::Store(err)) => {
            tracing::error!("decide approval failed: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
    }
}
