//! v1 schedules endpoints (C5). Manual body parsing throughout — axum
//! extractor rejections would violate the v1 error envelope (SPEC-002).

use agent24_protocol::{ScheduleCreate, ScheduleUpdate};
use agent24_scheduler::ScheduleError;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;

use crate::routes::read_body_or_response;
use crate::server::{AppState, error_response};

fn map_error(err: ScheduleError) -> Response {
    match err {
        ScheduleError::NotFound(what) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("schedule {what}"),
        ),
        ScheduleError::Invalid(msg) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request", &msg)
        }
        ScheduleError::Store(err) => {
            tracing::error!("schedule store error: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
    }
}

pub async fn create_schedule(State(state): State<AppState>, req: Request<Body>) -> Response {
    let body = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let create: ScheduleCreate = match serde_json::from_slice(&body) {
        Ok(c) => c,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid schedule body: {err}"),
            );
        }
    };
    match state.scheduler.create(create, Utc::now()).await {
        Ok(schedule) => (StatusCode::CREATED, Json(schedule)).into_response(),
        Err(err) => map_error(err),
    }
}

pub async fn list_schedules(State(state): State<AppState>) -> Response {
    match state.scheduler.list().await {
        Ok(schedules) => Json(serde_json::json!({ "schedules": schedules })).into_response(),
        Err(err) => map_error(err),
    }
}

pub async fn get_schedule(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.scheduler.get(&id).await {
        Ok(schedule) => Json(schedule).into_response(),
        Err(err) => map_error(err),
    }
}

pub async fn update_schedule(State(state): State<AppState>, req: Request<Body>) -> Response {
    let id = path_id(&req);
    let body = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let update: ScheduleUpdate = match serde_json::from_slice(&body) {
        Ok(u) => u,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid schedule update: {err}"),
            );
        }
    };
    if update.is_empty() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "update must set at least one field",
        );
    }
    match state.scheduler.update(&id, update, Utc::now()).await {
        Ok(schedule) => Json(schedule).into_response(),
        Err(err) => map_error(err),
    }
}

pub async fn delete_schedule(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.scheduler.delete(&id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => map_error(err),
    }
}

pub async fn run_now(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.scheduler.run_now(&id).await {
        Ok(run_id) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "run_id": run_id })),
        )
            .into_response(),
        Err(err) => map_error(err),
    }
}

/// The PATCH handler takes the whole Request (for the shared body helper), so
/// pull `{id}` off the path directly. Shape is /api/v1/schedules/{id}.
fn path_id(req: &Request<Body>) -> String {
    req.uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned()
}
