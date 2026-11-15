//! The daemon side of the private Knowledge host interface (KB-HOST-01 PR-B,
//! #1085 design v2 §2). Two hosts may use it. The desktop's ProductHost
//! capability, keyed `product:{capability_id}`. And the desktop
//! FirstPartyShell whose code-signing identifier is exactly
//! `ai.auraai.agent24`, keyed `shell:ai.auraai.agent24`. FirstPartyShell is
//! accepted only here, for the Knowledge host; it is not a ProductHost. The
//! host activates and deactivates Knowledge bindings and hands the daemon the
//! short-lived credential C of an active binding, or takes it back. C lives
//! only in this process's memory; no response and no log line carries it.
//!
//! Every record belongs to the host that wrote it (decided 2026-10-24): a
//! lookup always names that host key, so a binding cannot be found by its id
//! alone (design v2 §3). Renewal replaces a shell's capability id and leaves
//! its code-signing identifier, and therefore its host key, unchanged.
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
//! (decided 2026-10-25). A restarted daemon has lost both, so a pin could not
//! match again wherever it was kept, and a run that names a binding but has
//! no pin is refused, never bound anew.

use agent24_domain::http::read_body_or_response;
use agent24_knowledge::host::{
    Change, ChangeError, Credential, Endpoint, MAX_BINDING_ID_BYTES, MAX_CREDENTIAL_BYTES,
    MAX_KNOWLEDGE_BASE_ID_CHARS, MAX_KNOWLEDGE_BASES, RunBinding,
};
use agent24_protocol::state_file::AuthMode;
use axum::body::Body;
use axum::extract::rejection::PathRejection;
use axum::extract::{Extension, Path, State};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::capabilities::{Audience, Authorization, UnixSeconds, unix_now};
use crate::server::{AppState, error_response};

/// Code-signing identifier of the desktop app. Exact match only: a CLI
/// `agent24` or `agent24d` is not this shell.
pub const DESKTOP_SHELL_IDENTIFIER: &str = "ai.auraai.agent24";

/// Which kind of Knowledge host made the request. The key is prefixed so a
/// ProductHost capability id cannot collide with the desktop shell key.
enum HostKey {
    Product(String),
    DesktopShell,
}

impl HostKey {
    fn into_key(self) -> String {
        match self {
            Self::Product(id) => format!("product:{id}"),
            Self::DesktopShell => format!("shell:{DESKTOP_SHELL_IDENTIFIER}"),
        }
    }
}

/// The host key of this request. Legacy mode has no such authority (its
/// shared token also reaches every other route), and a Creative runtime is
/// not a host, so both get `None`. A ProductHost is `product:{capability_id}`.
/// A desktop FirstPartyShell is `shell:ai.auraai.agent24`, and only when its
/// peer identifier is exactly [`DESKTOP_SHELL_IDENTIFIER`]. Any other
/// audience, including a CLI shell, is not a host. FirstPartyShell is accepted
/// only here; it is not a ProductHost.
fn host_capability(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
) -> Option<String> {
    let (AuthMode::Capabilities, Some(Extension(authorization))) = (state.auth_mode, authorization)
    else {
        return None;
    };
    match authorization.claims.audience {
        Audience::ProductHost => {
            Some(HostKey::Product(authorization.claims.capability_id).into_key())
        }
        Audience::FirstPartyShell if is_desktop_shell(&authorization.claims) => {
            Some(HostKey::DesktopShell.into_key())
        }
        _ => None,
    }
}

/// Whether a FirstPartyShell's claims are the desktop's: its peer identifier
/// is exactly [`DESKTOP_SHELL_IDENTIFIER`]. `capabilities/middleware.rs` asks this too, before
/// it keeps an approval text, so a CLI shell is refused before any dialog.
pub(crate) fn is_desktop_shell(claims: &crate::capabilities::CapabilityClaims) -> bool {
    claims
        .first_party
        .as_ref()
        .is_some_and(|first_party| first_party.peer.identity.identifier == DESKTOP_SHELL_IDENTIFIER)
}

/// The refusal a caller that is not the desktop host gets.
pub(crate) fn not_host() -> Response {
    Refused::NotHost.into_response()
}

