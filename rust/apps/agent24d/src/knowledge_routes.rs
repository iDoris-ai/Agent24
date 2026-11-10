//! The daemon side of the private Knowledge host interface (KB-HOST-01 PR-B,
//! #1085 design v2 §2). Only the desktop's ProductHost capability may use it:
//! it activates and deactivates Knowledge bindings and hands the daemon the
//! short-lived credential C of an active binding, or takes it back. C lives
//! only in this process's memory; no response and no log line carries it.
//!
//! Every record belongs to the ProductHost capability that wrote it (decided
//! 2026-10-24): a lookup always names the capability it is made for, so a
//! binding cannot be found by its id alone (design v2 §3).
//!
//! Each change carries a sequence number from the host, starting at 1, that
//! must be greater than any it sent before for that binding; the binding and
//! its credential share one counter. The high-water mark outlives
//! deactivation, so a change that arrives after a later one cannot undo it: a
//! push overtaken by a revoke or a deactivation cannot bring the old C back.
//! Every change but a push drops C, so C is there only while the newest change
//! applied is the push that set it; a removal overtaken by a reactivation is
//! refused, but the reactivation has already dropped C.
//! A host that does not know whether a change landed sends the state it wants
//! again under a new number. A binding whose mark reaches `u64::MAX` takes no
//! further change until the daemon restarts.
//!
//! A binding id is the string axum's `Path` decodes from the URL, so `b1` and
//! `%62%31` name the same binding. It is only ever a key in this map.
//!
//! A run may name a binding (`RunCreate.knowledge_binding_id`, PR-C): only the
//! host that activated it can, and the run is pinned to that host and that
//! activation, its generation. The pin lives in memory, like the records
//! (decided 2026-10-25): a restarted daemon has a new host capability, so a
//! pin could not match again wherever it was kept, and a run that names a
//! binding but has no pin is refused, never bound anew.

use agent24_domain::http::read_body_or_response;
use agent24_knowledge::host::{
    Change, ChangeError, Credential, Endpoint, MAX_BINDING_ID_BYTES, RunBinding,
};
use agent24_protocol::state_file::AuthMode;
use axum::body::Body;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::capabilities::{Audience, Authorization, UnixSeconds};
use crate::server::{AppState, error_response};

/// The capability id of the ProductHost making this request. Legacy mode has
/// no such authority (its shared token also reaches every other route), and a
/// Creative runtime is not the host, so both get `None`.
fn host_capability(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
) -> Option<String> {
    match (state.auth_mode, authorization) {
        (AuthMode::Capabilities, Some(Extension(authorization)))
            if authorization.claims.audience == Audience::ProductHost =>
        {
            Some(authorization.claims.capability_id)
        }
        _ => None,
    }
}

/// Opaque, but never a path: an encoded `/` or `\` in the URL must not turn
/// into one after routing, and `.` and `..` are no ids either.
fn valid_binding_id(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && value.len() <= MAX_BINDING_ID_BYTES
        && !value
            .chars()
            .any(|c| c.is_control() || c == '/' || c == '\\')
}

fn change_response(result: Result<(), ChangeError>) -> Response {
    let (status, code, message) = match result {
        Ok(()) => return StatusCode::NO_CONTENT.into_response(),
        Err(ChangeError::StaleSequence) => (
            StatusCode::CONFLICT,
            "stale_sequence",
            "A later change for this binding was already applied",
        ),
        Err(ChangeError::BindingInactive) => (
            StatusCode::CONFLICT,
            "binding_inactive",
            "The binding is not active",
        ),
        Err(ChangeError::TooManyBindings) => (
            StatusCode::CONFLICT,
            "too_many_bindings",
            "Too many Knowledge bindings",
        ),
        Err(ChangeError::Unavailable) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "knowledge_host_unavailable",
            "Knowledge host state is unavailable",
        ),
        // Same answer as a body the handler could not read. The rule name
        // stays in the registry; this response does not repeat it.
        Err(ChangeError::Invalid(_)) => return invalid_request(),
    };
    error_response(status, code, message)
}

/// Says nothing about which part was wrong: a parse error could quote C.
fn invalid_request() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Malformed request",
    )
}

#[derive(Deserialize)]
pub struct SeqQuery {
    seq: u64,
}

/// Why a request was refused before it reached the registry.
pub enum Refused {
    NotHost,
    Malformed,
    Inactive,
    /// The session's earlier runs were bound to another activation, or took
    /// in Knowledge results from one it can no longer tell.
    SessionRebound,
    /// The session's tool result origins could not be read.
    SourcesUnavailable,
}

