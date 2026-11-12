//! Allowlist, accept mode, and the inbound event list (X11-2f).
//! `POST /api/v1/inbound/events` is the development wechat bridge (X11-2g).
//!
//! Writes (`PUT` allowlist and policy) are ProductHost-only. The gate is
//! [`crate::capabilities::middleware`]'s `host_only_route`, not a check in
//! these handlers. Reads are everyday: a first-party shell may call them.
//!
//! The POST is registered on the kernel router so the route scan and the
//! operation catalog stay in step. `A24_DEV_TOOL=1` opens it in both auth
//! modes. Legacy keeps the shared bearer. Capabilities mode accepts only the
//! dev-bridge component token; anything else is 403, and the route stays 404
//! when the dev gate is off.
//!
//! Nothing here publishes on the WebSocket `/api/v1/events` hub. The list
//! returns metadata plus the first 50 characters of `text`. Full text and
//! `meta` stay in the store.
//!
//! `policy_version` is [`super::admit::policy_version`]: sha256 of the sorted
//! allowlist, the accept mode, and the admit-table version. `updated_at` is
//! not an input to that hash.
//!
//! `PUT` allowlist is last writer wins. The body is the whole list. It does
//! not carry `expected_policy_version`, and this round does not do optimistic
//! concurrency. A `(source, actor)` that is already stored keeps its
//! `added_at`; a new pair is stamped now.

use agent24_protocol::state_file::AuthMode;
use agent24_store::{
    InboundAdmissionView, InboundAllowWrite, InboundEventSummary, Store, StoreError,
    format_inbound_timestamp,
};
use axum::extract::rejection::{BytesRejection, FailedToBufferBody, JsonRejection};
use axum::extract::{Extension, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use super::admit::{AcceptMode, AllowlistEntry, Source};
use super::service::{InboundMessage, InboundService, IngestError, IngestOutcome};
use crate::server::{AppState, error_response};

/// Sources that go through admission. `scheduler` and `self` do not
/// (plan §3.4) and are not allowlist sources.
const ALLOWLIST_SOURCES: &[&str] = &["hyphae", "wechat", "remote"];
const SOURCE_ERROR: &str = "source must be hyphae, wechat, or remote";
const MAX_ACTOR_CHARS: usize = 256;
const MAX_NOTE_CHARS: usize = 500;
const MAX_ENTRIES: usize = 200;
const DEFAULT_EVENT_LIMIT: usize = 50;
const MAX_EVENT_LIMIT: usize = 100;
const TEXT_PREVIEW_CHARS: usize = 50;

pub(crate) async fn get_allowlist(
    State(state): State<AppState>,
    RawQuery(raw): RawQuery,
) -> Response {
    if let Err(message) = reject_query(raw.as_deref()) {
        return invalid(message);
    }
    match read_admission(&state).await {
        Ok(view) => allowlist_response(&view),
        Err(response) => *response,
    }
}

/// Replace the allowlist. Last writer wins: the body is the whole list,
/// there is no `expected_policy_version`, and this round does not do
/// optimistic concurrency. A `(source, actor)` already stored keeps its
/// `added_at`. A new pair is stamped now.
pub(crate) async fn put_allowlist(
    State(state): State<AppState>,
    body: Result<Json<PutAllowlist>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejected_json(rejection),
    };
    let entries = match checked_entries(&body.entries) {
        Ok(entries) => entries,
        Err(message) => return invalid(message),
    };
    let now = format_inbound_timestamp(Utc::now());
    let view = match state.store.replace_inbound_allowlist(&entries, &now).await {
        Ok(view) => view,
        Err(err) => return store_failed("replace inbound allowlist", &err),
    };
    allowlist_response(&view)
}

pub(crate) async fn get_policy(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    if let Err(message) = reject_query(raw.as_deref()) {
        return invalid(message);
    }
    match read_admission(&state).await {
        Ok(view) => policy_response(&view),
        Err(response) => *response,
    }
}

/// Replace the stored accept mode. Last writer wins: there is no
/// `expected_policy_version`, and this round does not do optimistic concurrency.
pub(crate) async fn put_policy(
    State(state): State<AppState>,
    body: Result<Json<PutPolicy>, JsonRejection>,
) -> Response {
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejected_json(rejection),
    };
    if !matches!(body.accept_mode.as_str(), "off" | "observe" | "auto") {
        return invalid("accept_mode must be off, observe, or auto");
    }
    let now = format_inbound_timestamp(Utc::now());
    let view = match state
        .store
        .set_inbound_policy(&body.accept_mode, &now)
        .await
    {
        Ok(view) => view,
        Err(err) => return store_failed("set inbound policy", &err),
    };
    policy_response(&view)
}

pub(crate) async fn get_events(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let query = match parse_event_query(raw.as_deref()) {
        Ok(query) => query,
        Err(message) => return invalid(message),
    };
    let before = query.before_at.as_deref().zip(query.before_id.as_deref());
    // One extra row tells the caller whether another page exists, without
    // selecting the full text of that row either.
    let fetched = match state
        .store
        .list_inbound_event_summaries((query.limit + 1) as i64, before)
        .await
    {
        Ok(rows) => rows,
        Err(err) => return store_failed("list inbound events", &err),
    };
    let more = fetched.len() > query.limit;
    let events: Vec<Value> = fetched
        .into_iter()
        .take(query.limit)
        .map(event_json)
        .collect();
    let next = if more {
        let last = events.last().and_then(Value::as_object);
        last.map(|row| {
            json!({
                "before_at": row.get("at").and_then(Value::as_str).unwrap_or(""),
                "before_id": row.get("id").and_then(Value::as_str).unwrap_or(""),
            })
        })
    } else {
        None
    };
    Json(json!({ "events": events, "next": next })).into_response()
}

/// Development wechat ingest. `source` is always `wechat`. A body `source`
/// other than `wechat` is 400. The handler does not copy that field through.
///
/// The route is mounted only when `dev_wechat_events` is set (`A24_DEV_TOOL=1`).
/// Legacy mode keeps the shared bearer. Capabilities mode accepts only a
/// `ComponentRuntime` token for `wechat-bridge` that holds `InboundIngest`.
pub(crate) async fn post_wechat_event(
    State(state): State<AppState>,
    authorization: Option<Extension<crate::capabilities::Authorization>>,
    RawQuery(raw): RawQuery,
    body: Result<Json<PostWechatEvent>, JsonRejection>,
) -> Response {
    if !state.dev_wechat_events {
        return error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route");
    }
    if state.auth_mode == AuthMode::Capabilities {
        let Some(Extension(authorization)) = authorization else {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing or invalid bearer token",
            );
        };
        if crate::capabilities::bridge::component_may_ingest(&authorization).is_err() {
            return error_response(StatusCode::FORBIDDEN, "forbidden", "Operation not allowed");
        }
    } else if state.auth_mode != AuthMode::LegacySingleToken {
        return error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route");
    }
    if let Err(message) = reject_query(raw.as_deref()) {
        return invalid(message);
    }
    let Json(body) = match body {
        Ok(body) => body,
        Err(rejection) => return rejected_json(rejection),
    };
    let message = match checked_wechat_event(&body) {
        Ok(message) => message,
        Err(message) => return invalid(message),
    };
    let service = InboundService::new(state.store.clone(), std::sync::Arc::clone(&state.runs));
    // `InboundService::dispatch` (service.rs) says a request handler that may
    // be dropped mid-way should spawn the call. Dropping `ingest` after the
    // insert and before the run commits leaves the row `starting`, and that
    // sender stays rate-limited until restart. `tokio::spawn` detaches when
    // the `JoinHandle` is dropped, so a client disconnect does not cancel the
    // task. Awaiting the handle is how a client that stays connected receives
    // the outcome.
    //
    // No disconnect test: `oneshot` waits until this handler returns, so it
    // cannot abort the socket after the insert commits and before `dispatch`
    // returns. Opening that window needs a live connection dropped on a
    // timer, and the pass/fail would be a race.
    let joined = tokio::spawn(async move { service.ingest(message).await }).await;
    match joined {
        Ok(Ok(outcome)) => Json(outcome_json(&body.id, outcome)).into_response(),
        Ok(Err(err)) => ingest_error_response(err),
        Err(join_err) => {
            tracing::error!(error = %join_err, "wechat inbound ingest task panicked");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        }
    }
}

