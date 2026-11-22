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
    let accepted = Accepted {
        operation: op.to_owned(),
        authorization_ref: call.context.authorization_ref.clone(),
    };
    // In flight from here, so the end of its authorization waits for it;
    // under a grant already ended, refused here (K1-06a §2.5.1).
    let in_flight = state.calls().begin(accepted.clone());
    if in_flight.ended() {
        return Envelope::error(PERMISSION_DENIED, false);
    }
    // Once per call, before any operation reads storage (K1-06a §2.2).
    let Ok(scope) = Scope::parse(call.context.authorized_resources.as_ref()) else {
        return Envelope::error(PERMISSION_DENIED, false);
    };
    let ToolCall {
        context,
        mut arguments,
    } = call;
    let run = Run {
        run_id: context.run_id.clone(),
        tool_call_id: context.tool_call_id.clone(),
    };
    let scope = &scope;
    let claim_holds = in_flight.hold();
    let work = async move {
        // The scope is checked here, for every operation, before it runs
        // (K1-06a §4.1): an operation `admit` has no row for never runs.
        named_by_title(state, scope, op, &mut arguments).await?;
        admit(state, scope, op, &arguments).await?;
        match op {
            "extract" => extract(state, run, accepted, claim_holds, arguments).await,
            "list" => list(state, scope, arguments).await,
            "get" => get(state, arguments).await,
            "job_get" => job_get(state, arguments).await,
            "read_range" => read_range(state, accepted, arguments).await,
            "find" => find(state, accepted, arguments).await,
            "extraction_get" => extraction_get(state, arguments).await,
            "compose" => compose(state, arguments).await,
            "paragraphs" => paragraphs(state, arguments).await,
            "patch" => {
                let run_id = run.run_id.clone();
                patch(state, &run_id, arguments).await
            }
            "fields" => fields(state, arguments).await,
            "fill" => fill(state, arguments).await,
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

/// D2b: split or merge PDFs already here into a new document; the answer is
/// its import job, as `POST /compositions` answers. The parts were admitted.
async fn compose(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let req: crate::compose::ComposeRequest = parse(arguments)?;
    let storage = ready(state).await?;
    let (_, job) = crate::compose::run(&storage, req)
        .await
        .map_err(|e| refused(&e))?;
    completed::<Value>(Ok(settled(&storage, job.0).await))
}

/// How long a write waits for its import before it answers.
const SETTLE_WAIT: Duration = Duration::from_secs(20);

/// M1 (0.6.89, General/PM): after an approved patch went through, the model
/// was told only that a job was queued, called documents.patch again, twice,
/// and another card came up for the same edits. A write now waits (up to
/// [`SETTLE_WAIT`]) for its import and says what came of it: 「已完成：已生成
/// 《X》……不要再调用」, or that it failed, or, still going, that it is under way
/// and not to be asked again. The job is still all there (`job_id` stays where
/// agent24d's run record reads it).
async fn settled(storage: &Arc<Storage>, job: crate::jobs::Job) -> Value {
    let id = job.job_id.clone();
    let mut last = job;
    let deadline = tokio::time::Instant::now() + SETTLE_WAIT;
    while (last.status == "queued" || last.status == "running")
        && tokio::time::Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(250)).await;
        match jobs::get(storage, &id).await {
            Ok(next) => last = next,
            Err(_) => break,
        }
    }
    let made = if last.status == "succeeded" {
        let doc = last
            .result
            .as_ref()
            .and_then(|r| r.get("document_id"))
            .and_then(|d| d.as_str())
            .map(str::to_owned);
        match doc {
            Some(d) => documents::get(storage, &d)
                .await
                .ok()
                .map(|detail| detail.document.title),
            None => None,
        }
    } else {
        None
    };
    let note = match (last.status.as_str(), &made) {
        ("succeeded", Some(title)) => format!(
            "已完成：已生成新文档《{title}》。这一步已经完成，不要再调用同一个写入工具，直接告诉用户已生成《{title}》。"
        ),
        ("succeeded", None) => "已完成：新文档已生成。这一步已经完成，不要再调用同一个写入工具，直接告诉用户结果。".to_owned(),
        ("queued" | "running", _) => "已提交，新文档正在生成。这一步已经完成：不要再调用同一个写入工具，直接告诉用户正在生成。".to_owned(),
        _ => "没能生成新文档（导入失败）。如实告诉用户没有生成，不要说已完成。".to_owned(),
    };
    let mut v = serde_json::to_value(&last).unwrap_or(Value::Null);
    if let Value::Object(map) = &mut v {
        map.insert("note".to_owned(), Value::String(note));
        if let Some(title) = made {
            map.insert("title".to_owned(), Value::String(title));
        }
    }
    v
}

/// E1b/D5a: the request without `document_id`, and that id.
fn split_document(
    mut arguments: Map<String, Value>,
) -> Result<(String, Map<String, Value>), Envelope> {
    match arguments.remove("document_id") {
        Some(Value::String(id)) => Ok((id, arguments)),
        _ => Err(Envelope::error(INVALID_INPUT, false)),
    }
}

