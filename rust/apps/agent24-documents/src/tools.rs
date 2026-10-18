//! Module tools (DOC-1-06b §3.2): `POST /_a24/tools/{op}`, the entry the
//! kernel calls for `documents.<op>` (`agent24d/src/module_tool_runtime.rs`).
//!
//! The kernel sends `{"context": …, "arguments": …}` and reads back a result
//! envelope (`agent24-os-proto/src/kernel_call.rs`, `ModuleToolEnvelope`):
//! `completed{payload, replayed}` or `error{code, retryable, details}`. This
//! entry always answers 2xx with an envelope, so a refusal is never mistaken
//! for a broken module. Only the kernel calls it directly: clients going
//! through the kernel proxy cannot reach it, as the proxy answers 404 when
//! the first segment after the namespace canonicalises to `_a24`.
//!
//! Operations: `list`, `get`, `job_get`, each on the service layer of its
//! REST route. Any other `op` is `operation_unavailable`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::documents::{self, DEFAULT_LIMIT, MAX_LIMIT};
use crate::error::ApiError;
use crate::jobs;
use crate::state::{AppState, Storage};

/// The input does not have the shape the kernel sends (kernel code table,
/// `agent24-domain/src/tool.rs`).
pub const INVALID_INPUT: &str = "invalid_input";
/// No such operation in this build: a manifest and router out of step.
pub const OPERATION_UNAVAILABLE: &str = "operation_unavailable";
/// A payload's serialized JSON stays under this, so the envelope stays under
/// the kernel's 64 KiB with room to spare (DOC-1-06b §2).
pub const PAYLOAD_BYTES: usize = 60 * 1024;
/// The module's own time for one call (DOC-1-06b §3.2 期限): strictly under
/// the manifest's `inline_wait_ms` (25 000), with room to spare. Past it the
/// call answers `internal`, retryable, before the kernel's wait runs out.
pub const TOOL_BUDGET: Duration = Duration::from_secs(20);
/// The code for a call past [`TOOL_BUDGET`] (retryable) and for an
/// unexpected failure (not): not in the kernel table, so it reaches the
/// model as `module_code`. Never the kernel's `timeout`, which reads as a
/// host-side timeout.
pub const INTERNAL: &str = "internal";

/// What the kernel injects (`agent24-tools` `ModuleToolContext`). Lenient on
/// purpose: a newer kernel adding a field must not make every call fail.
#[derive(Debug, Deserialize)]
pub struct ToolContext {
    pub run_id: String,
    /// `null` for a run with no session.
    pub session_id: Option<String>,
    pub tool_call_id: String,
    pub module_id: String,
    pub operation: String,
    /// Accepted and not used in slice 1 (DOC-1-06b §3.2): in production it
    /// is always empty, so it is neither an allow-list nor a check.
    #[serde(default)]
    pub authorized_resources: Vec<String>,
    #[serde(default)]
    pub authorization_ref: Option<String>,
}

/// The request body.
#[derive(Debug, Deserialize)]
pub struct ToolCall {
    pub context: ToolContext,
    pub arguments: Map<String, Value>,
}

/// The result envelope, shaped as `ModuleToolEnvelope` reads it.
#[derive(Debug, Serialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Envelope {
    Completed {
        payload: Value,
        #[serde(skip_serializing_if = "is_false")]
        replayed: bool,
    },
    Error {
        code: &'static str,
        retryable: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Envelope {
    #[must_use]
    pub fn error(code: &'static str, retryable: bool) -> Self {
        Self::Error {
            code,
            retryable,
            details: None,
        }
    }

    /// What the log records as the outcome.
    fn code(&self) -> &'static str {
        match self {
            Self::Completed { .. } => "completed",
            Self::Error { code, .. } => code,
        }
    }
}

/// `POST /_a24/tools/{op}`. Extractor failures are taken as values so that
/// they too come back as a 2xx envelope, not axum's 4xx.
pub async fn call_tool(
    state: State<AppState>,
    op: Result<UrlPath<String>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
) -> Response {
    call_tool_within(state, op, body, TOOL_BUDGET).await
}

