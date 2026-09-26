//! ME4-S3 §2.6/§3.4 — the closed error set every client call returns, and
//! its two mappings: proto's connection-level [`agent24_os_proto::module::
//! CallError`] and the kernel's application-level `data.kind` (carried
//! inside `CallError::Rpc`'s `RpcErrorInfo`).
//!
//! The 18 kernel `kind`s (`agent24_os_proto::rpc::ErrorKind::ALL`) map to a
//! dedicated variant each, except: `auth_failed`/`manifest_mismatch`
//! (handshake-only — a module never sees them on an ordinary call) and
//! `invalid_lease`/`unknown_capability`/`version_mismatch` (no client here
//! ever sends the shapes that provoke them) fall to [`ClientError::Other`].
//! `-32602` (JSON-RPC's own invalid-params code, not a `kind`) maps to
//! [`ClientError::InvalidParams`]. `timeout` with `data.retryable == false`
//! is [`ClientError::RequestNotInFlight`] rather than [`ClientError::
//! Timeout`] — "the id you gave has already ended" is a different fact than
//! "no answer arrived in time" (§2.6).

use agent24_os_proto::module::CallError;
use serde_json::Value;

/// The kernel's `data.cause` for `unavailable` (ME4-S2 decision M6), as an
/// SDK-owned closed set (not the domain-coupled type Sin90 carried).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableCause {
    NoProvider,
    RequestRejected,
    BackendConfig,
    ResponseTooLarge,
}

impl UnavailableCause {
    fn from_str(s: &str) -> Option<Self> {
        match s {
            "no_provider" => Some(Self::NoProvider),
            "request_rejected" => Some(Self::RequestRejected),
            "backend_config" => Some(Self::BackendConfig),
            "response_too_large" => Some(Self::ResponseTooLarge),
            _ => None,
        }
    }
}

/// Every way a client call can fail: never `#[non_exhaustive]` (§8 Q6) — a
/// caller that writes a wildcard-free `match` over this gets a compile error
/// the day a new kernel `kind` is mapped in, which is the point.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ClientError {
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("rate limited: {0}")]
    RateLimited(String),
    #[error("busy: {0}")]
    Busy(String),
    #[error("quota exceeded: {0}")]
    QuotaExceeded(String),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("timeout: {0}")]
    Timeout(String),
    #[error("request not in flight: {0}")]
    RequestNotInFlight(String),
    #[error("not ready: {0}")]
    NotReady(String),
    #[error("draining: {0}")]
    Draining(String),
    #[error("revoked: {0}")]
    Revoked(String),
    #[error("token invalid: {0}")]
    TokenInvalid(String),
    #[error("payload too large: {0}")]
    PayloadTooLarge(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("callback connection was lost; outcome unknown")]
    ConnectionLost,
    #[error("not sent: {0}")]
    NotSent(String),
    #[error("unavailable (retryable={retryable}): {cause:?}")]
    Unavailable {
        retryable: bool,
        cause: UnavailableCause,
    },
    #[error("cancelled")]
    Cancelled,
    #[error("{0}")]
    Other(String),
}

impl ClientError {
    /// Ported verbatim from Sin90 `adapter_agent24/clients/error.rs`'s
    /// `is_permanent` (spec.md M3): retrying the exact same request can
    /// never turn this into success. Callers (the outbox / reconciler) flip
    /// the row to `failed` and surface it, rather than retrying forever.
    ///
    /// [`Self::Unavailable`] is the one exception to "fixed per variant": it
    /// carries the WIRE'S OWN `retryable` bool, so this reads that field
    /// directly (`!retryable`) instead of a hardcoded verdict — exactly one
    /// of [`Self::is_permanent`]/[`Self::is_retryable`] is true for it, same
    /// invariant every other variant keeps.
    ///
    /// Every other variant not named below (`ConnectionLost`, `Revoked`,
    /// `NotFound`, `RequestNotInFlight`, `Cancelled`, `Other`) is neither
    /// permanent nor retryable — Sin90 does not guess for them, the caller
    /// decides (see each variant's own doc upstream).
    #[must_use]
    pub fn is_permanent(&self) -> bool {
        if let Self::Unavailable { retryable, .. } = self {
            return !retryable;
        }
        matches!(
            self,
            Self::Forbidden(_)
                | Self::QuotaExceeded(_)
                | Self::InvalidParams(_)
                | Self::TokenInvalid(_)
                | Self::PayloadTooLarge(_)
        )
    }

