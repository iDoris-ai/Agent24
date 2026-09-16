//! The v1 HTTP envelope a mounted domain OS must speak (ME-1b).
//!
//! [`DomainModule::routes`](crate::DomainModule::routes) means a module's
//! handlers answer on the kernel's own HTTP surface, so a client should not have
//! to tell from the wire whether it is talking to the kernel or to a module. That
//! needs both sides to produce the same error shape and enforce the same body
//! limit — which makes these part of the contract rather than kernel-private
//! helpers. They are the shared DEFAULT, not an enforced one: a module's handler
//! that builds its own response bypasses them, and no trait can stop it.
//!
//! Before ME-1b these lived in `agent24d` (`server::error_response`,
//! `routes::read_body_or_response`, plus a THIRD open-coded body reader inside
//! `post_chat`) and Sin90 reached across into the kernel crate for them. A module
//! in its own crate cannot do that, and copies drift. Those three were DELETED,
//! not duplicated: the kernel now re-exports these, so there is exactly one
//! definition and the compiler enforces it.

use agent24_protocol::{ErrorBody, ErrorEnvelope};
use axum::Json;
use axum::body::{Body, Bytes};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};

/// The body cap for handlers that use [`read_body_or_response`] — every one in
/// the kernel today, and the limit a module is expected to adopt.
///
/// It is a KERNEL policy rather than a per-module choice: a module that raised it
/// would be spending the daemon's memory, and one that lowered it would answer a
/// different 413 threshold on the same surface. Nothing FORCES a module's handlers
/// through this helper, though — an axum extractor with its own limit would
/// bypass it. This is the shared default, not an enforced ceiling.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;

/// Build the v1 error envelope: `{ "error": { code, message } }`.
pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            hint: None,
            details: None,
        },
    };
    (status, Json(body)).into_response()
}

/// Build the v1 error envelope with a `hint` (ERR-1): `{ "error": { code,
/// message, hint } }`. A separate function rather than a new parameter on
/// [`error_response`] — that one has dozens of call sites that do not need a
/// hint, and forcing each to pass `None` is noise, not clarity.
pub fn error_response_with_hint(
    status: StatusCode,
    code: &str,
    message: &str,
    hint: &str,
) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            hint: Some(hint.to_owned()),
            details: None,
        },
    };
    (status, Json(body)).into_response()
}

/// The FU-64/ERR-1 canonical restart instruction, shared verbatim by every
/// response that tells an operator to restart the daemon (`GaveUp`,
/// `Stopped`, `PackageChanged`, `StopFailed` — both the proxy path and the
/// two control-plane call sites). One constant rather than six near-copies:
/// this line drifting apart across call sites is exactly the failure the
/// design review caught (FU-64/ERR-1 design doc, Codex round 4 Medium 1).
///
/// `agent24 service status` prints `loaded: yes`/`loaded: no`
/// ([`service::status`](../../agent24-cli/src/service.rs) via `agent24d`'s
/// dependents) — that is the field this sentence tells the reader to check,
/// not an abstract "is it managed". `gui/$(id -u)/ai.auraai.agent24` is the
/// real `launchctl` service target this daemon's own plist installs under
/// (`service.rs`'s `LABEL` and its `gui/{uid}/{LABEL}` target format) — not a
/// placeholder, so it can be copied and run as-is. `agent24 daemon stop &&
/// agent24 daemon start` is unsafe for a launchd-managed install (it detaches
/// the new process from launchd's supervision, tracked as FU-68) and must
/// never appear unconditionally — every caller of this constant keeps the
/// leading "check `agent24 service status` first" clause attached.
pub const RESTART_DAEMON_INSTRUCTION: &str = "restart the daemon — first run `agent24 service status`: if it says `loaded: yes`, run `launchctl kickstart -k gui/$(id -u)/ai.auraai.agent24`; if it says `loaded: no`, run `agent24 daemon stop && agent24 daemon start`";