/// E1b: a DOCX's paragraphs, by index (read).
/// What an Agent may narrow `paragraphs` to.
#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ParagraphsArgs {
    #[serde(default)]
    from: Option<usize>,
    #[serde(default)]
    count: Option<usize>,
    #[serde(default)]
    query: Option<String>,
}

/// Characters of a paragraph's opening in the table of contents: its
/// number and first words, enough to tell clauses apart. Its whole text is
/// asked for by index or word before it is rewritten.
const PREVIEW_CHARS: usize = 24;
/// FU-164 (General 0.6.98, balanced tier): a 40-character, 3000-character
/// table of contents in JSON objects took a long contract's second prompt to
/// 7076 tokens. It is one line per paragraph, `段号 开头`, within this many
/// characters.
const TOC_BUDGET: usize = 1200;
/// Characters of whole paragraphs in one answer.
const PREVIEW_BUDGET: usize = 3000;
const PREVIEW_MAX_COUNT: usize = 200;
/// PM 11-21: a narrowed ask (a range or a word) that comes to this many
/// paragraphs or fewer, within [`PREVIEW_BUDGET`], is answered with them
/// whole: the model rewrites from what it sees, and half a clause gives half
/// a rewrite.
const WHOLE_MAX_COUNT: usize = 10;

/// FU-151 (0.6.89, General): 《示范-山东健身合同2025》's 293 paragraphs came to
/// about 40 KB, and with them in its context the 4-bit model on an 8 GB Mac
/// was stopped at prefill. An Agent is shown the paragraphs that have text,
/// as a table of contents, a line `段号 开头` per paragraph of its first
/// [`PREVIEW_CHARS`] characters, from `from` on or only those containing
/// `query`, within [`TOC_BUDGET`] characters (FU-164);
/// `total` and `next_from` say what is left. The HTTP route and the CLI keep
/// the whole list.
async fn paragraphs(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let (id, rest) = split_document(arguments)?;
    let args: ParagraphsArgs = parse(rest)?;
    let storage = ready(state).await?;
    let all = crate::patch::paragraphs_of(&storage, id.clone())
        .await
        .map_err(|e| refused(&e))?;
    completed::<Value>(Ok(preview_paragraphs(&id, &all.paragraphs, &args)))
}

fn preview_paragraphs(id: &str, all: &[crate::patch::Paragraph], args: &ParagraphsArgs) -> Value {
    let query = args
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_lowercase);
    let wanted: Vec<&crate::patch::Paragraph> = all
        .iter()
        .filter(|p| !p.text.trim().is_empty())
        .filter(|p| {
            query
                .as_ref()
                .is_none_or(|q| p.text.to_lowercase().contains(q))
        })
        .collect();
    let from = args.from.unwrap_or(0);
    let count = args
        .count
        .unwrap_or(PREVIEW_MAX_COUNT)
        .min(PREVIEW_MAX_COUNT);
    let narrowed = args.from.is_some() || args.count.is_some() || query.is_some();
    let after: Vec<&&crate::patch::Paragraph> = wanted.iter().filter(|p| p.index >= from).collect();
    let picked = &after[..after.len().min(count)];
    let whole_size: usize = picked.iter().map(|p| p.text.chars().count()).sum();
    if narrowed && picked.len() <= WHOLE_MAX_COUNT && whole_size <= PREVIEW_BUDGET {
        let shown: Vec<Value> = picked
            .iter()
            .map(|p| serde_json::json!({ "index": p.index, "text": p.text, "in_table": p.in_table, "whole": true }))
            .collect();
        let mut out = serde_json::json!({
            "document_id": id,
            "total": wanted.len(),
            "paragraphs": shown,
            "note": "这些段落是全文（whole: true），改写时照这里的原文写新文字，anchor 照抄开头。",
        });
        if let Some(p) = after.get(picked.len()) {
            out["next_from"] = Value::from(p.index);
        }
        return out;
    }
    let mut toc = String::new();
    let mut next_from = None;
    for (lines, p) in wanted.iter().filter(|p| p.index >= from).enumerate() {
        let opening: String = p.text.trim().chars().take(PREVIEW_CHARS).collect();
        let line = format!("{} {opening}\n", p.index);
        if lines >= count || toc.chars().count() + line.chars().count() > TOC_BUDGET {
            next_from = Some(p.index);
            break;
        }
        toc.push_str(&line);
    }
    let mut out = serde_json::json!({
        "document_id": id,
        "total": wanted.len(),
        "toc": toc,
        "note": "这是目录：每行是「段号 开头几个字」，空段落不列。要改写某几段，先用 query（条款号或关键词，例如「第五条」「违约责任」）或 from 和 count（例如 from=段号, count=1）取这几段的全文，再照全文改写；目录后面的段落用 next_from 继续取。",
    });
    if let Some(n) = next_from {
        out["next_from"] = Value::from(n);
    }
    out
}