/// Development allowlist seed (plan §3.4). Capabilities mode returns before
/// the list is parsed, so the variable is not applied there.
///
/// Each call syncs `wechat` rows whose note is `dev-seed` to the actors in
/// `raw`. Actors that left the list are deleted. An empty list deletes every
/// such row, and when this route is on that also logs one warning. A row
/// with any other note is left alone, including its `added_at`. A pair that
/// is already stored is not rewritten.
///
/// When the seed runs, the wechat watermark is inserted if missing. A later
/// start does not move it.
pub(crate) async fn seed_dev_allowlist(
    store: &Store,
    auth_mode: AuthMode,
    dev_tool: bool,
    raw: &str,
) -> Result<usize, Box<StoreError>> {
    if auth_mode != AuthMode::LegacySingleToken || !dev_tool {
        return Ok(0);
    }
    let now = format_inbound_timestamp(Utc::now());
    let mut actors = Vec::new();
    for piece in raw.split(',') {
        let piece = piece.trim();
        if piece.is_empty() {
            continue;
        }
        let Some(actor) = piece.strip_prefix("wechat:") else {
            tracing::warn!("skipping inbound dev seed entry that is not wechat:<uid>");
            continue;
        };
        let actor: String = actor.nfc().collect();
        if !valid_actor(&actor) {
            tracing::warn!("skipping inbound dev seed actor that failed validation");
            continue;
        }
        if actors.iter().any(|have: &String| have == &actor) {
            continue;
        }
        actors.push(actor);
    }
    if actors.is_empty() {
        tracing::warn!("开发态微信入站已开但名单为空");
    }
    let inserted = store
        .sync_wechat_dev_seed_allowlist(&actors, &now)
        .await
        .map_err(Box::new)?;
    let max_age = i64::try_from(super::service::INBOUND_MAX_AGE_SECS).unwrap_or(600);
    store
        .ensure_inbound_source_watermark("wechat", &now, max_age)
        .await
        .map_err(Box::new)?;
    Ok(inserted)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutAllowlist {
    entries: Vec<AllowEntryIn>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AllowEntryIn {
    source: String,
    actor: String,
    #[serde(default)]
    note: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PutPolicy {
    accept_mode: String,
}

/// Body of `POST /api/v1/inbound/events`. `source` is optional only so a
/// caller who sends one can be rejected. The route always ingests `wechat`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PostWechatEvent {
    id: String,
    #[serde(default)]
    source: Option<String>,
    actor: String,
    at: String,
    #[serde(default)]
    text: Option<String>,
}

fn checked_wechat_event(body: &PostWechatEvent) -> Result<InboundMessage, &'static str> {
    if let Some(source) = body.source.as_deref()
        && source != "wechat"
    {
        return Err("source is fixed to wechat");
    }
    if !valid_wechat_event_id(&body.id) {
        return Err("id must look like wechat:<message_id>");
    }
    let actor: String = body.actor.nfc().collect();
    if !valid_actor(&actor) {
        return Err("actor must be 1 to 256 characters without control or format characters");
    }
    if !canonical_stamp(&body.at) {
        return Err("at must be YYYY-MM-DDTHH:MM:SSZ");
    }
    let text = match body.text.as_deref() {
        None | Some("") => None,
        Some(text) => Some(text.to_owned()),
    };
    Ok(InboundMessage {
        id: body.id.clone(),
        // Fixed here. Copying `body.source` would let a caller pick hyphae.
        source: "wechat".to_owned(),
        category: "dev_bridge".to_owned(),
        recipient_identity: None,
        thread_key: None,
        actor: Some(actor),
        human: true,
        addressed: "direct".to_owned(),
        at: body.at.clone(),
        text,
        meta: None,
    })
}

/// `wechat:` plus a decimal message id. One colon, no empty suffix.
fn valid_wechat_event_id(id: &str) -> bool {
    let Some(native) = id.strip_prefix("wechat:") else {
        return false;
    };
    !native.is_empty() && native.len() <= 32 && native.bytes().all(|byte| byte.is_ascii_digit())
}

fn outcome_json(id: &str, outcome: IngestOutcome) -> Value {
    match outcome {
        IngestOutcome::Duplicate => json!({"id": id, "outcome": "duplicate"}),
        IngestOutcome::Ignored { reason } => {
            json!({"id": id, "outcome": "ignored", "reason": reason})
        }
        IngestOutcome::Started { run_id } => {
            json!({"id": id, "outcome": "started", "run_id": run_id})
        }
        IngestOutcome::AlreadyLinked { run_id } => {
            let mut body = json!({"id": id, "outcome": "already_linked"});
            // `None` and "" are not a run id. Omit the field; do not send null.
            if let Some(run_id) = run_id.filter(|run_id| !run_id.is_empty()) {
                body["run_id"] = json!(run_id);
            }
            body
        }
    }
}

/// Store and start failures are logged. The body never includes the store
/// text or the prompt (E4). A start failure is `internal error`. A store
/// failure stays the generic `storage error` the other daemon routes use.
fn ingest_error_response(err: IngestError) -> Response {
    match err {
        IngestError::Invalid(message) => invalid(&message),
        IngestError::NotInbound => invalid("not an inbound event"),
        IngestError::Store(err) => store_failed("ingest wechat event", &err),
        IngestError::Start(detail) => {
            tracing::error!(error = %detail, "wechat inbound start failed");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "internal error",
            )
        }
    }
}

struct EventQuery {
    limit: usize,
    before_at: Option<String>,
    before_id: Option<String>,
}

fn invalid(message: &str) -> Response {
    error_response(StatusCode::BAD_REQUEST, "invalid_request", message)
}

/// Axum's `Json` extractor reports a body over `DefaultBodyLimit` as
/// [`FailedToBufferBody::LengthLimitError`] (413). Every other rejection
/// stays 400: syntax, unknown fields, and a failed read that is not the cap.
fn rejected_json(rejection: JsonRejection) -> Response {
    let too_large = matches!(
        rejection,
        JsonRejection::BytesRejection(BytesRejection::FailedToBufferBody(
            FailedToBufferBody::LengthLimitError(_)
        ))
    );
    if too_large {
        return error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            "Request body exceeds limit",
        );
    }
    invalid("Malformed request")
}

/// Caller-shaped store refusals are 4xx. The store text is logged and is
/// not copied into the body: a conflict repeats the rejected value. Every
/// other variant is an unexpected storage failure and answers with the same
/// generic 500 (`internal` / `storage error`) the other daemon routes use.
fn store_failed(what: &str, err: &agent24_store::StoreError) -> Response {
    match err {
        agent24_store::StoreError::NotFound(_) => {
            tracing::warn!(error = %err, "{what}");
            error_response(StatusCode::NOT_FOUND, "not_found", "not found")
        }
        agent24_store::StoreError::Conflict(_) => {
            tracing::warn!(error = %err, "{what}");
            error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "invalid inbound request",
            )
        }
        other => {
            tracing::error!(error = %other, "{what}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
    }
}

/// `Box<Response>`: clippy `result_large_err` (1.98+, denied in CI) flags a
/// `Response`-sized `Err` on an `async fn`. One `Result` in this file.
async fn read_admission(state: &AppState) -> Result<InboundAdmissionView, Box<Response>> {
    match state.store.read_inbound_admission().await {
        Ok(view) => Ok(view),
        Err(err) => Err(Box::new(store_failed("read inbound admission", &err))),
    }
}

fn allowlist_response(view: &InboundAdmissionView) -> Response {
    let Some(version) = admission_version(view) else {
        return broken_admission();
    };
    let items: Vec<Value> = view
        .entries
        .iter()
        .map(|entry| {
            json!({
                "source": entry.source,
                "actor": entry.actor,
                "added_at": entry.added_at,
                "note": entry.note,
            })
        })
        .collect();
    Json(json!({ "entries": items, "policy_version": version })).into_response()
}

fn policy_response(view: &InboundAdmissionView) -> Response {
    let Some(version) = admission_version(view) else {
        return broken_admission();
    };
    Json(json!({
        "accept_mode": view.accept_mode,
        "updated_at": view.updated_at,
        "policy_version": version,
    }))
    .into_response()
}

fn broken_admission() -> Response {
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "inbound store failed",
    )
}

fn admission_version(view: &InboundAdmissionView) -> Option<String> {
    let mode = AcceptMode::from_wire(&view.accept_mode).or_else(|| {
        tracing::error!(mode = %view.accept_mode, "inbound accept_mode is not a wire value");
        None
    })?;
    let mut entries = Vec::with_capacity(view.entries.len());
    for entry in &view.entries {
        let source = Source::from_wire(&entry.source).or_else(|| {
            tracing::error!(source = %entry.source, "inbound allowlist source is not a wire value");
            None
        })?;
        entries.push(AllowlistEntry {
            source,
            actor: entry.actor.clone(),
        });
    }
    Some(super::admit::policy_version(&entries, mode))
}

