//! ME4-S3 §2.4/§2.7/§3.5 — `ModelClient` (`_a24/model/complete`). The one
//! client with its own response deadline: [`MODEL_RESPONSE_TIMEOUT`] (125s)
//! exceeds the kernel's own `MODEL_CALL_TIMEOUT` (120s) so the kernel's
//! specific answer — including `unavailable`'s cause — wins the race instead
//! of the connection's generic 35s fallback timing it out first.

use std::sync::Arc;
use std::time::Duration;

use agent24_os_proto::module::Connection;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::{Core, malformed_result, set_opt};
use crate::context::RequestId;
use crate::error::ClientError;

pub const COMPLETE: &str = "_a24/model/complete";
pub(crate) const METHODS: [&str; 1] = [COMPLETE];
const OFFER_PREFIX: &str = "_a24/model/";

/// Exceeds the kernel's own `_a24/model/complete` timeout (120s) so the
/// kernel's specific error reaches the caller before the connection's
/// generic fallback would (§2.7).
pub const MODEL_RESPONSE_TIMEOUT: Duration = Duration::from_secs(125);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModelMessage {
    pub role: ModelRole,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Complexity {
    Simple,
    Complex,
}

/// A JSON-schema-constrained response format request.
#[derive(Debug, Clone, PartialEq)]
pub struct JsonSchemaFormat {
    pub name: String,
    pub schema: Value,
    pub strict: bool,
}

pub struct CompleteRequest {
    pub messages: Vec<ModelMessage>,
    pub response_format: Option<JsonSchemaFormat>,
    pub max_tokens: Option<u32>,
    pub complexity: Option<Complexity>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServedTier {
    Local,
    Remote,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CompleteResult {
    pub text: String,
    pub model_id: Option<String>,
    pub tier: ServedTier,
    pub usage: Usage,
}

pub struct ModelClient(Core);

impl ModelClient {
    #[must_use]
    pub fn new(conn: &Arc<Connection>) -> Option<Self> {
        Core::new(conn, OFFER_PREFIX).map(Self)
    }

    /// # Errors
    /// See [`ClientError`].
    pub async fn complete(
        &self,
        req: &CompleteRequest,
        request_id: Option<&RequestId>,
    ) -> Result<CompleteResult, ClientError> {
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| {
                json!({
                    "role": m.role,
                    "content": m.content,
                })
            })
            .collect();
        let mut params = Map::new();
        params.insert("messages".to_owned(), Value::Array(messages));
        set_opt(
            &mut params,
            "response_format",
            req.response_format.as_ref().map(|f| {
                json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": f.name,
                        "schema": f.schema,
                        "strict": f.strict,
                    },
                })
            }),
        );
        set_opt(&mut params, "max_tokens", req.max_tokens.map(Value::from));
        set_opt(
            &mut params,
            "complexity",
            req.complexity
                .map(|c| serde_json::to_value(c).unwrap_or(Value::Null)),
        );
        set_opt(
            &mut params,
            "request_id",
            request_id.map(|id| Value::String(id.as_str().to_owned())),
        );
        let value = self
            .0
            .call_with_timeout(COMPLETE, Value::Object(params), MODEL_RESPONSE_TIMEOUT)
            .await?;
        serde_json::from_value(value).map_err(|e| malformed_result("model complete result", e))
    }
}

#[cfg(all(test, feature = "test-util"))]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::testing;

    #[tokio::test]
    async fn without_the_offer_prefix_new_returns_none() {
        let (conn, _peer) = testing::fake_kernel(vec![]).await;
        assert!(ModelClient::new(&conn).is_none());
    }

    #[tokio::test]
    async fn complete_sends_wire_shape_and_parses_the_result() {
        let (conn, mut peer) = testing::fake_kernel(vec![OFFER_PREFIX.to_owned()]).await;
        let client = ModelClient::new(&conn).expect("offer covers model");
        let req = CompleteRequest {
            messages: vec![ModelMessage {
                role: ModelRole::User,
                content: "hi".to_owned(),
            }],
            response_format: None,
            max_tokens: None,
            complexity: None,
        };
        let call = tokio::spawn(async move { client.complete(&req, None).await });
        let sent = testing::read_request(&mut peer).await;
        assert_eq!(sent["method"], COMPLETE);
        assert_eq!(sent["params"]["messages"][0]["role"], "user");
        assert_eq!(sent["params"]["messages"][0]["content"], "hi");
        assert!(
            sent["params"]
                .as_object()
                .unwrap()
                .get("max_tokens")
                .is_none()
        );
        assert!(
            sent["params"]
                .as_object()
                .unwrap()
                .get("complexity")
                .is_none()
        );
        testing::respond(
            &mut peer,
            &sent,
            json!({
                "text": "hello back",
                "model_id": "stub-7b",
                "tier": "local",
                "usage": {"prompt_tokens": 3, "completion_tokens": 2},
            }),
        )
        .await;
        let result = call.await.unwrap().unwrap();
        assert_eq!(result.text, "hello back");
        assert_eq!(result.tier, ServedTier::Local);
        assert_eq!(result.usage.prompt_tokens, 3);
    }
}