/// E1b: some paragraphs edited into a new document (write; approved per call).
/// An Agent's edit (M1 (c), General 11-21): the paragraph's index, its new
/// text, and `anchor`, the paragraph's opening words as the model read them.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentEdit {
    index: usize,
    text: String,
    #[serde(default)]
    tracked: bool,
    anchor: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AgentPatch {
    edits: Vec<AgentEdit>,
    #[serde(default)]
    title: Option<String>,
}

/// Characters of a paragraph's opening said back when its anchor is wrong.
const OPENING_CHARS: usize = 24;

/// E1b, as an Agent asks it. The text an edit replaces is the paragraph the
/// document has at its index, never the model's retelling of it (0.6.75: it
/// left 「第三条」 out and the approved edit failed). The model's `anchor` must be
/// that paragraph's opening (`docx_patch::anchor_matches`): one that is not
/// (0.6.72: 第三条's words aimed at 乙方's line) writes nothing, and the answer
/// says how the paragraph really begins so the model can try again. The HTTP
/// route and the CLI keep their own `expect`.
async fn patch(state: &AppState, run_id: &str, arguments: Map<String, Value>) -> Outcome {
    let (id, rest) = split_document(arguments)?;
    // M1 (0.6.89, General): after its patch went through, a run asked for the
    // same document again and another card came up. Once per run and
    // document; the answer says what was already made.
    if let Some(title) = state.patched_in(run_id, &id) {
        return completed::<Value>(Ok(serde_json::json!({
            "written": false,
            "source_document_id": id,
            "reason": "already_submitted",
            "title": title,
            "message": format!("这一轮已经提交过对这份文档的改写，已生成《{title}》。不要再调用 documents.patch，直接告诉用户已生成《{title}》。"),
        })));
    }
    let asked: AgentPatch = parse(rest)?;
    let storage = ready(state).await?;
    let paras = crate::patch::paragraphs_of(&storage, id.clone())
        .await
        .map_err(|e| refused(&e))?;
    let mut edits = Vec::with_capacity(asked.edits.len());
    for edit in asked.edits {
        let Some(para) = paras.paragraphs.iter().find(|p| p.index == edit.index) else {
            return Err(Envelope::error(INVALID_INPUT, false));
        };
        if !crate::docx_patch::anchor_matches(&para.text, &edit.anchor) {
            let opening: String = para.text.chars().take(OPENING_CHARS).collect();
            return completed::<Value>(Ok(serde_json::json!({
                "written": false,
                "source_document_id": id,
                "reason": "anchor_mismatch",
                "index": edit.index,
                "paragraph_starts_with": opening,
                "message": format!(
                    "【未完成】本次没有改写，也没有生成任何文档：第 {} 段（index {}）开头是「{opening}」，和 anchor「{}」对不上。请先告诉用户这次没改成，不要说已保存或已完成；然后用 documents.paragraphs 确认段号，照抄那一段开头至少 12 个字作为 anchor，重新调用 documents.patch。",
                    edit.index + 1,
                    edit.index,
                    edit.anchor
                ),
            })));
        }
        edits.push(serde_json::json!({
            "index": edit.index,
            "text": edit.text,
            "tracked": edit.tracked,
            "expect": para.text,
        }));
    }
    let mut body = serde_json::json!({ "edits": edits });
    if let Some(title) = asked.title {
        body["title"] = Value::String(title);
    }
    let req: crate::patch::PatchRequest =
        serde_json::from_value(body).map_err(|_| Envelope::error(INVALID_INPUT, false))?;
    let (_, job) = match crate::patch::patch_run(&storage, id.clone(), req).await {
        Ok(made) => made,
        // General 0.6.94: a paragraph the rewrite would damage (a pending
        // revision, a formula, a page break, ...) came back as a bare
        // `invalid_request`, and the model said the document was made. The
        // model is told what was refused and why, in words it can pass on.
        Err(e) if matches!(e.code(), "invalid_request" | "revision_conflict") => {
            let why = refusal_in_words(&e);
            // PR-Daemon #1994 B2: a paragraph changed since it was read is the
            // one refusal the model can mend itself: told to read again and
            // retry once, as `anchor_mismatch` is.
            let message = if e.code() == "revision_conflict" {
                format!(
                    "【未完成】这份文档没有改，也没有生成新文档：{why}。请先告诉用户这次没改成，不要说已生成或已完成；然后用 documents.paragraphs 重新读取这几段，照新的原文重新调用一次 documents.patch。"
                )
            } else {
                format!(
                    "【未完成】这份文档没有改，也没有生成新文档：{why}。请如实告诉用户这一份没改成和原因，不要说已生成或已完成。"
                )
            };
            return completed::<Value>(Ok(serde_json::json!({
                "written": false,
                "source_document_id": id,
                "reason": e.code(),
                "detail": e.message(),
                "message": message,
            })));
        }
        Err(e) => return Err(refused(&e)),
    };
    let mut answer = settled(&storage, job.0).await;
    // PR-Daemon #1994 B1: which document this write was of, whatever the
    // call named it; the run's record compares writes by it.
    if let Value::Object(map) = &mut answer {
        map.insert("source_document_id".to_owned(), Value::String(id.clone()));
    }
    if answer["status"] == "succeeded"
        || answer["status"] == "queued"
        || answer["status"] == "running"
    {
        let title = answer["title"].as_str().unwrap_or("新文档").to_owned();
        state.note_patched(run_id, &id, &title);
    }
    completed::<Value>(Ok(answer))
}