fn checked_entries(entries: &[AllowEntryIn]) -> Result<Vec<InboundAllowWrite>, &'static str> {
    if entries.len() > MAX_ENTRIES {
        return Err("allowlist has too many entries");
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::with_capacity(entries.len());
    for entry in entries {
        // NFC before the duplicate check, so `e` + U+0301 and U+00E9 are one actor.
        let source: String = entry.source.nfc().collect();
        let actor: String = entry.actor.nfc().collect();
        if !ALLOWLIST_SOURCES.contains(&source.as_str()) {
            return Err(SOURCE_ERROR);
        }
        if !valid_actor(&actor) {
            return Err("actor must be 1 to 256 characters without control or format characters");
        }
        if !seen.insert((source.clone(), actor.clone())) {
            return Err("duplicate source and actor");
        }
        let note = match entry.note.as_deref() {
            None => None,
            Some(note) if has_control_or_format(note) => {
                return Err("note must not contain control or format characters");
            }
            Some(note) if note.chars().count() > MAX_NOTE_CHARS => {
                return Err("note is too long");
            }
            Some(note) => {
                let trimmed = note.trim();
                if trimmed.is_empty() {
                    None
                } else {
                    Some(trimmed.to_owned())
                }
            }
        };
        out.push(InboundAllowWrite {
            source,
            actor,
            note,
        });
    }
    Ok(out)
}

/// Cc (including C1 such as U+0085) and Cf. `is_ascii_control` would let
/// U+0085 through.
fn has_control_or_format(text: &str) -> bool {
    text.chars()
        .any(|ch| ch.is_control() || get_general_category(ch) == GeneralCategory::Format)
}

fn valid_actor(actor: &str) -> bool {
    let count = actor.chars().count();
    (1..=MAX_ACTOR_CHARS).contains(&count)
        && actor == actor.trim()
        && !has_control_or_format(actor)
        // U+2028 / U+2029 are line separators (Zl / Zp), not Cc or Cf.
        && !actor.chars().any(|ch| matches!(ch, '\u{2028}' | '\u{2029}'))
}

fn parse_event_query(raw: Option<&str>) -> Result<EventQuery, &'static str> {
    let mut limit: Option<usize> = None;
    let mut before_at: Option<String> = None;
    let mut before_id: Option<String> = None;
    for (key, value) in form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        match key.as_ref() {
            "limit" => {
                if limit.is_some() {
                    return Err("at most one limit query parameter is allowed");
                }
                let parsed: usize = value
                    .parse()
                    .map_err(|_| "limit must be an integer from 1 to 100")?;
                if !(1..=MAX_EVENT_LIMIT).contains(&parsed) {
                    return Err("limit must be an integer from 1 to 100");
                }
                limit = Some(parsed);
            }
            "before_at" => {
                let fresh = value.into_owned();
                if before_at.is_some() {
                    return Err("at most one before_at query parameter is allowed");
                }
                before_at = Some(fresh);
            }
            "before_id" => {
                let fresh = value.into_owned();
                if before_id.is_some() {
                    return Err("at most one before_id query parameter is allowed");
                }
                before_id = Some(fresh);
            }
            _ => return Err("unknown query parameter"),
        }
    }
    match (&before_at, &before_id) {
        (None, None) => {}
        (Some(at), Some(_)) if canonical_stamp(at) => {}
        (Some(_), Some(_)) => {
            return Err("before_at must be YYYY-MM-DDTHH:MM:SSZ");
        }
        _ => return Err("before_at and before_id must be sent together"),
    }
    Ok(EventQuery {
        limit: limit.unwrap_or(DEFAULT_EVENT_LIMIT),
        before_at,
        before_id,
    })
}

fn canonical_stamp(value: &str) -> bool {
    DateTime::parse_from_rfc3339(value)
        .ok()
        .map(|dt| format_inbound_timestamp(dt.with_timezone(&Utc)))
        .as_deref()
        == Some(value)
}

fn reject_query(raw: Option<&str>) -> Result<(), &'static str> {
    if form_urlencoded::parse(raw.unwrap_or("").as_bytes())
        .next()
        .is_some()
    {
        Err("unknown query parameter")
    } else {
        Ok(())
    }
}

fn event_json(row: InboundEventSummary) -> Value {
    json!({
        "id": row.id,
        "source": row.source,
        "category": row.category,
        "recipient_identity": row.recipient_identity,
        "thread_key": row.thread_key,
        "actor": row.actor,
        "human": row.human,
        "addressed": row.addressed,
        "at": row.at,
        "received_at": row.received_at,
        "text_preview": row.text_preview.as_deref().map(clip_preview),
        "text_scrubbed_at": row.text_scrubbed_at,
        "disposition": row.disposition,
        "disposition_reason": row.disposition_reason,
        "policy_version": row.policy_version,
        "run_state": row.run_state,
    })
}

