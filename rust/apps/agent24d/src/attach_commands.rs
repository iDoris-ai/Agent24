//! `POST /api/v1/os/{name}/commands/{command}` — the A3 reverse-command
//! surface (`docs/design/A3-ATTACHED-MODULE.md` §6, PR slice A3-3): the ONE
//! place the kernel originates a request on an attached module's connection
//! instead of merely answering one.
//!
//! Everything needed to REACH the module — the live [`crate::attach_registry::AttachRegistry`]
//! bookkeeping (declared `host_commands`, the current connection's
//! `KernelCalls` handle, the in-flight cap) — already lives in
//! `crate::attach_registry` (A3-2b's registry, extended there for this PR);
//! this file is the axum-facing translation of §6.1's REST mapping table,
//! nothing more. Sits behind the SAME bearer auth every other kernel route
//! does (`crate::server::auth`, applied after this router merges in, same as
//! `crate::attached_routes`) — an attached module's OWN token (minted at
//! `POST /api/v1/attached`) is never accepted here; the two are not the same
//! credential (§3.2).
//!
//! # Step order (§6.1)
//!
//! 1. Does `name` name a registered attached module at all? No → `404`.
//! 2. Does its manifest's `host_commands` declare `command`? No → `403`,
//!    **zero frames sent** — checked before the body is even read.
//! 3. Is the body a JSON object, ≤64 KiB? No → `400`.
//! 4. Is there a live (Running) generation for `name` right now, and is the
//!    in-flight cap ([`agent24_os_proto::attach_mux::MAX_KERNEL_CALLS_IN_FLIGHT`])
//!    not yet reached? No → `503 module_not_ready` / `429 busy` (also zero
//!    frames sent for the busy case).
//! 5. Send `_a24/command/invoke {name, body}` on that connection, wait up to
//!    [`agent24_os_proto::attach_mux::COMMAND_TIMEOUT`], and map the outcome
//!    per the table below (M5).

use agent24_domain::http::{error_response, read_body_or_response};
use agent24_os_proto::attach_mux::{COMMAND_METHOD, COMMAND_TIMEOUT, KernelCallFailed};
use agent24_os_proto::rpc::ErrorKind;
use agent24_protocol::{ErrorBody, ErrorEnvelope};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

use crate::attach_registry::{CommandLookupError, CommandRefusal};
use crate::server::AppState;

/// §6.1: "body 是 JSON 对象且 ≤64 KiB" — independent of, and much smaller
/// than, [`agent24_domain::http::MAX_BODY_BYTES`] (1 MiB), which
/// [`read_body_or_response`] already enforces upstream of this check. A
/// `413`-vs-`400` distinction is not drawn here: both are the caller sending
/// more than this endpoint accepts, and §6.1's table gives this whole branch
/// exactly one status, `400`.
const MAX_COMMAND_BODY_BYTES: usize = 64 * 1024;

