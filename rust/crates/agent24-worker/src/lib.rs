//! Agent24 ML worker client (M-D / D4a).
//!
//! The Rust-side half of the **Python ML Worker** boundary (ADR-026 §5): the
//! worker serves embeddings and speech-to-text now, LoRA training later, as a
//! separate process the daemon spawns. This crate defines the AUTHORITATIVE wire
//! contract (the serde types below) that the Python worker (D4b) must implement,
//! plus:
//! - [`MlWorker`] — the async trait the daemon calls;
//! - [`HttpMlWorker`] — an HTTP/JSON client, consistent with how `agent24d`
//!   already talks to oMLX/ComfyUI (no bespoke transport);
//! - [`MockMlWorker`] — a canned in-process implementation for tests.
//!
//! No Python lives here yet (that is D4b). Transport is HTTP so the contract is
//! language-neutral and inspectable with `curl`.
//!
//! Endpoints (base URL + path):
//! - `POST /v1/embed`      → [`EmbedRequest`]  → [`EmbedResponse`]
//! - `POST /v1/transcribe` → [`TranscribeRequest`] → [`TranscribeResponse`]
//! - `GET  /v1/health`     → [`HealthResponse`]

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

// ── Wire contract ────────────────────────────────────────────────────────────
//
// serde field names ARE the cross-language contract — the Python worker must
// match them exactly. Keep them snake_case and stable.

/// Request to embed one or more texts into vectors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedRequest {
    /// Optional model id; `None` lets the worker use its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Texts to embed, in order. The response preserves this order.
    pub input: Vec<String>,
}

/// Embeddings for each input text, in the same order as the request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// The model that actually produced the vectors.
    pub model: String,
    /// One vector per input text, each of length [`dims`](Self::dims).
    pub embeddings: Vec<Vec<f32>>,
    /// Dimensionality of every vector.
    pub dims: usize,
}

/// Request to transcribe an audio clip to text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscribeRequest {
    /// Optional model id; `None` lets the worker use its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Standard-base64-encoded audio bytes (the format is language-neutral over
    /// JSON). Use [`TranscribeRequest::from_audio`] to build it from raw bytes.
    pub audio_base64: String,
    /// Optional BCP-47 language hint (e.g. `"en"`); `None` = auto-detect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

impl TranscribeRequest {
    /// Build a request from raw audio bytes, base64-encoding them.
    pub fn from_audio(audio: &[u8]) -> Self {
        Self {
            model: None,
            audio_base64: base64::engine::general_purpose::STANDARD.encode(audio),
            language: None,
        }
    }

    #[must_use]
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = Some(model.into());
        self
    }

    #[must_use]
    pub fn with_language(mut self, language: impl Into<String>) -> Self {
        self.language = Some(language.into());
        self
    }
}

/// The transcription result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TranscribeResponse {
    pub text: String,
    /// The detected (or supplied) language, when the worker reports it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

/// Worker liveness + advertised capabilities.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    /// `"ok"` when the worker is ready. Any other value is treated as not-ready.
    pub status: String,
    /// Capabilities the worker currently serves, e.g. `["embed", "transcribe"]`.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl HealthResponse {
    /// Whether the worker reports itself ready.
    pub fn is_ok(&self) -> bool {
        self.status.eq_ignore_ascii_case("ok")
    }
}

/// Failure modes of a worker call. Mirrors the model layer's split so the daemon
/// can treat a spawn-not-up worker (`Unavailable`) differently from a reachable
/// worker that rejected the request (`Worker`).
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    /// Worker unreachable — not spawned yet, crashed, connection refused/timeout.
    #[error("worker unavailable: {0}")]
    Unavailable(String),
    /// Worker reachable but the call failed (non-2xx, bad body, oversized).
    #[error("worker error: {0}")]
    Worker(String),
    /// The call was cancelled via its [`CancellationToken`].
    #[error("cancelled")]
    Cancelled,
}

pub type Result<T> = std::result::Result<T, WorkerError>;