fn clip_preview(text: &str) -> String {
    text.chars().take(TEXT_PREVIEW_CHARS).collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    #[test]
    fn allowlist_sources_match_the_agent_inbound_channels() {
        // Mutation: add a name to either set and the two lists diverge.
        let mut routes = ALLOWLIST_SOURCES.to_vec();
        let mut agent = agent24_agent::RunManager::INBOUND_SOURCE_CHANNELS.to_vec();
        routes.sort_unstable();
        agent.sort_unstable();
        assert_eq!(routes, agent);
    }

    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;
    use std::sync::Arc;
    use std::time::Duration as StdDuration;

    use agent24_models::router::{ModelRouter, Tier};
    use agent24_models::{CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg};
    use agent24_protocol::state_file::AuthMode;
    use agent24_store::{SourceClassification, SourceKind, format_inbound_timestamp};
    use async_trait::async_trait;
    use chrono::Utc;

    use crate::enroll_api::EnrollAuthority;
    use crate::server::tests::{capability_state, state, state_with_router};

    async fn json_of(res: axum::response::Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        }
    }

    async fn call(
        app: &axum::Router,
        method: &str,
        uri: &str,
        bearer: &str,
        body: Option<&str>,
    ) -> (StatusCode, Value) {
        let res = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body.unwrap_or("").to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        (status, json_of(res).await)
    }

    fn shell_bearer(app_state: &AppState) -> String {
        let authority = app_state.first_party.as_ref().unwrap();
        EnrollAuthority::issue(
            authority.as_ref(),
            &crate::capabilities::first_party::test_peer("daemon-test"),
        )
        .unwrap()
        .secret
        .to_string()
    }

    fn error_code(body: &Value) -> &str {
        body["error"]["code"].as_str().unwrap_or("")
    }

    async fn seed_long_event(store: &agent24_store::Store) {
        let mut event = agent24_store::NewInboundEvent {
            id: "hyphae:long".into(),
            source: "hyphae".into(),
            category: "direct".into(),
            recipient_identity: Some("me".into()),
            thread_key: None,
            actor: Some("npub1".into()),
            human: true,
            addressed: "direct".into(),
            at: "2026-11-11T03:00:00Z".into(),
            received_at: "2026-11-11T03:00:01Z".into(),
            text: Some(format!("{}SECRET-TAIL", "你".repeat(50))),
            meta: Some("META-SECRET".into()),
            disposition: "ignore".into(),
            disposition_reason: Some("not_allowlisted".into()),
            policy_version: Some("a".repeat(64)),
            run_state: None,
        };
        assert_eq!(
            store.insert_inbound_event(&event).await.unwrap(),
            agent24_store::InboundInsert::Inserted
        );
        event.id = "hyphae:older".into();
        event.at = "2026-11-11T02:00:00Z".into();
        event.text = Some("older-body".into());
        event.meta = Some("older-meta".into());
        assert_eq!(
            store.insert_inbound_event(&event).await.unwrap(),
            agent24_store::InboundInsert::Inserted
        );
    }

    /// Host-only refusal. Moving either PUT out of `host_only_route` turns
    /// this red: the shell then gets `presence_required` instead of
    /// `forbidden` / "Operation not allowed".
    #[tokio::test]
    async fn inbound_shell_without_elevation_cannot_write_the_allowlist_or_policy() {
        let (app_state, _host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        for (uri, body) in [
            (
                "/api/v1/inbound/allowlist",
                r#"{"entries":[{"source":"hyphae","actor":"npub1"}]}"#,
            ),
            ("/api/v1/inbound/policy", r#"{"accept_mode":"auto"}"#),
        ] {
            let (status, json) = call(&app, "PUT", uri, &shell, Some(body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} {json}");
            assert_eq!(error_code(&json), "forbidden", "{uri} {json}");
            assert_eq!(json["error"]["message"], "Operation not allowed", "{uri}");
        }
    }

    /// Legacy shared token. Moving either PUT out of `host_only_route` turns
    /// this red: the handler then runs and answers 200.
    #[tokio::test]
    async fn inbound_legacy_token_cannot_write_the_allowlist_or_policy() {
        let app = crate::server::build_router(state().await);
        for (uri, body) in [
            (
                "/api/v1/inbound/allowlist",
                r#"{"entries":[{"source":"wechat","actor":"uid-1"}]}"#,
            ),
            ("/api/v1/inbound/policy", r#"{"accept_mode":"off"}"#),
        ] {
            let (status, json) = call(&app, "PUT", uri, "testtoken", Some(body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} {json}");
            assert_eq!(error_code(&json), "forbidden", "{uri}");
            assert_eq!(
                json["error"]["message"], "Operation not allowed in this auth mode",
                "{uri}"
            );
        }
        let (status, json) =
            call(&app, "GET", "/api/v1/inbound/allowlist", "testtoken", None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn inbound_product_host_writes_allowlist_and_policy() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(r#"{"entries":[{"source":"hyphae","actor":"npub1","note":" desk "},{"source":"wechat","actor":"uid-9"}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 2);
        assert_eq!(json["entries"][0]["source"], "hyphae");
        assert_eq!(json["entries"][0]["note"], "desk");
        let written = json["policy_version"].as_str().unwrap().to_owned();
        assert!(is_policy_version(&written), "{written}");

        let (status, read) = call(&app, "GET", "/api/v1/inbound/allowlist", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{read}");
        assert_eq!(read["entries"], json["entries"]);
        assert_eq!(read["policy_version"], written);

        let (status, policy) = call(
            &app,
            "PUT",
            "/api/v1/inbound/policy",
            &host,
            Some(r#"{"accept_mode":"auto"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{policy}");
        assert_eq!(policy["accept_mode"], "auto");
        assert!(policy["updated_at"].as_str().unwrap().ends_with('Z'));
        let changed = policy["policy_version"].as_str().unwrap();
        assert_ne!(changed, written);

        let (status, again) = call(
            &app,
            "PUT",
            "/api/v1/inbound/policy",
            &host,
            Some(r#"{"accept_mode":"auto"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{again}");
        assert_eq!(again["policy_version"], changed);

        let (status, seen) = call(&app, "GET", "/api/v1/inbound/policy", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{seen}");
        assert_eq!(seen["accept_mode"], "auto");
        assert_eq!(seen["policy_version"], changed);
    }

    #[tokio::test]
    async fn inbound_event_list_omits_the_full_text_and_does_not_publish() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        seed_long_event(&app_state.store).await;
        let mut rx = app_state.events.subscribe();
        app_state
            .events
            .broadcast(agent24_protocol::EventBody::RunCancelled(
                agent24_protocol::RunCancelledPayload {
                    run_id: "run_probe".into(),
                },
            ));
        match rx.try_recv() {
            Ok((_, agent24_protocol::EventBody::RunCancelled(body))) => {
                assert_eq!(body.run_id, "run_probe");
            }
            other => panic!("events bus did not deliver the probe: {other:?}"),
        }
        let app = crate::server::build_router(app_state);
        let (status, json) = call(&app, "GET", "/api/v1/inbound/events", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        let rendered = json.to_string();
        assert!(!rendered.contains("SECRET-TAIL"), "{rendered}");
        assert!(!rendered.contains("META-SECRET"), "{rendered}");
        assert!(!rendered.contains("older-meta"), "{rendered}");
        let events = json["events"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["id"], "hyphae:long");
        assert_eq!(
            events[0]["text_preview"].as_str().unwrap().chars().count(),
            50
        );
        assert!(json["next"].is_null());
        assert!(rx.try_recv().is_err());

        let (status, page) = call(&app, "GET", "/api/v1/inbound/events?limit=1", &host, None).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert_eq!(page["events"].as_array().unwrap().len(), 1);
        let before_at = page["next"]["before_at"].as_str().unwrap();
        let before_id = page["next"]["before_id"].as_str().unwrap();
        let (status, rest) = call(
            &app,
            "GET",
            &format!("/api/v1/inbound/events?limit=1&before_at={before_at}&before_id={before_id}"),
            &shell,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{rest}");
        assert_eq!(rest["events"][0]["id"], "hyphae:older");
        assert_eq!(rest["events"][0]["text_preview"], "older-body");
        assert!(rest["next"].is_null());

        let (status, bad) = call(
            &app,
            "GET",
            "/api/v1/inbound/events?limit=101",
            &shell,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{bad}");
        assert_eq!(error_code(&bad), "invalid_request");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn inbound_shell_reads_an_unset_policy_as_observe() {
        let (app_state, _host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        let (status, json) = call(&app, "GET", "/api/v1/inbound/policy", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["accept_mode"], "observe");
        assert!(json["updated_at"].is_null());
        assert!(is_policy_version(json["policy_version"].as_str().unwrap()));
    }

    /// Each `return Err` in `checked_entries` and `parse_event_query` has a
    /// case here, except the actor check. That one is locked by
    /// `unknown_fields_format_actors_and_oversize_bodies_are_refused`.
    /// `limit=nope` locks the parse `?`. Dropping the return turns this red.
    #[tokio::test]
    async fn inbound_field_and_query_guards_reject_before_the_store() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        let mut too_many = String::from(r#"{"entries":["#);
        for i in 0..201 {
            if i > 0 {
                too_many.push(',');
            }
            too_many.push_str(&format!(r#"{{"source":"hyphae","actor":"npub{i}"}}"#));
        }
        too_many.push_str("]}");
        let long_note = "n".repeat(501);
        let long_body = format!(
            r#"{{"entries":[{{"source":"hyphae","actor":"npub1","note":"{long_note}"}}]}}"#
        );
        let writes = [
            (too_many.as_str(), "allowlist has too many entries"),
            (
                r#"{"entries":[{"source":"email","actor":"npub1"}]}"#,
                SOURCE_ERROR,
            ),
            (
                r#"{"entries":[{"source":"scheduler","actor":"npub1"}]}"#,
                SOURCE_ERROR,
            ),
            (
                r#"{"entries":[{"source":"self","actor":"npub1"}]}"#,
                SOURCE_ERROR,
            ),
            (
                r#"{"entries":[{"source":"hyphae","actor":"npub1"},{"source":"hyphae","actor":"npub1"}]}"#,
                "duplicate source and actor",
            ),
            (
                "{\"entries\":[{\"source\":\"hyphae\",\"actor\":\"npub1\",\"note\":\"a\\nb\"}]}",
                "note must not contain control or format characters",
            ),
            (
                "{\"entries\":[{\"source\":\"hyphae\",\"actor\":\"npub1\",\"note\":\"a\\u0085b\"}]}",
                "note must not contain control or format characters",
            ),
            (
                "{\"entries\":[{\"source\":\"hyphae\",\"actor\":\"npub1\",\"note\":\"a\\u200Bb\"}]}",
                "note must not contain control or format characters",
            ),
            (long_body.as_str(), "note is too long"),
        ];
        for (body, message) in writes {
            let (status, json) =
                call(&app, "PUT", "/api/v1/inbound/allowlist", &host, Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{message} {json}");
            assert_eq!(error_code(&json), "invalid_request", "{message}");
            assert_eq!(json["error"]["message"], message, "{json}");
        }
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/policy",
            &host,
            Some(r#"{"accept_mode":"sometimes"}"#),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(
            json["error"]["message"], "accept_mode must be off, observe, or auto",
            "{json}"
        );
        let (status, json) = call(&app, "GET", "/api/v1/inbound/allowlist", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 0);

        let queries = [
            ("limit=0", "limit must be an integer from 1 to 100"),
            ("limit=101", "limit must be an integer from 1 to 100"),
            ("limit=nope", "limit must be an integer from 1 to 100"),
            (
                "limit=1&limit=2",
                "at most one limit query parameter is allowed",
            ),
            (
                "before_at=2026-11-11T00:00:00Z",
                "before_at and before_id must be sent together",
            ),
            (
                "before_id=hyphae:1",
                "before_at and before_id must be sent together",
            ),
            (
                "before_at=yesterday&before_id=hyphae:1",
                "before_at must be YYYY-MM-DDTHH:MM:SSZ",
            ),
            (
                "before_at=2026-11-11T00:00:00Z&before_at=2026-11-11T00:00:00Z&before_id=hyphae:1",
                "at most one before_at query parameter is allowed",
            ),
            (
                "before_at=2026-11-11T00:00:00Z&before_id=hyphae:1&before_id=hyphae:2",
                "at most one before_id query parameter is allowed",
            ),
            ("limt=5", "unknown query parameter"),
        ];
        for (query, message) in queries {
            let (status, json) = call(
                &app,
                "GET",
                &format!("/api/v1/inbound/events?{query}"),
                &shell,
                None,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query} {json}");
            assert_eq!(json["error"]["message"], message, "{query} {json}");
        }
        for uri in [
            "/api/v1/inbound/allowlist?limt=5",
            "/api/v1/inbound/policy?limt=5",
        ] {
            let (status, json) = call(&app, "GET", uri, &shell, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {json}");
            assert_eq!(error_code(&json), "invalid_request", "{uri}");
            assert_eq!(json["error"]["message"], "unknown query parameter", "{uri}");
        }
    }

    fn is_policy_version(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    }

    /// Dropping the `Conflict` arm (so the store text falls through to the
    /// 500 body) turns this red.
    #[tokio::test]
    async fn inbound_store_conflict_is_400_without_the_store_text() {
        let secret = "/var/folders/inbound-conflict.sqlite";
        let err = agent24_store::StoreError::Conflict(format!(
            "inbound added_at must be YYYY-MM-DDTHH:MM:SSZ, got {secret}"
        ));
        let response = store_failed("replace inbound allowlist", &err);
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let rendered = json_of(response).await.to_string();
        assert!(rendered.contains("invalid inbound request"), "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(!rendered.contains("added_at"), "{rendered}");
    }

    /// Dropping the `NotFound` arm turns this red.
    #[tokio::test]
    async fn inbound_store_not_found_is_404_without_the_store_text() {
        let secret = "/var/folders/inbound-missing.sqlite";
        let err = agent24_store::StoreError::NotFound(secret.to_owned());
        let response = store_failed("read inbound admission", &err);
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let rendered = json_of(response).await.to_string();
        assert!(rendered.contains("\"not_found\""), "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
    }

    /// A trigger abort and a closed pool both go through `store_failed`.
    /// Putting `err` into the 500 body turns this red.
    #[tokio::test]
    async fn inbound_store_failure_is_a_generic_500() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let store = app_state.store.clone();
        let db_path = agent24_store::test_hooks::pool(&store)
            .connect_options()
            .get_filename()
            .to_string_lossy()
            .into_owned();
        let secret = "private-trigger-detail-/var/folders/inbound-secret.sqlite";
        sqlx::query(&format!(
            "CREATE TRIGGER inbound_allowlist_fail BEFORE INSERT ON inbound_allowlist \
             BEGIN SELECT RAISE(ABORT, '{secret}'); END"
        ))
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        let app = crate::server::build_router(app_state);
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(r#"{"entries":[{"source":"hyphae","actor":"npub1"}]}"#),
        )
        .await;
        let rendered = json.to_string();
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{rendered}");
        assert_eq!(error_code(&json), "internal", "{rendered}");
        assert_eq!(json["error"]["message"], "storage error", "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
        assert!(!rendered.contains(&db_path), "{rendered}");
        assert!(!rendered.contains("inbound_allowlist"), "{rendered}");

        agent24_store::test_hooks::pool(&store).close().await;
        for (method, uri, bearer) in [
            ("GET", "/api/v1/inbound/allowlist", shell.as_str()),
            ("GET", "/api/v1/inbound/policy", shell.as_str()),
            ("GET", "/api/v1/inbound/events", shell.as_str()),
            ("PUT", "/api/v1/inbound/policy", host.as_str()),
        ] {
            let (status, json) = call(
                &app,
                method,
                uri,
                bearer,
                (method == "PUT").then_some(r#"{"accept_mode":"auto"}"#),
            )
            .await;
            let rendered = json.to_string();
            assert_eq!(
                status,
                StatusCode::INTERNAL_SERVER_ERROR,
                "{method} {uri} {rendered}"
            );
            assert_eq!(error_code(&json), "internal", "{method} {uri} {rendered}");
            assert_eq!(
                json["error"]["message"], "storage error",
                "{method} {uri} {rendered}"
            );
            assert!(
                !rendered.contains("closed pool"),
                "{method} {uri} {rendered}"
            );
            assert!(!rendered.contains(&db_path), "{method} {uri} {rendered}");
            assert!(!rendered.contains(secret), "{method} {uri} {rendered}");
        }
    }

    #[test]
    fn actor_rejects_controls_and_unicode_format_characters() {
        assert!(valid_actor("npub1"));
        assert!(valid_actor(&"a".repeat(256)));
        assert!(!valid_actor(""));
        assert!(!valid_actor(" npub1"));
        assert!(!valid_actor("npub1 "));
        assert!(!valid_actor("   "));
        assert!(!valid_actor("\u{00A0}npub1"));
        assert!(!valid_actor(&"a".repeat(257)));
        assert!(!valid_actor("a\nb"));
        // U+0085 is Cc but not an ASCII control. `is_ascii_control` turns this red.
        assert!(!valid_actor("a\u{0085}b"));
        assert!(!valid_actor("a\u{200B}b"));
        assert!(!valid_actor("a\u{200C}b"));
        assert!(!valid_actor("a\u{200D}b"));
        assert!(!valid_actor("a\u{202E}b"));
        assert!(!valid_actor("a\u{2066}b"));
        assert!(!valid_actor("a\u{FEFF}b"));
        assert!(!valid_actor("a\u{2028}b"));
        assert!(!valid_actor("a\u{2029}b"));
    }

    /// `source` is the three external sources that go through admission.
    /// `scheduler` and `self` are refused. `actor` is `valid_actor`. Empty,
    /// surrounding whitespace, over-long, controls, Unicode format (Cf), and
    /// U+2028 / U+2029 never reach the store: the kept row is still there
    /// afterwards. Dropping the allowlist-source check or the `valid_actor`
    /// return turns the matching assertion red.
    #[tokio::test]
    async fn source_and_actor_shape_is_rejected_before_the_store() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(r#"{"entries":[{"source":"hyphae","actor":"npub-keep"}]}"#),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");

        let source_msg = SOURCE_ERROR;
        let actor_msg = "actor must be 1 to 256 characters without control or format characters";
        let long_source = "s".repeat(257);
        let long_actor = "a".repeat(257);
        let cases = [
            (
                json!({ "entries": [{ "source": "", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": " hyphae", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae ", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": " ", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": &long_source, "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae\n", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae\u{0001}", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae\u{200B}", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "\u{200B}hyphae", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "Hyphae", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "scheduler", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "self", "actor": "npub1" }] }),
                source_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": " npub1" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "npub1 " }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "   " }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "\u{00A0}npub1" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": &long_actor }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\nb" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\u{0001}b" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\u{0085}b" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\u{2028}b" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\u{2029}b" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\u{200C}b" }] }),
                actor_msg,
            ),
            (
                json!({ "entries": [{ "source": "hyphae", "actor": "a\u{FEFF}b" }] }),
                actor_msg,
            ),
        ];
        for (body, message) in cases {
            let raw = body.to_string();
            let (status, json) =
                call(&app, "PUT", "/api/v1/inbound/allowlist", &host, Some(&raw)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{message} {json} {raw}");
            assert_eq!(error_code(&json), "invalid_request", "{message} {json}");
            assert_eq!(json["error"]["message"], message, "{raw} {json}");
        }
        let (status, json) = call(&app, "GET", "/api/v1/inbound/allowlist", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 1, "{json}");
        assert_eq!(json["entries"][0]["actor"], "npub-keep", "{json}");

        let max_actor = "b".repeat(256);
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(&json!({ "entries": [{ "source": "hyphae", "actor": max_actor }] }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"][0]["actor"], max_actor, "{json}");
    }

    #[tokio::test]
    async fn shell_elevation_header_and_creative_token_cannot_write() {
        let (app_state, _host, creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        for (uri, body) in [
            (
                "/api/v1/inbound/allowlist",
                r#"{"entries":[{"source":"hyphae","actor":"npub1"}]}"#,
            ),
            ("/api/v1/inbound/policy", r#"{"accept_mode":"auto"}"#),
        ] {
            let (status, json) = call_with(
                &app,
                "PUT",
                uri,
                &shell,
                Some(body),
                &[("X-Agent24-Elevation", "not-a-grant")],
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} {json}");
            assert_eq!(error_code(&json), "forbidden", "{uri}");
            assert_eq!(json["error"]["message"], "Operation not allowed", "{uri}");

            let (status, json) = call(&app, "PUT", uri, &creative, Some(body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} {json}");
            assert_eq!(error_code(&json), "forbidden", "{uri}");
            assert_eq!(json["error"]["message"], "Operation not allowed", "{uri}");
        }
        let (status, json) = call(&app, "GET", "/api/v1/inbound/allowlist", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn disguised_write_paths_do_not_reach_the_handler() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        let body = r#"{"entries":[{"source":"hyphae","actor":"npub1"}]}"#;
        for uri in [
            "/api/v1/inbound/allowlist/",
            "/api/v1/inbound/%61llowlist",
            "/api/v1/inbound/Allowlist",
            "/api/v1/inbound//allowlist",
            "/api/v1//inbound/allowlist",
        ] {
            let (status, json) = call(&app, "PUT", uri, &host, Some(body)).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{uri} {json}");
            assert_eq!(error_code(&json), "not_found", "{uri}");
        }
        for uri in [
            "/api/v1/inbound/allowlist?x=1",
            "/api/v1/inbound/policy?x=1",
        ] {
            let (status, json) = call(&app, "PUT", uri, &shell, Some(body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{uri} {json}");
            assert_eq!(error_code(&json), "forbidden", "{uri}");
            assert_eq!(json["error"]["message"], "Operation not allowed", "{uri}");
        }
        for (method, uri) in [
            ("POST", "/api/v1/inbound/allowlist"),
            ("PATCH", "/api/v1/inbound/allowlist"),
            ("POST", "/api/v1/inbound/policy"),
            ("PATCH", "/api/v1/inbound/policy"),
        ] {
            let status = call_status(&app, method, uri, &shell).await;
            assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn unknown_fields_format_actors_and_oversize_bodies_are_refused() {
        let (app_state, host, _creative) = capability_state().await;
        let app = crate::server::build_router(app_state);
        for (uri, body) in [
            (
                "/api/v1/inbound/allowlist",
                r#"{"entries":[],"extra":true}"#,
            ),
            (
                "/api/v1/inbound/allowlist",
                r#"{"entries":[{"source":"hyphae","actor":"npub1","nots":"x"}]}"#,
            ),
            (
                "/api/v1/inbound/policy",
                r#"{"accept_mode":"observe","extra":true}"#,
            ),
            (
                "/api/v1/inbound/allowlist",
                "{\"entries\":[{\"source\":\"hyphae\",\"actor\":\"a\\u200Bb\"}]}",
            ),
            (
                "/api/v1/inbound/allowlist",
                "{\"entries\":[{\"source\":\"hyphae\",\"actor\":\"a\\u202Eb\"}]}",
            ),
            ("/api/v1/inbound/policy", "{\"accept_mode\""),
        ] {
            let (status, json) = call(&app, "PUT", uri, &host, Some(body)).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri} {body} {json}");
            assert_eq!(error_code(&json), "invalid_request", "{uri}");
        }
        // axum `DefaultBodyLimit` is 2 MiB. One byte over is `LengthLimitError`,
        // which must not collapse into the malformed-JSON 400.
        let oversized = "x".repeat(2_097_152 + 1);
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/policy",
            &host,
            Some(&oversized),
        )
        .await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE, "{json}");
        assert_eq!(error_code(&json), "payload_too_large");
    }

    /// `MAX_ENTRIES` is 200. Raising it lets 201 through; lowering it rejects 200.
    #[tokio::test]
    async fn allowlist_accepts_two_hundred_entries_and_rejects_two_hundred_one() {
        let (app_state, host, _creative) = capability_state().await;
        let shell = shell_bearer(&app_state);
        let app = crate::server::build_router(app_state);
        let sources = ["hyphae", "wechat", "remote"];
        let mut entries = Vec::new();
        for i in 0..200 {
            entries.push(json!({
                "source": sources[i % 3],
                "actor": format!("actor-{i}"),
            }));
        }
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(&json!({ "entries": entries }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 200, "{json}");

        entries.push(json!({ "source": "hyphae", "actor": "actor-200" }));
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(&json!({ "entries": entries }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["error"]["message"], "allowlist has too many entries");
        let (status, json) = call(&app, "GET", "/api/v1/inbound/allowlist", &shell, None).await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"].as_array().unwrap().len(), 200, "{json}");
    }

    /// NFC before dedup: `e` + U+0301 and U+00E9 are one actor. A single NFD
    /// actor is stored as NFC. Skipping `.nfc()` turns both assertions red.
    #[tokio::test]
    async fn allowlist_normalizes_to_nfc_before_dedup() {
        let (app_state, host, _creative) = capability_state().await;
        let app = crate::server::build_router(app_state);
        let nfd = "e\u{0301}";
        let nfc = "\u{00e9}";
        assert_ne!(nfd, nfc);
        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(
                &json!({
                    "entries": [
                        {"source": "hyphae", "actor": nfd},
                        {"source": "hyphae", "actor": nfc},
                    ]
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["error"]["message"], "duplicate source and actor");

        let (status, json) = call(
            &app,
            "PUT",
            "/api/v1/inbound/allowlist",
            &host,
            Some(&json!({ "entries": [{ "source": "hyphae", "actor": nfd }] }).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["entries"][0]["actor"], nfc, "{json}");
    }

    /// The store preview here is already longer than 50, so `substr` is not
    /// what caps it. Dropping `clip_preview` in `event_json` turns this red.
    /// The store test locks the SQL `substr(..., 50)` on its own.
    #[test]
    fn route_clips_an_overlong_preview_the_store_did_not() {
        let overlong = format!("{}SECRET-TAIL", "你".repeat(50));
        assert!(overlong.chars().count() > TEXT_PREVIEW_CHARS);
        let item = event_json(InboundEventSummary {
            id: "hyphae:long".into(),
            source: "hyphae".into(),
            category: "direct".into(),
            recipient_identity: None,
            thread_key: None,
            actor: None,
            human: true,
            addressed: "direct".into(),
            at: "2026-11-11T03:00:00Z".into(),
            received_at: "2026-11-11T03:00:01Z".into(),
            text_preview: Some(overlong),
            text_scrubbed_at: None,
            disposition: "ignore".into(),
            disposition_reason: None,
            policy_version: None,
            run_state: None,
        });
        let preview = item["text_preview"].as_str().unwrap();
        assert_eq!(preview, "你".repeat(50));
        assert!(!item.to_string().contains("SECRET-TAIL"));
    }

    async fn call_with(
        app: &axum::Router,
        method: &str,
        uri: &str,
        bearer: &str,
        body: Option<&str>,
        extra: &[(&str, &str)],
    ) -> (StatusCode, Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
            .header(header::CONTENT_TYPE, "application/json");
        for (name, value) in extra {
            builder = builder.header(*name, *value);
        }
        let res = app
            .clone()
            .oneshot(
                builder
                    .body(Body::from(body.unwrap_or("").to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        (status, json_of(res).await)
    }

    async fn call_status(app: &axum::Router, method: &str, uri: &str, bearer: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    struct Fixed;

    #[async_trait]
    impl ModelProvider for Fixed {
        fn name(&self) -> &str {
            "fixed"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Ok(CompletionResponse {
                message: Msg::assistant(Some("pong".to_owned()), vec![]),
                usage: agent24_protocol::Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost_usd: 0.0,
                },
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &tokio_util::sync::CancellationToken,
        ) -> Result<Vec<agent24_protocol::Model>, ModelError> {
            Ok(vec![])
        }
    }

    async fn dev_legacy(start_runs: bool) -> AppState {
        let mut app = if start_runs {
            let router = Arc::new(ModelRouter::with_defaults(vec![(
                Arc::new(Fixed) as Arc<dyn ModelProvider>,
                Tier::Local,
            )]));
            state_with_router(router).await
        } else {
            state().await
        };
        app.dev_wechat_events = true;
        app
    }

    fn wechat_body(id: &str, actor: &str, at: &str, text: &str) -> String {
        json!({
            "id": id,
            "actor": actor,
            "at": at,
            "text": text,
        })
        .to_string()
    }

    async fn run_count(store: &agent24_store::Store) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(agent24_store::test_hooks::pool(store))
            .await
            .unwrap()
    }

    async fn wait_terminal(store: &agent24_store::Store, id: &str) {
        for _ in 0..100 {
            let run = store.get_run(id).await.unwrap().unwrap();
            if agent24_core::run_is_terminal(run.status) {
                return;
            }
            tokio::time::sleep(StdDuration::from_millis(20)).await;
        }
        panic!("run {id} never finished");
    }

    /// A missing link is not a run id. Sending `run_id: null` or `""` turns this red.
    #[test]
    fn already_linked_omits_run_id_when_the_link_row_is_gone() {
        let missing = outcome_json("wechat:1", IngestOutcome::AlreadyLinked { run_id: None });
        assert_eq!(
            missing,
            json!({"id": "wechat:1", "outcome": "already_linked"})
        );
        assert!(missing.get("run_id").is_none());

        let blank = outcome_json(
            "wechat:1",
            IngestOutcome::AlreadyLinked {
                run_id: Some(String::new()),
            },
        );
        assert!(blank.get("run_id").is_none(), "{blank}");

        let linked = outcome_json(
            "wechat:1",
            IngestOutcome::AlreadyLinked {
                run_id: Some("run_kept".into()),
            },
        );
        assert_eq!(linked["run_id"], "run_kept");
    }

    /// A start failure must not copy the prompt or the store text into the body.
    /// Calling it `storage error` turns this red.
    #[tokio::test]
    async fn wechat_start_failure_does_not_return_the_prompt() {
        let secret = "SECRET-wechat-prompt";
        let response = ingest_error_response(IngestError::Start(secret.to_owned()));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json = json_of(response).await;
        let rendered = json.to_string();
        assert_eq!(json["error"]["message"], "internal error", "{rendered}");
        assert!(!rendered.contains("storage error"), "{rendered}");
        assert!(!rendered.contains(secret), "{rendered}");
    }

    /// With the dev gate on, capabilities mode still refuses a host token and
    /// a creative token, and it does not import the seed. No row is written.
    /// Dropping the handler's audience check, while the token also carries
    /// `wechat-bridge` and `InboundIngest`, is what the impostor tests cover.
    /// A missing bearer is still 401.
    #[tokio::test]
    async fn capabilities_mode_rejects_non_bridge_tokens_and_skips_the_seed() {
        let (mut app_state, host, creative) = capability_state().await;
        app_state.dev_wechat_events = true;
        let seeded = seed_dev_allowlist(
            &app_state.store,
            AuthMode::Capabilities,
            true,
            "wechat:alice",
        )
        .await
        .unwrap();
        assert_eq!(seeded, 0);
        assert!(
            app_state
                .store
                .list_inbound_allowlist()
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            app_state
                .store
                .inbound_watermark_activated_at("wechat")
                .await
                .unwrap()
                .is_none()
        );
        let at = format_inbound_timestamp(Utc::now());
        let body = wechat_body("wechat:1", "alice", &at, "hello");
        let store = app_state.store.clone();
        let app = crate::server::build_router(app_state);
        for bearer in [host.as_str(), creative.as_str()] {
            let (status, json) =
                call(&app, "POST", "/api/v1/inbound/events", bearer, Some(&body)).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{bearer} {json}");
            assert_eq!(error_code(&json), "forbidden", "{json}");
        }
        assert!(store.get_inbound_event("wechat:1").await.unwrap().is_none());
        let missing = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/inbound/events")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    }

    /// The route stays dark unless startup saw `A24_DEV_TOOL=1`.
    #[tokio::test]
    async fn wechat_ingest_requires_the_dev_tool_flag() {
        let app_state = state().await;
        assert!(!app_state.dev_wechat_events);
        let app = crate::server::build_router(app_state);
        let at = format_inbound_timestamp(Utc::now());
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&wechat_body("wechat:1", "alice", &at, "hello")),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{json}");
        assert_eq!(error_code(&json), "not_found");
    }

    /// Bridge token: one run, Unknown tag. Each handler check refuses a token
    /// that fails only that check. Dropping one check lets that post start a
    /// second run.
    #[tokio::test]
    async fn bridge_token_starts_one_unknown_run_and_impostors_do_not() {
        use std::collections::BTreeSet;

        use crate::capabilities::{
            Audience, CapabilityClaims, Operation, Resource,
            bridge::{self, component_may_ingest},
            first_party::{FirstPartyAuthority, test_peer},
            unix_now,
        };

        let mut app = dev_legacy(true).await;
        let db = app.store.clone();
        let cap = crate::capabilities::CapabilityStore::new("daemon-test");
        let shell = FirstPartyAuthority::new(cap.clone())
            .issue(&test_peer("daemon-test"))
            .unwrap();
        let auth = cap
            .validate_bearer(
                &shell.secret,
                Operation::CapabilityMint,
                &Resource::global(),
                unix_now(),
            )
            .unwrap();
        let bridge = bridge::issue_wechat_bridge(&cap, &auth, unix_now()).unwrap();
        let (bridge_bearer, _, claims) = bridge.into_bearer_parts();
        assert_eq!(
            claims.expires_at - claims.created_at,
            bridge::BRIDGE_TTL_SECONDS
        );
        assert_eq!(claims.component_id.as_deref(), Some("wechat-bridge"));
        assert_eq!(claims.daemon_generation, "daemon-test");

        let now = unix_now();
        let forged = |id: &str, audience: Audience, component: Option<&str>, op: bool| {
            let mut allowed = BTreeSet::new();
            if op {
                allowed.insert(Operation::InboundIngest);
            }
            cap.insert(CapabilityClaims {
                first_party: None,
                capability_id: id.to_owned(),
                audience,
                component_id: component.map(str::to_owned),
                workspace_id: None,
                attachment_id: None,
                principal_id: None,
                daemon_generation: "daemon-test".to_owned(),
                host_generation: None,
                sidecar_generation: None,
                created_at: now,
                expires_at: now + 600,
                allowed_operations: allowed,
            })
            .unwrap()
            .into_bearer_parts()
            .0
        };
        let other = forged(
            "other-component",
            Audience::ComponentRuntime,
            Some("other-bridge"),
            true,
        );
        let shell_shaped = forged(
            "shell-shaped",
            Audience::FirstPartyShell,
            Some("wechat-bridge"),
            true,
        );
        let no_operation = forged(
            "bridge-without-op",
            Audience::ComponentRuntime,
            Some("wechat-bridge"),
            false,
        );
        assert!(component_may_ingest(&cap.authenticate_bearer(&other, now).unwrap()).is_err());
        assert!(
            component_may_ingest(&cap.authenticate_bearer(&shell_shaped, now).unwrap()).is_err()
        );
        assert!(
            component_may_ingest(&cap.authenticate_bearer(&no_operation, now).unwrap()).is_err()
        );

        app.enable_capability_auth(cap);
        app.dev_wechat_events = true;
        let at = format_inbound_timestamp(Utc::now());
        db.insert_inbound_allowlist_if_absent("wechat", "alice", &at, Some("host"))
            .await
            .unwrap();
        db.ensure_inbound_source_watermark("wechat", &at, 86_400)
            .await
            .unwrap();
        assert_eq!(
            seed_dev_allowlist(&db, AuthMode::Capabilities, true, "wechat:mallory")
                .await
                .unwrap(),
            0
        );
        assert_eq!(db.list_inbound_allowlist().await.unwrap().len(), 1);

        let router = crate::server::build_router(app);
        let (status, json) = call(
            &router,
            "POST",
            "/api/v1/inbound/events",
            &bridge_bearer,
            Some(&wechat_body("wechat:9", "alice", &at, "hello-bridge")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["outcome"], "started", "{json}");
        let run_id = json["run_id"].as_str().unwrap().to_owned();
        assert_eq!(run_count(&db).await, 1);
        wait_terminal(&db, &run_id).await;
        let tags = db.list_run_source_tags(&run_id).await.unwrap();
        let user = tags
            .iter()
            .find(|tag| tag.source.kind == SourceKind::UserInput)
            .unwrap();
        assert_eq!(user.source.classification, SourceClassification::Unknown);

        for (bearer, id) in [
            (other.as_str(), "wechat:10"),
            (shell_shaped.as_str(), "wechat:11"),
            (no_operation.as_str(), "wechat:12"),
        ] {
            let (status, json) = call(
                &router,
                "POST",
                "/api/v1/inbound/events",
                bearer,
                Some(&wechat_body(id, "alice", &at, "nope")),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{id} {json}");
        }
        assert_eq!(run_count(&db).await, 1);

        let (status, json) = call(
            &router,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&wechat_body("wechat:13", "alice", &at, "legacy")),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{json}");

        let (status, json) = call(&router, "GET", "/api/v1/models", &bridge_bearer, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{json}");

        let hyphae = json!({
            "id": "wechat:14",
            "source": "hyphae",
            "actor": "alice",
            "at": at,
            "text": "nope",
        })
        .to_string();
        let (status, json) = call(
            &router,
            "POST",
            "/api/v1/inbound/events",
            &bridge_bearer,
            Some(&hyphae),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(run_count(&db).await, 1);
    }

    /// A body `source` other than `wechat` returns in `checked_wechat_event`
    /// before ingest. Dropping that return turns the hyphae body into a 200.
    /// `deny_unknown_fields` still rejects `human`.
    #[tokio::test]
    async fn wechat_post_rejects_another_source_and_unknown_fields() {
        let app_state = dev_legacy(false).await;
        let app = crate::server::build_router(app_state.clone());
        let at = format_inbound_timestamp(Utc::now());
        let hyphae = json!({
            "id": "wechat:1",
            "source": "hyphae",
            "actor": "alice",
            "at": at,
            "text": "nope",
        })
        .to_string();
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&hyphae),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert_eq!(json["error"]["message"], "source is fixed to wechat");
        assert_eq!(
            app_state
                .store
                .list_inbound_event_summaries(10, None)
                .await
                .unwrap()
                .len(),
            0
        );

        let extra = json!({
            "id": "wechat:1",
            "actor": "alice",
            "at": at,
            "text": "nope",
            "human": false,
        })
        .to_string();
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&extra),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");

        for id in ["hyphae:1", "wechat:", "wechat:abc", "wechat:1:2"] {
            let (status, json) = call(
                &app,
                "POST",
                "/api/v1/inbound/events",
                "testtoken",
                Some(&wechat_body(id, "alice", &at, "x")),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{id} {json}");
            assert_eq!(
                json["error"]["message"],
                "id must look like wechat:<message_id>"
            );
        }
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&wechat_body("wechat:1", " alice", &at, "x")),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
    }

    /// Off the list: one ignore row, no run. The body does not echo the text.
    #[tokio::test]
    async fn an_unlisted_wechat_user_is_ignored_and_starts_no_run() {
        let app_state = dev_legacy(false).await;
        seed_dev_allowlist(&app_state.store, AuthMode::LegacySingleToken, true, "")
            .await
            .unwrap();
        let at = format_inbound_timestamp(Utc::now());
        let secret = "SECRET-unlisted-body";
        let app = crate::server::build_router(app_state.clone());
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&wechat_body("wechat:7", "mallory", &at, secret)),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["outcome"], "ignored");
        assert_eq!(json["reason"], "not_allowlisted");
        assert!(!json.to_string().contains(secret), "{json}");
        assert_eq!(run_count(&app_state.store).await, 0);
        let row = app_state
            .store
            .get_inbound_event("wechat:7")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.disposition, "ignore");
        assert_eq!(row.source, "wechat");
        assert_eq!(row.disposition_reason.as_deref(), Some("not_allowlisted"));
    }

    /// Seeded uid: one run, Unknown tag. The same id again does not start
    /// another. Shrinking the seed on this same store is
    /// `dev_seed_on_the_same_store_drops_uids_that_left_the_list`.
    #[tokio::test]
    async fn a_seeded_wechat_user_starts_one_unknown_run_and_a_replay_does_not() {
        let app_state = dev_legacy(true).await;
        let seeded = seed_dev_allowlist(
            &app_state.store,
            AuthMode::LegacySingleToken,
            true,
            "wechat:alice, wechat:bob",
        )
        .await
        .unwrap();
        assert_eq!(seeded, 2);
        let listed = app_state.store.list_inbound_allowlist().await.unwrap();
        assert_eq!(listed.len(), 2);
        assert!(
            listed
                .iter()
                .all(|row| row.note.as_deref() == Some("dev-seed"))
        );
        assert!(
            app_state
                .store
                .inbound_watermark_activated_at("wechat")
                .await
                .unwrap()
                .is_some()
        );

        let at = format_inbound_timestamp(Utc::now());
        let secret = "SECRET-seeded-body";
        let body = json!({
            "id": "wechat:9",
            "source": "wechat",
            "actor": "alice",
            "at": at,
            "text": secret,
        })
        .to_string();
        let app = crate::server::build_router(app_state.clone());
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["outcome"], "started", "{json}");
        assert!(!json.to_string().contains(secret), "{json}");
        let run_id = json["run_id"].as_str().unwrap().to_owned();
        assert_eq!(run_count(&app_state.store).await, 1);

        let (status, replay) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&body),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{replay}");
        assert_eq!(replay["outcome"], "duplicate");
        assert_eq!(run_count(&app_state.store).await, 1);

        wait_terminal(&app_state.store, &run_id).await;
        let tags = app_state.store.list_run_source_tags(&run_id).await.unwrap();
        let user = tags
            .iter()
            .find(|tag| tag.source.kind == SourceKind::UserInput)
            .unwrap();
        assert_eq!(user.source.classification, SourceClassification::Unknown);
    }

    /// Same store. An empty seed removes alice. Shrinking `alice,bob` to
    /// `alice` removes bob. A row whose note is not `dev-seed` stays.
    /// Dropping the `DELETE` in `sync_wechat_dev_seed_allowlist` leaves them
    /// allowlisted: alice's first POST and bob's POST become `started`.
    #[tokio::test]
    async fn dev_seed_on_the_same_store_drops_uids_that_left_the_list() {
        let app_state = dev_legacy(true).await;
        let store = &app_state.store;
        seed_dev_allowlist(store, AuthMode::LegacySingleToken, true, "wechat:alice")
            .await
            .unwrap();
        seed_dev_allowlist(store, AuthMode::LegacySingleToken, true, "")
            .await
            .unwrap();
        assert!(store.list_inbound_allowlist().await.unwrap().is_empty());

        let at = format_inbound_timestamp(Utc::now());
        let app = crate::server::build_router(app_state.clone());
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&wechat_body("wechat:21", "alice", &at, "hello")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["outcome"], "ignored", "{json}");
        assert_eq!(json["reason"], "not_allowlisted");
        assert_eq!(run_count(store).await, 0);

        seed_dev_allowlist(
            store,
            AuthMode::LegacySingleToken,
            true,
            "wechat:alice, wechat:bob",
        )
        .await
        .unwrap();
        seed_dev_allowlist(store, AuthMode::LegacySingleToken, true, "wechat:alice")
            .await
            .unwrap();
        let at = format_inbound_timestamp(Utc::now());
        let (status, json) = call(
            &app,
            "POST",
            "/api/v1/inbound/events",
            "testtoken",
            Some(&wechat_body("wechat:22", "bob", &at, "hello")),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{json}");
        assert_eq!(json["outcome"], "ignored", "{json}");
        assert_eq!(json["reason"], "not_allowlisted");
        assert_eq!(run_count(store).await, 0);
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert!(
            listed
                .iter()
                .any(|row| row.actor == "alice" && row.note.as_deref() == Some("dev-seed"))
        );
        assert!(listed.iter().all(|row| row.actor != "bob"));

        let stamp = format_inbound_timestamp(Utc::now());
        assert!(
            store
                .insert_inbound_allowlist_if_absent("wechat", "carol", &stamp, Some("host"))
                .await
                .unwrap()
        );
        seed_dev_allowlist(store, AuthMode::LegacySingleToken, true, "")
            .await
            .unwrap();
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert_eq!(listed.len(), 1, "{listed:?}");
        assert_eq!(listed[0].actor, "carol");
        assert_eq!(listed[0].note.as_deref(), Some("host"));
        assert_eq!(listed[0].added_at, stamp);
    }

    /// An allowlisted message with no body is refused before the insert.
    /// Mapping that `Invalid` onto a 500, or storing the row first, turns this red.
    #[tokio::test]
    async fn an_allowlisted_wechat_message_with_no_text_is_400_and_not_stored() {
        let app_state = dev_legacy(false).await;
        seed_dev_allowlist(
            &app_state.store,
            AuthMode::LegacySingleToken,
            true,
            "wechat:alice",
        )
        .await
        .unwrap();
        let at = format_inbound_timestamp(Utc::now());
        let app = crate::server::build_router(app_state.clone());
        let bodies = [
            json!({"id": "wechat:11", "actor": "alice", "at": at}).to_string(),
            json!({"id": "wechat:12", "actor": "alice", "at": at, "text": ""}).to_string(),
        ];
        for body in &bodies {
            let (status, json) = call(
                &app,
                "POST",
                "/api/v1/inbound/events",
                "testtoken",
                Some(body),
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
            assert_eq!(error_code(&json), "invalid_request", "{json}");
            let message = json["error"]["message"].as_str().unwrap();
            assert!(message.contains("no text"), "{json}");
            assert!(!json.to_string().contains("storage error"), "{json}");
            assert!(!json.to_string().contains("internal error"), "{json}");
        }
        assert_eq!(run_count(&app_state.store).await, 0);
        assert!(
            app_state
                .store
                .list_inbound_event_summaries(10, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    /// An existing host note is not replaced. A capabilities call with the
    /// same variable writes nothing. Deleting the auth-mode guard in
    /// `seed_dev_allowlist` turns the capabilities assertion red.
    #[tokio::test]
    async fn dev_seed_is_idempotent_and_does_not_run_in_capabilities_mode() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let stamp = format_inbound_timestamp(Utc::now());
        assert!(
            store
                .insert_inbound_allowlist_if_absent("wechat", "kept", &stamp, Some("host"))
                .await
                .unwrap()
        );
        let seeded = seed_dev_allowlist(
            &store,
            AuthMode::LegacySingleToken,
            true,
            "wechat:kept, not-wechat, wechat:new",
        )
        .await
        .unwrap();
        assert_eq!(seeded, 1);
        let listed = store.list_inbound_allowlist().await.unwrap();
        let kept = listed.iter().find(|row| row.actor == "kept").unwrap();
        assert_eq!(kept.note.as_deref(), Some("host"));
        assert_eq!(kept.added_at, stamp);
        assert!(
            listed
                .iter()
                .any(|row| row.actor == "new" && row.note.as_deref() == Some("dev-seed"))
        );

        let other = agent24_store::Store::open_memory().await.unwrap();
        let seeded = seed_dev_allowlist(&other, AuthMode::Capabilities, true, "wechat:alice")
            .await
            .unwrap();
        assert_eq!(seeded, 0);
        assert!(other.list_inbound_allowlist().await.unwrap().is_empty());
        assert!(
            other
                .inbound_watermark_activated_at("wechat")
                .await
                .unwrap()
                .is_none()
        );
        let seeded = seed_dev_allowlist(&other, AuthMode::LegacySingleToken, false, "wechat:alice")
            .await
            .unwrap();
        assert_eq!(seeded, 0);
        assert!(other.list_inbound_allowlist().await.unwrap().is_empty());
    }
}
