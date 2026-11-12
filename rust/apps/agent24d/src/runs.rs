//! v1 runs + sessions endpoints (C2).

use agent24_agent::{AgentError, SkillAdmissionError};
use agent24_core::util::{now_iso8601, ulid};
use agent24_protocol::{Run, RunCreate, RunStatus, Session, SessionCreate};
use agent24_store::{RunAdmissionDenial, StoreError, WorkspaceStoreError};
use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::routes::read_body_or_response;

use crate::capabilities::Authorization;
use crate::knowledge_tools::{RunKnowledge, run_knowledge};
use crate::server::{AppState, error_response};

fn map_agent_error(err: AgentError) -> Response {
    match err {
        AgentError::SessionNotFound(id) => {
            error_response(StatusCode::NOT_FOUND, "not_found", &format!("session {id}"))
        }
        AgentError::Store(err) => map_store_error(err),
        AgentError::Skill(err) => map_skill_error(&err),
        AgentError::Workspace(err) => map_workspace_error(err),
        AgentError::WorkspaceAdmission(denial) => map_workspace_admission_denial(denial),
        AgentError::WorkspaceService(_) | AgentError::WorkspaceServiceUnavailable => {
            error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "workspace_unavailable",
                "workspace authority unavailable",
            )
        }
    }
}

fn map_skill_error(err: &SkillAdmissionError) -> Response {
    let (status, message) = match err {
        SkillAdmissionError::NotAdmitted { reason } => {
            (StatusCode::BAD_REQUEST, (*reason).to_owned())
        }
        SkillAdmissionError::NotActive => (
            StatusCode::CONFLICT,
            "技能没有激活，不能用于运行".to_owned(),
        ),
        SkillAdmissionError::CapabilityMissing {
            capability,
            name_zh,
            registered_provider,
        } => {
            let name = name_zh.unwrap_or(capability.as_str());
            let provider = registered_provider
                .as_deref()
                .map(|p| format!("，提供者是 {p}"))
                .unwrap_or_default();
            (
                StatusCode::CONFLICT,
                format!("技能需要的能力 {capability}（{name}）当前没有可用的提供者{provider}"),
            )
        }
        SkillAdmissionError::ProviderAmbiguous {
            capability,
            components,
        } => (
            StatusCode::CONFLICT,
            format!(
                "能力 {capability} 有多个提供者（{}），v1 不自动挑选",
                components.join("、")
            ),
        ),
        SkillAdmissionError::Corrupt { detail } => {
            tracing::error!("skill admission: {detail}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "skill state is unreadable",
            );
        }
    };
    error_response(status, err.code(), &message)
}

fn map_workspace_admission_denial(denial: RunAdmissionDenial) -> Response {
    match denial {
        RunAdmissionDenial::SessionNotFound | RunAdmissionDenial::WorkspaceNotFound => {
            error_response(
                StatusCode::NOT_FOUND,
                "not_found",
                "workspace admission target not found",
            )
        }
        RunAdmissionDenial::WorkspaceUnavailable => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "workspace_unavailable",
            "workspace unavailable",
        ),
        RunAdmissionDenial::BindingConflict
        | RunAdmissionDenial::WorkspaceExpired
        | RunAdmissionDenial::WorkspaceReleased
        | RunAdmissionDenial::WorkspaceBusy => error_response(
            StatusCode::CONFLICT,
            "conflict",
            "workspace run admission denied",
        ),
    }
}

fn map_workspace_error(err: WorkspaceStoreError) -> Response {
    match err {
        WorkspaceStoreError::NotFound => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "workspace state not found",
        ),
        WorkspaceStoreError::InvalidValue { .. } => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid workspace value",
        ),
        WorkspaceStoreError::Conflict(_) => {
            error_response(StatusCode::CONFLICT, "conflict", "workspace conflict")
        }
        WorkspaceStoreError::CorruptRow { .. } | WorkspaceStoreError::Database => {
            tracing::error!("workspace store error: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "workspace storage error",
            )
        }
    }
}

