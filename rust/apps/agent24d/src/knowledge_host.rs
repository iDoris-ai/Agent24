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

use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::sync::Mutex;

use agent24_protocol::state_file::AuthMode;
use axum::body::Body;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::capabilities::{Audience, Authorization, UnixSeconds, unix_now};
use agent24_domain::http::read_body_or_response;

use crate::server::{AppState, error_response};

/// Bindings one host may hold records for, deactivated ones included.
pub const MAX_BINDINGS_PER_HOST: usize = 1024;
const MAX_BINDING_ID_BYTES: usize = 256;
/// Run pins kept; the oldest goes first, and its run is then refused.
const MAX_PINNED_RUNS: usize = 4096;
const MAX_CREDENTIAL_BYTES: usize = 16 * 1024;

/// How the Knowledge service expects C to be presented. `Debug` never prints
/// the secret.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    Bearer(String),
    ApiKey(String),
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bearer(_) => f.write_str("Credential::Bearer(<redacted>)"),
            Self::ApiKey(_) => f.write_str("Credential::ApiKey(<redacted>)"),
        }
    }
}

/// The credential of an active binding, with the sequence number it was
/// pushed under so a caller can tell which C a request used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurrentCredential {
    pub credential: Credential,
    pub seq: u64,
    pub expires_at: UnixSeconds,
}

#[derive(Clone, Default)]
struct Entry {
    seq: u64,
    active: bool,
    /// The sequence number of the activation in effect: the generation.
    activated_seq: u64,
    credential: Option<CurrentCredential>,
}

/// What a run was bound to when it was created (design v2 §3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunBinding {
    pub host: String,
    pub binding: String,
    pub generation: u64,
}

/// Why a run has no usable binding; Knowledge tools refuse with its code.
// Consumed by the Knowledge tools (PR-E).
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KnowledgeRefusal {
    /// The run names no binding.
    Unbound,
    /// It names one, but not one active under the host and generation it was
    /// pinned to, or the pin is gone.
    Stale,
}

impl KnowledgeRefusal {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn code(self) -> &'static str {
        match self {
            Self::Unbound => "knowledge_unbound",
            Self::Stale => "knowledge_binding_stale",
        }
    }
}

#[derive(Default)]
struct RunPins {
    pins: HashMap<String, RunBinding>,
    order: VecDeque<String>,
}

/// A change that carries nothing but its sequence number.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    /// Also drops the credential: the host pushes C again after activating.
    Activate,
    /// Also drops the credential.
    Deactivate,
    /// Drops the credential; the binding stays as it is.
    RevokeCredential,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeError {
    /// `seq` is not greater than one this host already sent for the binding.
    StaleSequence,
    /// A credential was pushed for a binding that is not active.
    BindingInactive,
    TooManyBindings,
    Unavailable,
}

/// Records keyed by (host capability id, binding id).
#[derive(Default)]
pub struct KnowledgeHostRegistry {
    entries: Mutex<HashMap<(String, String), Entry>>,
    runs: Mutex<RunPins>,
}

impl KnowledgeHostRegistry {
    pub fn apply(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        change: Change,
    ) -> Result<(), ChangeError> {
        self.change(host, binding, seq, |entry| {
            match change {
                Change::Activate => {
                    entry.active = true;
                    entry.activated_seq = seq;
                    entry.credential = None;
                }
                Change::Deactivate => {
                    entry.active = false;
                    entry.credential = None;
                }
                Change::RevokeCredential => entry.credential = None,
            }
            Ok(())
        })
    }

    /// Pushes or replaces C of an active binding.
    pub fn push_credential(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        credential: Credential,
        expires_at: UnixSeconds,
    ) -> Result<(), ChangeError> {
        self.change(host, binding, seq, |entry| {
            if !entry.active {
                return Err(ChangeError::BindingInactive);
            }
            entry.credential = Some(CurrentCredential {
                credential,
                seq,
                expires_at,
            });
            Ok(())
        })
    }

