//! ME4-S3 §3.3 — the per-request context a kernel-proxied HTTP request
//! carries into a module: the request id (correlates a call back to the
//! proxied request it happened inside) and the approval token (§2.10 —
//! `advise`/`gate` need it, and it is a secret).

use agent24_os_proto::proxy::{APPROVAL_TOKEN_HEADER, REQUEST_ID_HEADER};
use axum::extract::FromRequestParts;
use axum::http::HeaderMap;
use axum::http::request::Parts;

/// `X-A24-Request-Id` of the proxied request being handled. In the normal
/// API this can only come from [`RequestContext::from_headers`] — a module
/// cannot mint one and bind it to a request it was not actually handling.
#[derive(Clone, PartialEq, Eq)]
pub struct RequestId(String);

impl RequestId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Tests only (J-S7's all-`Some` wire-parity calls, and a module's own
    /// test-hooks routes): the normal API has no way to construct one out of
    /// thin air.
    #[cfg(feature = "test-util")]
    #[must_use]
    pub fn for_test(id: &str) -> Self {
        Self(id.to_owned())
    }
}

impl std::fmt::Debug for RequestId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("RequestId").field(&self.0).finish()
    }
}

/// `X-A24-Approval-Token`: a secret, redacted in `Debug` — never logged, and
/// never constructible outside [`RequestContext::from_headers`].
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalToken(String);

impl ApprovalToken {
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ApprovalToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ApprovalToken(<redacted>)")
    }
}

/// Kernel-injected per-request context. The extractor never fails: a request
/// the kernel did not proxy (or one calling a route this module registered
/// itself, outside `_a24/*`) simply has both fields `None`.
#[derive(Debug, Clone, Default)]
pub struct RequestContext {
    pub request_id: Option<RequestId>,
    pub approval_token: Option<ApprovalToken>,
}

impl RequestContext {
    #[must_use]
    pub fn from_headers(headers: &HeaderMap) -> Self {
        let request_id = headers
            .get(REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| RequestId(s.to_owned()));
        let approval_token = headers
            .get(APPROVAL_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(|s| ApprovalToken(s.to_owned()));
        Self {
            request_id,
            approval_token,
        }
    }
}

impl<S: Send + Sync> FromRequestParts<S> for RequestContext {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self::from_headers(&parts.headers))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn from_headers_reads_both_when_present() {
        let mut headers = HeaderMap::new();
        headers.insert(REQUEST_ID_HEADER, "req-1".parse().unwrap());
        headers.insert(APPROVAL_TOKEN_HEADER, "secret-token".parse().unwrap());
        let ctx = RequestContext::from_headers(&headers);
        assert_eq!(ctx.request_id.unwrap().as_str(), "req-1");
        assert_eq!(ctx.approval_token.unwrap().as_str(), "secret-token");
    }

    #[test]
    fn from_headers_is_none_when_absent_and_never_fails() {
        let ctx = RequestContext::from_headers(&HeaderMap::new());
        assert!(ctx.request_id.is_none());
        assert!(ctx.approval_token.is_none());
    }

    #[test]
    fn approval_token_debug_is_redacted() {
        let mut headers = HeaderMap::new();
        headers.insert(APPROVAL_TOKEN_HEADER, "very-secret".parse().unwrap());
        let ctx = RequestContext::from_headers(&headers);
        let shown = format!("{:?}", ctx.approval_token.unwrap());
        assert!(!shown.contains("very-secret"));
    }
}
