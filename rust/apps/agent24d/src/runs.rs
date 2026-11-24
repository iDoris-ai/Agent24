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
use crate::knowledge_tools::{RunKnowledge, run_knowledge_with};
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
    match new_session(&state.store, create).await {
        Ok(session) => (StatusCode::CREATED, Json(session)).into_response(),
        Err(err) => map_store_error(err),
    }
}

pub(crate) async fn new_session(
    store: &agent24_store::Store,
    create: SessionCreate,
) -> Result<Session, StoreError> {
    let now = now_iso8601();
    let session = Session {
        id: format!("sess_{}", ulid()),
        title: create.title,
        channel: create.channel,
        workspace_id: create.workspace_id,
        created_at: now.clone(),
        updated_at: now,
    };
    store.insert_session(&session).await?;
    Ok(session)
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
    if create.skill_id.as_deref().is_some_and(str::is_empty) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "skill_id must not be empty",
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
    let providers = crate::component_routes::ComponentProviders::new(&state);
    match start_bound_run(
        &state.runs,
        &state.knowledge_host,
        Some(&providers),
        create,
        binding,
        None,
    )
    .await
    {
        Ok(run) => (StatusCode::ACCEPTED, Json(run)).into_response(),
        Err(err) => map_agent_error(err),
    }
}

pub(crate) async fn start_bound_run(
    runs: &std::sync::Arc<agent24_agent::RunManager>,
    pins: &agent24_knowledge::host::KnowledgeHostRegistry,
    providers: Option<&dyn agent24_agent::SkillProviderSource>,
    create: RunCreate,
    binding: Option<agent24_knowledge::host::RunBinding>,
    system: Option<String>,
) -> Result<Run, AgentError> {
    runs.start_run_then_with_system(
        create,
        |run| {
            if let Some(binding) = binding {
                pins.pin_run(&run.id, binding);
            }
        },
        providers,
        system,
    )
    .await
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
    /// M1 (General/PM 11-21): what the run's document writes came to, told
    /// from its stored tool calls and results, never from the answer's text.
    /// Absent when the run asked for no document write.
    #[serde(skip_serializing_if = "Option::is_none")]
    documents: Option<RunDocuments>,
}

/// A run's document writes (compose, fill, patch, and any later documents
/// operation that is not a read): whether one was asked for, and the import
/// jobs of those that went through. A refusal, a denial, an error, a patch
/// whose anchor did not match: asked for, no job.
#[derive(Serialize, Debug, PartialEq)]
struct RunDocuments {
    attempted: bool,
    jobs: Vec<String>,
    /// General 0.6.94: the documents a write was asked for and none went
    /// through (each as the call named it: a document id, a file name, or a
    /// new document's title), so a run that made A and failed on B says both.
    /// A document a later write did make is not here.
    failed: Vec<String>,
    /// L6-F5: the writes saved as a document's next version (no import
    /// job): which document, its title and the new version.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    revisions: Vec<SavedRevision>,
}

#[derive(Serialize, Debug, PartialEq)]
struct SavedRevision {
    document_id: String,
    title: String,
    revision: i64,
}

/// L6-F5: a patch or fill the module saved as the document's next version.
fn saved_revision(payload: Option<&serde_json::Value>) -> Option<SavedRevision> {
    let p = payload?;
    // `committed: false` is content equal to the head: the module wrote
    // nothing (and told the model so), so it is no saved version.
    if p.get("saved_as").and_then(|v| v.as_str()) != Some("new_revision")
        || p.get("committed").and_then(|v| v.as_bool()) != Some(true)
    {
        return None;
    }
    Some(SavedRevision {
        document_id: p.get("document_id")?.as_str()?.to_owned(),
        title: p
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_owned(),
        revision: p.get("revision")?.as_i64()?,
    })
}

/// The documents operations that only read.
const DOCUMENT_READS: &[&str] = &[
    "list",
    "get",
    "paragraphs",
    "compare",
    "fields",
    "read_range",
    "find",
    "extract",
    "job_get",
];