    /// C of an active binding of `host`, unless it has expired by `now`; an
    /// expired C is dropped here. Only an active binding holds one: a push
    /// needs the binding active, and deactivation drops it.
    // Consumed by run binding (PR-C) and the Provider (KB-C1-01).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn credential(
        &self,
        host: &str,
        binding: &str,
        now: UnixSeconds,
    ) -> Option<CurrentCredential> {
        let mut entries = self.entries.lock().ok()?;
        let entry = entries.get_mut(&(host.to_owned(), binding.to_owned()))?;
        if entry
            .credential
            .as_ref()
            .is_some_and(|current| current.expires_at <= now)
        {
            entry.credential = None;
        }
        entry.credential.clone()
    }

    /// Applies `apply` to a copy of the record and stores it only when `seq`
    /// is fresh and `apply` succeeds, so a refused change leaves no trace.
    fn change(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        apply: impl FnOnce(&mut Entry) -> Result<(), ChangeError>,
    ) -> Result<(), ChangeError> {
        let mut entries = self.entries.lock().map_err(|_| ChangeError::Unavailable)?;
        let key = (host.to_owned(), binding.to_owned());
        let mut entry = match entries.get(&key) {
            Some(entry) => entry.clone(),
            None if entries.keys().filter(|(owner, _)| owner == host).count()
                >= MAX_BINDINGS_PER_HOST =>
            {
                return Err(ChangeError::TooManyBindings);
            }
            None => Entry::default(),
        };
        if seq <= entry.seq {
            return Err(ChangeError::StaleSequence);
        }
        apply(&mut entry)?;
        entry.seq = seq;
        entries.insert(key, entry);
        Ok(())
    }

    /// The generation of `binding` if `host` has it active. A missing record
    /// or a poisoned lock counts as inactive.
    pub fn generation(&self, host: &str, binding: &str) -> Option<u64> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(&(host.to_owned(), binding.to_owned()))?;
        entry.active.then_some(entry.activated_seq)
    }

    /// Pins `run_id` to what it was bound to at creation.
    pub fn pin_run(&self, run_id: &str, binding: RunBinding) {
        let Ok(mut runs) = self.runs.lock() else {
            return;
        };
        if runs.pins.insert(run_id.to_owned(), binding).is_none() {
            runs.order.push_back(run_id.to_owned());
        }
        while runs.order.len() > MAX_PINNED_RUNS {
            if let Some(oldest) = runs.order.pop_front() {
                runs.pins.remove(&oldest);
            }
        }
    }

    /// The binding a run may use now: the one it names in its persisted input
    /// (`selector`), still active under the host and generation it was pinned
    /// to. Nothing else is looked up, so a run never moves to a new binding.
    // Consumed by the Knowledge tools (PR-E).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn resolve_run(
        &self,
        run_id: &str,
        selector: Option<&str>,
    ) -> Result<RunBinding, KnowledgeRefusal> {
        let selector = selector.ok_or(KnowledgeRefusal::Unbound)?;
        let pin = self
            .runs
            .lock()
            .ok()
            .and_then(|runs| runs.pins.get(run_id).cloned())
            .filter(|pin| pin.binding == selector)
            .ok_or(KnowledgeRefusal::Stale)?;
        match self.generation(&pin.host, &pin.binding) {
            Some(generation) if generation == pin.generation => Ok(pin),
            _ => Err(KnowledgeRefusal::Stale),
        }
    }

    /// Whether `host` has `binding` active.
    // Consumed by run binding (PR-C) and the Provider (KB-C1-01).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_active(&self, host: &str, binding: &str) -> bool {
        self.entries.lock().is_ok_and(|entries| {
            entries
                .get(&(host.to_owned(), binding.to_owned()))
                .is_some_and(|entry| entry.active)
        })
    }
}

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
/// the run to: only the product host may name a binding, and only one it has
/// active.
pub fn bind_run(
    state: &AppState,
    authorization: Option<Extension<Authorization>>,
    binding_id: &str,
) -> Result<RunBinding, Refused> {
    let host = host_capability(state, authorization).ok_or(Refused::NotHost)?;
    if !valid_binding_id(binding_id) {
        return Err(Refused::Malformed);
    }
    let generation = state
        .knowledge_host
        .generation(&host, binding_id)
        .ok_or(Refused::Inactive)?;
    Ok(RunBinding {
        host,
        binding: binding_id.to_owned(),
        generation,
    })
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}?seq=n` activates the
/// binding, `DELETE` of the same deactivates it.
pub async fn change(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    method: Method,
    path: Result<Path<String>, PathRejection>,
    query: Result<Query<SeqQuery>, QueryRejection>,
) -> Response {
    let (host, binding_id, seq) = match host_and_seq(&state, authorization, path, query) {
        Ok(found) => found,
        Err(refused) => return refused.into_response(),
    };
    let change = if method == Method::PUT {
        Change::Activate
    } else {
        Change::Deactivate
    };
    change_response(state.knowledge_host.apply(&host, &binding_id, seq, change))
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
    // Visible ASCII only: C goes into a request header as it is.
    let value_ok = !body.value.is_empty()
        && body.value.len() <= MAX_CREDENTIAL_BYTES
        && body.value.bytes().all(|b| b.is_ascii_graphic());
    if !value_ok || body.expires_at <= unix_now() {
        return invalid_request();
    }
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