/// [`call_tool`] with the processing budget as a parameter, so a test can
/// run past a short one without waiting 20 s.
async fn call_tool_within(
    State(state): State<AppState>,
    op: Result<UrlPath<String>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
    budget: Duration,
) -> Response {
    let Ok(UrlPath(op)) = op else {
        let (bytes, res) = reply(&Envelope::error(OPERATION_UNAVAILABLE, false));
        tracing::info!(code = OPERATION_UNAVAILABLE, bytes, "documents: tool call");
        return res;
    };
    let call = body
        .ok()
        .and_then(|b| serde_json::from_slice::<ToolCall>(&b).ok());
    let Some(call) = call else {
        // Never the body: it may carry document content.
        let (bytes, res) = reply(&Envelope::error(INVALID_INPUT, false));
        tracing::info!(op = ?op, code = INVALID_INPUT, bytes, "documents: tool call");
        return res;
    };
    let (run_id, tool_call_id) = (
        call.context.run_id.clone(),
        call.context.tool_call_id.clone(),
    );
    let envelope = dispatch(&state, &op, call, budget).await;
    let (bytes, res) = reply(&envelope);
    // Never the arguments or the payload: they may carry document content.
    tracing::info!(
        op = ?op,
        run_id = ?run_id,
        tool_call_id = ?tool_call_id,
        code = envelope.code(),
        bytes,
        "documents: tool call"
    );
    res
}

/// The envelope as a JSON response, and its size in bytes for the log.
fn reply(envelope: &Envelope) -> (usize, Response) {
    // Serializing an `Envelope` cannot fail: every map key is a string.
    let body = serde_json::to_vec(envelope).unwrap_or_default();
    let len = body.len();
    let headers = [(header::CONTENT_TYPE, "application/json")];
    (len, (headers, body).into_response())
}

/// Each tool calls the same service layer as its REST route, never the
/// route itself over HTTP. Each runs within `budget`.
async fn dispatch(state: &AppState, op: &str, call: ToolCall, budget: Duration) -> Envelope {
    // The path names the operation; a context naming another is not what
    // the kernel sends.
    if op != call.context.operation {
        return Envelope::error(INVALID_INPUT, false);
    }
    let ToolCall { context, arguments } = call;
    let work = async move {
        match op {
            "list" => list(state, arguments).await,
            "get" => get(state, arguments).await,
            "job_get" => job_get(state, arguments).await,
            _ => Err(Envelope::error(OPERATION_UNAVAILABLE, false)),
        }
    };
    // Dropping `work` at the deadline is safe: all three operations only
    // read (a pool wait, a query). Storage they find unavailable may be
    // reopened first, which writes (migrations, `jobs::recover`, the header's
    // `user_version`), but the reopen runs in its own task: a dropped call
    // stops waiting for it without interrupting it (`AppState::storage`). A
    // tool that writes (slice D2's `extract`) must first be checked for what
    // a cancel mid-way leaves behind.
    match tokio::time::timeout(budget, work).await {
        Ok(outcome) => outcome.unwrap_or_else(|refused| refused),
        Err(_) => {
            // Never the arguments: they may carry document content.
            tracing::warn!(
                op = ?op,
                run_id = ?context.run_id,
                tool_call_id = ?context.tool_call_id,
                budget_ms = u64::try_from(budget.as_millis()).unwrap_or(u64::MAX),
                "documents: tool call over its processing budget"
            );
            Envelope::error(INTERNAL, true)
        }
    }
}

/// `GET /documents`'s query, typed: `cursor?`, `limit?` (1 to 200). A field
/// left out is `None`; one sent as `null` is not a string or an integer, so
/// it fails the schema like any other wrong type.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListArgs {
    #[serde(default, deserialize_with = "present")]
    cursor: Option<String>,
    #[serde(default, deserialize_with = "present")]
    limit: Option<i64>,
}

