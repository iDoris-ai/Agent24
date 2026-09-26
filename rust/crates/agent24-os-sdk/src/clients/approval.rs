//! ME4-S3 §2.4/§2.10/§3.5 — `ApprovalClient` (`_a24/approval/{gate,advise,
//! status}`). See §2.10 for the `advise`-specific constraints (submit inside
//! the proxied request's own handler, same-request-only retry, orphan
//! compensation) this client's documentation passes on to callers.

use std::sync::Arc;

use agent24_os_proto::module::Connection;
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Core, malformed_result, set_opt};
use crate::context::{ApprovalToken, RequestId};
use crate::error::ClientError;

pub const GATE: &str = "_a24/approval/gate";
pub const ADVISE: &str = "_a24/approval/advise";
pub const STATUS: &str = "_a24/approval/status";
pub(crate) const METHODS: [&str; 3] = [GATE, ADVISE, STATUS];
const OFFER_PREFIX: &str = "_a24/approval/";

/// Which of the two submit methods produced/answers an
/// [`ApprovalAnswer`] — carried explicitly so a caller can never mistake an
/// `advise` result for a `gate` one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalKind {
    Gate,
    Advise,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    Pending,
    Approved,
    Denied,
    TimedOut,
}

/// What `gate`/`advise`/`status` all return.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ApprovalAnswer {
    pub approval_id: String,
    pub kind: ApprovalKind,
    /// `false` for `Advise`; `true` for `Gate`.
    pub binding: bool,
    pub decision: ApprovalDecision,
    pub executed_at: Option<String>,
}

/// The submit shape shared by `gate` and `advise`.
pub struct ApprovalSubmit<'a> {
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub payload: Value,
    pub request_id: &'a RequestId,
    pub approval_token: &'a ApprovalToken,
}

pub struct ApprovalClient(Core);

impl ApprovalClient {
    #[must_use]
    pub fn new(conn: &Arc<Connection>) -> Option<Self> {
        Core::new(conn, OFFER_PREFIX).map(Self)
    }

    /// Kernel-executed closed set only (today: `schedule_callback`).
    ///
    /// # Errors
    /// See [`ClientError`].
    pub async fn gate(&self, s: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> {
        self.submit(GATE, s).await
    }

    /// A module-domain action (§2.10): call INSIDE the proxied request's own
    /// handler; resubmitting the same `{request_id, approval_token}` is
    /// idempotent at the wire.
    ///
    /// # Errors
    /// See [`ClientError`].
    pub async fn advise(&self, s: &ApprovalSubmit<'_>) -> Result<ApprovalAnswer, ClientError> {
        self.submit(ADVISE, s).await
    }

    async fn submit(
        &self,
        method: &'static str,
        s: &ApprovalSubmit<'_>,
    ) -> Result<ApprovalAnswer, ClientError> {
        let mut params = Map::new();
        params.insert("action".to_owned(), Value::String(s.action.to_owned()));
        set_opt(
            &mut params,
            "target",
            s.target.map(|t| Value::String(t.to_owned())),
        );
        params.insert("payload".to_owned(), s.payload.clone());
        params.insert(
            "request_id".to_owned(),
            Value::String(s.request_id.as_str().to_owned()),
        );
        params.insert(
            "approval_token".to_owned(),
            Value::String(s.approval_token.as_str().to_owned()),
        );
        let value = self.0.call(method, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("approval answer", e))
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn status(&self, approval_id: &str) -> Result<ApprovalAnswer, ClientError> {
        let mut params = Map::new();
        params.insert(
            "approval_id".to_owned(),
            Value::String(approval_id.to_owned()),
        );
        let value = self.0.call(STATUS, Value::Object(params)).await?;
        serde_json::from_value(value).map_err(|e| malformed_result("approval answer", e))
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::testing;

    #[tokio::test]
    async fn without_the_offer_prefix_new_returns_none() {
        let (conn, _peer) = testing::fake_kernel(vec![]).await;
        assert!(ApprovalClient::new(&conn).is_none());
    }

    #[tokio::test]
    async fn advise_sends_the_submit_shape_and_parses_the_answer() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = ApprovalClient::new(&conn).expect("offer covers approval");
        let request_id = RequestId::for_test("req-1");
        let token = approval_token_for_test("secret-1");
        let submit = ApprovalSubmit {
            action: "award_points",
            target: None,
            payload: json!({"award_id": "a1"}),
            request_id: &request_id,
            approval_token: &token,
        };
        // `tokio::join!`, not `tokio::spawn`: `submit` borrows locals that
        // are not `'static`, and `spawn` requires its future to be.
        let (answer, ()) = tokio::join!(client.advise(&submit), async {
            let req = testing::read_request(&mut peer).await;
            assert_eq!(req["method"], ADVISE);
            assert_eq!(req["params"]["action"], "award_points");
            assert!(req["params"].as_object().unwrap().get("target").is_none());
            assert_eq!(req["params"]["request_id"], "req-1");
            assert_eq!(req["params"]["approval_token"], "secret-1");
            testing::respond(
                &mut peer,
                &req,
                json!({
                    "approval_id": "appr-1",
                    "kind": "advise",
                    "binding": false,
                    "decision": "pending",
                    "executed_at": null,
                }),
            )
            .await;
        });
        let answer = answer.unwrap();
        assert_eq!(answer.approval_id, "appr-1");
        assert_eq!(answer.kind, ApprovalKind::Advise);
        assert_eq!(answer.decision, ApprovalDecision::Pending);
    }

    #[tokio::test]
    async fn status_sends_approval_id_and_parses_the_answer() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = ApprovalClient::new(&conn).expect("offer covers approval");
        let call = tokio::spawn(async move { client.status("appr-2").await });
        let req = testing::read_request(&mut peer).await;
        assert_eq!(req["method"], STATUS);
        assert_eq!(req["params"]["approval_id"], "appr-2");
        testing::respond(
            &mut peer,
            &req,
            json!({
                "approval_id": "appr-2",
                "kind": "gate",
                "binding": true,
                "decision": "approved",
                "executed_at": "2026-01-01T00:00:00Z",
            }),
        )
        .await;
        let answer = call.await.unwrap().unwrap();
        assert_eq!(answer.kind, ApprovalKind::Gate);
        assert_eq!(answer.decision, ApprovalDecision::Approved);
        assert_eq!(answer.executed_at.as_deref(), Some("2026-01-01T00:00:00Z"));
    }

    /// Test-only construction of an [`ApprovalToken`] — the normal API has
    /// none (it only ever comes from [`crate::RequestContext::from_headers`]).
    fn approval_token_for_test(token: &str) -> ApprovalToken {
        let headers_name = agent24_os_proto::proxy::APPROVAL_TOKEN_HEADER;
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            headers_name,
            axum::http::HeaderValue::from_str(token).unwrap(),
        );
        crate::RequestContext::from_headers(&headers)
            .approval_token
            .unwrap()
    }
}