fn is_document_write(tool: &str) -> bool {
    tool.strip_prefix("documents.")
        .is_some_and(|op| !DOCUMENT_READS.contains(&op))
}

fn run_documents(messages: &[agent24_store::RunMessage]) -> Option<RunDocuments> {
    // PR-Daemon #1986 Low 1: a model may reuse a tool_call id across turns, so
    // each result is paired with the call of that id seen most recently before
    // it, in thread order, never with a later one.
    let mut names = std::collections::HashMap::new();
    let mut attempted = false;
    let mut jobs = Vec::new();
    let mut made: Vec<String> = Vec::new();
    let mut tried: Vec<String> = Vec::new();
    let mut deleted_any = false;
    let mut revisions: Vec<SavedRevision> = Vec::new();
    // PR-Daemon #1994 B1: a document named by its file name in one call and
    // by its id in another is one document. What the module answered (the
    // `document_id` of a read, the `source_document_id` of a write) says
    // which document a name stood for; both sides are compared by that.
    let mut alias: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for m in messages {
        if m.role == "assistant" {
            for call in m.tool_calls.as_array().into_iter().flatten() {
                if let (Some(id), Some(name)) = (
                    call.get("id").and_then(|v| v.as_str()),
                    call.get("name").and_then(|v| v.as_str()),
                ) {
                    names.insert(id.to_owned(), (name.to_owned(), write_target(call)));
                }
            }
            continue;
        }
        if m.role != "tool" {
            continue;
        }
        let Some((name, target)) = m.tool_call_id.as_ref().and_then(|id| names.get(id)) else {
            continue;
        };
        if !name.starts_with("documents.") {
            continue;
        }
        let payload = m
            .content
            .as_deref()
            .and_then(|c| serde_json::from_str::<serde_json::Value>(c).ok())
            .filter(|v| v.get("kind").and_then(|k| k.as_str()) == Some("completed"))
            .and_then(|v| v.get("payload").cloned());
        let write = is_document_write(name);
        let resolved = payload.as_ref().and_then(|p| {
            p.get(if write {
                "source_document_id"
            } else {
                "document_id"
            })
            .and_then(|d| d.as_str())
        });
        if let Some(resolved) = resolved
            && !target.is_empty()
        {
            alias.insert(target.clone(), resolved.to_owned());
        }
        if !write {
            continue;
        }
        // L6-F2: a delete that went through makes no document and changed
        // what it was asked to; it is not a write that came to nothing, and
        // it settles earlier tries at that document (refused by the user,
        // then approved). PR-Daemon #2026 ①.
        if payload
            .as_ref()
            .is_some_and(|p| p.get("deleted") == Some(&serde_json::Value::Bool(true)))
        {
            if let Some(id) = payload
                .as_ref()
                .and_then(|p| p.get("document_id"))
                .and_then(|d| d.as_str())
                && !target.is_empty()
            {
                alias.insert(target.clone(), id.to_owned());
            }
            made.push(target.clone());
            deleted_any = true;
            continue;
        }
        // L6-F5: saved as the document's next version: a write that went
        // through, with no import job.
        if let Some(saved) = saved_revision(payload.as_ref()) {
            attempted = true;
            made.push(target.clone());
            if !revisions.contains(&saved) {
                revisions.push(saved);
            }
            continue;
        }
        // A delete the kernel refused before any card (a file name for its
        // id, `off_pattern`) asked nothing of the user: the model is told
        // to call again with the id, so it is not a try that failed.
        // Only the two pre-card refusals (`missing_required`, `off_pattern`);
        // an approved call refused later (`unknown tool`) still counts.
        if name == "documents.delete"
            && m.content.as_deref().is_some_and(|c| {
                c.starts_with("tool error: invalid: ")
                    && (c.contains(" needs the argument ")
                        || c.contains(" is not in the form it needs "))
            })
        {
            continue;
        }
        attempted = true;
        let job = payload
            .as_ref()
            .and_then(|p| p.get("job_id"))
            .and_then(|j| j.as_str())
            .map(str::to_owned);
        match job {
            Some(job) => {
                jobs.push(job);
                made.push(target.clone());
            }
            None => tried.push(target.clone()),
        }
    }
    let key = |t: &String| alias.get(t).cloned().unwrap_or_else(|| t.clone());
    let made: std::collections::HashSet<String> = made.iter().map(key).collect();
    let mut failed: Vec<String> = Vec::new();
    for t in tried.iter().filter(|t| !t.is_empty()).map(key) {
        if !made.contains(&t) && !failed.contains(&t) {
            failed.push(t);
        }
    }
    // PR-Daemon #2026 round 2: every try was a delete that a later delete of
    // the same document settled (refused, then approved): nothing reads as
    // unchanged. A try with no target (never settled) keeps it as before.
    let unsettled = tried
        .iter()
        .any(|t| t.is_empty() || !made.contains(&key(t)));
    if deleted_any && jobs.is_empty() && !unsettled {
        return None;
    }
    attempted.then_some(RunDocuments {
        attempted,
        jobs,
        failed,
        revisions,
    })
}