/// An optional field that, when present, must be a `T`: unlike plain
/// `Option<T>`, an explicit `null` is refused, not read as absent.
fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GetArgs {
    document_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JobGetArgs {
    job_id: String,
}

/// A completed envelope, or the error one that answers instead.
type Outcome = Result<Envelope, Envelope>;

async fn list(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: ListArgs = parse(arguments)?;
    let limit = match args.limit {
        None => DEFAULT_LIMIT,
        Some(l) if (1..=MAX_LIMIT).contains(&l) => l,
        // Outside the input schema's range: structural, not business.
        Some(_) => return Err(Envelope::error(INVALID_INPUT, false)),
    };
    let storage = ready(state).await?;
    let page = documents::list(&storage, args.cursor.as_deref(), limit, PAYLOAD_BYTES).await;
    completed(page)
}

async fn get(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: GetArgs = parse(arguments)?;
    let storage = ready(state).await?;
    completed(documents::get(&storage, &args.document_id).await)
}

async fn job_get(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: JobGetArgs = parse(arguments)?;
    let storage = ready(state).await?;
    completed(jobs::get(&storage, &args.job_id).await)
}

/// The arguments as `T`, or `invalid_input`: a missing, extra or mistyped
/// field fails the tool's input schema.
fn parse<T: DeserializeOwned>(arguments: Map<String, Value>) -> Result<T, Envelope> {
    serde_json::from_value(Value::Object(arguments))
        .map_err(|_| Envelope::error(INVALID_INPUT, false))
}

async fn ready(state: &AppState) -> Result<Arc<Storage>, Envelope> {
    state
        .storage()
        .await
        .map_err(|cause| refused(&ApiError::storage_unavailable(cause)))
}

/// A service error under its own code (ADR-DOC-02 §6): the kernel keeps an
/// unknown code as `module_code`, and keeps `retryable` as given.
fn refused(e: &ApiError) -> Envelope {
    Envelope::error(e.code(), e.retryable())
}

/// The service's answer as the payload: the same JSON its REST route sends.
/// A paged answer is already within the budget; a single entry over it is
/// `payload_too_large`, never cut.
fn completed<T: Serialize>(answer: Result<T, ApiError>) -> Outcome {
    let answer = answer.map_err(|e| refused(&e))?;
    let payload = serde_json::to_value(answer)
        .map_err(|_| refused(&ApiError::internal("cannot serialize the answer")))?;
    let size = serde_json::to_vec(&payload).map_or(usize::MAX, |v| v.len());
    if size > PAYLOAD_BYTES {
        return Err(refused(&ApiError::over_budget(PAYLOAD_BYTES)));
    }
    Ok(Envelope::Completed {
        payload,
        replayed: false,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::error::StorageCause;
    use agent24_domain::tool::{ModuleToolCallError, ModuleToolErrorCode, ModuleToolResult};
    use agent24_os_proto::kernel_call::parse_module_tool_envelope;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::json;
    use tower::ServiceExt;

    fn context() -> Value {
        json!({
            "run_id": "run-1",
            "session_id": "ses-1",
            "tool_call_id": "call-1",
            "module_id": "documents",
            "operation": "list",
            "authorized_resources": [],
            "authorization_ref": "auth-1",
        })
    }

    fn context_for(op: &str) -> Value {
        let mut ctx = context();
        ctx["operation"] = json!(op);
        ctx
    }

    /// Calls `op` as the kernel does, with a matching context.
    async fn call(state: &AppState, op: &str, arguments: Value) -> Vec<u8> {
        let body = json!({ "context": context_for(op), "arguments": arguments });
        post_to(state, op, body.to_string()).await
    }

    /// The payload of a completed call, as the kernel's parser reads it.
    fn payload(body: &[u8]) -> Value {
        match parse_module_tool_envelope(body) {
            Ok(ModuleToolResult::Completed {
                payload,
                replayed: false,
            }) => payload,
            other => panic!("{other:?}: {}", String::from_utf8_lossy(body)),
        }
    }

    /// The kernel's parser reads a module code outside its table as
    /// `module_error`, keeping the code and `retryable`.
    fn assert_module_error(body: &[u8], code: &str, retryable: bool) {
        assert_eq!(
            parse_module_tool_envelope(body),
            Err(ModuleToolCallError::Module {
                code: ModuleToolErrorCode::ModuleError,
                retryable,
                details: None,
                unknown_code: Some(code.to_owned()),
            }),
            "{}",
            String::from_utf8_lossy(body)
        );
    }

    async fn rest(state: &AppState, path: &str) -> (StatusCode, Value) {
        let res = crate::router(state.clone())
            .oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn exec(state: &AppState, sql: &str) {
        let storage = state.storage().await.unwrap();
        sqlx::raw_sql(sql).execute(storage.db.pool()).await.unwrap();
    }

    /// Document `n` and its r1, titled by the SQL expression `title`.
    async fn add_document(state: &AppState, n: u32, title: &str) -> String {
        let id = format!("doc_01K74Z3QJ8V5N2W9RTX6Y{n:05}");
        let sha = format!("sha256:{}", "ab".repeat(32));
        exec(
            state,
            &format!(
                "BEGIN;
                 INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                        VALUES ('{id}', {title}, 'application/pdf', 1,
                                '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
                 INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                        VALUES ('{id}', 1, '{sha}', {n}, 'application/pdf', 'import');
                 COMMIT;"
            ),
        )
        .await;
        id
    }

    const JOB: &str = "job_01K75A0B1C2D3E4F5G6H7J8K9M";

    /// A failed job whose error message is the SQL expression `message`.
    async fn add_failed_job(state: &AppState, message: &str) {
        exec(
            state,
            &format!(
                "INSERT INTO jobs (id, kind, status, origin, error)
                 VALUES ('{JOB}', 'probe', 'failed', '{{\"kind\":\"page\"}}',
                         json_object('code', 'parse_failed', 'message', {message}))"
            ),
        )
        .await;
    }

    async fn opened() -> (tempfile::TempDir, AppState) {
        let dir = tempfile::tempdir().unwrap();
        let state = AppState::open(dir.path()).await;
        (dir, state)
    }

    /// Posts `body` to `op` and returns the raw response body, after checking
    /// the status is 2xx.
    async fn post(op: &str, body: impl Into<Body>) -> Vec<u8> {
        post_to(&AppState::unavailable(StorageCause::Busy), op, body).await
    }

    /// [`post`] against `state`. The body is read with the kernel's 64 KiB
    /// limit, so a larger answer fails here.
    async fn post_to(state: &AppState, op: &str, body: impl Into<Body>) -> Vec<u8> {
        let res = crate::router(state.clone())
            .oneshot(
                Request::post(format!("/_a24/tools/{op}"))
                    .header("content-type", "application/json")
                    .body(body.into())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap()
            .to_vec()
    }

    /// The kernel's own parser accepts the body as an error with this code.
    fn assert_kernel_error(body: &[u8], expected: ModuleToolErrorCode) {
        assert_eq!(
            parse_module_tool_envelope(body),
            Err(ModuleToolCallError::Module {
                code: expected,
                retryable: false,
                details: None,
                unknown_code: None,
            }),
            "{}",
            String::from_utf8_lossy(body)
        );
    }

    #[tokio::test]
    async fn a_body_that_is_not_json_is_invalid_input() {
        let body = post("list", "not json").await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "kind": "error", "code": "invalid_input", "retryable": false })
        );
    }

    #[tokio::test]
    async fn a_body_without_context_is_invalid_input() {
        let body = post("list", json!({ "arguments": {} }).to_string()).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn a_body_without_arguments_is_invalid_input() {
        let body = post("list", json!({ "context": context() }).to_string()).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn a_context_missing_a_required_field_is_invalid_input() {
        for field in ["run_id", "tool_call_id", "module_id", "operation"] {
            let mut ctx = context();
            ctx.as_object_mut().unwrap().remove(field);
            let body = post(
                "list",
                json!({ "context": ctx, "arguments": {} }).to_string(),
            )
            .await;
            assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
        }
    }

    #[tokio::test]
    async fn a_null_session_and_unknown_fields_still_parse() {
        let mut ctx = context();
        ctx["session_id"] = Value::Null;
        ctx["added_by_a_newer_kernel"] = json!(true);
        let body = post(
            "list",
            json!({ "context": ctx, "arguments": { "limit": 5 } }).to_string(),
        )
        .await;
        // Parsed, so it reaches `list`, which finds storage busy.
        assert_module_error(&body, "storage_unavailable", true);
    }

    #[tokio::test]
    async fn authorization_fields_are_accepted_and_not_used() {
        let mut ctx = context();
        ctx["operation"] = json!("no_such_op");
        ctx["authorized_resources"] = json!(["doc_01"]);
        ctx["authorization_ref"] = json!("auth-2");
        let body = post(
            "no_such_op",
            json!({ "context": ctx, "arguments": {} }).to_string(),
        )
        .await;
        // Parsed (not `invalid_input`) and not an allow-list: it reaches
        // dispatch like any other call.
        assert_kernel_error(&body, ModuleToolErrorCode::OperationUnavailable);
    }

    #[tokio::test]
    async fn an_unknown_operation_is_operation_unavailable() {
        let call = json!({ "context": context_for("no_such_op"), "arguments": {} }).to_string();
        let body = post("no_such_op", call).await;
        assert_kernel_error(&body, ModuleToolErrorCode::OperationUnavailable);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "kind": "error", "code": "operation_unavailable", "retryable": false })
        );
    }

    #[test]
    fn the_kernel_parses_every_envelope_shape() {
        let done = Envelope::Completed {
            payload: json!({ "items": [] }),
            replayed: false,
        };
        let body = serde_json::to_vec(&done).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "kind": "completed", "payload": { "items": [] } })
        );
        assert_eq!(
            parse_module_tool_envelope(&body),
            Ok(ModuleToolResult::Completed {
                payload: json!({ "items": [] }),
                replayed: false,
            })
        );
        let again = Envelope::Completed {
            payload: json!({}),
            replayed: true,
        };
        assert_eq!(
            parse_module_tool_envelope(&serde_json::to_vec(&again).unwrap()),
            Ok(ModuleToolResult::Completed {
                payload: json!({}),
                replayed: true,
            })
        );
        // A module code outside the kernel table keeps its name.
        let err = Envelope::Error {
            code: "engine_unavailable",
            retryable: true,
            details: Some(json!({ "engine": "apple-pdfkit" })),
        };
        assert_eq!(
            parse_module_tool_envelope(&serde_json::to_vec(&err).unwrap()),
            Err(ModuleToolCallError::Module {
                code: ModuleToolErrorCode::ModuleError,
                retryable: true,
                details: Some(json!({ "engine": "apple-pdfkit" })),
                unknown_code: Some("engine_unavailable".to_owned()),
            })
        );
    }

    #[tokio::test]
    async fn list_answers_what_rest_answers_with_the_same_parameters() {
        let (_dir, state) = opened().await;
        // An empty store is an empty page, not `null`.
        let (_, empty) = rest(&state, "/documents").await;
        assert_eq!(payload(&call(&state, "list", json!({})).await), empty);
        for n in 0..5 {
            add_document(&state, n, &format!("'doc {n}'")).await;
        }
        let (_, all) = rest(&state, "/documents").await;
        assert_eq!(payload(&call(&state, "list", json!({})).await), all);
        let (_, first) = rest(&state, "/documents?limit=2").await;
        let got = payload(&call(&state, "list", json!({ "limit": 2 })).await);
        assert_eq!(got, first);
        let cursor = first["next_cursor"].as_str().unwrap();
        let (_, second) = rest(&state, &format!("/documents?limit=2&cursor={cursor}")).await;
        let args = json!({ "limit": 2, "cursor": cursor });
        assert_eq!(payload(&call(&state, "list", args).await), second);
        // A cursor that passes the schema but not the service is the
        // service's own `invalid_request`.
        let body = call(&state, "list", json!({ "cursor": "nope" })).await;
        assert_module_error(&body, "invalid_request", false);
    }

    #[tokio::test]
    async fn list_cuts_its_pages_at_the_tool_budget_and_pages_back_to_the_rest_list() {
        let (_dir, state) = opened().await;
        // 500 NULs: the longest title the import allows, six JSON bytes per
        // character, so about 19 documents fill 60 KiB.
        for n in 0..50 {
            add_document(&state, n, "CAST(zeroblob(500) AS TEXT)").await;
        }
        // REST's 512 KiB page holds all 50 at once, as before.
        let (status, rest_page) = rest(&state, "/documents?limit=200").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rest_page["documents"].as_array().unwrap().len(), 50);
        assert!(rest_page["next_cursor"].is_null());
        let mut paged = Vec::new();
        let mut cursor: Option<String> = None;
        let mut pages = 0;
        loop {
            let mut args = json!({ "limit": 200 });
            if let Some(c) = &cursor {
                args["cursor"] = json!(c);
            }
            let page = payload(&call(&state, "list", args).await);
            let size = serde_json::to_vec(&page).unwrap().len();
            assert!(size <= PAYLOAD_BYTES, "{size} bytes");
            let rows = page["documents"].as_array().unwrap().clone();
            pages += 1;
            match page["next_cursor"].as_str() {
                Some(c) => {
                    // Cut at the budget: one more row would not have fit.
                    let one = serde_json::to_vec(&rows[0]).unwrap().len();
                    // (Rows differ by a digit or so; 8 bytes of slack.)
                    assert!(size + one + 8 > PAYLOAD_BYTES, "{size} bytes");
                    cursor = Some(c.to_owned());
                }
                None => cursor = None,
            }
            paged.extend(rows);
            if cursor.is_none() {
                break;
            }
        }
        assert!(pages >= 3, "{pages} pages");
        assert_eq!(Value::Array(paged), rest_page["documents"]);
    }

    #[tokio::test]
    async fn a_single_document_over_the_budget_is_payload_too_large() {
        let (_dir, state) = opened().await;
        add_document(&state, 1, "'a title'").await;
        let storage = state.storage().await.unwrap();
        // No row the contract allows reaches 60 KiB; a tiny budget stands in.
        let e = documents::list(&storage, None, 50, 64).await.unwrap_err();
        assert_eq!(e.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = serde_json::to_vec(&refused(&e)).unwrap();
        assert_module_error(&body, "payload_too_large", false);
    }

    #[tokio::test]
    async fn get_and_job_get_answer_what_rest_answers() {
        let (_dir, state) = opened().await;
        let id = add_document(&state, 7, "'report'").await;
        let (status, doc) = rest(&state, &format!("/documents/{id}")).await;
        assert_eq!(status, StatusCode::OK);
        let got = payload(&call(&state, "get", json!({ "document_id": id })).await);
        assert_eq!(got, doc);
        add_failed_job(&state, "'it broke'").await;
        let (status, job) = rest(&state, &format!("/jobs/{JOB}")).await;
        assert_eq!(status, StatusCode::OK);
        let got = payload(&call(&state, "job_get", json!({ "job_id": JOB })).await);
        assert_eq!(got, job);
    }

    #[tokio::test]
    async fn an_unknown_document_or_job_is_not_found_and_a_bad_id_invalid_request() {
        let (_dir, state) = opened().await;
        let missing_doc = json!({ "document_id": "doc_01K74Z3QJ8V5N2W9RTX6Y99999" });
        assert_module_error(&call(&state, "get", missing_doc).await, "not_found", false);
        let missing_job = json!({ "job_id": JOB });
        let body = call(&state, "job_get", missing_job).await;
        assert_module_error(&body, "not_found", false);
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({ "kind": "error", "code": "not_found", "retryable": false })
        );
        let bad = json!({ "document_id": "x" });
        assert_module_error(&call(&state, "get", bad).await, "invalid_request", false);
    }

    #[tokio::test]
    async fn a_job_over_the_budget_is_payload_too_large_not_cut() {
        let (_dir, state) = opened().await;
        // An 80 000-character message.
        add_failed_job(&state, "hex(zeroblob(40000))").await;
        let body = call(&state, "job_get", json!({ "job_id": JOB })).await;
        assert_module_error(&body, "payload_too_large", false);
    }

    #[tokio::test]
    async fn arguments_outside_the_schema_are_invalid_input() {
        let (_dir, state) = opened().await;
        for (op, args) in [
            ("list", json!({ "limit": 5, "extra": true })),
            ("list", json!({ "limit": "5" })),
            ("list", json!({ "limit": 0 })),
            ("list", json!({ "limit": 201 })),
            ("list", json!({ "cursor": 7 })),
            // `null` is neither an integer nor a string: not "left out".
            ("list", json!({ "limit": null })),
            ("list", json!({ "cursor": null })),
            ("list", json!({ "limit": 5, "cursor": null })),
            ("get", json!({})),
            ("get", json!({ "document_id": null })),
            ("job_get", json!({ "job_id": null })),
            ("get", json!({ "document_id": 1 })),
            ("get", json!({ "document_id": "doc_x", "revision": 1 })),
            ("job_get", json!({ "id": JOB })),
            ("job_get", json!({ "job_id": JOB, "x": null })),
        ] {
            let body = call(&state, op, args.clone()).await;
            assert_eq!(
                parse_module_tool_envelope(&body),
                Err(ModuleToolCallError::Module {
                    code: ModuleToolErrorCode::InvalidInput,
                    retryable: false,
                    details: None,
                    unknown_code: None,
                }),
                "{op} {args}"
            );
        }
    }

    #[tokio::test]
    async fn a_context_naming_another_operation_is_invalid_input() {
        let (_dir, state) = opened().await;
        let id = add_document(&state, 1, "'t'").await;
        // A valid `get` call, posted to `get` with a context saying `list`.
        let body = json!({ "context": context_for("list"), "arguments": { "document_id": id } });
        let body = post_to(&state, "get", body.to_string()).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn storage_contention_is_retryable_and_other_causes_are_not() {
        for (cause, retryable) in [
            (StorageCause::Locked, true),
            (StorageCause::Busy, true),
            (StorageCause::Corrupt, false),
            (StorageCause::DiskFull, false),
        ] {
            let state = AppState::unavailable(cause);
            for (op, args) in [
                ("list", json!({})),
                (
                    "get",
                    json!({ "document_id": "doc_01K74Z3QJ8V5N2W9RTX6Y00001" }),
                ),
                ("job_get", json!({ "job_id": JOB })),
            ] {
                let body = call(&state, op, args).await;
                assert_module_error(&body, "storage_unavailable", retryable);
            }
        }
    }

    /// A call past its processing budget answers `internal`, retryable, by
    /// the budget: here the pool is held, so each operation waits for a
    /// connection (sqlx's own acquire timeout is 30 s, past the kernel's
    /// 25 s `inline_wait`).
    #[tokio::test]
    async fn a_call_past_its_budget_is_200_internal_retryable() {
        let (_dir, state) = opened().await;
        let storage = state.storage().await.unwrap();
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(storage.db.pool().acquire().await.unwrap());
        }
        let budget = Duration::from_millis(100);
        let app = axum::Router::new()
            .route(
                "/_a24/tools/{op}",
                axum::routing::post(
                    move |s: State<AppState>,
                          o: Result<UrlPath<String>, PathRejection>,
                          b: Result<Bytes, BytesRejection>| {
                        call_tool_within(s, o, b, budget)
                    },
                ),
            )
            .with_state(state.clone());
        for (op, args) in [
            ("list", json!({})),
            (
                "get",
                json!({ "document_id": "doc_01K74Z3QJ8V5N2W9RTX6Y00001" }),
            ),
            ("job_get", json!({ "job_id": JOB })),
        ] {
            let body = json!({ "context": context_for(op), "arguments": args });
            let req = Request::post(format!("/_a24/tools/{op}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            // Longer than the budget, far shorter than sqlx's 30 s: without
            // the budget the call is still waiting when this runs out.
            let res = tokio::time::timeout(Duration::from_secs(3), app.clone().oneshot(req))
                .await
                .unwrap_or_else(|_| panic!("{op}: no answer within its budget"))
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                json!({ "kind": "error", "code": "internal", "retryable": true }),
                "{op}"
            );
            assert_module_error(&bytes, "internal", true);
        }
        drop(held);
        // The pool is free again: the same call completes.
        assert!(payload(&call(&state, "list", json!({})).await)["documents"].is_array());
    }

    #[tokio::test]
    async fn a_body_over_the_2_mib_limit_is_still_200_invalid_input() {
        let big = format!(
            r#"{{"context":{},"arguments":{{"cursor":"{}"}}}}"#,
            context(),
            "a".repeat(2 * 1024 * 1024 + 1)
        );
        let body = post("list", big).await;
        assert_kernel_error(&body, ModuleToolErrorCode::InvalidInput);
    }

    #[tokio::test]
    async fn an_operation_segment_that_is_not_utf8_is_operation_unavailable() {
        let call = json!({ "context": context(), "arguments": {} }).to_string();
        let body = post("%FF", call).await;
        assert_kernel_error(&body, ModuleToolErrorCode::OperationUnavailable);
    }

    /// Through the kernel proxy a client never reaches the entry: the
    /// proxy answers 404 before it looks for a running module.
    #[tokio::test]
    async fn the_kernel_proxy_answers_404_for_the_tool_entry() {
        use agent24_os_proto::drain::{Current, Generation};
        let ns = "/api/v1/documents";
        let current = Current::new(Generation::starting());
        let app = agent24_os_proto::proxy::mount(axum::Router::new(), ns, current);
        let status = |path: String| {
            let app = app.clone();
            async move {
                let req = Request::post(path).body(Body::from("{}")).unwrap();
                app.oneshot(req).await.unwrap().status()
            }
        };
        for path in ["/_a24/tools/list", "/_A24/tools/get", "/_a24/tools/job_get"] {
            assert_eq!(
                status(format!("{ns}{path}")).await,
                StatusCode::NOT_FOUND,
                "{path}"
            );
        }
        // Not just any path: a REST one is forwarded, and finds the module
        // not ready yet.
        assert_ne!(
            status(format!("{ns}/documents")).await,
            StatusCode::NOT_FOUND
        );
    }
}