/// Read a request body, capped at [`MAX_BODY_BYTES`].
///
/// The length-limit case is distinguished from every other read failure by
/// walking the error's `source` chain for `LengthLimitError`, because axum
/// reports both through the same error type — conflating them would answer 400
/// for an oversized upload, which reads as "your JSON is malformed" and sends
/// the caller looking in the wrong place.
// `Err` is the shared v1-envelope `Response`, which is the whole point of this
// helper — boxing it to shrink the `Result` would just move the allocation and
// make every call site unwrap it again.
//
// The allow travelled with the function from `agent24d::routes` in ME-1b-a and
// was DROPPED in the move. It fires only on clippy 1.98+ (which extended
// `result_large_err` to `async fn` return types), so a local 1.95 reported a
// clean workspace while CI failed — and it then blocked three consecutive PRs,
// each of which I had checked locally and believed green. Note it here rather
// than re-learning it a fourth time.
#[allow(clippy::result_large_err)]
pub async fn read_body_or_response(req: Request<Body>) -> Result<Bytes, Response> {
    match axum::body::to_bytes(req.into_body(), MAX_BODY_BYTES).await {
        Ok(b) => Ok(b),
        Err(err) => {
            let mut source: Option<&(dyn std::error::Error + 'static)> = Some(&err);
            let mut is_limit = false;
            while let Some(e) = source {
                if e.is::<http_body_util::LengthLimitError>() {
                    is_limit = true;
                    break;
                }
                source = e.source();
            }
            Err(if is_limit {
                error_response(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "payload_too_large",
                    &format!("Request body exceeds {MAX_BODY_BYTES} bytes"),
                )
            } else {
                error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_request",
                    "failed to read request body",
                )
            })
        }
    }
}

/// The response a mounted module's namespace serves when the kernel could not
/// bring it up — its [`open_store`](crate::DomainModule::open_store) failed, or
/// its directory could not be prepared.
///
/// The kernel builds this itself rather than trusting the module to: a module
/// whose store is gone is exactly the one least able to answer correctly, and the
/// trait cannot enforce that it would. The message deliberately does NOT name the
/// cause — the operator gets that from the mount log, and a client on the far side
/// of the auth boundary gets "this module is down", which is all it can act on.
pub fn module_unavailable(module: &str) -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "module_unavailable",
        &format!("the {module} module is not available"),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::body::to_bytes;

    async fn body_json(r: Response) -> serde_json::Value {
        let bytes = to_bytes(r.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn error_response_is_the_v1_envelope() {
        let r = error_response(StatusCode::NOT_FOUND, "not_found", "nope");
        assert_eq!(r.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(r).await,
            serde_json::json!({"error": {"code": "not_found", "message": "nope"}})
        );
    }

    #[tokio::test]
    async fn an_oversized_body_is_413_not_400() {
        // The distinction is the whole point of the source-chain walk: a 400
        // would tell the caller their JSON is malformed and send them looking in
        // the wrong place.
        let req = Request::builder()
            .body(Body::from(vec![b'x'; MAX_BODY_BYTES + 1]))
            .unwrap();
        let err = read_body_or_response(req).await.unwrap_err();
        assert_eq!(err.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(body_json(err).await["error"]["code"], "payload_too_large");
    }

    #[tokio::test]
    async fn a_body_at_the_limit_is_read() {
        let req = Request::builder()
            .body(Body::from(vec![b'x'; MAX_BODY_BYTES]))
            .unwrap();
        let bytes = read_body_or_response(req).await.unwrap();
        assert_eq!(bytes.len(), MAX_BODY_BYTES);
    }

    #[tokio::test]
    async fn module_unavailable_names_the_module() {
        let r = module_unavailable("sin90");
        assert_eq!(r.status(), StatusCode::SERVICE_UNAVAILABLE);
        let j = body_json(r).await;
        assert_eq!(j["error"]["code"], "module_unavailable");
        assert!(
            j["error"]["message"].as_str().unwrap().contains("sin90"),
            "{j}"
        );
    }
}