/// The ML worker contract the daemon depends on. Implemented by
/// [`HttpMlWorker`] in production and [`MockMlWorker`] in tests.
#[async_trait]
pub trait MlWorker: Send + Sync {
    async fn embed(&self, req: &EmbedRequest, cancel: &CancellationToken) -> Result<EmbedResponse>;

    async fn transcribe(
        &self,
        req: &TranscribeRequest,
        cancel: &CancellationToken,
    ) -> Result<TranscribeResponse>;

    async fn health(&self, cancel: &CancellationToken) -> Result<HealthResponse>;
}

// ── HTTP client ──────────────────────────────────────────────────────────────

/// Response-body budgets — a misbehaving worker must not allocate unbounded
/// memory in the daemon. Embeddings can be large (many vectors); transcripts and
/// health are small.
const MAX_EMBED_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
const MAX_SMALL_RESPONSE_BYTES: usize = 1024 * 1024;

/// An [`MlWorker`] backed by the worker's HTTP/JSON endpoints.
pub struct HttpMlWorker {
    base_url: String,
    client: reqwest::Client,
    /// Budget for potentially-slow inference calls (embed/transcribe).
    call_timeout: Duration,
    /// Budget for cheap calls (health).
    quick_timeout: Duration,
}

impl HttpMlWorker {
    /// Build a client for a worker at `base_url` (e.g. `http://127.0.0.1:8099`).
    /// A trailing slash is trimmed so path joining is unambiguous.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        Self {
            base_url,
            // A worker that accepts TCP but never answers must not hang the
            // daemon: a bounded connect timeout classifies as Unavailable.
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .build()
                .unwrap_or_default(),
            call_timeout: Duration::from_secs(120),
            quick_timeout: Duration::from_secs(5),
        }
    }

    /// Override request budgets (tests use tiny values against hanging servers).
    #[must_use]
    pub fn with_timeouts(mut self, call: Duration, quick: Duration) -> Self {
        self.call_timeout = call;
        self.quick_timeout = quick;
        self
    }

    async fn post_json<Req: Serialize, Res: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &Req,
        cap: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Res> {
        let url = format!("{}{path}", self.base_url);
        let send = self.client.post(&url).timeout(timeout).json(body).send();
        let response = tokio::select! {
            r = send => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        };
        read_response(response, cap, path, cancel).await
    }

    async fn get_json<Res: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        cap: usize,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Res> {
        let url = format!("{}{path}", self.base_url);
        let send = self.client.get(&url).timeout(timeout).send();
        let response = tokio::select! {
            r = send => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        };
        read_response(response, cap, path, cancel).await
    }
}

#[async_trait]
impl MlWorker for HttpMlWorker {
    async fn embed(&self, req: &EmbedRequest, cancel: &CancellationToken) -> Result<EmbedResponse> {
        self.post_json(
            "/v1/embed",
            req,
            MAX_EMBED_RESPONSE_BYTES,
            self.call_timeout,
            cancel,
        )
        .await
    }

    async fn transcribe(
        &self,
        req: &TranscribeRequest,
        cancel: &CancellationToken,
    ) -> Result<TranscribeResponse> {
        self.post_json(
            "/v1/transcribe",
            req,
            MAX_SMALL_RESPONSE_BYTES,
            self.call_timeout,
            cancel,
        )
        .await
    }

    async fn health(&self, cancel: &CancellationToken) -> Result<HealthResponse> {
        self.get_json(
            "/v1/health",
            MAX_SMALL_RESPONSE_BYTES,
            self.quick_timeout,
            cancel,
        )
        .await
    }
}

/// Check status, then read the body (capped, cancellable) and parse it.
async fn read_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    cap: usize,
    path: &str,
    cancel: &CancellationToken,
) -> Result<T> {
    let status = response.status();
    if !status.is_success() {
        // Reachable worker that refused the call — terminal, not retryable.
        return Err(WorkerError::Worker(format!(
            "{path} returned HTTP {}",
            status.as_u16()
        )));
    }
    read_json_capped(response, cap, path, cancel).await
}

