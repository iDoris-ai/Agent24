//! COMM-2a: the REST layer's closed error set (COMM-HYPHAE.md §4).
//!
//! Every comm route returns exactly one of these — never a bespoke shape —
//! so a caller (the CLI, the desktop UI, a test) can match on the `error`
//! field alone. The JSON body is always Hyphae's own envelope shape,
//! `{"ok":false,"error":...,"message":...,"data"?}`, even for errors comm
//! raises itself before ever invoking Hyphae (a bad npub, an unconfigured
//! binary): one shape everywhere, not "Hyphae's shape, except when comm
//! short-circuits".

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::binary::BinaryError;
use crate::password_store::StoreError;
use crate::runner::RunnerError;

/// §4's closed set, one variant per row of that table.
#[derive(Debug, thiserror::Error)]
pub enum CommError {
    #[error("binary_rejected: {0}")]
    BinaryRejected(String),
    /// No platform other than darwin/linux-{x64,arm64} is reachable yet
    /// (§9 R2) — nothing in COMM-2a constructs this today. Kept so the
    /// Windows gate (a later task) has a variant ready, and so the set this
    /// enum models stays the same seven-plus-error-shapes the design
    /// documents rather than a COMM-2a-shaped subset of it.
    #[error("unsupported_platform")]
    #[allow(dead_code)]
    UnsupportedPlatform,
    #[error("locked: {0}")]
    Locked(String),
    #[error("not_configured: {0}")]
    NotConfigured(String),
    #[error("invalid: {0}")]
    Invalid(String),
    #[error("not_found: {0}")]
    #[allow(
        dead_code,
        reason = "no COMM-2a route looks an id up before acting (that is COMM-2b/3's import/outbox work); kept so this closed set matches §4 exactly"
    )]
    NotFound(String),
    #[error("confirm_required")]
    ConfirmRequired,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("network: {0}")]
    Network(String),
    #[error("partial: {0}")]
    Partial(Value),
    #[error("timeout")]
    Timeout,
    #[error("upstream: {0}")]
    Upstream(String),
}

impl CommError {
    fn parts(&self) -> (StatusCode, &'static str, String, Option<Value>) {
        match self {
            CommError::BinaryRejected(m) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "binary_rejected",
                m.clone(),
                None,
            ),
            CommError::UnsupportedPlatform => (
                StatusCode::NOT_IMPLEMENTED,
                "unsupported_platform",
                "this platform is not supported yet".to_owned(),
                None,
            ),
            CommError::Locked(m) => (StatusCode::LOCKED, "locked", m.clone(), None),
            CommError::NotConfigured(m) => {
                (StatusCode::CONFLICT, "not_configured", m.clone(), None)
            }
            CommError::Invalid(m) => (StatusCode::BAD_REQUEST, "invalid", m.clone(), None),
            CommError::NotFound(m) => (StatusCode::NOT_FOUND, "not_found", m.clone(), None),
            CommError::ConfirmRequired => (
                StatusCode::BAD_REQUEST,
                "confirm_required",
                "this operation requires confirm:true".to_owned(),
                None,
            ),
            CommError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m.clone(), None),
            CommError::Network(m) => (StatusCode::BAD_GATEWAY, "network", m.clone(), None),
            CommError::Partial(data) => (
                StatusCode::BAD_GATEWAY,
                "partial",
                "hyphae reported a partial result".to_owned(),
                Some(data.clone()),
            ),
            CommError::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "hyphae did not respond in time; the underlying command may still have \
                 taken effect"
                    .to_owned(),
                None,
            ),
            CommError::Upstream(m) => (StatusCode::BAD_GATEWAY, "upstream", m.clone(), None),
        }
    }
}

impl IntoResponse for CommError {
    fn into_response(self) -> Response {
        let (status, error, message, data) = self.parts();
        let mut body = json!({"ok": false, "error": error, "message": message});
        if let Some(data) = data {
            body["data"] = data;
        }
        (status, Json(body)).into_response()
    }
}

