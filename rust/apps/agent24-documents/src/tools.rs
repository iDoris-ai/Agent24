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
//! Operations: `list`, `get`, `job_get`, `read_range`, `find`,
//! `extraction_get` (read only) and `extract` (submits or replays a job),
//! each on the service layer of its REST route. Any other `op` is
//! `operation_unavailable`.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::authorization::{Accepted, Call};
use crate::documents::{self, DEFAULT_LIMIT, MAX_LIMIT};
use crate::engine::Requester;
use crate::error::ApiError;
use crate::extract::handler::{self, Fail, Prepared, Schema};
use crate::extractions;
use crate::id::{IdKind, is_id};
use crate::jobs;
use crate::oplog::{Origin, Run};
use crate::read::find::{self, FindRequest};
use crate::read::text::{self, TextParams};
use crate::scope::{self, PERMISSION_DENIED, Scope};
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
    /// Read by [`Scope::parse`] (K1-06a §2.2): kept as sent, so that a
    /// malformed list refuses the call as `permission_denied` instead of
    /// failing the body as `invalid_input`. `None` when absent; an explicit
    /// `null` is `Some(Value::Null)`, which is malformed.
    #[serde(default, deserialize_with = "present")]
    pub authorized_resources: Option<Value>,
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
    // In flight from here, so the end of its authorization waits for it.
    let in_flight = state.calls().begin(op);
    // Once per call, before any operation reads storage (K1-06a §2.2).
    let Ok(scope) = Scope::parse(call.context.authorized_resources.as_ref()) else {
        return Envelope::error(PERMISSION_DENIED, false);
    };
    let ToolCall { context, arguments } = call;
    let run = Run {
        run_id: context.run_id.clone(),
        tool_call_id: context.tool_call_id.clone(),
    };
    let accepted = Accepted {
        operation: op.to_owned(),
        authorization_ref: context.authorization_ref.clone(),
    };
    let scope = &scope;
    let claim_holds = in_flight.hold();
    let work = async move {
        // The scope is checked here, for every operation, before it runs
        // (K1-06a §4.1): an operation `admit` has no row for never runs.
        admit(state, scope, op, &arguments).await?;
        match op {
            "extract" => extract(state, run, accepted, claim_holds, arguments).await,
            "list" => list(state, scope, arguments).await,
            "get" => get(state, arguments).await,
            "job_get" => job_get(state, arguments).await,
            "read_range" => read_range(state, arguments).await,
            "find" => find(state, arguments).await,
            "extraction_get" => extraction_get(state, arguments).await,
            _ => Err(Envelope::error(OPERATION_UNAVAILABLE, false)),
        }
    };
    // Dropping `work` at the deadline is safe: every operation but `extract`
    // only reads (a pool wait, a query, a blob; `extraction_get` one
    // read-only transaction, rolled back when dropped). Storage they find
    // unavailable may be reopened first, which writes (migrations,
    // `jobs::recover`, the header's `user_version`), and `read_range` /
    // `find` may build a text layer, but both run in their own task: a
    // dropped call stops waiting for them without interrupting them
    // (`AppState::storage`, `Layers::layer`). `extract` writes only in
    // `claim`'s one transaction (job, key, `oplog` row), and `claim` runs in
    // its own task: past the deadline this call answers `internal`
    // (retryable) while the claim still commits and starts its worker; the
    // retry hits the same key and replays that job (`replayed: true` unless
    // it has since failed and runs again; one more `oplog` row either way).
    let envelope = match tokio::time::timeout(budget, work).await {
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
    };
    // Its authorization ended while it ran (K1-06a-grant-path §2.5): what it
    // found is not given. Decided as it leaves the ledger, so an end cannot
    // fall between the check and the answer (#1206 gate 1).
    if in_flight.finish() {
        return Envelope::error(PERMISSION_DENIED, false);
    }
    envelope
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

/// `GET …/revisions/{rev}/text`'s path and query, typed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadRangeArgs {
    document_id: String,
    revision: i64,
    #[serde(default, deserialize_with = "present")]
    block: Option<String>,
    #[serde(default, deserialize_with = "present")]
    cursor: Option<String>,
    #[serde(default, deserialize_with = "present")]
    limit: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    require_complete: Option<bool>,
}

