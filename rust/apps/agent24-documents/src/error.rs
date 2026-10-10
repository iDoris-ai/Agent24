//! The OS's error envelope (ADR-DOC-02 §6):
//! `{"error": {"code", "message", "details": {"retryable", ...}}}`.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Map, Value, json};

/// The closed §6 code set (`DocumentsErrorCode`).
pub const CODES: &[&str] = &[
    "invalid_request",
    "not_found",
    "payload_too_large",
    "unsupported_format",
    "parse_failed",
    "partial_parse",
    "idempotency_key_reused",
    "change_set_incomplete",
    "upload_checksum_mismatch",
    "knowledge_disabled",
    "revision_conflict",
    "change_set_stale",
    "change_set_committed",
    "upload_offset_mismatch",
    "stale_index",
    "permission_denied",
    "knowledge_unavailable",
    "engine_unavailable",
    "storage_unavailable",
];

/// Why storage cannot serve requests (§6 `storage_unavailable.details.cause`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageCause {
    Locked,
    Busy,
    Corrupt,
    NotWritable,
    DiskFull,
}

impl StorageCause {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            StorageCause::Locked => "locked",
            StorageCause::Busy => "busy",
            StorageCause::Corrupt => "corrupt",
            StorageCause::NotWritable => "not_writable",
            StorageCause::DiskFull => "disk_full",
        }
    }

    /// Only lock contention (`locked`, `busy`) clears by itself; the rest
    /// need the user (#814 review).
    #[must_use]
    pub fn retryable(self) -> bool {
        matches!(self, StorageCause::Locked | StorageCause::Busy)
    }
}

/// One error response. Build it with the per-code constructors so each code
/// carries the status, `retryable` and details §6 gives it.
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: String,
    details: Map<String, Value>,
}

impl ApiError {
    fn new(status: StatusCode, code: &'static str, message: String, retryable: bool) -> Self {
        let mut details = Map::new();
        details.insert("retryable".into(), Value::Bool(retryable));
        Self {
            status,
            code,
            message,
            details,
        }
    }