async fn read_json_capped<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    cap: usize,
    path: &str,
    cancel: &CancellationToken,
) -> Result<T> {
    let mut body = Vec::new();
    loop {
        let chunk = tokio::select! {
            c = response.chunk() => c.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        if body.len() + chunk.len() > cap {
            return Err(WorkerError::Worker(format!(
                "{path} response exceeds {cap} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|e| WorkerError::Worker(format!("{path} returned invalid JSON: {e}")))
}

/// A connect/timeout failure means "try later" (`Unavailable`); anything else is
/// a `Worker` error.
fn classify(err: &reqwest::Error) -> WorkerError {
    if err.is_connect() || err.is_timeout() {
        WorkerError::Unavailable(err.to_string())
    } else {
        WorkerError::Worker(err.to_string())
    }
}

// ── Mock ─────────────────────────────────────────────────────────────────────

/// A canned in-process [`MlWorker`] for tests: returns deterministic embeddings
/// (a fixed-width vector per input) and echoes transcription text.
pub struct MockMlWorker {
    model: String,
    dims: usize,
}

impl Default for MockMlWorker {
    fn default() -> Self {
        Self {
            model: "mock-embed".to_owned(),
            dims: 3,
        }
    }
}

impl MockMlWorker {
    pub fn new(model: impl Into<String>, dims: usize) -> Self {
        Self {
            model: model.into(),
            dims,
        }
    }
}

#[async_trait]
impl MlWorker for MockMlWorker {
    async fn embed(
        &self,
        req: &EmbedRequest,
        _cancel: &CancellationToken,
    ) -> Result<EmbedResponse> {
        // Deterministic, order-preserving: vector[i] = text.len() + i.
        let embeddings = req
            .input
            .iter()
            .map(|text| {
                (0..self.dims)
                    .map(|i| (text.len() + i) as f32)
                    .collect::<Vec<f32>>()
            })
            .collect();
        Ok(EmbedResponse {
            model: req.model.clone().unwrap_or_else(|| self.model.clone()),
            embeddings,
            dims: self.dims,
        })
    }

    async fn transcribe(
        &self,
        req: &TranscribeRequest,
        _cancel: &CancellationToken,
    ) -> Result<TranscribeResponse> {
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(req.audio_base64.as_bytes())
            .map_err(|e| WorkerError::Worker(format!("invalid base64 audio: {e}")))?;
        Ok(TranscribeResponse {
            text: format!("[mock transcript of {} bytes]", bytes.len()),
            language: req.language.clone(),
        })
    }

    async fn health(&self, _cancel: &CancellationToken) -> Result<HealthResponse> {
        Ok(HealthResponse {
            status: "ok".to_owned(),
            capabilities: vec!["embed".to_owned(), "transcribe".to_owned()],
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    // ── contract (serde) tests ──────────────────────────────────────────────

    #[test]
    fn embed_request_omits_absent_model() {
        let req = EmbedRequest {
            model: None,
            input: vec!["hi".to_owned()],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"input":["hi"]}"#);
    }

    #[test]
    fn embed_response_round_trips() {
        let res = EmbedResponse {
            model: "e5".to_owned(),
            embeddings: vec![vec![1.0, 2.0, 3.0]],
            dims: 3,
        };
        let json = serde_json::to_string(&res).unwrap();
        assert_eq!(serde_json::from_str::<EmbedResponse>(&json).unwrap(), res);
    }

    #[test]
    fn transcribe_request_from_audio_base64_encodes() {
        let req = TranscribeRequest::from_audio(b"abc").with_language("en");
        assert_eq!(req.audio_base64, "YWJj"); // base64("abc")
        assert_eq!(req.language.as_deref(), Some("en"));
        assert!(req.model.is_none());
    }

    #[test]
    fn health_ok_is_case_insensitive() {
        let h = HealthResponse {
            status: "OK".to_owned(),
            capabilities: vec![],
        };
        assert!(h.is_ok());
        let bad = HealthResponse {
            status: "starting".to_owned(),
            capabilities: vec![],
        };
        assert!(!bad.is_ok());
    }

    // ── mock worker tests ───────────────────────────────────────────────────

    #[tokio::test]
    async fn mock_embed_is_order_preserving_and_shaped() {
        let worker = MockMlWorker::new("m", 4);
        let req = EmbedRequest {
            model: None,
            input: vec!["a".to_owned(), "bbb".to_owned()],
        };
        let res = worker.embed(&req, &CancellationToken::new()).await.unwrap();
        assert_eq!(res.dims, 4);
        assert_eq!(res.embeddings.len(), 2);
        assert!(res.embeddings.iter().all(|v| v.len() == 4));
        // deterministic: first component = text.len()
        assert_eq!(res.embeddings[0][0], 1.0);
        assert_eq!(res.embeddings[1][0], 3.0);
    }

    #[tokio::test]
    async fn mock_transcribe_round_trips_audio() {
        let worker = MockMlWorker::default();
        let req = TranscribeRequest::from_audio(b"hello");
        let res = worker
            .transcribe(&req, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(res.text, "[mock transcript of 5 bytes]");
    }

    #[tokio::test]
    async fn mock_health_reports_capabilities() {
        let h = MockMlWorker::default()
            .health(&CancellationToken::new())
            .await
            .unwrap();
        assert!(h.is_ok());
        assert!(h.capabilities.contains(&"embed".to_owned()));
    }

    // ── HTTP client tests (hand-rolled canned server, no external mock dep) ──

    /// Serve exactly one HTTP response (status line + JSON body) then close.
    /// Returns the bound base URL.
    async fn serve_once(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // drain the request headers enough to let the client finish sending
                let mut buf = [0u8; 2048];
                let _ = sock.read(&mut buf).await;
                let response = format!(
                    "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(response.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn http_embed_parses_a_200() {
        let base = serve_once(
            "HTTP/1.1 200 OK",
            r#"{"model":"e5","embeddings":[[0.1,0.2]],"dims":2}"#,
        )
        .await;
        let worker = HttpMlWorker::new(base);
        let res = worker
            .embed(
                &EmbedRequest {
                    model: None,
                    input: vec!["x".to_owned()],
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(res.model, "e5");
        assert_eq!(res.dims, 2);
        assert_eq!(res.embeddings, vec![vec![0.1, 0.2]]);
    }

    #[tokio::test]
    async fn http_non_2xx_is_a_worker_error() {
        let base = serve_once("HTTP/1.1 500 Internal Server Error", r#"{"error":"boom"}"#).await;
        let worker = HttpMlWorker::new(base);
        let err = worker.health(&CancellationToken::new()).await.unwrap_err();
        assert!(
            matches!(err, WorkerError::Worker(ref m) if m.contains("500")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn http_invalid_json_is_a_worker_error() {
        let base = serve_once("HTTP/1.1 200 OK", "not json at all").await;
        let worker = HttpMlWorker::new(base);
        let err = worker.health(&CancellationToken::new()).await.unwrap_err();
        assert!(
            matches!(err, WorkerError::Worker(ref m) if m.contains("invalid JSON")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn http_unreachable_is_unavailable() {
        // Nothing listening on this port → connect refused → Unavailable.
        let worker = HttpMlWorker::new("http://127.0.0.1:1")
            .with_timeouts(Duration::from_millis(200), Duration::from_millis(200));
        let err = worker.health(&CancellationToken::new()).await.unwrap_err();
        assert!(matches!(err, WorkerError::Unavailable(_)), "{err:?}");
    }

    #[tokio::test]
    async fn http_cancellation_is_prompt() {
        // A server that accepts but never replies; cancel must win quickly.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((sock, _)) = listener.accept().await {
                tokio::time::sleep(Duration::from_secs(30)).await;
                drop(sock);
            }
        });
        let worker = HttpMlWorker::new(format!("http://{addr}"));
        let cancel = CancellationToken::new();
        let c = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            c.cancel();
        });
        let started = std::time::Instant::now();
        let err = worker.health(&cancel).await.unwrap_err();
        assert!(matches!(err, WorkerError::Cancelled), "{err:?}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "cancel not prompt"
        );
    }
}