fn map_store_error(err: StoreError) -> Response {
    match err {
        StoreError::NotFound(what) => error_response(StatusCode::NOT_FOUND, "not_found", &what),
        StoreError::Conflict(what) => error_response(StatusCode::CONFLICT, "conflict", &what),
        StoreError::Transition(err) => {
            error_response(StatusCode::CONFLICT, "conflict", &err.to_string())
        }
        StoreError::SkillPointerConflict => error_response(
            StatusCode::CONFLICT,
            "skill_pointer_conflict",
            "技能指针刚被改动，请重试",
        ),
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
            workspace_id: None,
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
        workspace_id: create.workspace_id,
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

pub async fn create_run(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    req: Request<Body>,
) -> Response {
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
    // Checked before the run exists, pinned once it is stored and before it
    // executes, so its first Knowledge tool call already finds the pin.
    let binding = match create.knowledge_binding_id.as_deref() {
        None => None,
        Some(id) => match crate::knowledge_routes::bind_run(
            &state,
            authorization,
            id,
            create.session_id.as_deref(),
        )
        .await
        {
            Ok(binding) => Some(binding),
            Err(refused) => return refused.into_response(),
        },
    };
    let pins = &state.knowledge_host;
    let providers = crate::component_routes::ComponentProviders::new(&state);
    let started = state
        .runs
        .start_run_then_with(
            create,
            |run| {
                if let Some(binding) = binding {
                    pins.pin_run(&run.id, binding);
                }
            },
            Some(&providers),
        )
        .await;
    match started {
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

/// A run as `GET /api/v1/runs/{id}` shows it. Only this read says what the
/// run's Knowledge tools did: in a list it would cost every run's calls.
#[derive(Serialize)]
struct RunDetail {
    #[serde(flatten)]
    run: Run,
    /// Null when the run has no Knowledge binding.
    knowledge: Option<RunKnowledge>,
}

pub async fn get_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_run(&id).await {
        Ok(Some(run)) => {
            let knowledge = match run.input.knowledge_binding_id {
                None => None,
                Some(_) => match state.store.list_tool_calls(&run.id).await {
                    Ok(calls) => Some(run_knowledge(&calls)),
                    Err(err) => return map_store_error(err),
                },
            };
            Json(RunDetail { run, knowledge }).into_response()
        }
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

#[cfg(test)]
mod skill_run_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use crate::server::build_router_with_modules;

    async fn post_run(state: &crate::server::AppState, body: Value) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/runs")
            .header("authorization", "Bearer testtoken")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        let response = build_router_with_modules(state.clone(), Router::new())
            .oneshot(request)
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn activate(state: &crate::server::AppState, requires: &str) {
        let markdown = format!(
            "---\nname: demo\nversion: 1.0.0\ndescription: d\nrequires: [{requires}]\nevals: evals/c.jsonl\n---\nhello\n"
        );
        let digest = agent24_skill::parse_skill_markdown(&markdown)
            .unwrap()
            .content_digest();
        let pool = agent24_store::test_hooks::pool(&state.store);
        for (id, kind, digest, content) in [
            ("rb", "body", digest.as_str(), markdown.as_str()),
            ("re", "evals", "evals-digest", "{}\n"),
        ] {
            sqlx::query(
                "INSERT INTO skill_revision (revision_id, skill_id, origin, semver, kind, content_digest, content, created_at) \
                 VALUES (?, 'demo', 'user', '1.0.0', ?, ?, ?, '2026-11-12T00:00:00Z')",
            )
            .bind(id)
            .bind(kind)
            .bind(digest)
            .bind(content.as_bytes())
            .execute(pool)
            .await
            .unwrap();
        }
        sqlx::query(
            "INSERT INTO skill_pointer (skill_id, body_revision_id, evals_revision_id, generation) VALUES ('demo', 'rb', 're', 1)",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn a_run_with_a_skill_id_maps_each_refusal_to_its_code() {
        let state = crate::server::tests::state().await;
        let (status, value) = post_run(&state, json!({"prompt": "hi", "skill_id": "demo"})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{value}");
        assert_eq!(value["error"]["code"], "skill_not_active");

        let (status, value) = post_run(
            &state,
            json!({"prompt": "hi", "skill_id": "demo", "workspace_id": "ws_00000000000000000000000000"}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "skill_not_admitted");

        // A required capability nobody provides: the test daemon mounts none.
        activate(&state, "nostr-dm").await;
        let (status, value) = post_run(&state, json!({"prompt": "hi", "skill_id": "demo"})).await;
        assert_eq!(status, StatusCode::CONFLICT, "{value}");
        assert_eq!(value["error"]["code"], "skill_capability_missing");
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("hyphae"),
            "{value}"
        );
        assert!(state.store.list_runs(None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_skill_with_no_requirements_starts_and_is_pinned() {
        let state = crate::server::tests::state().await;
        activate(&state, "").await;
        let (status, value) = post_run(&state, json!({"prompt": "hi", "skill_id": "demo"})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
        let run_id = value["id"].as_str().unwrap();
        let pinned = state
            .store
            .get_run_skill_pin(run_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(pinned.pin.skill_id, "demo");
        // Old clients omit the field and still deserialize.
        let (status, value) = post_run(&state, json!({"prompt": "hi"})).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{value}");
    }
}