/// D5a: a template's `{{fields}}` (read).
async fn fields(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let (id, rest) = split_document(arguments)?;
    if !rest.is_empty() {
        return Err(Envelope::error(INVALID_INPUT, false));
    }
    let storage = ready(state).await?;
    completed(crate::patch::fields_of(&storage, id).await)
}

/// D5a: a new document of the template with its fields filled (write; approved per call).
/// Always named 「<template>（填写）」: a title the model made up is dropped (General 11-19).
async fn fill(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let (id, mut rest) = split_document(arguments)?;
    rest.remove("title");
    let req: crate::patch::FillRequest = parse(rest)?;
    let storage = ready(state).await?;
    let (_, job) = crate::patch::fill_run(&storage, id, req)
        .await
        .map_err(|e| refused(&e))?;
    completed::<Value>(Ok(settled(&storage, job.0).await))
}

async fn job_get(state: &AppState, arguments: Map<String, Value>) -> Outcome {
    let args: JobGetArgs = parse(arguments)?;
    let storage = ready(state).await?;
    completed(jobs::get(&storage, &args.job_id).await)
}

async fn read_range(state: &AppState, by: Accepted, arguments: Map<String, Value>) -> Outcome {
    let args: ReadRangeArgs = parse(arguments)?;
    let params = TextParams {
        block: args.block,
        cursor: args.cursor,
        limit: bounded(args.limit)?.and_then(|l| usize::try_from(l).ok()),
        require_complete: args.require_complete.unwrap_or(false),
    };
    let storage = ready(state).await?;
    let (id, rev) = (&args.document_id, args.revision);
    let by = Requester::Tool(by);
    completed(text::read_range(state, &storage, id, rev, params, PAYLOAD_BYTES, by).await)
}

async fn find(state: &AppState, by: Accepted, arguments: Map<String, Value>) -> Outcome {
    let args: FindArgs = parse(arguments)?;
    let req = FindRequest {
        revision: args.revision,
        query: args.query,
        cursor: args.cursor,
        limit: bounded(args.limit)?.and_then(|l| usize::try_from(l).ok()),
        require_complete: args.require_complete.unwrap_or(false),
    };
    let storage = ready(state).await?;
    let by = Requester::Tool(by);
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
    /// Every `parts[].document_id` (`compose`).
    Parts,
}

fn resource(op: &str) -> Option<Resource> {
    match op {
        "get" | "read_range" | "find" | "extract" | "paragraphs" | "patch" | "fields" | "fill" => {
            Some(Resource::Document)
        }
        "job_get" => Some(Resource::Job),
        "extraction_get" => Some(Resource::Extraction),
        "list" => Some(Resource::Listing),
        "compose" => Some(Resource::Parts),
        _ => None,
    }
}

