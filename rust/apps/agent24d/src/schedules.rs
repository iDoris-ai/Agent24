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
use crate::server::{AppState, error_response, error_response_with_hint};

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
        // Design §8.2/§8.3: a module row rejected a PATCH that touched
        // anything other than a lone `enabled` field.
        ScheduleError::ModuleOwned(id) => error_response_with_hint(
            StatusCode::CONFLICT,
            "module_owned_schedule",
            &format!("schedule {id} is owned by a module"),
            "this field is controlled by the module; use POST .../suspend, \
             POST .../resume, a PATCH of exactly {\"enabled\": true|false}, \
             or DELETE",
        ),
        // Design §8.2/§8.3: suspend/resume called on a user (AgentRun) row.
        ScheduleError::NotModuleOwned(id) => error_response_with_hint(
            StatusCode::CONFLICT,
            "not_a_module_schedule",
            &format!("schedule {id} is not a module-owned schedule"),
            "use PATCH {\"enabled\": true|false} on a user-owned schedule",
        ),
        // Design §2.4/§8.2 (v2 M9): the CAS write-back lost every retry.
        ScheduleError::Conflict(id) => error_response(
            StatusCode::CONFLICT,
            "schedule_conflict",
            &format!("schedule {id} was modified concurrently; retry the request"),
        ),
        // Design §8.3: RPC-only in practice (`_a24/scheduler/upsert`'s
        // per-owner quota) — no REST call site constructs this today, but
        // `ScheduleError` is shared, so this match must stay exhaustive.
        ScheduleError::QuotaExceeded(n) => error_response(
            StatusCode::CONFLICT,
            "quota_exceeded",
            &format!("module schedule quota exceeded ({n} rows)"),
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

/// `POST /api/v1/schedules/{id}/suspend` (design §8.2): module rows only,
/// idempotent, `200 Schedule`; `409 not_a_module_schedule` on a user row.
pub async fn suspend_schedule(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.scheduler.suspend(&id, Utc::now()).await {
        Ok(schedule) => Json(schedule).into_response(),
        Err(err) => map_error(err),
    }
}

/// `POST /api/v1/schedules/{id}/resume` (design §8.2): module rows only,
/// idempotent, `200 Schedule`; `409 not_a_module_schedule` on a user row.
pub async fn resume_schedule(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.scheduler.resume(&id, Utc::now()).await {
        Ok(schedule) => Json(schedule).into_response(),
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

/// ME4-1.2.2c: the REST guardrails on top of module-owned schedules (design
/// §8, judged by §11 C2.1–C2.4 — C2.5–C2.9 either live in `agent24-scheduler`
/// or, for C2.9, are `-p agent24-scheduler update_cas`'s job) — plus a
/// regression check that an existing user-row REST round trip is unchanged.
#[cfg(test)]
mod schedules_rest_guard {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_store::ModuleScheduleDesired;

    fn req_json(method: &str, uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    async fn body_json(r: Response) -> (StatusCode, serde_json::Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .unwrap();
        let value = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, value)
    }

    /// Creates a module-owned row directly through the store, the way a
    /// module's own `_a24/scheduler/upsert` (ME4-1.4.1, not built yet) will
    /// eventually wrap.
    async fn make_module_row(state: &AppState, id: &str, owner: &str, key: &str, secs: u32) {
        let now = Utc::now();
        let desired = ModuleScheduleDesired {
            spec: agent24_protocol::ScheduleSpec::Every { secs },
            enabled: true,
            label: key.to_owned(),
        };
        let next = agent24_scheduler::next_fire::next_fire(&desired.spec, now)
            .unwrap()
            .map(agent24_scheduler::next_fire::fmt_iso);
        state
            .store
            .upsert_module_schedule(
                id,
                owner,
                key,
                &desired,
                next.as_deref(),
                &agent24_scheduler::next_fire::fmt_iso(now),
                256,
            )
            .await
            .unwrap();
    }

    async fn create_user_row(state: &AppState, name: &str) -> serde_json::Value {
        let body = serde_json::json!({
            "name": name,
            "spec": {"type": "every", "secs": 3600},
            "action": {"type": "agent_run", "prompt": "hi"},
        });
        let (status, body) = body_json(
            create_schedule(
                State(state.clone()),
                req_json("POST", "/api/v1/schedules", body),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        body
    }

    // ── C2.1: REST can never construct a module row ─────────────────────────

    #[tokio::test]
    async fn post_module_delivery_action_is_400() {
        let state = crate::server::tests::state().await;
        let body = serde_json::json!({
            "name": "x",
            "spec": {"type": "every", "secs": 3600},
            "action": {"type": "module_delivery"},
        });
        let (status, _) = body_json(
            create_schedule(State(state), req_json("POST", "/api/v1/schedules", body)).await,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_with_an_owner_module_field_is_dropped_and_creates_a_user_row() {
        // S1-2: `ScheduleCreate` has no `owner` field and there is no
        // `deny_unknown_fields` (design §8.1) — an extra `owner_module` key
        // is simply ignored, not an error, and never reaches storage.
        let state = crate::server::tests::state().await;
        let body = serde_json::json!({
            "name": "x",
            "spec": {"type": "every", "secs": 3600},
            "action": {"type": "agent_run", "prompt": "hi"},
            "owner_module": "mod-a",
        });
        let (status, body) = body_json(
            create_schedule(State(state), req_json("POST", "/api/v1/schedules", body)).await,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(body["owner"], serde_json::Value::Null, "{body}");
    }

    // ── C2.2 ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn patch_module_row_any_field_other_than_a_lone_enabled_is_409_module_owned() {
        let state = crate::server::tests::state().await;
        make_module_row(&state, "sch_m1", "mod-a", "k", 60).await;

        let cases = [
            serde_json::json!({"name": "renamed"}),
            serde_json::json!({"spec": {"type": "every", "secs": 120}}),
            serde_json::json!({"action": {"type": "agent_run", "prompt": "x"}}),
            serde_json::json!({"delivery": []}),
            serde_json::json!({"enabled": false, "name": "x"}),
        ];
        for body in cases {
            let (status, err) = body_json(
                update_schedule(
                    State(state.clone()),
                    req_json("PATCH", "/api/v1/schedules/sch_m1", body.clone()),
                )
                .await,
            )
            .await;
            assert_eq!(status, StatusCode::CONFLICT, "{body} -> {err}");
            assert_eq!(
                err["error"]["code"], "module_owned_schedule",
                "{body} -> {err}"
            );
        }
        // the row is unchanged by any of the rejected PATCHes.
        let after = state.scheduler.get("sch_m1").await.unwrap();
        assert_eq!(after.name, "k");
        assert!(!after.user_suspended);
        assert!(after.enabled);

        // positive control: the identical PATCH shape on a USER row is fine.
        let created = create_user_row(&state, "u1").await;
        let id = created["id"].as_str().unwrap();
        let (status, _) = body_json(
            update_schedule(
                State(state.clone()),
                req_json(
                    "PATCH",
                    &format!("/api/v1/schedules/{id}"),
                    serde_json::json!({"name": "renamed"}),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn patch_module_row_enabled_only_suspends_and_resumes() {
        let state = crate::server::tests::state().await;
        make_module_row(&state, "sch_m2", "mod-a", "k", 60).await;

        let (status, body) = body_json(
            update_schedule(
                State(state.clone()),
                req_json(
                    "PATCH",
                    "/api/v1/schedules/sch_m2",
                    serde_json::json!({"enabled": false}),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["user_suspended"], true, "{body}");
        assert_eq!(body["effective_enabled"], false, "{body}");
        assert_eq!(body["disabled_by"], "user", "{body}");

        let (status2, body2) = body_json(
            update_schedule(
                State(state.clone()),
                req_json(
                    "PATCH",
                    "/api/v1/schedules/sch_m2",
                    serde_json::json!({"enabled": true}),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(status2, StatusCode::OK, "{body2}");
        assert_eq!(body2["user_suspended"], false, "{body2}");
        assert_eq!(body2["effective_enabled"], true, "{body2}");
        assert_eq!(body2["disabled_by"], serde_json::Value::Null, "{body2}");
    }

    // ── C2.3 ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn suspend_survives_a_module_upsert_that_turns_enabled_back_on() {
        let state = crate::server::tests::state().await;
        make_module_row(&state, "sch_m3", "mod-a", "k", 60).await;

        let (status, _) =
            body_json(suspend_schedule(State(state.clone()), Path("sch_m3".to_owned())).await)
                .await;
        assert_eq!(status, StatusCode::OK);

        // the module upserts again: enabled=true, spec changed too.
        let now = Utc::now();
        let desired = ModuleScheduleDesired {
            spec: agent24_protocol::ScheduleSpec::Every { secs: 90 },
            enabled: true,
            label: "k".to_owned(),
        };
        let next = agent24_scheduler::next_fire::next_fire(&desired.spec, now)
            .unwrap()
            .map(agent24_scheduler::next_fire::fmt_iso);
        state
            .store
            .upsert_module_schedule(
                "sch_m3",
                "mod-a",
                "k",
                &desired,
                next.as_deref(),
                &agent24_scheduler::next_fire::fmt_iso(now),
                256,
            )
            .await
            .unwrap();

        let after = state.scheduler.get("sch_m3").await.unwrap();
        assert!(after.user_suspended, "still suspended after the upsert");
        assert_eq!(after.next_run_at, None, "still not scheduled to fire");

        // positive control: resume makes it live again.
        let (status2, body2) =
            body_json(resume_schedule(State(state.clone()), Path("sch_m3".to_owned())).await).await;
        assert_eq!(status2, StatusCode::OK, "{body2}");
        assert_eq!(body2["user_suspended"], false, "{body2}");
        assert!(body2["next_run_at"].is_string(), "{body2}");
    }

    // ── C2.3b ─────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn resume_clears_a_kernel_system_disable() {
        let state = crate::server::tests::state().await;
        make_module_row(&state, "sch_m4", "mod-a", "k", 60).await;
        sqlx::query("UPDATE schedules SET system_disabled_reason = 'boom' WHERE id = ?")
            .bind("sch_m4")
            .execute(agent24_store::test_hooks::pool(&state.store))
            .await
            .unwrap();
        let before = state.scheduler.get("sch_m4").await.unwrap();
        assert_eq!(
            before.disabled_by,
            Some(agent24_protocol::DisabledBy::System)
        );

        let (status, body) =
            body_json(resume_schedule(State(state.clone()), Path("sch_m4".to_owned())).await).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["system_disabled_reason"], serde_json::Value::Null);
        assert!(body["next_run_at"].is_string(), "{body}");
        assert_eq!(body["consecutive_failures"], 0);
    }

    // ── C2.4 ──────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn suspend_and_resume_on_a_user_row_is_409_not_a_module_schedule() {
        let state = crate::server::tests::state().await;
        let created = create_user_row(&state, "u2").await;
        let id = created["id"].as_str().unwrap().to_owned();

        let (status, body) =
            body_json(suspend_schedule(State(state.clone()), Path(id.clone())).await).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(body["error"]["code"], "not_a_module_schedule");

        let (status2, body2) =
            body_json(resume_schedule(State(state.clone()), Path(id)).await).await;
        assert_eq!(status2, StatusCode::CONFLICT);
        assert_eq!(body2["error"]["code"], "not_a_module_schedule");
    }

    // ── regression: existing user-row REST behaviour is unchanged ───────────

    #[tokio::test]
    async fn user_row_patch_run_now_and_delete_regression() {
        let state = crate::server::tests::state().await;
        let created = create_user_row(&state, "u3").await;
        let id = created["id"].as_str().unwrap().to_owned();

        let (status, body) = body_json(
            update_schedule(
                State(state.clone()),
                req_json(
                    "PATCH",
                    &format!("/api/v1/schedules/{id}"),
                    serde_json::json!({"enabled": false}),
                ),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["enabled"], false);
        assert_eq!(body["next_run_at"], serde_json::Value::Null);

        let (run_status, run_body) =
            body_json(run_now(State(state.clone()), Path(id.clone())).await).await;
        assert_eq!(run_status, StatusCode::ACCEPTED, "{run_body}");
        assert!(run_body["run_id"].as_str().is_some(), "{run_body}");

        let del = delete_schedule(State(state.clone()), Path(id)).await;
        assert_eq!(del.status(), StatusCode::NO_CONTENT);
    }
}