/// Maps a finished Hyphae invocation's failure half to the closed set, per
/// §4's "来源" column. Order matters: `auth_error`/`user_error`/
/// `write_conflict` are classified by name regardless of whether they carry
/// `data` (the table gives them no such qualifier); everything else that
/// carries `data` is `partial` (data is transported, never dropped); what's
/// left — `network_error`/`other_error`/anything unrecognized, all without
/// data — falls to `network` or `upstream`.
pub fn map_envelope_failure(error: &str, message: &str, data: Option<Value>) -> CommError {
    match error {
        "auth_error" => return CommError::Locked(message.to_owned()),
        "user_error" => return CommError::Invalid(message.to_owned()),
        "write_conflict" => return CommError::Conflict(message.to_owned()),
        _ => {}
    }
    if let Some(data) = data {
        return CommError::Partial(data);
    }
    match error {
        "network_error" => CommError::Network(message.to_owned()),
        _ => CommError::Upstream(message.to_owned()),
    }
}

/// [`RunnerError`] never reaches Hyphae's own envelope (it means the
/// invocation itself never produced one) — §4's `timeout` and `upstream`
/// rows are the only ones that apply.
pub fn map_runner_error(err: RunnerError) -> CommError {
    match err {
        RunnerError::Timeout(_) => CommError::Timeout,
        RunnerError::Signaled
        | RunnerError::UnknownExit(_)
        | RunnerError::BadEnvelope { .. }
        | RunnerError::OutputTooLarge
        | RunnerError::PasswordLength(_)
        | RunnerError::Spawn(_)
        | RunnerError::Io(_) => CommError::Upstream(err.to_string()),
    }
}

/// §4: `binary_rejected` is the ONLY row [`BinaryError`] maps to — comm
/// reports "no binary was ever configured" separately, as `not_configured`
/// (see `router::CommState::unconfigured`), because that is a different
/// fact from "a binary was named and failed verification".
pub fn map_binary_error(err: BinaryError) -> CommError {
    CommError::BinaryRejected(err.to_string())
}

/// Not a row of its own in §4: a password comm cannot retrieve is the same
/// externally-visible fact as a password Hyphae itself rejected — both mean
/// the caller cannot act on the keystore right now.
pub fn map_store_error(err: StoreError) -> CommError {
    match err {
        StoreError::NotFound => {
            CommError::Locked("no password is on file for this keystore".to_owned())
        }
        StoreError::Unavailable(reason) => {
            CommError::Locked(format!("keychain_unavailable: {reason}"))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn auth_error_is_locked_even_with_data() {
        let err = map_envelope_failure("auth_error", "bad password", Some(json!({"x": 1})));
        assert!(matches!(err, CommError::Locked(m) if m == "bad password"));
    }

    #[test]
    fn user_error_is_invalid() {
        let err = map_envelope_failure("user_error", "bad arg", None);
        assert!(matches!(err, CommError::Invalid(m) if m == "bad arg"));
    }

    #[test]
    fn write_conflict_is_conflict() {
        let err = map_envelope_failure("write_conflict", "raced", None);
        assert!(matches!(err, CommError::Conflict(_)));
    }

    #[test]
    fn network_error_without_data_is_network() {
        let err = map_envelope_failure("network_error", "down", None);
        assert!(matches!(err, CommError::Network(_)));
    }

    #[test]
    fn network_error_with_data_is_partial() {
        let data = json!({"event_id": "abc", "published_to": 0});
        let err = map_envelope_failure("network_error", "down", Some(data.clone()));
        assert!(matches!(err, CommError::Partial(d) if d == data));
    }

    #[test]
    fn other_error_without_data_is_upstream() {
        let err = map_envelope_failure("other_error", "?", None);
        assert!(matches!(err, CommError::Upstream(_)));
    }

    #[test]
    fn status_codes_match_the_closed_set() {
        let cases: &[(CommError, u16)] = &[
            (CommError::BinaryRejected("x".into()), 503),
            (CommError::UnsupportedPlatform, 501),
            (CommError::Locked("x".into()), 423),
            (CommError::NotConfigured("x".into()), 409),
            (CommError::Invalid("x".into()), 400),
            (CommError::NotFound("x".into()), 404),
            (CommError::ConfirmRequired, 400),
            (CommError::Conflict("x".into()), 409),
            (CommError::Network("x".into()), 502),
            (CommError::Partial(json!({})), 502),
            (CommError::Timeout, 504),
            (CommError::Upstream("x".into()), 502),
        ];
        for (err, expected) in cases {
            let (status, ..) = err.parts();
            assert_eq!(status.as_u16(), *expected, "{err:?}");
        }
    }
}
