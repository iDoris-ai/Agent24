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
        // ME4-1.2.2c1 stopgap: `agent24-scheduler` gained these variants in
        // this same cut (module-row PATCH/suspend/resume guardrails, design
        // §8.2/§8.3), but nothing on THIS side constructs them yet — the
        // REST routes/error mapping that produce them land in the stacked
        // ME4-1.2.2c-rest-guards cut. Kept as one internal-error catch-all
        // purely to keep this match exhaustive in the meantime; replaced
        // there with the real per-variant 409 mapping (module_owned_schedule
        // / not_a_module_schedule / schedule_conflict / quota_exceeded).
        ScheduleError::ModuleOwned(_)
        | ScheduleError::NotModuleOwned(_)
        | ScheduleError::Conflict(_)
        | ScheduleError::QuotaExceeded(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "unmapped schedule error (ME4-1.2.2c-rest-guards not yet applied)",
        ),
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
    // ME4-1.2.2b2: `Scheduler::run_now` now answers with the design's
    // `RunNowOutcome` (§4.7) instead of a bare `run_id` string — forced by
    // the same trait-signature swap that touched `server.rs`'s trigger
    // adapter (Rust compiles a workspace atomically). The `Run` arm is
    // byte-identical to before (still `202 {"run_id"}`); the `Fire` arm (a
    // module row) is not reachable yet — `Scheduler::run_now`'s module
    // branch still errors out until ME4-1.2.2b3 — but the match must be
    // exhaustive, so it is wired to its final §4.7 shape now rather than
    // left to panic.
    match state.scheduler.run_now(&id, Utc::now()).await {
        Ok(agent24_scheduler::RunNowOutcome::Run { run_id }) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "run_id": run_id })),
        )
            .into_response(),
        Ok(agent24_scheduler::RunNowOutcome::Fire { fire_id }) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "fire_id": fire_id.as_str() })),
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