/// `POST /documents/{id}/find`'s path and body, typed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FindArgs {
    document_id: String,
    revision: i64,
    query: String,
    #[serde(default, deserialize_with = "present")]
    cursor: Option<String>,
    #[serde(default, deserialize_with = "present")]
    limit: Option<i64>,
    #[serde(default, deserialize_with = "present")]
    require_complete: Option<bool>,
}

/// `GET /extractions/{id}`'s path and query, typed.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractionGetArgs {
    extraction_id: String,
    #[serde(default, deserialize_with = "present")]
    cursor: Option<String>,
    #[serde(default, deserialize_with = "present")]
    limit: Option<i64>,
}

/// `POST /documents/{id}/extractions`'s path and body, without `rerun`: a
/// rerun is keyed by an `Idempotency-Key` header a tool call cannot send.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExtractArgs {
    document_id: String,
    revision: i64,
    schema: Value,
}

/// A completed envelope, or the error one that answers instead.
type Outcome = Result<Envelope, Envelope>;

/// Only the documents in `scope`: counts and cursors as well (K1-06a §2.4).
async fn list(state: &AppState, scope: &Scope, arguments: Map<String, Value>) -> Outcome {
    let args: ListArgs = parse(arguments)?;
    let limit = bounded(args.limit)?.unwrap_or(DEFAULT_LIMIT);
    let storage = ready(state).await?;
    let only = Some(scope.documents());
    let cursor = args.cursor.as_deref();
    let page = documents::list(&storage, only, cursor, limit, PAYLOAD_BYTES).await;
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

async fn read_range(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: ReadRangeArgs = parse(arguments)?;
    let params = TextParams {
        block: args.block,
        cursor: args.cursor,
        limit: bounded(args.limit)?.and_then(|l| usize::try_from(l).ok()),
        require_complete: args.require_complete.unwrap_or(false),
    };
    let storage = ready(state).await?;
    let (id, rev) = (&args.document_id, args.revision);
    let by = Requester::Tool("read_range".to_owned());
    completed(text::read_range(state, &storage, id, rev, params, PAYLOAD_BYTES, by).await)
}

async fn find(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: FindArgs = parse(arguments)?;
    let req = FindRequest {
        revision: args.revision,
        query: args.query,
        cursor: args.cursor,
        limit: bounded(args.limit)?.and_then(|l| usize::try_from(l).ok()),
        require_complete: args.require_complete.unwrap_or(false),
    };
    let storage = ready(state).await?;
    let by = Requester::Tool("find".to_owned());
    completed(find::search(state, &storage, &args.document_id, req, PAYLOAD_BYTES, by).await)
}

async fn extraction_get(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: ExtractionGetArgs = parse(arguments)?;
    let limit = bounded(args.limit)?
        .and_then(|l| usize::try_from(l).ok())
        .unwrap_or(extractions::DEFAULT_LIMIT);
    let storage = ready(state).await?;
    let (id, cursor) = (&args.extraction_id, args.cursor.as_deref());
    completed(extractions::get(&storage, id, cursor, limit, PAYLOAD_BYTES).await)
}

/// Submits or replays an extract job, as REST does (DOC-1-06b §2.3), from
/// `run`; never waits for it. The hash is taken of `{revision, schema}`, the
/// body the page sends, so a page's job and a run's share a key.
async fn extract(
    state: &AppState,
    run: Run,
    accepted: Accepted,
    in_flight: Call,
    arguments: Map<String, Value>,
) -> Outcome {
    // The oplog's CHECK would refuse it inside the claim, read as a
    // storage failure.
    if run.run_id.is_empty() || run.tool_call_id.is_empty() {
        return Err(Envelope::error(INVALID_INPUT, false));
    }
    let args: ExtractArgs = parse(arguments)?;
    let schema: Schema = serde_json::from_value(args.schema.clone())
        .map_err(|_| Envelope::error(INVALID_INPUT, false))?;
    let body = json!({ "revision": args.revision, "schema": args.schema });
    let Prepared {
        storage,
        key,
        request,
        input,
    } = handler::prepare(
        state,
        args.document_id,
        args.revision,
        schema.fields,
        &body,
        None,
    )
    .await
    .map_err(|e| refused(&e))?;
    // In a task of its own: dropped at the deadline, this call must not stop
    // the claim halfway from the worker start that belongs with it. The
    // call stays in flight until the claim ends.
    let origin = Origin::Run(run);
    let claim = handler::claim(
        state.clone(),
        storage.clone(),
        key,
        request,
        input,
        origin,
        Some(accepted),
    );
    let task = tokio::spawn(async move {
        let _in_flight = in_flight;
        claim.await
    });
    match task.await {
        Ok(Ok((job, started))) => Ok(Envelope::Completed {
            payload: json!({ "status": job.status, "job_id": job.job_id }),
            replayed: !started,
        }),
        Ok(Err(Fail::Api(e))) => Err(refused(&e)),
        Ok(Err(Fail::Db(e))) => Err(refused(&storage.db_failure(e).await)),
        // A spawned task is only cancelled when the runtime shuts down.
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// A `limit` outside the input schema's 1 to 200 is structural, not
/// business: `invalid_input`. The service checks it again, for REST.
fn bounded(limit: Option<i64>) -> Result<Option<i64>, Envelope> {
    match limit {
        Some(l) if !(1..=MAX_LIMIT).contains(&l) => Err(Envelope::error(INVALID_INPUT, false)),
        l => Ok(l),
    }
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

/// What an operation touches: the one table the entry checks the scope
/// against (K1-06a §4.1, §4.3). `None`: no such operation, refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resource {
    /// By `document_id`.
    Document,
    /// By `job_id`, through the job's document.
    Job,
    /// By `extraction_id`, through the extraction's document.
    Extraction,
    /// None: the operation filters by the scope itself (`list`).
    Listing,
}

fn resource(op: &str) -> Option<Resource> {
    match op {
        "get" | "read_range" | "find" | "extract" => Some(Resource::Document),
        "job_get" => Some(Resource::Job),
        "extraction_get" => Some(Resource::Extraction),
        "list" => Some(Resource::Listing),
        _ => None,
    }
}

/// Lets `op` run only if what it names is in `scope`, by its row in
/// [`resource`]; before any other argument is checked, so outside the
/// scope always reads as absent: the service's own not-found (K1-06a
/// §2.4), whatever else is wrong. A missing or non-string id is
/// `invalid_input`; a malformed one passes, as nothing has it and the
/// service refuses it. Only a job's or an extraction's owner is read.
async fn admit(
    state: &AppState,
    scope: &Scope,
    op: &str,
    arguments: &Map<String, Value>,
) -> Result<(), Envelope> {
    let named = |key: &str| match arguments.get(key) {
        Some(Value::String(id)) => Ok(id.as_str()),
        _ => Err(Envelope::error(INVALID_INPUT, false)),
    };
    let (kind, id, absent) = match resource(op) {
        None => return Err(Envelope::error(OPERATION_UNAVAILABLE, false)),
        Some(Resource::Listing) => return Ok(()),
        Some(Resource::Document) => (IdKind::Document, named("document_id")?, "no such document"),
        Some(Resource::Job) => (IdKind::Job, named("job_id")?, "no such job"),
        Some(Resource::Extraction) => (
            IdKind::Extraction,
            named("extraction_id")?,
            "no such extraction",
        ),
    };
    if !is_id(kind, id) {
        return Ok(());
    }
    let owner = match kind {
        IdKind::Job => scope::job_document(&*ready(state).await?, id).await,
        IdKind::Extraction => scope::extraction_document(&*ready(state).await?, id).await,
        _ => Ok(Some(id.to_owned())),
    };
    match owner.map_err(|e| refused(&e))? {
        Some(doc) if scope.allows_document(&doc) => Ok(()),
        _ => Err(refused(&ApiError::not_found(absent))),
    }
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

    mod extract;
    mod extraction;
    mod read;
    mod scope;

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

    /// [`context_for`] with a scope of these documents (K1-06a §2.1).
    fn context_with(op: &str, docs: &[&str]) -> Value {
        let mut ctx = context_for(op);
        ctx["authorized_resources"] = docs.iter().map(|d| format!("doc:{d}")).collect();
        ctx
    }

    /// Calls `op` as the kernel does, authorized: its scope holds every
    /// stored document and the one `arguments` names (when that one can be
    /// a scope entry), so it tests the operation, not the scope. The scope
    /// itself is tested through [`call_in`] (`tests/scope.rs`).
    async fn call(state: &AppState, op: &str, arguments: Value) -> Vec<u8> {
        let mut docs: Vec<String> = match state.storage().await {
            Ok(storage) => sqlx::query_scalar("SELECT id FROM documents")
                .fetch_all(storage.db.pool())
                .await
                .unwrap(),
            Err(_) => Vec::new(),
        };
        if let Some(id) = arguments.get("document_id").and_then(Value::as_str) {
            let entry = json!([format!("doc:{id}")]);
            if Scope::parse(Some(&entry)).is_ok() {
                docs.push(id.to_owned());
            }
        }
        let docs: Vec<&str> = docs.iter().map(String::as_str).collect();
        call_in(state, op, &docs, arguments).await
    }

    /// Calls `op` as the kernel does, with a scope of exactly `docs`.
    async fn call_in(state: &AppState, op: &str, docs: &[&str], arguments: Value) -> Vec<u8> {
        let body = json!({ "context": context_with(op, docs), "arguments": arguments });
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
    const DOC: &str = "doc_01K74Z3QJ8V5N2W9RTX6Y00001";

    /// A failed job on [`DOC`] r1 (added here unless already there) whose
    /// error message is the SQL expression `message`. A job reaches a tool
    /// call only through its document (K1-06a §2.3).
    async fn add_failed_job(state: &AppState, message: &str) {
        let storage = state.storage().await.unwrap();
        let found: Option<String> = sqlx::query_scalar("SELECT id FROM documents WHERE id = ?1")
            .bind(DOC)
            .fetch_optional(storage.db.pool())
            .await
            .unwrap();
        if found.is_none() {
            add_document(state, 1, "'the job''s document'").await;
        }
        exec(
            state,
            &format!(
                "INSERT INTO jobs (id, kind, status, origin, error, document_id, revision)
                 VALUES ('{JOB}', 'probe', 'failed', '{{\"kind\":\"page\"}}',
                         json_object('code', 'parse_failed', 'message', {message}),
                         '{DOC}', 1)"
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
    async fn a_malformed_scope_is_permission_denied_not_invalid_input() {
        let mut ctx = context();
        ctx["operation"] = json!("no_such_op");
        ctx["authorized_resources"] = json!(["doc_01"]);
        ctx["authorization_ref"] = json!("auth-2");
        let body = post(
            "no_such_op",
            json!({ "context": ctx, "arguments": {} }).to_string(),
        )
        .await;
        // The body parses; the scope does not (K1-06a §2.2).
        assert_kernel_error(&body, ModuleToolErrorCode::PermissionDenied);
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
        let e = documents::list(&storage, None, None, 50, 64)
            .await
            .unwrap_err();
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
        // In the scope (`call` authorizes every stored document): outside
        // it, the scope answers first, as absent (K1-06a §4.1).
        add_failed_job(&state, "'it broke'").await;
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
                ("read_range", json!({ "document_id": DOC, "revision": 1 })),
                (
                    "find",
                    json!({ "document_id": DOC, "revision": 1, "query": "a" }),
                ),
                (
                    "extraction_get",
                    json!({ "extraction_id": "ext_01K75A0B1C2D3E4F5G6H7J8K9M" }),
                ),
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
            ("read_range", json!({ "document_id": DOC, "revision": 1 })),
            (
                "find",
                json!({ "document_id": DOC, "revision": 1, "query": "a" }),
            ),
        ] {
            let body = json!({ "context": context_with(op, &[DOC]), "arguments": args });
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