/// The document a write call was aimed at, as the call named it: its
/// `document_id` (an id or a file name), else the new document's `title`.
fn write_target(call: &serde_json::Value) -> String {
    let arguments = call.get("arguments");
    let parsed = match arguments {
        Some(serde_json::Value::String(s)) => serde_json::from_str(s).unwrap_or_default(),
        Some(v) => v.clone(),
        None => serde_json::Value::Null,
    };
    ["document_id", "title"]
        .iter()
        .find_map(|k| parsed.get(k).and_then(|v| v.as_str()))
        .unwrap_or("")
        .to_owned()
}

pub async fn get_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_run(&id).await {
        Ok(Some(run)) => {
            let documents = match state.store.list_run_messages(&run.id).await {
                Ok(messages) => run_documents(&messages),
                Err(err) => return map_store_error(err),
            };
            let knowledge = match run.input.knowledge_binding_id {
                None => None,
                Some(_) => match state.store.list_tool_calls(&run.id).await {
                    Ok(calls) => {
                        // K3: what the run's tools answered, for its searches' hits.
                        let answers: Vec<String> =
                            match state.store.list_run_messages(&run.id).await {
                                Ok(messages) => messages
                                    .into_iter()
                                    .filter(|m| m.role == "tool")
                                    .filter_map(|m| m.content)
                                    .collect(),
                                Err(err) => return map_store_error(err),
                            };
                        Some(run_knowledge_with(&calls, &answers))
                    }
                    Err(err) => return map_store_error(err),
                },
            };
            Json(RunDetail {
                run,
                knowledge,
                documents,
            })
            .into_response()
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

        // An empty id is a malformed request, not a missing skill.
        let (status, value) = post_run(&state, json!({"prompt": "hi", "skill_id": ""})).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
        assert_eq!(value["error"]["code"], "invalid_request");

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

#[cfg(test)]
mod documents_tests {
    use super::*;
    use serde_json::json;

    fn msg(
        role: &str,
        content: Option<&str>,
        calls: serde_json::Value,
        id: Option<&str>,
    ) -> agent24_store::RunMessage {
        agent24_store::RunMessage {
            run_id: "r".into(),
            seq: 0,
            role: role.into(),
            content: content.map(str::to_owned),
            tool_calls: calls,
            tool_call_id: id.map(str::to_owned),
            created_at: String::new(),
        }
    }

    /// M1 (General/PM 11-21): told from the tool results, never the answer.
    #[test]
    fn a_runs_document_writes_are_told_from_their_results() {
        let ask = |id: &str, name: &str| {
            msg(
                "assistant",
                None,
                json!([{ "id": id, "name": name, "arguments": "{}" }]),
                None,
            )
        };
        let job = r#"{"kind":"completed","payload":{"job_id":"job_1","kind":"import","status":"queued"}}"#;
        let refused =
            r#"{"kind":"completed","payload":{"written":false,"reason":"anchor_mismatch"}}"#;
        // Only reads: nothing to say.
        let reads = [
            ask("a", "documents.paragraphs"),
            msg("tool", Some("{}"), json!([]), Some("a")),
        ];
        assert_eq!(run_documents(&reads), None);
        // A patch the card sent back, then the answer 「已保存」: asked, no job.
        let denied = [
            ask("b", "documents.patch"),
            msg(
                "tool",
                Some("denied by policy: denied by user: 【未完成】…"),
                json!([]),
                Some("b"),
            ),
            msg("assistant", Some("已保存为新文档《x》"), json!([]), None),
        ];
        assert_eq!(
            run_documents(&denied),
            Some(RunDocuments {
                attempted: true,
                jobs: vec![],
                failed: vec![],
                revisions: vec![]
            })
        );
        // The module's own refusal and an error: asked, no job.
        let refusals = [
            ask("c", "documents.patch"),
            msg("tool", Some(refused), json!([]), Some("c")),
            ask("d", "documents.compose"),
            msg("tool", Some("tool error: failed: {}"), json!([]), Some("d")),
        ];
        assert_eq!(
            run_documents(&refusals),
            Some(RunDocuments {
                attempted: true,
                jobs: vec![],
                failed: vec![],
                revisions: vec![]
            })
        );
        // Sent back, then retried right: the job.
        let retried = [
            ask("e", "documents.patch"),
            msg("tool", Some(refused), json!([]), Some("e")),
            ask("f", "documents.patch"),
            msg("tool", Some(job), json!([]), Some("f")),
        ];
        assert_eq!(
            run_documents(&retried),
            Some(RunDocuments {
                attempted: true,
                jobs: vec!["job_1".into()],
                failed: vec![],
                revisions: vec![]
            })
        );
        // PR-Daemon #1986 Low 1: the same call id reused, a read then a write:
        // each result goes with the call just before it.
        let reused = [
            ask("x", "documents.patch"),
            msg("tool", Some(job), json!([]), Some("x")),
            ask("x", "documents.paragraphs"),
            msg("tool", Some("{}"), json!([]), Some("x")),
        ];
        assert_eq!(
            run_documents(&reused),
            Some(RunDocuments {
                attempted: true,
                jobs: vec!["job_1".into()],
                failed: vec![],
                revisions: vec![]
            })
        );
        let reused_back = [
            ask("y", "documents.paragraphs"),
            msg("tool", Some("{}"), json!([]), Some("y")),
            ask("y", "documents.patch"),
            msg("tool", Some(refused), json!([]), Some("y")),
        ];
        assert_eq!(
            run_documents(&reused_back),
            Some(RunDocuments {
                attempted: true,
                jobs: vec![],
                failed: vec![],
                revisions: vec![]
            })
        );
        // General 0.6.94: two documents in one run, A made, B refused by the
        // module (a pending revision): both told. A's earlier refusal is not
        // a failure once A was made.
        let to = |id: &str, doc: &str| {
            msg(
                "assistant",
                None,
                json!([{ "id": id, "name": "documents.patch",
                         "arguments": format!("{{\"document_id\": \"{doc}\", \"edits\": []}}") }]),
                None,
            )
        };
        let two = [
            to("g", "doc_A"),
            msg("tool", Some(refused), json!([]), Some("g")),
            to("h", "doc_A"),
            msg("tool", Some(job), json!([]), Some("h")),
            to("i", "doc_B"),
            msg(
                "tool",
                Some(
                    r#"{"kind":"completed","payload":{"written":false,"reason":"invalid_request"}}"#,
                ),
                json!([]),
                Some("i"),
            ),
            to("j", "doc_B"),
            msg("tool", Some("tool error: failed: {}"), json!([]), Some("j")),
        ];
        assert_eq!(
            run_documents(&two),
            Some(RunDocuments {
                attempted: true,
                jobs: vec!["job_1".into()],
                failed: vec!["doc_B".into()],
                revisions: vec![]
            })
        );
        // PR-Daemon #1994 B1: refused under its file name, made under its id:
        // one document, made. The module's answers say which id a name is.
        let named = [
            msg(
                "assistant",
                None,
                json!([{ "id": "p", "name": "documents.paragraphs", "arguments": { "document_id": "合同.docx" } }]),
                None,
            ),
            msg(
                "tool",
                Some(r#"{"kind":"completed","payload":{"document_id":"doc_X","paragraphs":[]}}"#),
                json!([]),
                Some("p"),
            ),
            to("q", "合同.docx"),
            msg(
                "tool",
                Some("denied by policy: denied by user: 【未完成】…"),
                json!([]),
                Some("q"),
            ),
            to("r", "doc_X"),
            msg(
                "tool",
                Some(
                    r#"{"kind":"completed","payload":{"job_id":"job_9","source_document_id":"doc_X"}}"#,
                ),
                json!([]),
                Some("r"),
            ),
        ];
        assert_eq!(
            run_documents(&named),
            Some(RunDocuments {
                attempted: true,
                jobs: vec!["job_9".into()],
                failed: vec![],
                revisions: vec![]
            })
        );
        // The same the other way: the write's own answer names the document.
        let named_write = [
            to("s", "合同.docx"),
            msg(
                "tool",
                Some(
                    r#"{"kind":"completed","payload":{"written":false,"source_document_id":"doc_X"}}"#,
                ),
                json!([]),
                Some("s"),
            ),
            to("t", "doc_X"),
            msg(
                "tool",
                Some(r#"{"kind":"completed","payload":{"job_id":"job_9"}}"#),
                json!([]),
                Some("t"),
            ),
        ];
        assert_eq!(run_documents(&named_write).map(|d| d.failed), Some(vec![]));
        // A compose names its new document by title.
        let compose = [
            msg(
                "assistant",
                None,
                json!([{ "id": "k", "name": "documents.compose", "arguments": { "title": "周报" } }]),
                None,
            ),
            msg("tool", Some("tool error: failed: {}"), json!([]), Some("k")),
        ];
        assert_eq!(
            run_documents(&compose).map(|d| d.failed),
            Some(vec!["周报".to_owned()])
        );
        // fill and a later write operation are writes too.
        assert!(is_document_write("documents.fill") && is_document_write("documents.delete"));
        // L6-F2: a delete that went through is not 「没有改动」; a denied one is.
        let delete = |id: &str, result: &str| {
            vec![
                msg(
                    "assistant",
                    None,
                    json!([{ "id": id, "name": "documents.delete", "arguments": { "document_id": "doc_A" } }]),
                    None,
                ),
                msg("tool", Some(result), json!([]), Some(id)),
            ]
        };
        let done = delete(
            "d1",
            r#"{"kind":"completed","payload":{"deleted":true,"document_id":"doc_A"}}"#,
        );
        assert_eq!(run_documents(&done), None);
        let denied = delete("d2", "denied by user: 不删");
        assert_eq!(
            run_documents(&denied).map(|d| d.failed),
            Some(vec!["doc_A".to_owned()])
        );
        // PR-Daemon #2026 ①: a file name refused before any card, then the id
        // (with or without a get between), or a refusal then an approval of
        // the same id: the delete went through, nothing reads as unchanged.
        let refused = delete(
            "d3",
            "tool error: invalid: documents.delete: the argument \"document_id\" is not in the form it needs (its id); call it again with such a value",
        );
        let mut by_name = refused.clone();
        by_name[0].tool_calls = json!([{ "id": "d3", "name": "documents.delete", "arguments": { "document_id": "合同.pdf" } }]);
        let get = vec![
            msg(
                "assistant",
                None,
                json!([{ "id": "g", "name": "documents.get", "arguments": { "document_id": "合同.pdf" } }]),
                None,
            ),
            msg(
                "tool",
                Some(r#"{"kind":"completed","payload":{"document_id":"doc_A"}}"#),
                json!([]),
                Some("g"),
            ),
        ];
        for between in [vec![], get] {
            let thread: Vec<_> = by_name
                .iter()
                .chain(between.iter())
                .chain(done.iter())
                .cloned()
                .collect();
            assert_eq!(run_documents(&thread), None);
        }
        let again: Vec<_> = denied.iter().chain(done.iter()).cloned().collect();
        assert_eq!(run_documents(&again), None);
        // An approved delete the kernel could not run afterwards still counts.
        let lost = delete("d4", "tool error: invalid: unknown tool: documents.delete");
        assert_eq!(
            run_documents(&lost).map(|d| d.failed),
            Some(vec!["doc_A".to_owned()])
        );
    }

    /// L6-F5: a rewrite saved as the original's next version is a write
    /// that went through (no import job): it is listed with its version,
    /// and an earlier denied try at the same document is settled by it.
    #[test]
    fn a_write_saved_as_a_new_version_is_made_not_unchanged() {
        let to = |id: &str, doc: &str| {
            msg(
                "assistant",
                None,
                json!([{ "id": id, "name": "documents.patch", "arguments": { "document_id": doc } }]),
                None,
            )
        };
        let saved = r#"{"kind":"completed","payload":{"document_id":"doc_X","title":"合同.docx","revision":2,"previous_revision":1,"committed":true,"saved_as":"new_revision","source_document_id":"doc_X"}}"#;
        let run = [
            to("a", "合同.docx"),
            msg(
                "tool",
                Some("denied by policy: denied by user: 不对"),
                json!([]),
                Some("a"),
            ),
            to("b", "合同.docx"),
            msg("tool", Some(saved), json!([]), Some("b")),
        ];
        assert_eq!(
            run_documents(&run),
            Some(RunDocuments {
                attempted: true,
                jobs: vec![],
                failed: vec![],
                revisions: vec![SavedRevision {
                    document_id: "doc_X".into(),
                    title: "合同.docx".into(),
                    revision: 2,
                }],
            })
        );
        let shown = serde_json::to_value(run_documents(&run)).ok();
        assert_eq!(
            shown.as_ref().map(|v| &v["revisions"][0]["revision"]),
            Some(&json!(2))
        );
        // Without one, the field is left out, as an older daemon's answer.
        let none = serde_json::to_value(run_documents(&[to("c", "doc_Y")])).ok();
        assert!(none.is_some_and(|v| v.get("revisions").is_none()));
    }

    /// PR-Daemon #2034 Medium: a save whose content equals the head wrote
    /// nothing (`committed: false`); it is no saved version, and the run
    /// still reads as unchanged.
    #[test]
    fn a_save_that_wrote_nothing_is_not_a_saved_version() {
        let same = r#"{"kind":"completed","payload":{"document_id":"doc_X","title":"合同.docx","revision":1,"previous_revision":1,"committed":false,"saved_as":"new_revision","source_document_id":"doc_X"}}"#;
        let run = [
            msg(
                "assistant",
                None,
                json!([{ "id": "a", "name": "documents.fill", "arguments": { "document_id": "doc_X" } }]),
                None,
            ),
            msg("tool", Some(same), json!([]), Some("a")),
        ];
        let got = run_documents(&run);
        assert!(
            got.as_ref()
                .is_some_and(|d| d.revisions.is_empty() && d.jobs.is_empty())
        );
    }

    /// PR-Daemon #1979 Low: every documents operation the module declares is
    /// classed by its own risk: a read the module declares `read` is in
    /// `DOCUMENT_READS`, every other one is a write. A new read-only operation
    /// left out of the list turns this red instead of a red line under answers.
    #[test]
    fn every_declared_documents_operation_is_classed_by_its_risk() {
        let manifest = include_str!("../../agent24-documents/domain-os.yml");
        let mut op = None;
        let mut seen = 0;
        for line in manifest.lines() {
            let line = line.trim();
            if let Some(name) = line.strip_prefix("- operation:") {
                op = Some(name.trim().to_owned());
            } else if let Some(risk) = line.strip_prefix("risk:")
                && let Some(name) = op.take()
            {
                let read = risk.trim() == "read";
                assert_eq!(
                    !is_document_write(&format!("documents.{name}")),
                    read,
                    "documents.{name} is declared {risk}"
                );
                seen += 1;
            }
        }
        assert!(seen >= 7, "{seen}");
        assert!(!is_document_write("documents.get") && !is_document_write("knowledge_search"));
    }
}
