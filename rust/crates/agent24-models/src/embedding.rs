//! Loopback embedding client (VR-1).
//!
//! Chat's route switch is reused: when `IDORIS_URL` is set, embeddings go to
//! that origin's `/v1/embeddings`; otherwise they go straight to oMLX at
//! `OMLX_URL` / `OMLX_API_KEY` (the same variables and defaults as
//! [`crate::ProviderRegistry::from_env`], sent as `Authorization: Bearer` when
//! the key is non-empty — the provider's `api_key` field). The model name is
//! `A24_EMBED_MODEL`, default [`DEFAULT_EMBED_MODEL`].
//!
//! Locality is not the same in the two modes (§3.7):
//! - **Direct oMLX** is local only because the URL is loopback (and the client
//!   does not use a proxy or follow redirects, same as a Local chat provider).
//!   There is no served-locality response header. A process listening on
//!   loopback can still forward the body off-box; that is a configuration
//!   choice, not something this client can see.
//! - **iDoris** has that URL rule and also requires the response header
//!   `x-idoris-served-locality: loopback`, with `X-iDoris-Privacy: local_only`
//!   on the request. A missing or different header fails the call. There is
//!   no fallback to direct oMLX.
//!
//! A non-loopback URL fails construction. It is not relabeled and not used.

use std::time::Duration;

use serde::Deserialize;
use zeroize::Zeroizing;

use crate::ModelError;

/// bge-m3 on oMLX, fp16. VR-0: 1024 dimensions, already L2-normalized.
pub const DEFAULT_EMBED_MODEL: &str = "bge-m3-mlx-fp16";

/// Dimension this client accepts. VR-0 measured bge-m3 at 1024; a different
/// length is a bad batch, not a vector we store.
pub const EMBED_DIMS: usize = 1024;

const MAX_EMBED_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

/// Texts per `/v1/embeddings` request. A longer `embed` call sends several.
const EMBED_BATCH_LIMIT: usize = 64;

/// Bytes of an error body we inspect for `error.type` / `reason_code`.
const MAX_EMBED_ERROR_BYTES: usize = 4096;

/// Longest `error.type` or `reason_code` copied into an error string.
const MAX_EMBED_ERROR_FIELD: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Route {
    Direct,
    Idoris,
}

/// OpenAI-compatible `/v1/embeddings` against a loopback server.
#[derive(Clone)]
pub struct EmbeddingClient {
    route: Route,
    base_url: String,
    /// Same role as [`crate::OpenAiCompatProvider`]'s `api_key`: `None` or empty
    /// sends no `Authorization` header; otherwise `Bearer <key>`. Zeroized on drop.
    api_key: Option<Zeroizing<String>>,
    model: String,
    client: reqwest::Client,
    timeout: Duration,
}