impl IntoResponse for Refused {
    fn into_response(self) -> Response {
        match self {
            Self::NotHost => error_response(
                StatusCode::FORBIDDEN,
                "host_authority_required",
                "Only the product host may use this interface",
            ),
            Self::Malformed => invalid_request(),
            Self::Inactive => error_response(
                StatusCode::CONFLICT,
                "knowledge_binding_inactive",
                "The product host has no active Knowledge binding by that id",
            ),
            Self::SessionRebound => error_response(
                StatusCode::CONFLICT,
                "knowledge_session_rebound",
                "This session's runs used another activation of that Knowledge binding; start a new session",
            ),
            Self::SourcesUnavailable => error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "source_policy_unavailable",
                "This session's sources could not be read",
            ),
        }
    }
}

/// The host making the request, the binding and the change's sequence number.
/// An id that is not UTF-8 gets the same answer as any other malformed one.
fn host_and_seq(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
) -> Result<(String, String, u64), Refused> {
    let host = host_capability(state, authorization).ok_or(Refused::NotHost)?;
    match (path, query) {
        (Ok(Path(binding_id)), Ok(Query(SeqQuery { seq })))
            if seq > 0 && valid_binding_id(&binding_id) =>
        {
            Ok((host, binding_id, seq))
        }
        _ => Err(Refused::Malformed),
    }
}

/// Checks a run's `knowledge_binding_id` at creation and returns what to pin
/// the run to: only the product host may name a binding, only one it has
/// active, and in a session only the activation its earlier runs had.
pub async fn bind_run(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
    binding_id: &str,
    session_id: Option<&str>,
) -> Result<RunBinding, Refused> {
    let host = host_capability(state, authorization).ok_or(Refused::NotHost)?;
    if !valid_binding_id(binding_id) {
        return Err(Refused::Malformed);
    }
    let generation = state
        .knowledge_host
        .generation(&host, binding_id)
        .ok_or(Refused::Inactive)?;
    let binding = RunBinding {
        host,
        binding: binding_id.to_owned(),
        generation,
    };
    if let Some(session_id) = session_id {
        // Read before the pin is taken; a run that adds such results has
        // pinned the session before it executes.
        let took_knowledge = state
            .store
            .session_tool_result_origins(session_id)
            .await
            .map_err(|_| Refused::SourcesUnavailable)?
            .iter()
            .any(|origin| agent24_knowledge::host::KNOWLEDGE_ORIGINS.contains(&origin.as_str()));
        if !state
            .knowledge_host
            .pin_session(session_id, &binding, took_knowledge)
        {
            return Err(Refused::SessionRebound);
        }
    }
    Ok(binding)
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}?seq=n` activates the
/// binding, with `{"endpoint", "knowledge_base_ids"}` for its calls or with
/// no body (then every call is refused); `DELETE` of the same deactivates it.
pub async fn change(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    method: Method,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    let registry = &state.knowledge_host;
    if method != Method::PUT {
        return change_response(registry.apply(&host, &binding_id, seq, Change::Deactivate));
    }
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    if body.is_empty() {
        return change_response(registry.apply(&host, &binding_id, seq, Change::Activate));
    }
    let Some(endpoint) = endpoint_of(&body) else {
        return invalid_request();
    };
    change_response(registry.activate(&host, &binding_id, seq, endpoint))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivateBody {
    endpoint: String,
    knowledge_base_ids: Vec<String>,
}

/// The activation body `{"endpoint", "knowledge_base_ids"}`. The registry
/// checks the URL and the ids; a body that is not this shape is refused here.
fn endpoint_of(body: &[u8]) -> Option<Endpoint> {
    let body: ActivateBody = serde_json::from_slice(body).ok()?;
    Some(Endpoint {
        base_url: body.endpoint,
        knowledge_base_ids: body.knowledge_base_ids,
    })
}

/// `DELETE /api/v1/host/knowledge/bindings/{binding_id}/credential?seq=n`
/// drops C at once. Accepted for a binding this host never activated too, so
/// it can be sent before the push it overtakes.
pub async fn revoke_credential(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    change_response(
        state
            .knowledge_host
            .apply(&host, &binding_id, seq, Change::RevokeCredential),
    )
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialKind {
    Bearer,
    ApiKey,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialBody {
    kind: CredentialKind,
    value: String,
    expires_at: UnixSeconds,
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}/credential?seq=n` with
/// `{"kind": "bearer"|"api_key", "value", "expires_at"}` pushes or replaces C.
pub async fn push_credential(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    // The kernel's body cap and its 413 envelope, like every other handler.
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    let Ok(body) = serde_json::from_slice::<CredentialBody>(&body) else {
        return invalid_request();
    };
    let credential = match body.kind {
        CredentialKind::Bearer => Credential::Bearer(body.value),
        CredentialKind::ApiKey => Credential::ApiKey(body.value),
    };
    change_response(state.knowledge_host.push_credential(
        &host,
        &binding_id,
        seq,
        credential,
        body.expires_at,
    ))
}

#[cfg(test)]
mod tests;