    /// Ported verbatim from Sin90's `is_retryable` (spec.md M3): a transient
    /// condition — backoff and try again is the right response, PROVIDED
    /// the call being retried is itself idempotent. This predicate does not
    /// and cannot check that — it only says "the kernel-side or
    /// connection-side condition that failed this call is the kind that
    /// goes away on its own," not "it is safe for THIS caller to resend
    /// THIS request."
    ///
    /// [`Self::ConnectionLost`] is deliberately `false` here (and in
    /// [`Self::is_permanent`]): the call may have already executed
    /// server-side before the response was lost, so the outcome is
    /// genuinely unknown — the same posture as `Timeout` deserves a
    /// callout for (it IS retryable, but a retry only helps if the caller's
    /// action is idempotent).
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        if let Self::Unavailable { retryable, .. } = self {
            return *retryable;
        }
        matches!(
            self,
            Self::RateLimited(_)
                | Self::Busy(_)
                | Self::Timeout(_)
                | Self::NotReady(_)
                | Self::Draining(_)
                | Self::NotSent(_)
        )
    }
}

/// `-32602`, JSON-RPC's own code (not an application `kind`).
const INVALID_PARAMS_CODE: i64 = -32602;

impl From<CallError> for ClientError {
    fn from(e: CallError) -> Self {
        // Computed before the match so every non-`Rpc` arm can reuse
        // `CallError`'s own `Display` text without re-deriving it.
        let text = e.to_string();
        match e {
            CallError::NotSent => Self::NotSent(text),
            CallError::ConnectionLost => Self::ConnectionLost,
            CallError::Busy => Self::Busy(text),
            CallError::FrameTooLarge => Self::PayloadTooLarge(text),
            CallError::Timeout => Self::Timeout(text),
            CallError::IdCollision => Self::Other(text),
            CallError::Rpc(info) => {
                if info.code == INVALID_PARAMS_CODE {
                    return Self::InvalidParams(info.message);
                }
                let retryable_false = matches!(
                    info.data.as_ref().and_then(|d| d.get("retryable")),
                    Some(Value::Bool(false))
                );
                match info.kind.as_deref() {
                    Some("forbidden") => Self::Forbidden(info.message),
                    Some("busy") => Self::Busy(info.message),
                    Some("cancelled") => Self::Cancelled,
                    Some("timeout") if retryable_false => Self::RequestNotInFlight(info.message),
                    Some("timeout") => Self::Timeout(info.message),
                    Some("quota_exceeded") => Self::QuotaExceeded(info.message),
                    Some("not_ready") => Self::NotReady(info.message),
                    Some("draining") => Self::Draining(info.message),
                    Some("revoked") => Self::Revoked(info.message),
                    Some("rate_limited") => Self::RateLimited(info.message),
                    Some("payload_too_large") => Self::PayloadTooLarge(info.message),
                    Some("token_invalid") => Self::TokenInvalid(info.message),
                    Some("not_found") => Self::NotFound(info.message),
                    Some("unavailable") => unavailable_from_data(&info.message, info.data.as_ref()),
                    // `invalid_lease` / `unknown_capability` / `version_mismatch`
                    // (no client here provokes them), `auth_failed` /
                    // `manifest_mismatch` (handshake-only), or no kind at
                    // all (a protocol-level JSON-RPC error): Sin90's "don't
                    // guess" rule (L4) — anything not positively identified
                    // falls here rather than being forced into a specific
                    // variant.
                    _ => Self::Other(info.message),
                }
            }
        }
    }
}

