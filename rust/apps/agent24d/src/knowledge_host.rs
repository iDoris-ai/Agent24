//! The daemon side of the private Knowledge host interface (KB-HOST-01 PR-B,
//! #1085 design v2 §2). Only the desktop's ProductHost capability may use it.
//! This part activates and deactivates Knowledge bindings; handing the daemon
//! the short-lived credential of an active binding comes next (PR-B1b).
//!
//! Every record belongs to the ProductHost capability that wrote it (decided
//! 2026-10-24): a lookup always names the capability it is made for, so a
//! binding cannot be found by its id alone (design v2 §3).
//!
//! Each change carries a sequence number from the host that must be greater
//! than any it sent before for that binding. The high-water mark outlives
//! deactivation, so a change that arrives after a later one cannot undo it. A
//! host that does not know whether a change landed sends the state it wants
//! again under a new number.

use std::collections::HashMap;
use std::sync::Mutex;

use agent24_protocol::state_file::AuthMode;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::capabilities::{Audience, Authorization};
use crate::server::{AppState, error_response};

/// Bindings one host may hold records for, deactivated ones included.
pub const MAX_BINDINGS_PER_HOST: usize = 1024;
const MAX_BINDING_ID_BYTES: usize = 256;

#[derive(Clone, Default)]
struct Entry {
    seq: u64,
    active: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Activate,
    Deactivate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeError {
    /// `seq` is not greater than one this host already sent for the binding.
    StaleSequence,
    TooManyBindings,
    Unavailable,
}

/// Records keyed by (host capability id, binding id).
#[derive(Default)]
pub struct KnowledgeHostRegistry {
    entries: Mutex<HashMap<(String, String), Entry>>,
}

impl KnowledgeHostRegistry {
    /// Stores the change only when `seq` is fresh, so a refused change leaves
    /// no trace.
    pub fn apply(
        &self,
        host: &str,
        binding: &str,
        seq: u64,
        change: Change,
    ) -> Result<(), ChangeError> {
        let mut entries = self.entries.lock().map_err(|_| ChangeError::Unavailable)?;
        let key = (host.to_owned(), binding.to_owned());
        let seen = match entries.get(&key) {
            Some(entry) => entry.seq,
            None if entries.keys().filter(|(owner, _)| owner == host).count()
                >= MAX_BINDINGS_PER_HOST =>
            {
                return Err(ChangeError::TooManyBindings);
            }
            None => 0,
        };
        if seq <= seen {
            return Err(ChangeError::StaleSequence);
        }
        let active = change == Change::Activate;
        entries.insert(key, Entry { seq, active });
        Ok(())
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
/// into one after routing.
fn valid_binding_id(value: &str) -> bool {
    !value.is_empty()
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

#[derive(Deserialize)]
pub struct SeqQuery {
    seq: u64,
}

/// `PUT /api/v1/host/knowledge/bindings/{binding_id}?seq=n` activates the
/// binding, `DELETE` of the same deactivates it.
pub async fn change(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    method: Method,
    Path(binding_id): Path<String>,
    query: Result<Query<SeqQuery>, QueryRejection>,
) -> Response {
    let Some(host) = host_capability(&state, authorization) else {
        return error_response(
            StatusCode::FORBIDDEN,
            "host_authority_required",
            "Only the product host may use this interface",
        );
    };
    let change = if method == Method::PUT {
        Change::Activate
    } else {
        Change::Deactivate
    };
    let (Ok(Query(SeqQuery { seq })), true) = (query, valid_binding_id(&binding_id)) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "Malformed request",
        );
    };
    change_response(state.knowledge_host.apply(&host, &binding_id, seq, change))
}

#[cfg(test)]
mod tests;
