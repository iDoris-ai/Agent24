//! Exclusive local gateway transport. Inference locality comes from the response.
use super::*;
use crate::router::{Complexity, Served, TaskProfile, Tier};
use reqwest::header::{AUTHORIZATION, HeaderValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdorisEvidence {
    pub record_id: String,
    pub served_locality: String,
    pub cached: bool,
    pub origin_record_id: Option<String>,
}

pub(crate) struct IdorisClient {
    base: reqwest::Url,
    key: HeaderValue,
    client: reqwest::Client,
    request_timeout: Duration,
}

#[derive(Deserialize)]
struct Identity {
    service: String,
    version: String,
    contract_version: String,
    instance_id: String,
    dev_no_key_enabled: bool,
}

fn invalid(message: &str) -> ModelError {
    ModelError::Provider(message.into())
}

fn response_error(error: ModelError) -> ModelError {
    match error {
        ModelError::Cancelled => ModelError::Cancelled,
        ModelError::Unavailable(_) => ModelError::Unavailable("iDoris unavailable".into()),
        _ => invalid("invalid iDoris response"),
    }
}

impl IdorisClient {
    pub(crate) fn from_env() -> Result<Self, String> {
        let raw = std::env::var("IDORIS_URL").map_err(|_| "invalid IDORIS_URL")?;
        let base = reqwest::Url::parse(&raw).map_err(|_| "invalid IDORIS_URL")?;
        if !crate::router::is_loopback_url(base.as_str())
            || !base.username().is_empty()
            || base.password().is_some()
            || base.query().is_some()
            || base.fragment().is_some()
            || base.path() != "/"
        {
            return Err("IDORIS_URL must be a loopback HTTP(S) origin".into());
        }
        let secret = std::env::var("IDORIS_API_KEY").map_err(|_| "missing iDoris data key")?;
        if !secret.starts_with("idk_") || secret.len() <= 4 {
            return Err("iDoris requires a virtual data key, not an Admin token".into());
        }
        let mut key = HeaderValue::from_str(&format!("Bearer {secret}"))
            .map_err(|_| "invalid iDoris data key")?;
        key.set_sensitive(true);
        let client = base_client_builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(120))
            .build()
            .map_err(|_| "cannot build iDoris client")?;
        Ok(Self {
            base,
            key,
            client,
            request_timeout: Duration::from_secs(120),
        })
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        cancel: &CancellationToken,
    ) -> Result<reqwest::Response, ModelError> {
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
            response = request.send() =>
                response.map_err(|_| ModelError::Unavailable("iDoris unavailable".into()))?,
        };
        let status = response.status();
        if !status.is_success() {
            if status.is_server_error() || status.as_u16() == 429 {
                return Err(ModelError::Unavailable(format!(
                    "iDoris unavailable (HTTP {})",
                    status.as_u16()
                )));
            }
            return Err(ModelError::Rejected {
                status: status.as_u16(),
                message: format!("iDoris refused the request (HTTP {})", status.as_u16()),
            });
        }
        Ok(response)
    }

    fn endpoint(&self, path: &str) -> reqwest::Url {
        let mut url = self.base.clone();
        url.set_path(path);
        url
    }

    async fn identity(&self, cancel: &CancellationToken) -> Result<(), ModelError> {
        let response = self
            .send(
                self.client
                    .get(self.endpoint("/health"))
                    .timeout(Duration::from_secs(2)),
                cancel,
            )
            .await?;
        let identity: Identity = read_json_capped(response, 4096, cancel, "iDoris")
            .await
            .map_err(response_error)?;
        if identity.service != "idoris"
            || identity.contract_version != "1.0.1"
            || identity.version.is_empty()
            || identity.instance_id.is_empty()
            || identity.dev_no_key_enabled
        {
            return Err(invalid("iDoris identity or security mode mismatch"));
        }
        Ok(())
    }

    pub(crate) async fn complete(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
        request_id: Option<&str>,
    ) -> Result<Served, ModelError> {
        tokio::time::timeout(
            self.request_timeout,
            self.complete_inner(profile, req, cancel, request_id),
        )
        .await
        .map_err(|_| ModelError::Unavailable("iDoris request timed out".into()))?
    }

    async fn complete_inner(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
        request_id: Option<&str>,
    ) -> Result<Served, ModelError> {
        self.identity(cancel).await?;
        let (complexity, role) = match profile.complexity {
            Complexity::Simple => ("simple", "idoris/daily"),
            Complexity::Complex => ("complex", "idoris/deep"),
        };
        // This first consumer supports local-only execution; Any may tighten to local.
        let body = completion_body(req, role, true);
        let response = self
            .send(
                self.client
                    .post(self.endpoint("/v1/chat/completions"))
                    .header(AUTHORIZATION, self.key.clone())
                    .header("X-iDoris-Privacy", "local_only")
                    .header("X-iDoris-Complexity", complexity)
                    .header(
                        "X-iDoris-Request-Id",
                        request_id
                            .map(str::to_owned)
                            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string()),
                    )
                    .json(&body),
                cancel,
            )
            .await?;
        let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok());
        if header("x-idoris-served-locality") != Some("loopback") {
            return Err(invalid("iDoris did not attest loopback inference"));
        }
        let record_id = header("x-idoris-record-id")
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or_else(|| invalid("iDoris response has no valid record ID"))?
            .to_owned();
        let cached = match header("x-idoris-cached") {
            None | Some("false") => false,
            Some("true") => true,
            _ => return Err(invalid("invalid iDoris cache evidence")),
        };
        let origin_record_id = if cached {
            Some(
                header("x-idoris-origin-record-id")
                    .filter(|id| {
                        !id.is_empty()
                            && id.len() <= 128
                            && id
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    })
                    .ok_or_else(|| invalid("cached iDoris response has no valid origin record ID"))?
                    .to_owned(),
            )
        } else {
            None
        };
        let parsed = read_json_capped(response, MAX_CHAT_RESPONSE_BYTES, cancel, "iDoris")
            .await
            .map_err(response_error)?;
        let response = completion_response(parsed, "iDoris").map_err(response_error)?;
        if response.model_id.is_none() {
            return Err(invalid("iDoris response has no actual model identity"));
        }
        tracing::info!(idoris_record_id = %record_id, served_locality = "loopback", actual_model_id = ?response.model_id, cached, origin_record_id = ?origin_record_id, "iDoris completion");
        Ok(Served {
            provider: "idoris-local".into(),
            tier: Tier::Local,
            response,
            idoris: Some(IdorisEvidence {
                record_id,
                served_locality: "loopback".into(),
                cached,
                origin_record_id,
            }),
        })
    }

    pub(crate) async fn models(
        &self,
        cancel: &CancellationToken,
    ) -> Result<Vec<Model>, ModelError> {
        self.identity(cancel).await?;
        let response = self
            .send(
                self.client
                    .get(self.endpoint("/v1/models"))
                    .header(AUTHORIZATION, self.key.clone())
                    .timeout(Duration::from_secs(2)),
                cancel,
            )
            .await?;
        #[derive(Deserialize)]
        struct Inventory {
            data: Vec<OaModelEntry>,
        }
        let parsed: Inventory =
            read_json_capped(response, MAX_MODELS_RESPONSE_BYTES, cancel, "iDoris")
                .await
                .map_err(response_error)?;
        Ok(parsed
            .data
            .into_iter()
            .map(|model| Model {
                id: model.id,
                provider: "idoris-local".into(),
                tier: "local".into(),
                // An inventory entry is not evidence that the backend model is loaded.
                loaded: false,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::router::Privacy;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn hanging_gateway() -> (
        IdorisClient,
        tokio::sync::oneshot::Receiver<()>,
        tokio::task::JoinHandle<()>,
    ) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base =
            reqwest::Url::parse(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        let (sent, received) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let mut sent = Some(sent);
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                let mut buf = [0_u8; 4096];
                loop {
                    let n = socket.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    data.extend_from_slice(&buf[..n]);
                    if let Some(end) = data.windows(4).position(|s| s == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&data[..end]);
                        let len = headers
                            .lines()
                            .find_map(|s| {
                                s.split_once(':')
                                    .filter(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                                    .and_then(|(_, v)| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if data.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                if data.starts_with(b"POST ") {
                    sent.take().unwrap().send(()).unwrap();
                    std::future::pending::<()>().await;
                }
                let body = r#"{"service":"idoris","version":"0.3.0","contract_version":"1.0.1","instance_id":"fixture","dev_no_key_enabled":false}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
        });
        let gateway = IdorisClient {
            base,
            key: HeaderValue::from_static("Bearer idk_fixture"),
            client: base_client_builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            request_timeout: Duration::from_secs(1),
        };
        (gateway, received, server)
    }

    async fn pending_call(
        cancel: CancellationToken,
        gateway: IdorisClient,
    ) -> Result<Served, ModelError> {
        let request = CompletionRequest {
            messages: vec![Msg::user("fixture")],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
            disable_thinking: false,
        };
        gateway
            .complete(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    complexity: Complexity::Simple,
                },
                &request,
                &cancel,
                None,
            )
            .await
    }

    #[tokio::test]
    async fn cancellation_after_real_post_terminates_pending_response() {
        let (gateway, received, server) = hanging_gateway().await;
        let cancel = CancellationToken::new();
        let task = tokio::spawn(pending_call(cancel.clone(), gateway));
        received.await.unwrap();
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert!(matches!(result, Err(ModelError::Cancelled)));
    }

    #[tokio::test]
    async fn full_request_deadline_terminates_after_real_post() {
        let (gateway, received, server) = hanging_gateway().await;
        let task = tokio::spawn(pending_call(CancellationToken::new(), gateway));
        received.await.unwrap();
        let result = tokio::time::timeout(Duration::from_secs(3), task)
            .await
            .unwrap()
            .unwrap();
        server.abort();
        assert!(
            matches!(result,Err(ModelError::Unavailable(message)) if message=="iDoris request timed out")
        );
    }
}