/// `unavailable` requires BOTH a legal `cause` and a boolean `retryable` —
/// missing or malformed either falls to `Other` rather than guessing (Sin90
/// L4).
fn unavailable_from_data(message: &str, data: Option<&Value>) -> ClientError {
    let cause = data
        .and_then(|d| d.get("cause"))
        .and_then(Value::as_str)
        .and_then(UnavailableCause::from_str);
    let retryable = data
        .and_then(|d| d.get("retryable"))
        .and_then(Value::as_bool);
    match (cause, retryable) {
        (Some(cause), Some(retryable)) => ClientError::Unavailable { retryable, cause },
        _ => ClientError::Other(message.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_os_proto::module::RpcErrorInfo;
    use serde_json::json;

    fn rpc(code: i64, kind: Option<&str>, data: Option<Value>) -> CallError {
        CallError::Rpc(RpcErrorInfo {
            code,
            kind: kind.map(str::to_owned),
            message: "m".to_owned(),
            data,
        })
    }

    /// Nit (external review of #516): driven from `agent24_os_proto::rpc::
    /// ErrorKind::ALL` — the source of truth for "the 18 kernel kinds" this
    /// module's doc comment and `From<CallError>` both reference — instead
    /// of a hand-copied array of kind strings, so a kind ADDED to `ALL`
    /// without a mapping decision here fails this test instead of silently
    /// going untested. Each arm's comment mirrors this module's top-level
    /// doc comment for why that kind maps where it does.
    fn expected_check(kind: agent24_os_proto::rpc::ErrorKind) -> fn(ClientError) -> bool {
        use agent24_os_proto::rpc::ErrorKind;
        match kind {
            ErrorKind::Forbidden => |e| matches!(e, ClientError::Forbidden(_)),
            ErrorKind::Busy => |e| matches!(e, ClientError::Busy(_)),
            ErrorKind::Cancelled => |e| matches!(e, ClientError::Cancelled),
            ErrorKind::Timeout => |e| matches!(e, ClientError::Timeout(_)),
            ErrorKind::QuotaExceeded => |e| matches!(e, ClientError::QuotaExceeded(_)),
            // No client here ever sends the shapes that provoke these three,
            // and the handshake-only pair below is never seen on an ordinary
            // call — both fall to `Other` (module doc comment, L4 "don't
            // guess").
            ErrorKind::InvalidLease
            | ErrorKind::UnknownCapability
            | ErrorKind::VersionMismatch
            | ErrorKind::AuthFailed
            | ErrorKind::ManifestMismatch => |e| matches!(e, ClientError::Other(_)),
            ErrorKind::NotReady => |e| matches!(e, ClientError::NotReady(_)),
            ErrorKind::Draining => |e| matches!(e, ClientError::Draining(_)),
            ErrorKind::Revoked => |e| matches!(e, ClientError::Revoked(_)),
            ErrorKind::RateLimited => |e| matches!(e, ClientError::RateLimited(_)),
            ErrorKind::PayloadTooLarge => |e| matches!(e, ClientError::PayloadTooLarge(_)),
            ErrorKind::TokenInvalid => |e| matches!(e, ClientError::TokenInvalid(_)),
            ErrorKind::NotFound => |e| matches!(e, ClientError::NotFound(_)),
            // No `cause`/`retryable` data on this bare call:
            // `unavailable_from_data` documents that as falling to `Other`
            // rather than guessing; the WITH-data cases are covered
            // separately by `unavailable_needs_both_cause_and_retryable_else_other`.
            ErrorKind::Unavailable => |e| matches!(e, ClientError::Other(_)),
        }
    }

    // J-S4: every one of the 18 kernel kinds maps somewhere sane, plus the
    // three non-kind special cases.
    #[test]
    fn all_eighteen_kernel_kinds_map_to_a_documented_variant() {
        for kind in agent24_os_proto::rpc::ErrorKind::ALL {
            let mapped = ClientError::from(rpc(-32000, Some(kind.as_str()), None));
            let check = expected_check(kind);
            assert!(
                check(mapped.clone()),
                "kind {} mapped to {mapped:?}",
                kind.as_str()
            );
        }
    }

    #[test]
    fn invalid_params_code_wins_regardless_of_kind() {
        let mapped = ClientError::from(rpc(-32602, None, None));
        assert!(matches!(mapped, ClientError::InvalidParams(_)));
    }

    #[test]
    fn timeout_with_retryable_false_is_request_not_in_flight() {
        let mapped = ClientError::from(rpc(
            -32000,
            Some("timeout"),
            Some(json!({"retryable": false})),
        ));
        assert!(matches!(mapped, ClientError::RequestNotInFlight(_)));
    }

    #[test]
    fn timeout_without_retryable_false_stays_timeout() {
        let mapped = ClientError::from(rpc(-32000, Some("timeout"), None));
        assert!(matches!(mapped, ClientError::Timeout(_)));
    }

    #[test]
    fn unavailable_needs_both_cause_and_retryable_else_other() {
        let full = ClientError::from(rpc(
            -32000,
            Some("unavailable"),
            Some(json!({"cause": "no_provider", "retryable": true})),
        ));
        assert_eq!(
            full,
            ClientError::Unavailable {
                retryable: true,
                cause: UnavailableCause::NoProvider
            }
        );

        for bad in [
            json!({"retryable": true}),
            json!({"cause": "no_provider"}),
            json!({"cause": "not_a_real_cause", "retryable": true}),
            json!({"cause": "no_provider", "retryable": "yes"}),
        ] {
            let mapped = ClientError::from(rpc(-32000, Some("unavailable"), Some(bad.clone())));
            assert!(
                matches!(mapped, ClientError::Other(_)),
                "{bad:?} must not be guessed at, got {mapped:?}"
            );
        }
    }

    #[test]
    fn connection_level_errors_map_directly() {
        assert_eq!(
            ClientError::from(CallError::ConnectionLost),
            ClientError::ConnectionLost
        );
        assert!(matches!(
            ClientError::from(CallError::NotSent),
            ClientError::NotSent(_)
        ));
        assert!(matches!(
            ClientError::from(CallError::FrameTooLarge),
            ClientError::PayloadTooLarge(_)
        ));
    }

    /// The full classification table, one row per variant — ported from
    /// Sin90 `adapter_agent24/clients/error.rs`'s
    /// `classification_matches_spec_md_m3_exactly`. spec.md M3: permanent =
    /// forbidden/quota_exceeded/invalid_params/token_invalid/
    /// payload_too_large; retryable = rate_limited/busy/timeout/not_ready/
    /// draining/not_sent; connection_lost/revoked/not_found/
    /// request_not_in_flight/cancelled/other are neither. `Unavailable`
    /// reads the wire's own `retryable` bool instead of a fixed verdict.
    #[test]
    fn classification_matches_sin90_error_rs_exactly() {
        let cases: &[(ClientError, bool, bool)] = &[
            (ClientError::Forbidden("x".into()), true, false),
            (ClientError::QuotaExceeded("x".into()), true, false),
            (ClientError::InvalidParams("x".into()), true, false),
            (ClientError::TokenInvalid("x".into()), true, false),
            (ClientError::PayloadTooLarge("x".into()), true, false),
            (ClientError::RateLimited("x".into()), false, true),
            (ClientError::Busy("x".into()), false, true),
            (ClientError::Timeout("x".into()), false, true),
            (ClientError::NotReady("x".into()), false, true),
            (ClientError::Draining("x".into()), false, true),
            (ClientError::NotSent("x".into()), false, true),
            (ClientError::ConnectionLost, false, false),
            (ClientError::Revoked("x".into()), false, false),
            (ClientError::NotFound("x".into()), false, false),
            (ClientError::RequestNotInFlight("x".into()), false, false),
            (ClientError::Other("x".into()), false, false),
            (ClientError::Cancelled, false, false),
            (
                ClientError::Unavailable {
                    retryable: false,
                    cause: UnavailableCause::BackendConfig,
                },
                true,
                false,
            ),
            (
                ClientError::Unavailable {
                    retryable: true,
                    cause: UnavailableCause::NoProvider,
                },
                false,
                true,
            ),
        ];
        for (err, want_permanent, want_retryable) in cases {
            assert_eq!(
                err.is_permanent(),
                *want_permanent,
                "{err:?}.is_permanent()"
            );
            assert_eq!(
                err.is_retryable(),
                *want_retryable,
                "{err:?}.is_retryable()"
            );
            // No variant is ever BOTH — the two predicates must stay
            // mutually exclusive as the table grows.
            assert!(
                !(err.is_permanent() && err.is_retryable()),
                "{err:?} is both"
            );
        }
    }

    /// Mutation guard ported from Sin90: if `is_retryable` accidentally
    /// classified `quota_exceeded` as retryable (e.g. someone "fixes" a
    /// perceived gap by adding it to the retryable `matches!` arm), this
    /// goes red. Kept as its own test so it survives even if the table
    /// above's shape changes later.
    #[test]
    fn quota_exceeded_must_never_be_retryable() {
        assert!(!ClientError::QuotaExceeded("x".into()).is_retryable());
        assert!(ClientError::QuotaExceeded("x".into()).is_permanent());
    }
}