/// Opaque, but never a path: an encoded `/` or `\` in the URL must not turn
/// into one after routing, and `.` and `..` are no ids either.
pub(crate) fn valid_binding_id(value: &str) -> bool {
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
/// `capabilities/middleware.rs` also uses it for a query on a change that has no grant yet.
pub(crate) fn invalid_request() -> Response {
    error_response(
        StatusCode::BAD_REQUEST,
        "invalid_request",
        "Malformed request",
    )
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
                "Only the desktop host may use this interface",
            ),
            Self::Malformed => invalid_request(),
            Self::Inactive => error_response(
                StatusCode::CONFLICT,
                "knowledge_binding_inactive",
                "The desktop host has no active Knowledge binding by that id",
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

/// The host making the request and the binding. An id that is not UTF-8 gets
/// the same answer as any other malformed one. The host is decided before the
/// path and the query, and both of those before the body.
fn host_and_binding(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    req: &Request<Body>,
) -> Result<(String, String), Refused> {
    let host = host_capability(state, authorization).ok_or(Refused::NotHost)?;
    match path {
        Ok(Path(binding_id)) if req.uri().query().is_none() && valid_binding_id(&binding_id) => {
            Ok((host, binding_id))
        }
        _ => Err(Refused::Malformed),
    }
}

/// Checks a run's `knowledge_binding_id` at creation and returns what to pin
/// the run to: only a Knowledge host may name a binding, only one it has
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

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}` with `{"seq"}` activates
/// the binding with no endpoint (then every call is refused), or with
/// `{"seq", "endpoint", "knowledge_base_ids"}` for its calls. `DELETE` of the
/// same path with `{"seq"}` deactivates it. A query string is refused.
pub async fn change(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    method: Method,
    path: Result<Path<String>, PathRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id) = match host_and_binding(&state, authorization, path, &req) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    let Some(change) = parse_change_body(method.as_str(), false, &body) else {
        return invalid_request();
    };
    let registry = &state.knowledge_host;
    match change {
        ParsedChange::Activate {
            seq,
            endpoint: None,
        } => change_response(registry.apply(&host, &binding_id, seq, Change::Activate)),
        ParsedChange::Activate {
            seq,
            endpoint: Some(endpoint),
        } => change_response(registry.activate(&host, &binding_id, seq, endpoint)),
        ParsedChange::Deactivate { seq } => {
            change_response(registry.apply(&host, &binding_id, seq, Change::Deactivate))
        }
        ParsedChange::PushCredential { .. } | ParsedChange::RevokeCredential { .. } => {
            invalid_request()
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivateBody {
    endpoint: String,
    knowledge_base_ids: Vec<String>,
}

/// The activation body `{"endpoint", "knowledge_base_ids"}`: an API base the
/// HTTP transport accepts (`https`, or `http` to a loopback IP) and 1 to
/// [`MAX_KNOWLEDGE_BASES`] distinct, non-blank ids.
/// Same rules as the registry's own check; a test pins the two together.
/// The endpoint is kept as `validate_base_url` normalises it, which is the
/// string the HTTP transport connects to (ruling 2026-11-14-f1-endpoint-canonical).
fn endpoint_of(body: &[u8]) -> Option<Endpoint> {
    let body: ActivateBody = serde_json::from_slice(body).ok()?;
    let base_url = agent24_knowledge::validate_base_url(&body.endpoint).ok()?;
    let ids = &body.knowledge_base_ids;
    let id_ok =
        |id: &String| !id.trim().is_empty() && id.chars().count() <= MAX_KNOWLEDGE_BASE_ID_CHARS;
    let distinct = ids.iter().collect::<std::collections::HashSet<_>>().len() == ids.len();
    let ids_ok = !ids.is_empty() && ids.len() <= MAX_KNOWLEDGE_BASES && distinct;
    let ok = ids_ok && ids.iter().all(id_ok);
    ok.then_some(Endpoint {
        base_url,
        knowledge_base_ids: body.knowledge_base_ids,
    })
}

/// One Knowledge host change, parsed the same way the handler applies it and
/// the presence summary describes it. `seq` is greater than 0.
pub(crate) enum ParsedChange {
    /// `endpoint` absent: every call of the binding is refused.
    Activate {
        seq: u64,
        endpoint: Option<Endpoint>,
    },
    Deactivate {
        seq: u64,
    },
    PushCredential {
        seq: u64,
        credential: Credential,
        expires_at: UnixSeconds,
    },
    RevokeCredential {
        seq: u64,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SeqBody {
    seq: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ActivateFull {
    seq: u64,
    endpoint: String,
    knowledge_base_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum CredentialKind {
    Bearer,
    ApiKey,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CredentialRequest {
    seq: u64,
    kind: CredentialKind,
    value: String,
    expires_at: UnixSeconds,
}

fn seq_of(seq: u64) -> Option<u64> {
    (seq > 0).then_some(seq)
}

/// Body of a Knowledge host change. `method` is `PUT` or `DELETE`.
/// `is_credential` selects the credential routes. Empty, non-JSON, unknown
/// fields, a missing or zero `seq`, or a value the handler would refuse all
/// return `None`. Nothing in the error path quotes the body.
pub(crate) fn parse_change_body(
    method: &str,
    is_credential: bool,
    bytes: &[u8],
) -> Option<ParsedChange> {
    if bytes.is_empty() {
        return None;
    }
    match (method, is_credential) {
        ("DELETE", false) => {
            let body: SeqBody = serde_json::from_slice(bytes).ok()?;
            Some(ParsedChange::Deactivate {
                seq: seq_of(body.seq)?,
            })
        }
        ("DELETE", true) => {
            let body: SeqBody = serde_json::from_slice(bytes).ok()?;
            Some(ParsedChange::RevokeCredential {
                seq: seq_of(body.seq)?,
            })
        }
        ("PUT", false) => parse_activation(bytes),
        ("PUT", true) => parse_credential(bytes),
        _ => None,
    }
}

/// `{"seq"}` activates with no endpoint. `{"seq","endpoint","knowledge_base_ids"}`
/// both extra fields, or neither; one of them is refused. Both go through
/// [`endpoint_of`].
fn parse_activation(bytes: &[u8]) -> Option<ParsedChange> {
    if let Ok(body) = serde_json::from_slice::<ActivateFull>(bytes) {
        let seq = seq_of(body.seq)?;
        let raw = serde_json::to_vec(&serde_json::json!({
            "endpoint": body.endpoint,
            "knowledge_base_ids": body.knowledge_base_ids,
        }))
        .ok()?;
        return Some(ParsedChange::Activate {
            seq,
            endpoint: Some(endpoint_of(&raw)?),
        });
    }
    let body: SeqBody = serde_json::from_slice(bytes).ok()?;
    Some(ParsedChange::Activate {
        seq: seq_of(body.seq)?,
        endpoint: None,
    })
}

/// Same rules as the registry's own check; a test pins the two together.
fn parse_credential(bytes: &[u8]) -> Option<ParsedChange> {
    let body: CredentialRequest = serde_json::from_slice(bytes).ok()?;
    let seq = seq_of(body.seq)?;
    // Visible ASCII only: C goes into a request header as it is.
    let value_ok = !body.value.is_empty()
        && body.value.len() <= MAX_CREDENTIAL_BYTES
        && body.value.bytes().all(|b| b.is_ascii_graphic());
    if !value_ok || body.expires_at <= unix_now() {
        return None;
    }
    let credential = match body.kind {
        CredentialKind::Bearer => Credential::Bearer(body.value),
        CredentialKind::ApiKey => Credential::ApiKey(body.value),
    };
    Some(ParsedChange::PushCredential {
        seq,
        credential,
        expires_at: body.expires_at,
    })
}

/// `DELETE /api/v1/host/knowledge/bindings/{binding_id}/credential` with
/// `{"seq"}` drops C at once. Accepted for a binding this host never
/// activated too, so it can be sent before the push it overtakes.
pub async fn revoke_credential(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id) = match host_and_binding(&state, authorization, path, &req) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    let Some(ParsedChange::RevokeCredential { seq }) = parse_change_body("DELETE", true, &body)
    else {
        return invalid_request();
    };
    change_response(
        state
            .knowledge_host
            .apply(&host, &binding_id, seq, Change::RevokeCredential),
    )
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}/credential` with
/// `{"seq", "kind": "bearer"|"api_key", "value", "expires_at"}` pushes or
/// replaces C.
pub async fn push_credential(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    path: Result<Path<String>, PathRejection>,
    req: Request<Body>,
) -> Response {
    let (host, binding_id) = match host_and_binding(&state, authorization, path, &req) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    // The kernel's body cap and its 413 envelope, like every other handler.
    let body = match read_body_or_response(req).await {
        Ok(body) => body,
        Err(refused) => return refused,
    };
    let Some(ParsedChange::PushCredential {
        seq,
        credential,
        expires_at,
    }) = parse_change_body("PUT", true, &body)
    else {
        return invalid_request();
    };
    change_response(state.knowledge_host.push_credential(
        &host,
        &binding_id,
        seq,
        credential,
        expires_at,
    ))
}

#[cfg(test)]
mod tests;