impl EmbeddingClient {
    /// Direct oMLX. `base_url` must be loopback (`http://127.0.0.1:8088` in
    /// production). Fails closed on any other host.
    pub fn direct(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, ModelError> {
        Self::build(Route::Direct, base_url, Some(api_key.into()), model)
    }

    /// iDoris `/v1/embeddings`. The URL must be a loopback origin (the same
    /// shape chat accepts) and `api_key` must be a virtual data key (`idk_…`).
    pub fn idoris(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let base_url = base_url.into();
        ensure_idoris_origin(&base_url)?;
        let api_key = api_key.into();
        if !api_key.starts_with("idk_") || api_key.len() <= 4 {
            return Err(ModelError::Provider(
                "iDoris requires a virtual data key, not an Admin token".into(),
            ));
        }
        Self::build(Route::Idoris, base_url, Some(api_key), model)
    }

    /// `IDORIS_URL` set → [`Self::idoris`] (no fallback). Otherwise the oMLX
    /// URL and key chat already reads.
    pub fn from_env() -> Result<Self, ModelError> {
        let model = resolve_model(std::env::var("A24_EMBED_MODEL"))?;
        if std::env::var_os("IDORIS_URL").is_some() {
            let url = std::env::var("IDORIS_URL")
                .map_err(|_| ModelError::Provider("invalid IDORIS_URL".into()))?;
            let key = std::env::var("IDORIS_API_KEY")
                .map_err(|_| ModelError::Provider("missing iDoris data key".into()))?;
            return Self::idoris(url, key, model);
        }
        // Same variables and defaults as `ProviderRegistry::from_env`.
        let url = std::env::var("OMLX_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned());
        let key = std::env::var("OMLX_API_KEY").unwrap_or_else(|_| "xiaobao8088".to_owned());
        Self::direct(url, key, model)
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    /// Per-request HTTP timeout. The default is 120s, the chat-provider ceiling,
    /// so a cold model load is not cut off here. `embed` takes no cancellation
    /// token; this timeout is the deadline on the request, including the body.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Vectors in input order. A single text in a request is a JSON string;
    /// more than one is an array (VR-0). Inputs longer than 64 go out as
    /// several requests. A failed request fails the call. Each response is
    /// rejected whole when the count, the dimension, or any component is wrong.
    pub async fn embed(&self, input: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        if input.is_empty() {
            return Err(ModelError::Provider("embedding input is empty".into()));
        }
        let mut out = Vec::with_capacity(input.len());
        for batch in input.chunks(EMBED_BATCH_LIMIT) {
            out.extend(self.embed_batch(batch).await?);
        }
        Ok(out)
    }

    async fn embed_batch(&self, input: &[&str]) -> Result<Vec<Vec<f32>>, ModelError> {
        let input_value = if input.len() == 1 {
            serde_json::Value::String(input[0].to_owned())
        } else {
            serde_json::Value::Array(
                input
                    .iter()
                    .map(|text| serde_json::Value::String((*text).to_owned()))
                    .collect(),
            )
        };
        let body = serde_json::json!({ "model": self.model, "input": input_value });
        let mut request = self
            .client
            .post(embeddings_url(&self.base_url))
            .timeout(self.timeout);
        if self.route == Route::Idoris {
            request = request.header("X-iDoris-Privacy", "local_only");
        }
        let response = self
            .authed(request)
            .json(&body)
            .send()
            .await
            .map_err(|err| crate::classify(&err))?;
        if !response.status().is_success() {
            let status = response.status();
            let fields = embed_error_fields(response).await;
            return Err(ModelError::Provider(embed_http_error(status, &fields)));
        }
        if self.route == Route::Idoris && locality_header(&response)? != Some("loopback") {
            return Err(ModelError::Provider(
                "iDoris did not attest loopback inference".into(),
            ));
        }
        let parsed: EmbedResponse = read_embed_json(response).await?;
        take_vectors(input.len(), parsed.data)
    }

    fn build(
        route: Route,
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
    ) -> Result<Self, ModelError> {
        let base_url = base_url.into();
        let model = model.into();
        if model.trim().is_empty() {
            return Err(ModelError::Provider("embedding model name is empty".into()));
        }
        ensure_loopback(&base_url)?;
        let api_key = api_key.filter(|key| !key.is_empty()).map(Zeroizing::new);
        let client = crate::base_client_builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|err| ModelError::Provider(format!("cannot build embedding client: {err}")))?;
        Ok(Self {
            route,
            base_url,
            api_key,
            model,
            client,
            timeout: Duration::from_secs(120),
        })
    }

    fn authed(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) if !key.is_empty() => request.bearer_auth(key.as_str()),
            _ => request,
        }
    }
}

struct EmbedErrorFields {
    error_type: Option<String>,
    reason_code: Option<String>,
}

fn embed_http_error(status: reqwest::StatusCode, fields: &EmbedErrorFields) -> String {
    let code = status.as_u16();
    match (fields.error_type.as_deref(), fields.reason_code.as_deref()) {
        (Some(kind), Some(reason)) => format!("embeddings HTTP {code}: {kind} {reason}"),
        (Some(kind), None) => format!("embeddings HTTP {code}: {kind}"),
        (None, Some(reason)) => format!("embeddings HTTP {code}: {reason}"),
        (None, None) => format!("embeddings HTTP {code}"),
    }
}

fn short_code(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.chars().take(MAX_EMBED_ERROR_FIELD).collect())
    }
}

/// Status plus `error.type` / `reason_code` when the body has them. The rest
/// of the body stays out of the error string.
async fn embed_error_fields(mut response: reqwest::Response) -> EmbedErrorFields {
    let mut body = Vec::new();
    loop {
        if body.len() >= MAX_EMBED_ERROR_BYTES {
            break;
        }
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            _ => break,
        };
        let room = MAX_EMBED_ERROR_BYTES - body.len();
        let take = room.min(chunk.len());
        body.extend_from_slice(&chunk[..take]);
        if take < chunk.len() {
            break;
        }
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return EmbedErrorFields {
            error_type: None,
            reason_code: None,
        };
    };
    EmbedErrorFields {
        error_type: short_code(value.pointer("/error/type")),
        reason_code: short_code(
            value
                .pointer("/error/reason_code")
                .or_else(|| value.get("reason_code")),
        ),
    }
}