pub async fn post_command(
    State(state): State<AppState>,
    Path((name, command)): Path<(String, String)>,
    req: Request<Body>,
) -> Response {
    // Steps ①/②: an unknown module or an undeclared command must cost
    // nothing more than a map lookup, and an undeclared command must never
    // reach the module — `declared_command` never touches the connection.
    if let Err(refusal) = state.attach_registry.declared_command(&name, &command) {
        return match refusal {
            CommandLookupError::UnknownModule => error_response(
                StatusCode::NOT_FOUND,
                "not_found",
                &format!("no attached module named {name:?}"),
            ),
            CommandLookupError::CommandNotDeclared => error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                &format!("{name:?}'s manifest does not declare {command:?} in host_commands"),
            ),
        };
    }

    // Step ③: a JSON object, ≤64 KiB.
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    if bytes.len() > MAX_COMMAND_BODY_BYTES {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("command body exceeds {MAX_COMMAND_BODY_BYTES} bytes"),
        );
    }
    let body: Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid JSON body: {e}"),
            );
        }
    };
    if !body.is_object() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "command body must be a JSON object",
        );
    }

    // Step ④ + the in-flight cap: reserved atomically with the readiness
    // check by `reserve_ready` (one registry-lock acquisition) — see its own
    // doc for why that atomicity matters.
    let slot = match state.attach_registry.reserve_ready(&name) {
        Ok(slot) => slot,
        Err(CommandRefusal::NotReady) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "module_not_ready",
                &format!("{name:?} is not currently attached"),
            );
        }
        Err(CommandRefusal::Busy) => {
            return error_response(
                StatusCode::TOO_MANY_REQUESTS,
                "busy",
                &format!("{name:?} already has the maximum number of commands in flight"),
            );
        }
    };

    // Step ⑤: `_a24/command/invoke {name, body}`, `body` verbatim (the
    // kernel does not interpret its business shape — §6.1) — 5s.
    let params = json!({ "name": command, "body": body });
    let outcome = slot
        .calls()
        .call(COMMAND_METHOD, params, COMMAND_TIMEOUT)
        .await;
    drop(slot);
    match outcome {
        Ok(result) => (
            StatusCode::OK,
            Json(json!({ "result": Value::Object(result) })),
        )
            .into_response(),
        Err(KernelCallFailed::Timeout) => error_response(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the module did not answer within the 5s deadline; the outcome is unknown",
        ),
        Err(KernelCallFailed::ConnectionLost) => error_response(
            StatusCode::BAD_GATEWAY,
            "connection_lost",
            "the connection ended after the command was sent; the outcome is unknown",
        ),
        // §6.1: "未写出（连接已断/代已撤销）" — sent nowhere, so unlike
        // `ConnectionLost` this outcome IS known: the module never saw it.
        Err(KernelCallFailed::NotSent) => error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "module_not_ready",
            "the connection ended before the command could be sent",
        ),
        Err(KernelCallFailed::Rpc(e)) => {
            module_error_response(Some(e.code), e.kind.map(ErrorKind::as_str), &e.message)
        }
        // §6.1 M5: every malformed shape (bad `error`, neither/both of
        // `result`/`error`, non-object `result`) collapses to the SAME fixed
        // message and no `rpc_code` — `why` (the specific reason) is kept
        // only as a `details.reason` breadcrumb for operators, never
        // substituted into `message` itself.
        Err(KernelCallFailed::Malformed(why)) => {
            module_error_response_with_reason(None, None, "malformed response", Some(&why))
        }
    }
}

/// §6.1 M5's `502 module_error` shape: `code`/`message` as usual, plus
/// `rpc_code`/`kind` (when known) folded into `details` — the extension
/// point [`agent24_protocol::ErrorBody::details`] already reserves for
/// exactly this, rather than adding two new top-level fields to a struct
/// every other kernel route also produces.
fn module_error_response(rpc_code: Option<i32>, kind: Option<&str>, message: &str) -> Response {
    module_error_response_with_reason(rpc_code, kind, message, None)
}

