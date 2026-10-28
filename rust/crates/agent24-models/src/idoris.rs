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

/// iDoris `correlation.rs` rejects a trace id that is blank (`trim` empty),
/// longer than 128 UTF-16 code units, or contains a control character
/// (`INVALID_CORRELATION_TRACE_ID`). Omit the header instead of sending one.
const IDORIS_TRACE_ID_MAX_UTF16_UNITS: usize = 128;

fn correlation_trace_id(raw: &str) -> Option<&str> {
    if raw.trim().is_empty()
        || raw.encode_utf16().count() > IDORIS_TRACE_ID_MAX_UTF16_UNITS
        || raw.chars().any(char::is_control)
    {
        None
    } else {
        Some(raw)
    }
}

fn response_error(error: ModelError) -> ModelError {
    match error {
        ModelError::Cancelled => ModelError::Cancelled,
        ModelError::Unavailable(_) => ModelError::Unavailable("iDoris unavailable".into()),
        _ => invalid("invalid iDoris response"),
    }
}

/// Reads at most 4 KiB of an error body, cancellably, and returns only the
/// fixed `reason_code` (or `error.type`); nothing else from the body is ever
/// surfaced. Shared by the 503 and 502 handling.
async fn error_reason_code(
    mut response: reqwest::Response,
    cancel: &CancellationToken,
) -> Result<Option<String>, ModelError> {
    let read = async {
        let mut body = Vec::new();
        while body.len() < 4096 {
            match response.chunk().await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                _ => break,
            }
        }
        body.truncate(4096);
        body
    };
    let body = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(ModelError::Cancelled),
        body = read => body,
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return Ok(None);
    };
    Ok(value
        .pointer("/error/reason_code")
        .or_else(|| value.get("reason_code"))
        .or_else(|| value.pointer("/error/type"))
        .and_then(|v| v.as_str())
        .map(str::to_owned))
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
            let code = status.as_u16();
            if code == 503 || code == 502 {
                let reason = error_reason_code(response, cancel).await?;
                match (code, reason.as_deref()) {
                    (503, Some("local_only_unavailable")) => {
                        return Err(ModelError::Unavailable(
                            "no local model available in iDoris (local_only_unavailable)".into(),
                        ));
                    }
                    // Terminal: iDoris's own upstream credential or access was
                    // refused. Retrying or switching provider cannot fix it.
                    (502, Some("upstream_auth_failed")) => {
                        return Err(ModelError::Rejected {
                            status: 502,
                            message: "iDoris's upstream provider rejected authentication or access (upstream_auth_failed): check the iDoris operator's upstream key (e.g. IDORIS_OMLX_API_KEY) or the provider's permissions".into(),
                        });
                    }
                    _ => {}
                }
            }
            if status.is_server_error() || code == 429 {
                return Err(ModelError::Unavailable(format!(
                    "iDoris unavailable (HTTP {code})"
                )));
            }
            let message = match code {
                401 => "iDoris rejected the data key (HTTP 401): check IDORIS_API_KEY".to_string(),
                403 => "iDoris refused this role for the data key (HTTP 403): check the key's role scope or IDORIS_API_KEY".to_string(),
                _ => format!("iDoris refused the request (HTTP {code})"),
            };
            return Err(ModelError::Rejected {
                status: code,
                message,
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
        if identity.service == "idoris" && !contract_version_accepted(&identity.contract_version) {
            return Err(invalid(&format!(
                "iDoris contract_version {:?} not supported: expected 1.x and >= 1.0.1",
                identity.contract_version
            )));
        }
        if identity.service != "idoris"
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
        let body = gateway_body(req, role);
        // Request-Id is iDoris's idempotency key: the same id with a different
        // payload is 409 `request_id_conflict`. One admitted kernel id may bind
        // many calls, so it travels on Trace-Id and every call mints its own key.
        let mut call = self
            .client
            .post(self.endpoint("/v1/chat/completions"))
            .header(AUTHORIZATION, self.key.clone())
            .header("X-iDoris-Privacy", "local_only")
            .header("X-iDoris-Complexity", complexity)
            .header("X-iDoris-Request-Id", uuid::Uuid::new_v4().to_string());
        if let Some(trace_id) = request_id.and_then(correlation_trace_id) {
            call = call.header("X-iDoris-Trace-Id", trace_id);
        }
        let response = self.send(call.json(&body), cancel).await?;
        // Evidence headers must be unambiguous: absent, or exactly one valid UTF-8
        // value. Repeated or non-UTF-8 values are contradictory evidence and fail closed.
        let header = |name: &str| -> Result<Option<&str>, ModelError> {
            let mut values = response.headers().get_all(name).iter();
            let Some(first) = values.next() else {
                return Ok(None);
            };
            if values.next().is_some() {
                return Err(invalid("iDoris sent a duplicated evidence header"));
            }
            first
                .to_str()
                .map(Some)
                .map_err(|_| invalid("iDoris evidence header is not valid UTF-8"))
        };
        if header("x-idoris-served-locality")? != Some("loopback") {
            return Err(invalid("iDoris did not attest loopback inference"));
        }
        let record_id = header("x-idoris-record-id")?
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            })
            .ok_or_else(|| invalid("iDoris response has no valid record ID"))?
            .to_owned();
        let cached = match header("x-idoris-cached")? {
            None | Some("false") => false,
            Some("true") => true,
            _ => return Err(invalid("invalid iDoris cache evidence")),
        };
        let origin_record_id = if cached {
            Some(
                header("x-idoris-origin-record-id")?
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

/// Request body for the iDoris gateway. The lifecycle path whitelists request
/// fields (`model, messages, stream, max_tokens, tools, response_format, ...`)
/// and answers 400 `unsupported_parameter` to anything else, so `temperature`
/// is never forwarded here even when the caller set it (other providers send it).
fn gateway_body(req: &CompletionRequest, role: &str) -> serde_json::Value {
    let mut body = completion_body(req, role, true, false);
    if let Some(obj) = body.as_object_mut() {
        obj.remove("temperature");
    }
    body
}

/// 契约版本检查：major == 1 且 (minor, patch) >= (0, 1)，即 1.x 且 >= 1.0.1。
/// 必须恰为 `主.次.修` 三段十进制整数；多余段、前导 `v`、空串一律拒绝。
/// 预发布/构建后缀（如 `-rc1`、`+build`）保守起见一律拒绝。
fn contract_version_accepted(v: &str) -> bool {
    let mut parts = v.split('.');
    let mut next = || -> Option<u64> {
        let p = parts.next()?;
        if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        p.parse().ok()
    };
    let (Some(major), Some(minor), Some(patch)) = (next(), next(), next()) else {
        return false;
    };
    parts.next().is_none() && major == 1 && (minor, patch) >= (0, 1)
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
            progress: None,
            temperature: None,
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

    #[test]
    fn contract_version_range_is_1x_at_least_1_0_1() {
        for ok in ["1.0.1", "1.0.2", "1.1.0", "1.9.3"] {
            assert!(contract_version_accepted(ok), "should accept {ok:?}");
        }
        for bad in ["1.0.0", "0.9.9", "2.0.0", "1.1", "v1.1.0", "1.1.0-rc1", ""] {
            assert!(!contract_version_accepted(bad), "should reject {bad:?}");
        }
    }

    #[test]
    fn gateway_body_never_carries_temperature_while_other_providers_do() {
        let request = CompletionRequest {
            messages: vec![Msg::user("fixture")],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
            disable_thinking: false,
            progress: None,
            temperature: Some(0.7),
        };
        let gateway = gateway_body(&request, "idoris/daily");
        assert!(gateway.get("temperature").is_none(), "{gateway}");
        assert_eq!(gateway["model"], "idoris/daily");
        // Control: the shared builder (used by every other provider) still sends it.
        let other = completion_body(&request, "m", false, false);
        assert_eq!(other["temperature"].as_f64(), Some(0.7));
    }

    #[test]
    fn correlation_trace_id_keeps_sendable_ids_and_omits_rejected_ones() {
        assert_eq!(correlation_trace_id("req_ok"), Some("req_ok"));
        // iDoris preserves surrounding spaces; only a trim-empty value is blank.
        assert_eq!(correlation_trace_id("  kept  "), Some("  kept  "));
        let ascii_max = "a".repeat(128);
        assert_eq!(correlation_trace_id(&ascii_max), Some(ascii_max.as_str()));
        assert_eq!(correlation_trace_id(&"a".repeat(129)), None);
        // U+1F600 is one scalar and two UTF-16 units. 64 of them is the limit.
        let emoji_max = "😀".repeat(64);
        assert_eq!(emoji_max.encode_utf16().count(), 128);
        assert_eq!(correlation_trace_id(&emoji_max), Some(emoji_max.as_str()));
        assert!(reqwest::header::HeaderValue::from_str(&emoji_max).is_ok());
        assert_eq!(correlation_trace_id(&format!("{emoji_max}a")), None);
        for blank in ["", " ", "\t", "\n", "\u{00A0}", "\u{2028}"] {
            assert_eq!(correlation_trace_id(blank), None, "{blank:?}");
        }
        for control in ["bad\u{0000}id", "bad\u{007F}id", "bad\u{0085}id", "a\nb"] {
            assert_eq!(correlation_trace_id(control), None, "{control:?}");
        }
    }

    async fn status_server(status: u16, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let mut buf = [0_u8; 4096];
                let _ = socket.read(&mut buf).await;
                let _ = socket.write_all(format!("HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", body.len()).as_bytes()).await;
            }
        });
        url
    }

    async fn error_for(status: u16, body: &'static str) -> ModelError {
        let url = status_server(status, body).await;
        let gateway = IdorisClient {
            base: reqwest::Url::parse(&url).unwrap(),
            key: HeaderValue::from_static("Bearer idk_secret_fixture_key"),
            client: base_client_builder().no_proxy().build().unwrap(),
            request_timeout: Duration::from_secs(5),
        };
        let request = gateway.client.get(&url);
        gateway
            .send(request, &CancellationToken::new())
            .await
            .unwrap_err()
    }

    #[tokio::test]
    async fn status_401_points_at_the_data_key() {
        match error_for(401, "{}").await {
            ModelError::Rejected { status, message } => {
                assert_eq!(status, 401);
                assert!(message.contains("IDORIS_API_KEY"), "{message}");
                assert!(!message.contains("idk_"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn status_403_points_at_the_role() {
        match error_for(403, r#"{"error":{"code":"VIRTUAL_KEY_ROLE_FORBIDDEN"}}"#).await {
            ModelError::Rejected { status, message } => {
                assert_eq!(status, 403);
                assert!(message.contains("role"), "{message}");
                assert!(message.contains("IDORIS_API_KEY"), "{message}");
                assert!(!message.contains("idk_"), "{message}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn status_503_local_only_unavailable_names_missing_local_model() {
        match error_for(
            503,
            r#"{"error":{"reason_code":"local_only_unavailable","detail":"LEAK"}}"#,
        )
        .await
        {
            ModelError::Unavailable(message) => {
                assert!(message.contains("no local model"), "{message}");
                assert!(!message.contains("LEAK"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        match error_for(503, r#"{"reason_code":"local_only_unavailable"}"#).await {
            ModelError::Unavailable(message) => {
                assert!(message.contains("no local model"), "{message}");
            }
            other => panic!("{other:?}"),
        }
        for body in [
            r#"{"error":{"reason_code":"other","detail":"LEAK"}}"#,
            "not json",
        ] {
            match error_for(503, body).await {
                ModelError::Unavailable(message) => {
                    assert_eq!(message, "iDoris unavailable (HTTP 503)")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn status_503_stalled_body_is_cancellable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            socket
                .write_all(
                    b"HTTP/1.1 503 X\r\ncontent-length: 100\r\nconnection: close\r\n\r\n123456789",
                )
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let gateway = IdorisClient {
            base: reqwest::Url::parse(&url).unwrap(),
            key: HeaderValue::from_static("Bearer idk_fixture"),
            client: base_client_builder().no_proxy().build().unwrap(),
            request_timeout: Duration::from_secs(120),
        };
        let cancel = CancellationToken::new();
        let request = gateway.client.get(&url);
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { gateway.send(request, &cancel).await.unwrap_err() }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("cancel must interrupt the 503 body read")
            .unwrap();
        server.abort();
        assert!(matches!(result, ModelError::Cancelled), "{result:?}");
    }

    #[tokio::test]
    async fn status_502_upstream_auth_failed_is_terminal_rejected() {
        for body in [
            r#"{"error":{"type":"upstream_auth_failed","reason_code":"upstream_auth_failed","detail":"LEAK idk_secret_fixture_key"}}"#,
            r#"{"error":{"type":"upstream_auth_failed"}}"#,
        ] {
            match error_for(502, body).await {
                ModelError::Rejected { status, message } => {
                    assert_eq!(status, 502);
                    assert!(message.contains("upstream_auth_failed"), "{message}");
                    assert!(message.contains("IDORIS_OMLX_API_KEY"), "{message}");
                    assert!(!message.contains("LEAK"), "{message}");
                    assert!(!message.contains("idk_"), "{message}");
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn status_502_without_the_code_stays_unavailable() {
        for body in [
            r#"{"error":{"reason_code":"other","detail":"LEAK"}}"#,
            "not json",
            "{}",
        ] {
            match error_for(502, body).await {
                ModelError::Unavailable(message) => {
                    assert_eq!(message, "iDoris unavailable (HTTP 502)")
                }
                other => panic!("{other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn status_502_stalled_body_is_cancellable() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut buf = [0_u8; 4096];
            let _ = socket.read(&mut buf).await;
            socket
                .write_all(
                    b"HTTP/1.1 502 X\r\ncontent-length: 100\r\nconnection: close\r\n\r\n123456789",
                )
                .await
                .unwrap();
            std::future::pending::<()>().await;
        });
        let gateway = IdorisClient {
            base: reqwest::Url::parse(&url).unwrap(),
            key: HeaderValue::from_static("Bearer idk_fixture"),
            client: base_client_builder().no_proxy().build().unwrap(),
            request_timeout: Duration::from_secs(120),
        };
        let cancel = CancellationToken::new();
        let request = gateway.client.get(&url);
        let task = tokio::spawn({
            let cancel = cancel.clone();
            async move { gateway.send(request, &cancel).await.unwrap_err() }
        });
        tokio::time::sleep(Duration::from_millis(500)).await;
        cancel.cancel();
        let result = tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("cancel must interrupt the 502 body read")
            .unwrap();
        server.abort();
        assert!(matches!(result, ModelError::Cancelled), "{result:?}");
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