/// D2b (Loop 2): a model often names a document by its file name rather than
/// its id. For `get` and `compose`, a `document_id` that is not an id is
/// looked up among the titles of the documents in `scope`: the one whose
/// title, without extension, case and 《》, equals it (or else the only one
/// containing it) stands for it. Anything else is left as it was, and the
/// service answers not found.
async fn named_by_title(
    state: &AppState,
    scope: &Scope,
    op: &str,
    arguments: &mut Map<String, Value>,
) -> Result<(), Envelope> {
    let mut slots: Vec<&mut Value> = match op {
        "get" | "paragraphs" | "patch" | "fields" | "fill" => {
            arguments.get_mut("document_id").into_iter().collect()
        }
        "compose" => match arguments.get_mut("parts") {
            Some(Value::Array(parts)) => parts
                .iter_mut()
                .filter_map(|p| p.get_mut("document_id"))
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    slots.retain(|v| v.as_str().is_some_and(|s| !is_id(IdKind::Document, s)));
    if slots.is_empty() {
        return Ok(());
    }
    let storage = ready(state).await?;
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, title FROM documents")
        .fetch_all(storage.db.pool())
        .await
        .map_err(|_| refused(&ApiError::internal("cannot read the titles")))?;
    let titles: Vec<(String, String)> = rows
        .into_iter()
        .filter(|(id, _)| scope.allows_document(id))
        .map(|(id, title)| (id, plain_title(&title)))
        .collect();
    for slot in slots {
        let wanted = plain_title(slot.as_str().unwrap_or_default());
        if wanted.is_empty() {
            continue;
        }
        let only = |hits: Vec<&String>| (hits.len() == 1).then(|| hits[0].clone());
        let exact = only(
            titles
                .iter()
                .filter(|(_, t)| *t == wanted)
                .map(|(id, _)| id)
                .collect(),
        );
        let found = exact.or_else(|| {
            only(
                titles
                    .iter()
                    .filter(|(_, t)| t.contains(&wanted))
                    .map(|(id, _)| id)
                    .collect(),
            )
        });
        if let Some(id) = found {
            *slot = Value::String(id);
        }
    }
    Ok(())
}

/// A title or a name for it, compared without case, quotes, surrounding
/// space and a file extension.
fn plain_title(raw: &str) -> String {
    let s = raw
        .trim()
        .trim_matches(|c| matches!(c, '《' | '》' | '「' | '」' | '"' | '“' | '”'))
        .trim()
        .to_lowercase();
    match s.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && (1..=5).contains(&ext.len())
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem.to_owned()
        }
        _ => s,
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
        Some(Resource::Parts) => {
            // Each part's document in the scope, or the call reads as one
            // naming a document that is not here (K1-06a §2.4).
            let Some(Value::Array(parts)) = arguments.get("parts") else {
                return Err(Envelope::error(INVALID_INPUT, false));
            };
            for part in parts {
                let Some(Value::String(id)) = part.get("document_id") else {
                    return Err(Envelope::error(INVALID_INPUT, false));
                };
                if is_id(IdKind::Document, id) && !scope.allows_document(id) {
                    return Err(refused(&ApiError::not_found("no such document")));
                }
            }
            return Ok(());
        }
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

/// Why a patch was refused, in words for the user (General 0.6.94).
fn refusal_in_words(e: &ApiError) -> String {
    let m = e.message();
    if e.code() == "revision_conflict" {
        return "这一段在读取之后被改过，需要重新读取段落".to_owned();
    }
    const KINDS: &[(&str, &str)] = &[
        ("w:del", "已有未处理的修订（删除痕迹）"),
        ("w:ins", "已有未处理的修订（插入痕迹）"),
        ("m:oMath", "公式"),
        ("picture", "图片"),
        ("embedded object", "嵌入对象"),
        ("link", "链接"),
        ("field", "域（自动编号、页码等）"),
        ("footnote", "脚注"),
        ("endnote", "尾注"),
        ("content control", "内容控件"),
        ("text box", "文本框"),
        ("page or column break", "分页符或分栏符"),
        ("comment reference", "批注"),
        ("w:sym", "特殊符号"),
        ("w:ruby", "注音"),
    ];
    if let Some(rest) = m.strip_prefix("paragraph ")
        && let Some((index, what)) = rest.split_once(" contains ")
    {
        let what = KINDS
            .iter()
            .find(|(k, _)| what.contains(k))
            .map_or("Word 的特殊内容", |(_, zh)| zh);
        let n = index
            .parse::<usize>()
            .map_or_else(|_| index.to_owned(), |i| (i + 1).to_string());
        return format!(
            "第 {n} 段里有{what}，自动改写会把它弄丢，所以没有改；这一段请在 Word 或 WPS 里手动修改"
        );
    }
    if m.contains("out of range") || m.contains("no paragraph") {
        return "段落编号超出了这份文档的范围".to_owned();
    }
    "改写请求不符合要求".to_owned()
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
    async fn a_document_named_by_its_file_name_is_found_in_the_scope() {
        let (_dir, state) = opened().await;
        let policy = add_document(&state, 1, "'CFYAD26-9770136.pdf'").await;
        let _other = add_document(&state, 2, "'other.pdf'").await;
        let outside = add_document(&state, 3, "'secret.pdf'").await;
        let scope = [policy.as_str(), _other.as_str()];
        // The file name without extension, as the model wrote it: that document.
        let body = call_in(
            &state,
            "get",
            &scope,
            json!({ "document_id": "CFYAD26-9770136" }),
        )
        .await;
        assert_eq!(payload(&body)["document_id"], json!(policy));
        // With quotes and the extension too.
        let body = call_in(
            &state,
            "get",
            &scope,
            json!({ "document_id": "《cfyad26-9770136.PDF》" }),
        )
        .await;
        assert_eq!(payload(&body)["document_id"], json!(policy));
        // A title outside the scope stays unknown.
        let body = call_in(&state, "get", &scope, json!({ "document_id": "secret" })).await;
        assert!(
            String::from_utf8_lossy(&body).contains("\"error\""),
            "{outside}"
        );
        // compose: each part by name; past the scope (the blob is no PDF here).
        let args = json!({ "parts": [{ "document_id": "CFYAD26-9770136", "pages": "1" }] });
        let body =
            String::from_utf8_lossy(&call_in(&state, "compose", &scope, args).await).into_owned();
        assert!(!body.contains("not_found"), "{body}");
    }

    #[test]
    fn a_title_is_compared_without_case_quotes_or_extension() {
        assert_eq!(plain_title(" 《Report.PDF》 "), "report");
        assert_eq!(plain_title("a.b.docx"), "a.b");
        assert_eq!(plain_title("v1.2026"), "v1");
        assert_eq!(plain_title("no-extension"), "no-extension");
    }

    #[tokio::test]
    async fn compose_takes_only_documents_in_the_scope() {
        let (_dir, state) = opened().await;
        let inside = add_document(&state, 1, "'a'").await;
        let outside = add_document(&state, 2, "'b'").await;
        let parts = |ids: &[&str]| json!({ "parts": ids.iter().map(|id| json!({ "document_id": id, "pages": "1" })).collect::<Vec<_>>() });
        // One part outside the scope: as if no such document, nothing composed.
        let body = call_in(&state, "compose", &[&inside], parts(&[&inside, &outside])).await;
        assert_module_error(&body, "not_found", false);
        // All inside: past the scope, to the service (whose blob is no PDF here).
        let body = call_in(&state, "compose", &[&inside], parts(&[&inside])).await;
        let answer = String::from_utf8_lossy(&body).into_owned();
        assert!(
            !answer.contains("not_found") && answer.contains("\"error\""),
            "{answer}"
        );
        // No parts array: invalid input before storage is read.
        let body = call_in(&state, "compose", &[&inside], json!({ "title": "x" })).await;
        assert!(String::from_utf8_lossy(&body).contains("invalid_input"));
    }

    #[tokio::test]
    async fn fill_and_patch_take_only_documents_in_the_scope_named_by_title_too() {
        let (_dir, state) = opened().await;
        let inside = add_document(&state, 1, "'租赁合同模板.docx'").await;
        let outside = add_document(&state, 2, "'secret.docx'").await;
        let fill = |id: &str| json!({ "document_id": id, "values": { "甲方": "x" } });
        for (op, args) in [
            ("fill", fill(&outside)),
            ("fields", json!({ "document_id": outside })),
            ("paragraphs", json!({ "document_id": outside })),
            (
                "patch",
                json!({ "document_id": outside, "edits": [{ "index": 0, "text": "x" }] }),
            ),
        ] {
            let body = call_in(&state, op, &[&inside], args).await;
            assert_module_error(&body, "not_found", false);
        }
        // By its title, inside: past the scope, to the service (the blob is no DOCX here).
        let body = call_in(&state, "fill", &[&inside], fill("租赁合同模板")).await;
        let answer = String::from_utf8_lossy(&body).into_owned();
        assert!(
            !answer.contains("not_found") && answer.contains("\"error\""),
            "{answer}"
        );
        // A key the tool does not take: invalid input.
        let body = call_in(
            &state,
            "fields",
            &[&inside],
            json!({ "document_id": inside, "x": 1 }),
        )
        .await;
        assert!(String::from_utf8_lossy(&body).contains("invalid_input"));
    }

    /// PR-Daemon #1994 L2/B2: each refusal in words, the fallback included,
    /// and a stale paragraph told to be read again.
    #[test]
    fn a_refusal_is_said_in_words() {
        let say = |m: &str| refusal_in_words(&ApiError::invalid_request(m.to_owned()));
        assert!(say("paragraph 4 contains a picture (w:drawing), which rewriting it would lose; edit it in Word or WPS").starts_with("第 5 段里有图片"));
        assert!(
            say("paragraph 0 contains m:oMath, which rewriting it would lose").contains("公式")
        );
        assert!(
            say("paragraph 2 contains w:fooBar in a run, which rewriting it would lose")
                .contains("第 3 段里有Word 的特殊内容")
        );
        assert_eq!(
            say("there is no paragraph 99"),
            "段落编号超出了这份文档的范围"
        );
        assert_eq!(say("something else"), "改写请求不符合要求");
    }

    #[tokio::test]
    async fn a_refused_patch_tells_the_model_what_and_why() {
        // General 0.6.94: 「第二条 押金…」 holds a pending deletion (w:del); the
        // rewrite was refused as a bare invalid_request and the model said
        // the document was made. Now: not written, and why, in words.
        let (_dir, state) = opened().await;
        let storage = state.storage().await.unwrap();
        let file: &[u8] = include_bytes!("../tests/fixtures/contract-formula-revision-break.docx");
        let sha = storage.blobs.put_bytes(file).unwrap().sha256;
        let id = "doc_01K74Z3QJ8V5N2W9RTX6Y00079";
        let docx = crate::office::DOCX;
        exec(
            &state,
            &format!(
                "INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                        VALUES ('{id}', '合同-公式修订分页.docx', '{docx}', 1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
                 INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                        VALUES ('{id}', 1, '{sha}', {}, '{docx}', 'import');",
                file.len()
            ),
        )
        .await;
        let body = json!({ "document_id": id, "edits": [{
            "index": 2, "anchor": "第二条 押金为两个月租金，期满退还。",
            "text": "第二条 押金金额为两个月租金总额，租赁期满后予以全额返还。", "tracked": true }] });
        let answer = call_in(&state, "patch", &[id], body).await;
        let answer = payload(&answer);
        assert_eq!(answer["written"], false, "{answer}");
        assert_eq!(answer["reason"], "invalid_request");
        let message = answer["message"].as_str().unwrap();
        assert!(
            message.contains("没有改") && message.contains("第 3 段") && message.contains("修订"),
            "{message}"
        );
        let uploads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
        assert_eq!(uploads, 0);
    }

    /// M1 (c), General 11-21: an Agent's edit names its paragraph by index and
    /// opening words. Without an anchor it is invalid; with another
    /// paragraph's opening (0.6.72: 第三条's words at 乙方's line) nothing is
    /// written and the answer says how that paragraph begins; with its own
    /// opening, numbering left out (0.6.75), the new document is made.
    #[tokio::test]
    async fn an_agent_patch_names_its_paragraph_by_its_opening_words() {
        let (_dir, state) = opened().await;
        let storage = state.storage().await.unwrap();
        let template: &[u8] = include_bytes!("../tests/fixtures/template-lease.docx");
        let sha = storage.blobs.put_bytes(template).unwrap().sha256;
        let id = "doc_01K74Z3QJ8V5N2W9RTX6Y00078";
        let docx = crate::office::DOCX;
        exec(
            &state,
            &format!(
                "INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                        VALUES ('{id}', '合同.docx', '{docx}', 1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
                 INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                        VALUES ('{id}', 1, '{sha}', {}, '{docx}', 'import');",
                template.len()
            ),
        )
        .await;
        let paras: Vec<_> = crate::docx_patch::list_paragraphs(template)
            .unwrap()
            .into_iter()
            .filter(|p| p.text.trim().chars().count() >= 8)
            .collect();
        let (this, other) = (&paras[0], &paras[1]);
        let opening = |t: &str| t.trim().chars().take(16).collect::<String>();
        let edit = |anchor: Option<String>| {
            let mut e = json!({ "index": this.index, "text": "新的文字", "tracked": true });
            if let Some(a) = anchor {
                e["anchor"] = Value::String(a);
            }
            json!({ "document_id": id, "edits": [e] })
        };
        let body = call_in(&state, "patch", &[id], edit(None)).await;
        assert!(String::from_utf8_lossy(&body).contains("invalid_input"));
        // Another paragraph's opening: not written, and told how it begins.
        let body = call_in(&state, "patch", &[id], edit(Some(opening(&other.text)))).await;
        let answer = payload(&body);
        assert_eq!(answer["written"], false, "{answer}");
        assert_eq!(answer["reason"], "anchor_mismatch");
        assert!(
            this.text
                .starts_with(answer["paragraph_starts_with"].as_str().unwrap())
        );
        let uploads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
        assert_eq!(uploads, 0);
        // Its own opening: the new document is made.
        let body = call_in(&state, "patch", &[id], edit(Some(opening(&this.text)))).await;
        assert_eq!(
            payload(&body)["kind"],
            "import",
            "{}",
            String::from_utf8_lossy(&body)
        );
        // M1 (0.6.89, PM): the answer waits for the import and says what was
        // made, done, and not to be called again.
        let answer = payload(&body);
        assert!(answer["job_id"].is_string());
        assert_eq!(answer["status"], "succeeded", "{answer}");
        let note = answer["note"].as_str().unwrap();
        assert!(
            note.starts_with("已完成：已生成新文档《") && note.contains("不要再调用"),
            "{note}"
        );
        assert!(answer["title"].is_string());
        // The same run asking for the same document again: refused, told what
        // was made (General 11-21), no new upload.
        let again = call_in(&state, "patch", &[id], edit(Some(opening(&this.text)))).await;
        let again = payload(&again);
        assert_eq!(again["reason"], "already_submitted", "{again}");
        assert_eq!(again["title"], answer["title"]);
        let uploads: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM uploads")
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
        assert_eq!(uploads, 1);
    }

    /// FU-151 (0.6.89): 293 paragraphs of a real contract came to 40 KB in an
    /// Agent's context. FU-164 (0.6.98, balanced tier): the 3000-character
    /// table of contents in JSON objects still took the second prompt to 7076
    /// tokens. It is one line per paragraph with text, `段号 开头`, within
    /// 1200 characters, and the Agent is told how to ask for the rest.
    /// FU-164 on the contract of 0.6.98's run (《示范-山东健身合同2025》, 293
    /// paragraphs): its table of contents as the model reads it, and the
    /// clause found by a word, whole.
    #[test]
    fn a_real_contracts_table_of_contents_stays_small() {
        let file: &[u8] = include_bytes!("../tests/fixtures/samr-shandong-fitness-2025.docx");
        let all: Vec<crate::patch::Paragraph> = crate::docx_patch::list_paragraphs(file)
            .unwrap()
            .into_iter()
            .map(|p| crate::patch::Paragraph {
                index: p.index,
                text: p.text,
                in_table: p.in_table,
            })
            .collect();
        let toc = preview_paragraphs("doc_1", &all, &ParagraphsArgs::default());
        let size = toc.to_string().chars().count();
        assert!(size < 1700, "{size}");
        let found = preview_paragraphs(
            "doc_1",
            &all,
            &ParagraphsArgs {
                query: Some("违约责任".into()),
                ..Default::default()
            },
        );
        assert!(found.to_string().chars().count() < 3600, "{found}");
    }

    #[test]
    fn an_agent_is_shown_a_long_documents_paragraphs_in_parts() {
        let all: Vec<crate::patch::Paragraph> = (0..293)
            .map(|i| crate::patch::Paragraph {
                index: i,
                text: if i % 3 == 0 {
                    String::new()
                } else {
                    format!("第{i}条 {}", "合同条款内容".repeat(20))
                },
                in_table: false,
            })
            .collect();
        let first = preview_paragraphs("doc_1", &all, &ParagraphsArgs::default());
        let toc = first["toc"].as_str().unwrap();
        assert!(toc.chars().count() <= 1200, "{}", toc.chars().count());
        assert_eq!(first["total"], 195);
        let lines: Vec<&str> = toc.lines().collect();
        assert!(
            lines.iter().all(|l| {
                let n: usize = l.split(' ').next().unwrap().parse().unwrap();
                !n.is_multiple_of(3)
            }),
            "no empty paragraph"
        );
        assert_eq!(
            lines[0],
            format!("1 {}", all[1].text.chars().take(24).collect::<String>())
        );
        // The answer as the model reads it stays small: about the budget,
        // not JSON objects around every line.
        assert!(
            first.to_string().chars().count() < 1700,
            "{}",
            first.to_string().chars().count()
        );
        let next = first["next_from"].as_u64().unwrap() as usize;
        // The rest, from where it stopped.
        let more = preview_paragraphs(
            "doc_1",
            &all,
            &ParagraphsArgs {
                from: Some(next),
                ..Default::default()
            },
        );
        assert!(
            more["toc"]
                .as_str()
                .unwrap()
                .starts_with(&format!("{next} "))
        );
        // A word picks the paragraphs that have it, whole (PM 11-21).
        let found = preview_paragraphs(
            "doc_1",
            &all,
            &ParagraphsArgs {
                query: Some("第250条".into()),
                ..Default::default()
            },
        );
        assert_eq!(found["total"], 1);
        assert_eq!(found["paragraphs"][0]["index"], 250);
        assert_eq!(found["paragraphs"][0]["whole"], true);
        assert_eq!(
            found["paragraphs"][0]["text"].as_str().unwrap(),
            all[250].text
        );
        assert!(found.get("next_from").is_none());
        // One paragraph by its index: whole too.
        let one = preview_paragraphs(
            "doc_1",
            &all,
            &ParagraphsArgs {
                from: Some(4),
                count: Some(1),
                ..Default::default()
            },
        );
        assert_eq!(one["paragraphs"][0]["index"], 4);
        assert_eq!(one["paragraphs"][0]["text"].as_str().unwrap(), all[4].text);
        assert_eq!(one["next_from"], 5);
        // A range too wide for whole text is a table of contents again.
        let wide = preview_paragraphs(
            "doc_1",
            &all,
            &ParagraphsArgs {
                from: Some(0),
                count: Some(50),
                ..Default::default()
            },
        );
        assert!(wide.get("paragraphs").is_none());
        let wide_toc = wide["toc"].as_str().unwrap();
        assert!(wide_toc.lines().count() <= 50 && wide_toc.chars().count() <= 1200);
    }

    #[tokio::test]
    async fn fill_names_the_new_document_after_the_template_whatever_title_the_model_gives() {
        let (_dir, state) = opened().await;
        let storage = state.storage().await.unwrap();
        let template: &[u8] = include_bytes!("../tests/fixtures/template-lease.docx");
        let sha = storage.blobs.put_bytes(template).unwrap().sha256;
        let id = "doc_01K74Z3QJ8V5N2W9RTX6Y00077";
        let docx = crate::office::DOCX;
        exec(
            &state,
            &format!(
                "INSERT INTO documents (id, title, media_type, head_revision, created_at, updated_at)
                        VALUES ('{id}', '租赁模板.docx', '{docx}', 1, '2026-10-01T00:00:00.000Z', '2026-10-01T00:00:00.000Z');
                 INSERT INTO revisions (document_id, revision, content_sha256, size, media_type, origin)
                        VALUES ('{id}', 1, '{sha}', {}, '{docx}', 'import');",
                template.len()
            ),
        )
        .await;
        let args = json!({ "document_id": id, "values": { "出租方": "明远" }, "title": "房屋租赁合同-李四" });
        let body = call_in(&state, "fill", &[id], args).await;
        assert_eq!(
            payload(&body)["kind"],
            "import",
            "{}",
            String::from_utf8_lossy(&body)
        );
        let filename: String = sqlx::query_scalar("SELECT filename FROM uploads")
            .fetch_one(storage.db.pool())
            .await
            .unwrap();
        assert_eq!(filename, "租赁模板（填写）.docx");
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