fn module_error_response_with_reason(
    rpc_code: Option<i32>,
    kind: Option<&str>,
    message: &str,
    reason: Option<&str>,
) -> Response {
    let mut details = Map::new();
    if let Some(code) = rpc_code {
        details.insert("rpc_code".to_owned(), json!(code));
    }
    if let Some(kind) = kind {
        details.insert("kind".to_owned(), json!(kind));
    }
    if let Some(reason) = reason {
        details.insert("reason".to_owned(), json!(reason));
    }
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: "module_error".to_owned(),
            message: message.to_owned(),
            hint: None,
            details: if details.is_empty() {
                None
            } else {
                Some(details)
            },
        },
    };
    (StatusCode::BAD_GATEWAY, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::body::to_bytes;

    async fn body_json(r: Response) -> (StatusCode, serde_json::Value) {
        let status = r.status();
        let bytes = to_bytes(r.into_body(), 64 * 1024).await.unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
        )
    }

    fn req(body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn an_unregistered_module_is_404() {
        let state = crate::server::tests::state().await;
        let r = post_command(
            State(state),
            Path(("nobody".to_owned(), "speak".to_owned())),
            req(serde_json::json!({})),
        )
        .await;
        let (status, body) = body_json(r).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn a_body_over_the_64kib_cap_is_400_even_for_a_registered_module_and_command() {
        let state = crate::server::tests::state().await;
        state
            .attach_registry
            .on_change(&crate::attached::Change::Registered {
                name: "agentear",
                rotated: false,
                manifest: &agent24_domain::DomainOsManifest::from_yaml(
                    "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
                 event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
                 impl_kind: attached_process\nkernel_capabilities: [events]\n\
                 host_commands: [speak]\n",
                )
                .unwrap(),
                manifest_digest: "sha256:deadbeef",
                token_sha256_hex: &"ab".repeat(32),
                token_id: "tok_1",
            });
        let big = "x".repeat(MAX_COMMAND_BODY_BYTES + 1);
        let oversized = Request::builder()
            .method("POST")
            .body(Body::from(format!(r#"{{"pad":"{big}"}}"#)))
            .unwrap();
        let r = post_command(
            State(state),
            Path(("agentear".to_owned(), "speak".to_owned())),
            oversized,
        )
        .await;
        let (status, body) = body_json(r).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn a_non_object_body_is_400() {
        let state = crate::server::tests::state().await;
        state
            .attach_registry
            .on_change(&crate::attached::Change::Registered {
                name: "agentear",
                rotated: false,
                manifest: &agent24_domain::DomainOsManifest::from_yaml(
                    "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
                 event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
                 impl_kind: attached_process\nkernel_capabilities: [events]\n\
                 host_commands: [speak]\n",
                )
                .unwrap(),
                manifest_digest: "sha256:deadbeef",
                token_sha256_hex: &"ab".repeat(32),
                token_id: "tok_1",
            });
        let array_body = Request::builder()
            .method("POST")
            .body(Body::from("[1,2,3]"))
            .unwrap();
        let r = post_command(
            State(state),
            Path(("agentear".to_owned(), "speak".to_owned())),
            array_body,
        )
        .await;
        let (status, body) = body_json(r).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"]["code"], "invalid_request");
    }

    #[tokio::test]
    async fn an_undeclared_command_is_403_and_a_declared_one_with_no_live_connection_is_503() {
        let state = crate::server::tests::state().await;
        state
            .attach_registry
            .on_change(&crate::attached::Change::Registered {
                name: "agentear",
                rotated: false,
                manifest: &agent24_domain::DomainOsManifest::from_yaml(
                    "name: agentear\nversion: \"1\"\nroute_namespace: /api/v1/agentear\n\
                 event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
                 impl_kind: attached_process\nkernel_capabilities: [events]\n\
                 host_commands: [speak]\n",
                )
                .unwrap(),
                manifest_digest: "sha256:deadbeef",
                token_sha256_hex: &"ab".repeat(32),
                token_id: "tok_1",
            });

        let undeclared = post_command(
            State(state.clone()),
            Path(("agentear".to_owned(), "record".to_owned())),
            req(serde_json::json!({})),
        )
        .await;
        let (status, body) = body_json(undeclared).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(body["error"]["code"], "forbidden");

        let not_attached = post_command(
            State(state),
            Path(("agentear".to_owned(), "speak".to_owned())),
            req(serde_json::json!({})),
        )
        .await;
        let (status, body) = body_json(not_attached).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"]["code"], "module_not_ready");
    }

    #[test]
    fn module_error_response_omits_rpc_code_when_none_is_known() {
        let r = module_error_response(None, None, "malformed response");
        assert_eq!(r.status(), StatusCode::BAD_GATEWAY);
    }
}