async fn read_embed_json<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
) -> Result<T, ModelError> {
    let mut body = Vec::new();
    let read_error = |err: reqwest::Error| {
        if crate::hung_up(&err) {
            ModelError::Unavailable(format!("embeddings: the answer broke off: {err}"))
        } else {
            crate::classify(&err)
        }
    };
    loop {
        let Some(chunk) = response.chunk().await.map_err(read_error)? else {
            break;
        };
        if body.len() + chunk.len() > MAX_EMBED_RESPONSE_BYTES {
            return Err(ModelError::Provider(format!(
                "embeddings response exceeds {MAX_EMBED_RESPONSE_BYTES} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|err| ModelError::Provider(format!("embeddings returned invalid JSON: {err}")))
}

fn resolve_model(value: Result<String, std::env::VarError>) -> Result<String, ModelError> {
    match value {
        Ok(name) if !name.trim().is_empty() => Ok(name),
        Ok(_) => Err(ModelError::Provider("A24_EMBED_MODEL is empty".into())),
        Err(std::env::VarError::NotPresent) => Ok(DEFAULT_EMBED_MODEL.to_owned()),
        Err(std::env::VarError::NotUnicode(_)) => Err(ModelError::Provider(
            "A24_EMBED_MODEL is not Unicode".into(),
        )),
    }
}

/// The one loopback gate. Both the configured base and the URL that will
/// actually be requested have to pass [`crate::router::is_loopback_url`].
fn ensure_loopback(base: &str) -> Result<(), ModelError> {
    let request = embeddings_url(base);
    if crate::router::is_loopback_url(base) && crate::router::is_loopback_url(&request) {
        Ok(())
    } else {
        Err(ModelError::Provider(format!(
            "embedding URL is not loopback ({base})"
        )))
    }
}

fn embeddings_url(base: &str) -> String {
    format!("{}/v1/embeddings", base.trim_end_matches('/'))
}

/// Chat's iDoris client refuses anything that is not an HTTP(S) origin.
/// Embeddings use the same rule so a path or query on `IDORIS_URL` cannot
/// aim the request somewhere chat would have rejected.
fn ensure_idoris_origin(url: &str) -> Result<(), ModelError> {
    let base =
        reqwest::Url::parse(url).map_err(|_| ModelError::Provider("invalid IDORIS_URL".into()))?;
    if !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.path() != "/"
    {
        return Err(ModelError::Provider(
            "IDORIS_URL must be a loopback HTTP(S) origin".into(),
        ));
    }
    Ok(())
}

fn locality_header(response: &reqwest::Response) -> Result<Option<&str>, ModelError> {
    let mut values = response
        .headers()
        .get_all("x-idoris-served-locality")
        .iter();
    let Some(first) = values.next() else {
        return Ok(None);
    };
    if values.next().is_some() {
        return Err(ModelError::Provider(
            "iDoris sent a duplicated served-locality header".into(),
        ));
    }
    first.to_str().map(Some).map_err(|_| {
        ModelError::Provider("iDoris served-locality header is not valid UTF-8".into())
    })
}

#[derive(Deserialize)]
struct EmbedItem {
    embedding: Vec<f64>,
    index: Option<usize>,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedItem>,
}

fn take_vectors(input_len: usize, items: Vec<EmbedItem>) -> Result<Vec<Vec<f32>>, ModelError> {
    if items.len() != input_len {
        return Err(ModelError::Provider(format!(
            "embedding count {} != {input_len}",
            items.len()
        )));
    }
    let mut slots: Vec<Option<Vec<f32>>> = (0..input_len).map(|_| None).collect();
    for (pos, item) in items.into_iter().enumerate() {
        let idx = item.index.unwrap_or(pos);
        if idx >= input_len || slots[idx].is_some() {
            return Err(ModelError::Provider(format!(
                "embedding count: bad index {idx}"
            )));
        }
        slots[idx] = Some(finite_dims(item.embedding)?);
    }
    let mut out = Vec::with_capacity(input_len);
    for slot in slots {
        match slot {
            Some(vector) => out.push(vector),
            None => {
                return Err(ModelError::Provider("embedding count: missing row".into()));
            }
        }
    }
    Ok(out)
}

fn finite_dims(values: Vec<f64>) -> Result<Vec<f32>, ModelError> {
    if values.len() != EMBED_DIMS {
        return Err(ModelError::Provider(format!(
            "embedding dims {} != {EMBED_DIMS}",
            values.len()
        )));
    }
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let component = value as f32;
        if !component.is_finite() {
            return Err(ModelError::Provider(
                "non-finite embedding component".into(),
            ));
        }
        out.push(component);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn must_fail(result: Result<EmbeddingClient, ModelError>, what: &str) -> ModelError {
        match result {
            Ok(_) => panic!("{what}: construction succeeded"),
            Err(err) => err,
        }
    }

    #[test]
    fn remote_address_is_refused() {
        let err = must_fail(
            EmbeddingClient::direct("http://10.0.0.2", "k", DEFAULT_EMBED_MODEL),
            "a non-loopback embedding URL must not construct",
        );
        let message = err.to_string();
        assert!(message.contains("not loopback"), "{message}");
        assert!(message.contains("10.0.0.2"), "{message}");
    }

    #[test]
    fn idoris_refuses_a_non_origin_and_a_non_virtual_key() {
        let err = must_fail(
            EmbeddingClient::idoris("http://10.0.0.2", "idk_fixture", DEFAULT_EMBED_MODEL),
            "remote iDoris URL",
        );
        assert!(err.to_string().contains("not loopback"), "{err}");
        let err = must_fail(
            EmbeddingClient::idoris(
                "http://127.0.0.1:9/extra",
                "idk_fixture",
                DEFAULT_EMBED_MODEL,
            ),
            "path on IDORIS_URL",
        );
        assert!(err.to_string().contains("origin"), "{err}");
        let err = must_fail(
            EmbeddingClient::idoris("http://127.0.0.1:9", "admin-token", DEFAULT_EMBED_MODEL),
            "admin token",
        );
        assert!(err.to_string().contains("virtual data key"), "{err}");
    }

    #[test]
    fn unset_embed_model_defaults_to_bge_m3() {
        let missing: Result<String, std::env::VarError> = Err(std::env::VarError::NotPresent);
        assert_eq!(resolve_model(missing).unwrap(), DEFAULT_EMBED_MODEL);
        assert!(resolve_model(Ok("  ".to_owned())).is_err());
        assert_eq!(
            resolve_model(Ok("other-embed".to_owned())).unwrap(),
            "other-embed"
        );
    }

    struct Scripted {
        status: &'static str,
        extra_headers: &'static str,
        body: String,
    }

    async fn read_http(sock: &mut tokio::net::TcpStream) -> (String, String) {
        let mut got = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            assert!(n > 0, "client closed before the request ended");
            got.extend_from_slice(&buf[..n]);
            if let Some(idx) = got.windows(4).position(|w| w == b"\r\n\r\n") {
                let header_end = idx + 4;
                let headers = String::from_utf8_lossy(&got[..idx]).into_owned();
                let len = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if got.len() >= header_end + len {
                    let body =
                        String::from_utf8_lossy(&got[header_end..header_end + len]).into_owned();
                    return (headers, body);
                }
            }
        }
    }

    async fn serve(
        script: Vec<Scripted>,
    ) -> (String, tokio::task::JoinHandle<Vec<(String, String)>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut captured = Vec::new();
            for reply in script {
                let (mut sock, _) = listener.accept().await.unwrap();
                let (headers, body) = read_http(&mut sock).await;
                captured.push((headers, body));
                let raw = format!(
                    "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {len}\r\nconnection: close\r\n{extra}\r\n{body}",
                    status = reply.status,
                    len = reply.body.len(),
                    extra = reply.extra_headers,
                    body = reply.body,
                );
                sock.write_all(raw.as_bytes()).await.unwrap();
            }
            captured
        });
        (url, server)
    }

    fn axis(axis: usize) -> Vec<f64> {
        let mut vector = vec![0.0; EMBED_DIMS];
        vector[axis] = 1.0;
        vector
    }

    fn embed_json(vectors: &[Vec<f64>], order: &[usize]) -> String {
        let data: Vec<_> = order
            .iter()
            .map(|&index| {
                serde_json::json!({
                    "object": "embedding",
                    "index": index,
                    "embedding": vectors[index],
                })
            })
            .collect();
        serde_json::json!({"object": "list", "data": data, "model": DEFAULT_EMBED_MODEL})
            .to_string()
    }

    fn ok(body: String) -> Scripted {
        Scripted {
            status: "200 OK",
            extra_headers: "",
            body,
        }
    }

    #[tokio::test]
    async fn direct_omlx_returns_reordered_batch_without_a_locality_header() {
        let first = axis(0);
        let second = axis(1);
        let one_body = embed_json(std::slice::from_ref(&first), &[0]);
        let batch_body = embed_json(&[first, second], &[1, 0]);
        let (url, server) = serve(vec![ok(one_body), ok(batch_body)]).await;
        let client = EmbeddingClient::direct(&url, "test-key", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));
        let one = client.embed(&["only"]).await.unwrap();
        let batch = client.embed(&["a", "b"]).await.unwrap();
        let captured = server.await.unwrap();

        assert_eq!(one.len(), 1);
        assert_eq!(one[0].len(), EMBED_DIMS);
        assert_eq!(one[0][0], 1.0);
        assert_eq!(batch[0][0], 1.0, "index 0 restored to the front");
        assert_eq!(batch[1][1], 1.0);

        let (headers, body) = &captured[0];
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["model"], DEFAULT_EMBED_MODEL);
        assert_eq!(json["input"], "only");
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer test-key"),
            "{headers}"
        );
        assert!(
            !headers.to_ascii_lowercase().contains("x-idoris-privacy"),
            "direct oMLX must not pretend to be iDoris: {headers}"
        );
        let (_, batch_body) = &captured[1];
        let batch_json: serde_json::Value = serde_json::from_str(batch_body).unwrap();
        assert_eq!(batch_json["input"], serde_json::json!(["a", "b"]));
    }

    #[tokio::test]
    async fn idoris_requires_the_loopback_locality_header() {
        let vector = embed_json(&[axis(0)], &[0]);
        let (url, server) = serve(vec![
            Scripted {
                status: "200 OK",
                extra_headers: "",
                body: vector.clone(),
            },
            Scripted {
                status: "200 OK",
                extra_headers: "x-idoris-served-locality: remote\r\n",
                body: vector.clone(),
            },
            Scripted {
                status: "200 OK",
                extra_headers: "x-idoris-served-locality: loopback\r\n",
                body: vector,
            },
        ])
        .await;
        let client = EmbeddingClient::idoris(&url, "idk_fixture", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));
        let missing = client.embed(&["q"]).await.expect_err("no locality header");
        assert!(
            missing.to_string().contains("did not attest loopback"),
            "{missing}"
        );
        let remote = client.embed(&["q"]).await.expect_err("remote locality");
        assert!(
            remote.to_string().contains("did not attest loopback"),
            "{remote}"
        );
        let got = client.embed(&["q"]).await.unwrap();
        assert_eq!(got[0][0], 1.0);
        let captured = server.await.unwrap();
        let headers = captured[2].0.to_ascii_lowercase();
        assert!(
            headers.contains("x-idoris-privacy: local_only"),
            "{headers}"
        );
        assert!(
            headers.contains("authorization: bearer idk_fixture"),
            "{headers}"
        );
    }

    #[tokio::test]
    async fn wrong_dims_non_finite_and_count_reject_the_whole_batch() {
        let mut overflow = vec!["0"; EMBED_DIMS];
        overflow[3] = "1e39";
        let overflow_body = format!(r#"{{"data":[{{"embedding":[{}]}}]}}"#, overflow.join(","));
        let short = r#"{"data":[{"embedding":[0.1,0.2,0.3]}]}"#;
        let nan = r#"{"data":[{"embedding":[NaN]}]}"#;
        let one = embed_json(&[axis(0)], &[0]);
        let (url, server) = serve(vec![
            ok(short.to_owned()),
            ok(nan.to_owned()),
            ok(overflow_body),
            ok(one),
        ])
        .await;
        let client = EmbeddingClient::direct(&url, "k", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));

        let dims = client.embed(&["q"]).await.expect_err("3 dims");
        assert!(dims.to_string().contains("dims"), "{dims}");
        let nan_err = client.embed(&["q"]).await.expect_err("NaN token");
        assert!(
            nan_err.to_string().contains("invalid JSON")
                || nan_err.to_string().contains("non-finite"),
            "{nan_err}"
        );
        let infinite = client.embed(&["q"]).await.expect_err("f32 overflow");
        assert!(infinite.to_string().contains("non-finite"), "{infinite}");
        let count = client
            .embed(&["a", "b"])
            .await
            .expect_err("one row for two inputs");
        assert!(count.to_string().contains("count"), "{count}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn http_error_keeps_status_and_short_codes_not_the_body() {
        let leak = "passport number 123456789";
        let typed = serde_json::json!({
            "error": {
                "type": "upstream_error",
                "reason_code": "backend_down",
                "message": leak,
            },
            "input": leak,
        })
        .to_string();
        let bare = format!("remembered fact: {leak}");
        let (url, server) = serve(vec![
            Scripted {
                status: "503 Service Unavailable",
                extra_headers: "",
                body: typed,
            },
            Scripted {
                status: "500 Internal Server Error",
                extra_headers: "",
                body: bare,
            },
        ])
        .await;
        let client = EmbeddingClient::direct(&url, "k", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));
        let typed_err = client.embed(&["q"]).await.expect_err("typed error");
        let typed_msg = typed_err.to_string();
        assert!(typed_msg.contains("503"), "{typed_msg}");
        assert!(typed_msg.contains("upstream_error"), "{typed_msg}");
        assert!(typed_msg.contains("backend_down"), "{typed_msg}");
        assert!(!typed_msg.contains(leak), "{typed_msg}");
        let bare_err = client.embed(&["q"]).await.expect_err("bare body");
        let bare_msg = bare_err.to_string();
        assert!(bare_msg.contains("500"), "{bare_msg}");
        assert!(!bare_msg.contains(leak), "{bare_msg}");
        assert!(!bare_msg.contains("remembered"), "{bare_msg}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn duplicated_locality_header_is_rejected() {
        let vector = embed_json(&[axis(0)], &[0]);
        let (url, server) = serve(vec![Scripted {
            status: "200 OK",
            extra_headers: "x-idoris-served-locality: loopback\r\nx-idoris-served-locality: loopback\r\n",
            body: vector,
        }])
        .await;
        let client = EmbeddingClient::idoris(&url, "idk_fixture", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));
        let err = client.embed(&["q"]).await.expect_err("duplicate locality");
        assert!(
            err.to_string().contains("duplicated served-locality"),
            "{err}"
        );
        server.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_or_out_of_range_index_rejects_the_batch() {
        let good = axis(0);
        let duplicated = serde_json::json!({
            "data": [
                {"index": 0, "embedding": good},
                {"index": 0, "embedding": []},
            ]
        })
        .to_string();
        let out_of_range = serde_json::json!({
            "data": [{"index": 2, "embedding": []}]
        })
        .to_string();
        let (url, server) = serve(vec![ok(duplicated), ok(out_of_range)]).await;
        let client = EmbeddingClient::direct(&url, "k", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));
        let dup = client
            .embed(&["a", "b"])
            .await
            .expect_err("duplicate index");
        assert!(dup.to_string().contains("bad index"), "{dup}");
        let range = client.embed(&["q"]).await.expect_err("index 2");
        assert!(range.to_string().contains("bad index"), "{range}");
        server.await.unwrap();
    }

    #[tokio::test]
    async fn long_input_is_split_into_batches() {
        let mut inputs = Vec::new();
        let mut first = Vec::new();
        for i in 0..EMBED_BATCH_LIMIT {
            inputs.push(format!("t{i}"));
            let mut vector = vec![0.0; EMBED_DIMS];
            vector[0] = i as f64;
            first.push(vector);
        }
        inputs.push("tail".to_owned());
        let mut tail = vec![0.0; EMBED_DIMS];
        tail[0] = 64.0;
        let order: Vec<usize> = (0..EMBED_BATCH_LIMIT).collect();
        let (url, server) = serve(vec![
            ok(embed_json(&first, &order)),
            ok(embed_json(&[tail], &[0])),
        ])
        .await;
        let client = EmbeddingClient::direct(&url, "k", DEFAULT_EMBED_MODEL)
            .unwrap()
            .with_timeout(Duration::from_secs(5));
        let refs: Vec<&str> = inputs.iter().map(String::as_str).collect();
        let got = client.embed(&refs).await.unwrap();
        assert_eq!(got.len(), EMBED_BATCH_LIMIT + 1);
        assert_eq!(got[0][0], 0.0);
        assert_eq!(got[63][0], 63.0);
        assert_eq!(got[64][0], 64.0);
        let captured = server.await.unwrap();
        assert_eq!(captured.len(), 2);
        let first_body: serde_json::Value = serde_json::from_str(&captured[0].1).unwrap();
        assert_eq!(
            first_body["input"].as_array().unwrap().len(),
            EMBED_BATCH_LIMIT
        );
        let tail_body: serde_json::Value = serde_json::from_str(&captured[1].1).unwrap();
        assert_eq!(tail_body["input"], "tail");
    }
}
