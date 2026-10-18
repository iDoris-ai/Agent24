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
//! Slice A has no operation yet; every `op` is `operation_unavailable`.

use axum::Json;
use axum::body::Bytes;
use axum::extract::rejection::{BytesRejection, PathRejection};
use axum::extract::{Path as UrlPath, State};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::state::AppState;

/// The input does not have the shape the kernel sends (kernel code table,
/// `agent24-domain/src/tool.rs`).
pub const INVALID_INPUT: &str = "invalid_input";
/// No such operation in this build: a manifest and router out of step.
pub const OPERATION_UNAVAILABLE: &str = "operation_unavailable";

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
    /// Accepted and not used in slice 1 (DOC-1-06b §3.2): in production it
    /// is always empty, so it is neither an allow-list nor a check.
    #[serde(default)]
    pub authorized_resources: Vec<String>,
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
    State(state): State<AppState>,
    op: Result<UrlPath<String>, PathRejection>,
    body: Result<Bytes, BytesRejection>,
) -> Json<Envelope> {
    let Ok(UrlPath(op)) = op else {
        tracing::info!(code = OPERATION_UNAVAILABLE, "documents: tool call");
        return Json(Envelope::error(OPERATION_UNAVAILABLE, false));
    };
    let call = body
        .ok()
        .and_then(|b| serde_json::from_slice::<ToolCall>(&b).ok());
    let Some(call) = call else {
        // Never the body: it may carry document content.
        tracing::info!(op = ?op, code = INVALID_INPUT, "documents: tool call");
        return Json(Envelope::error(INVALID_INPUT, false));
    };
    let (run_id, tool_call_id) = (
        call.context.run_id.clone(),
        call.context.tool_call_id.clone(),
    );
    let envelope = dispatch(&state, &op, call).await;
    tracing::info!(
        op = ?op,
        run_id = ?run_id,
        tool_call_id = ?tool_call_id,
        code = envelope.code(),
        "documents: tool call"
    );
    Json(envelope)
}

/// One arm per tool from slice B on (`match op { "list" => …, … }`), each
/// calling the same service layer as its REST route.
async fn dispatch(state: &AppState, op: &str, call: ToolCall) -> Envelope {
    let _ = (state, op, call);
    Envelope::error(OPERATION_UNAVAILABLE, false)
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

    /// Posts `body` to `op` and returns the raw response body, after checking
    /// the status is 2xx.
    async fn post(op: &str, body: impl Into<Body>) -> Vec<u8> {
        let res = crate::router(AppState::unavailable(StorageCause::Busy))
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
        // Parsed, so it reaches dispatch: no operation exists yet.
        assert_kernel_error(&body, ModuleToolErrorCode::OperationUnavailable);
    }

    #[tokio::test]
    async fn authorization_fields_are_accepted_and_not_used() {
        let mut ctx = context();
        ctx["authorized_resources"] = json!(["doc_01"]);
        ctx["authorization_ref"] = json!("auth-2");
        let body = post(
            "no_such_op",
            json!({ "context": ctx, "arguments": {} }).to_string(),
        )
        .await;
        // Parsed (not `invalid_input`) and not an allow-list: it reaches
        // dispatch like any other call.
        assert_kernel_error(&body, ModuleToolErrorCode::OperationUnavailable);
    }

    #[tokio::test]
    async fn an_unknown_operation_is_operation_unavailable() {
        let call = json!({ "context": context(), "arguments": {} }).to_string();
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
}
