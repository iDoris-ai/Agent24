//! `/api/v1/sin90/*` — the Sin90 Personal-OS module mounted on the daemon.
//!
//! The daemon owns a [`Sin90Store`] (its OWN `sin90.db`, physically isolated
//! from the kernel's `agent24.db`) and exposes a minimal REST surface for the
//! SPIKE-00 loop: create a direction / schedule block, complete a block,
//! submit + accept a Proposal, and read the attention reconciliation. Every
//! mutation broadcasts a `sin90.*` event through the SHARED WS hub as an
//! [`EventBody::Module`] envelope — the kernel relays it without understanding
//! Sin90's semantics (one-way dependency; SIN90-domain.md §4.2).

use agent24_protocol::EventBody;
use agent24_protocol::events::ModuleEventPayload;
use agent24_sin90::{ScheduleBlockStatus, Sin90Proposal};
use agent24_sin90_store::StoreError;
use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::routes::read_body_or_response;
use crate::server::{AppState, error_response};

const MODULE: &str = "sin90";

/// Broadcast a `sin90.<kind>` event on the shared WS hub. `payload` should be a
/// JSON object; a non-object is coerced to `{}` (the envelope guarantees object).
fn emit(state: &AppState, kind: &str, payload: serde_json::Value) {
    let payload = match payload {
        serde_json::Value::Object(m) => m,
        _ => serde_json::Map::new(),
    };
    state
        .events
        .broadcast(EventBody::Module(ModuleEventPayload {
            module: MODULE.to_owned(),
            kind: kind.to_owned(),
            payload,
        }));
}

/// Map a store error to an HTTP response, mirroring the kernel's envelope shape.
fn map_err(err: StoreError) -> Response {
    match err {
        StoreError::NotFound(m) => error_response(StatusCode::NOT_FOUND, "not_found", &m),
        StoreError::Transition(e) => {
            error_response(StatusCode::CONFLICT, "conflict", &e.to_string())
        }
        StoreError::Proposal(e) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_request", &e.to_string())
        }
        StoreError::Conflict(m) => error_response(StatusCode::CONFLICT, "conflict", &m),
        StoreError::WeekNotOpen(m) => error_response(
            StatusCode::CONFLICT,
            "conflict",
            &format!("task {m}'s week is not open"),
        ),
        StoreError::SameWeekCarry(m) => error_response(
            StatusCode::CONFLICT,
            "conflict",
            &format!("cannot carry task {m} into its own week"),
        ),
        StoreError::Sqlx(_) | StoreError::Migrate(_) | StoreError::Serde(_) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", "store error")
        }
    }
}

// Err is the shared v1-envelope Response (as in `read_body_or_response`); the
// lint only fires here because this helper is sync, not because it's avoidable.
#[allow(clippy::result_large_err)]
fn parse<T: for<'de> Deserialize<'de>>(bytes: &Bytes, what: &str) -> Result<T, Response> {
    serde_json::from_slice(bytes).map_err(|e| {
        error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("invalid {what}: {e}"),
        )
    })
}

// ---- request/query bodies -------------------------------------------------

#[derive(Deserialize)]
struct NewDirectionReq {
    title: String,
    target_window: String,
}

#[derive(Deserialize)]
struct NewBlockReq {
    #[serde(default)]
    direction_id: Option<String>,
    #[serde(default)]
    task_id: Option<String>,
    planned_minutes: u32,
}

#[derive(Deserialize)]
struct TransitionReq {
    to: ScheduleBlockStatus,
}

/// Half-open window `[start, end)` — ISO-8601 bounds, fixed-width so a lexical
/// compare is chronological.
#[derive(Deserialize)]
pub struct AttentionQuery {
    start: String,
    end: String,
}

// ---- handlers -------------------------------------------------------------

pub async fn create_direction(State(state): State<AppState>, req: Request<Body>) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: NewDirectionReq = match parse(&bytes, "direction") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state
        .sin90
        .create_direction(&body.title, &body.target_window)
        .await
    {
        Ok(d) => {
            emit(
                &state,
                "direction.created",
                serde_json::json!({ "id": d.id, "title": d.title }),
            );
            (StatusCode::CREATED, Json(d)).into_response()
        }
        Err(e) => map_err(e),
    }
}

pub async fn create_block(State(state): State<AppState>, req: Request<Body>) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: NewBlockReq = match parse(&bytes, "block") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state
        .sin90
        .create_block(
            body.direction_id.as_deref(),
            body.task_id.as_deref(),
            body.planned_minutes,
        )
        .await
    {
        Ok(b) => (StatusCode::CREATED, Json(b)).into_response(),
        Err(e) => map_err(e),
    }
}

pub async fn transition_block(State(state): State<AppState>, req: Request<Body>) -> Response {
    let id = crate::routes::path_last(&req);
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: TransitionReq = match parse(&bytes, "transition") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state.sin90.transition_block(&id, body.to).await {
        Ok(b) => {
            emit(
                &state,
                "block.transitioned",
                serde_json::json!({ "block_id": b.id, "to": b.status }),
            );
            Json(b).into_response()
        }
        Err(e) => map_err(e),
    }
}

pub async fn submit_proposal(State(state): State<AppState>, req: Request<Body>) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let proposal: Sin90Proposal = match parse(&bytes, "proposal") {
        Ok(b) => b,
        Err(r) => return r,
    };
    match state.sin90.submit_proposal(&proposal).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({ "id": proposal.id, "status": "pending" })),
        )
            .into_response(),
        Err(e) => map_err(e),
    }
}

pub async fn accept_proposal(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.sin90.apply_proposal(&id).await {
        Ok(receipt) => {
            emit(
                &state,
                "proposal.applied",
                serde_json::json!({ "proposal_id": receipt.proposal_id }),
            );
            Json(receipt).into_response()
        }
        Err(e) => map_err(e),
    }
}

pub async fn attention(State(state): State<AppState>, Query(q): Query<AttentionQuery>) -> Response {
    match state.sin90.attention(&q.start, &q.end).await {
        Ok(rows) => Json(serde_json::json!({ "attention": rows })).into_response(),
        Err(e) => map_err(e),
    }
}