    #[must_use]
    pub fn storage_unavailable(cause: StorageCause) -> Self {
        let mut e = Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "storage_unavailable",
            format!("document storage is unavailable ({})", cause.as_str()),
            cause.retryable(),
        );
        e.details.insert("cause".into(), cause.as_str().into());
        e
    }

    #[must_use]
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            message.into(),
            false,
        )
    }

    #[must_use]
    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", message.into(), false)
    }

    /// 413: a body over the route's limit.
    #[must_use]
    pub fn payload_too_large(limit: usize) -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "payload_too_large",
            format!("the body is over {limit} bytes"),
            false,
        )
    }

    /// 409: a chunk that neither appends nor replays (§5.6). `received_offset`
    /// is the upload's `received`, which 0001's CHECK keeps ≥ 0.
    #[must_use]
    pub fn upload_offset_mismatch(received_offset: i64) -> Self {
        let mut e = Self::new(
            StatusCode::CONFLICT,
            "upload_offset_mismatch",
            format!("the upload has {received_offset} bytes; send the chunk at that offset"),
            false,
        );
        e.details
            .insert("received_offset".into(), received_offset.into());
        e
    }

    /// 503: the read engine is not here (Linux, D10), busy, or still
    /// building the text layer; retry later (§6, §3.1).
    #[must_use]
    pub fn engine_unavailable(engine: &str, message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "engine_unavailable",
            message.into(),
            true,
        )
        .with_detail("engine", engine.to_owned())
    }

    /// 422 with a code from the closed set: a request the OS understood but
    /// cannot carry out, e.g. `unsupported_format` (§6).
    #[must_use]
    pub fn unprocessable(code: &'static str, message: impl Into<String>) -> Self {
        debug_assert!(CODES.contains(&code), "{code} is not in the closed set");
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            code,
            message.into(),
            false,
        )
    }

    /// 422: the same key was sent before with a different request (§5.4).
    #[must_use]
    pub fn idempotency_key_reused() -> Self {
        Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "idempotency_key_reused",
            "this Idempotency-Key was used for a different request".into(),
            false,
        )
    }

    /// 500 for a failure the OS did not expect (a bug, not storage). Not in
    /// the closed §6 set: it is the kernel's generic `internal` code, in the
    /// envelope of the documents 500 response (`ModuleProxyError`), and
    /// clients treat it by §6's default rule (outcome unknown, resend).
    #[must_use]
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            message.into(),
            false,
        )
    }

    /// Adds a code-specific detail, e.g. `invalid_request` with `status`.
    #[must_use]
    pub fn with_detail(mut self, key: &str, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = json!({
            "error": { "code": self.code, "message": self.message, "details": self.details }
        });
        (self.status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    async fn body(e: ApiError) -> (StatusCode, Value) {
        let res = e.into_response();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn storage_unavailable_carries_its_cause_and_retries_only_lock_contention() {
        for (cause, name, retryable) in [
            (StorageCause::Locked, "locked", true),
            (StorageCause::Busy, "busy", true),
            (StorageCause::Corrupt, "corrupt", false),
            (StorageCause::NotWritable, "not_writable", false),
            (StorageCause::DiskFull, "disk_full", false),
        ] {
            let (status, v) = body(ApiError::storage_unavailable(cause)).await;
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(v["error"]["code"], "storage_unavailable");
            assert_eq!(
                v["error"]["details"],
                json!({ "retryable": retryable, "cause": name })
            );
            assert!(v["error"]["message"].is_string());
        }
    }

    #[tokio::test]
    async fn client_errors_are_never_retryable() {
        let (status, v) = body(ApiError::invalid_request("bad")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            v,
            json!({ "error": { "code": "invalid_request", "message": "bad", "details": { "retryable": false } } })
        );
        let (status, v) = body(ApiError::not_found("gone")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(v["error"]["code"], "not_found");
        assert_eq!(v["error"]["details"], json!({ "retryable": false }));
    }

    /// `CODES` is the OpenAPI `DocumentsErrorCode` enum, read from the spec.
    #[test]
    fn the_closed_code_set_matches_the_contract() {
        let spec = std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../../protocol/openapi.yaml"
        ))
        .unwrap();
        let start = spec.find("    DocumentsErrorCode:").unwrap();
        let block = &spec[start..];
        let listed: Vec<&str> = block
            .lines()
            .skip_while(|l| l.trim() != "enum:")
            .skip(1)
            .map_while(|l| l.trim().strip_prefix("- "))
            .collect();
        assert_eq!(listed, CODES);
    }

    #[tokio::test]
    async fn a_detail_is_added_beside_retryable() {
        let e = ApiError::invalid_request("x").with_detail("status", "queued");
        let (_, v) = body(e).await;
        assert_eq!(
            v["error"]["details"],
            json!({ "retryable": false, "status": "queued" })
        );
    }

    #[tokio::test]
    async fn an_offset_mismatch_is_409_with_the_received_offset_and_a_big_body_413() {
        let (status, v) = body(ApiError::upload_offset_mismatch(7)).await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(v["error"]["code"], "upload_offset_mismatch");
        assert_eq!(
            v["error"]["details"],
            json!({ "retryable": false, "received_offset": 7 })
        );
        let (status, v) = body(ApiError::payload_too_large(786_432)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(v["error"]["code"], "payload_too_large");
    }

    #[tokio::test]
    async fn an_unprocessable_request_is_422_with_its_code() {
        let (status, v) = body(ApiError::unprocessable("unsupported_format", "x")).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["error"]["code"], "unsupported_format");
        assert_eq!(v["error"]["details"], json!({ "retryable": false }));
    }

    #[tokio::test]
    async fn a_reused_key_is_422_and_an_unexpected_failure_is_500_internal() {
        let (status, v) = body(ApiError::idempotency_key_reused()).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(v["error"]["code"], "idempotency_key_reused");
        assert_eq!(v["error"]["details"], json!({ "retryable": false }));
        let (status, v) = body(ApiError::internal("boom")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(v["error"]["code"], "internal");
        assert_eq!(v["error"]["message"], "boom");
    }
}
