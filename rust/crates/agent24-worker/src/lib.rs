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
    ///
    /// FU-74: unlike `agent24-models`' providers (which point at
    /// user-configured `OMLX_URL`/`OLLAMA_URL` and so can legitimately be
    /// remote, hence that crate's `env_local_tier` + `loopback_only` gate),
    /// the ML worker is — per this module's doc comment — "a separate process
    /// the daemon spawns": there is no caller anywhere in the workspace today
    /// that points `base_url` at anything but a locally spawned worker, and no
    /// env var configures it. So this client is unconditionally loopback-only
    /// rather than gated on parsing `base_url`. Embed requests carry raw
    /// memory content, and the default reqwest client both reads
    /// `HTTP_PROXY`/`ALL_PROXY` (without bypassing loopback) and follows
    /// redirects — either would ship that content off-box whenever the
    /// caller's shell happens to export a proxy. If `base_url` ever becomes
    /// remote-configurable, replace this with the same `reqwest::Url`-based
    /// loopback check `agent24-models` uses instead of assuming local.
    pub fn new(base_url: impl Into<String>) -> Self {
        let base_url = base_url.into().trim_end_matches('/').to_owned();
        // A worker that accepts TCP but never answers must not hang the
        // daemon: a bounded connect timeout classifies as Unavailable.
        // FU-74: no_proxy() + redirect::none(), unconditionally — see this
        // method's doc comment for why.
        #[expect(
            clippy::expect_used,
            reason = "unwrap_or_default() here would silently rebuild the proxy-reading, \
                      redirect-following client FU-74 exists to rule out; fail closed instead"
        )]
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(2))
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("building the loopback-only ML worker HTTP client failed");
        Self {
            base_url,
            client,
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
        let res: EmbedResponse = self
            .post_json(
                "/v1/embed",
                req,
                MAX_EMBED_RESPONSE_BYTES,
                self.call_timeout,
                cancel,
            )
            .await?;
        // Validate the worker's response against the contract BEFORE handing it
        // back — a misbehaving worker must surface here as a Worker error, not as
        // malformed vectors that blow up later in memory/vector-search code.
        validate_embed(req, &res)?;
        Ok(res)
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

/// Enforce the [`EmbedResponse`] contract: one vector per input (order-preserving)
/// and every vector exactly `dims` long, with `dims` non-zero when any input was
/// sent. A violation is a reachable-but-broken worker → [`WorkerError::Worker`].
fn validate_embed(req: &EmbedRequest, res: &EmbedResponse) -> Result<()> {
    if res.embeddings.len() != req.input.len() {
        return Err(WorkerError::Worker(format!(
            "embed returned {} vectors for {} inputs",
            res.embeddings.len(),
            req.input.len()
        )));
    }
    if !req.input.is_empty() && res.dims == 0 {
        return Err(WorkerError::Worker("embed returned dims=0".to_owned()));
    }
    if let Some((i, v)) = res
        .embeddings
        .iter()
        .enumerate()
        .find(|(_, v)| v.len() != res.dims)
    {
        return Err(WorkerError::Worker(format!(
            "embed vector {i} has length {} but dims={}",
            v.len(),
            res.dims
        )));
    }
    Ok(())
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
    async fn http_embed_rejects_dims_mismatch() {
        // dims says 3 but the vector is length 2 → contract violation → Worker.
        let base = serve_once(
            "HTTP/1.1 200 OK",
            r#"{"model":"e5","embeddings":[[0.1,0.2]],"dims":3}"#,
        )
        .await;
        let worker = HttpMlWorker::new(base);
        let err = worker
            .embed(
                &EmbedRequest {
                    model: None,
                    input: vec!["x".to_owned()],
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkerError::Worker(ref m) if m.contains("dims")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn http_embed_rejects_vector_count_mismatch() {
        // Two inputs but only one vector returned → Worker error.
        let base = serve_once(
            "HTTP/1.1 200 OK",
            r#"{"model":"e5","embeddings":[[0.1,0.2]],"dims":2}"#,
        )
        .await;
        let worker = HttpMlWorker::new(base);
        let err = worker
            .embed(
                &EmbedRequest {
                    model: None,
                    input: vec!["a".to_owned(), "b".to_owned()],
                },
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(
            matches!(err, WorkerError::Worker(ref m) if m.contains("vectors")),
            "{err:?}"
        );
    }

    #[test]
    fn validate_embed_accepts_a_consistent_response() {
        let req = EmbedRequest {
            model: None,
            input: vec!["a".to_owned(), "b".to_owned()],
        };
        let res = EmbedResponse {
            model: "e5".to_owned(),
            embeddings: vec![vec![1.0, 2.0], vec![3.0, 4.0]],
            dims: 2,
        };
        assert!(validate_embed(&req, &res).is_ok());
    }

    #[test]
    fn validate_embed_rejects_zero_dims_with_inputs() {
        let req = EmbedRequest {
            model: None,
            input: vec!["a".to_owned()],
        };
        let res = EmbedResponse {
            model: "e5".to_owned(),
            embeddings: vec![vec![]],
            dims: 0,
        };
        assert!(matches!(
            validate_embed(&req, &res).unwrap_err(),
            WorkerError::Worker(_)
        ));
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

    // ── FU-74: HttpMlWorker's client must not honour HTTP_PROXY ────────────
    //
    // Same shape as agent24-models' `from_env_local_providers_ignore_http_proxy`
    // (rust/crates/agent24-models/src/router.rs): a child process gets
    // HTTP_PROXY/ALL_PROXY pointed at a proxy stub and no NO_PROXY, then makes
    // one request; the proxy stub's connection count tells us whether the
    // client obeyed the proxy env. A positive control (plain
    // `reqwest::Client::builder()...build()`, no `no_proxy()`) proves the env
    // was actually in effect for the child, so a green test isn't measuring a
    // proxy that was never reachable in the first place.
    //
    // The child tests below use a lowercase env var name
    // (`fu74_worker_target_port`) to pass the stub's port, deliberately NOT
    // SCREAMING_SNAKE_CASE: `agent24-cli`'s `passthrough_list_matches_what_the_daemon_actually_reads`
    // test scans every `env::var("...")`/`env::var_os("...")` literal under
    // `crates/` (this crate included) and demands every SHOUTY-cased name be in
    // `PASSTHROUGH_VARS` — a launchd LaunchAgent gets none of the login shell's
    // env otherwise. A test-only variable would trip that scanner for no
    // reason; the scanner's own shouty-case filter is the documented escape
    // hatch for exactly this.

    /// A blocking stub on its own thread: counts connections, answers `reply`.
    fn thread_stub(reply: String) -> (u16, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let n2 = n.clone();
        std::thread::spawn(move || {
            for s in l.incoming() {
                let Ok(mut s) = s else { continue };
                n2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
                let mut buf = [0u8; 65536];
                let _ = s.read(&mut buf);
                let _ = s.write_all(reply.as_bytes());
            }
        });
        (port, n)
    }

    fn health_ok_reply() -> String {
        let body = r#"{"status":"ok","capabilities":["embed"]}"#;
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn target_port() -> u16 {
        // Deliberately lowercase — see the module comment above.
        std::env::var("fu74_worker_target_port")
            .expect("run_child must set fu74_worker_target_port")
            .parse()
            .expect("fu74_worker_target_port must be a u16")
    }

    /// Child: the production path — `HttpMlWorker::new` must be loopback-only.
    #[tokio::test]
    #[ignore = "child process of http_ml_worker_ignores_http_proxy"]
    async fn proxy_child_http_ml_worker() {
        let url = format!("http://127.0.0.1:{}", target_port());
        let worker = HttpMlWorker::new(url)
            .with_timeouts(Duration::from_millis(500), Duration::from_millis(500));
        let _ = worker.health(&CancellationToken::new()).await;
    }

    /// Child: positive control — the bare default client, same URL, same env.
    #[tokio::test]
    #[ignore = "child process of http_ml_worker_ignores_http_proxy"]
    async fn proxy_child_default_client() {
        let url = format!("http://127.0.0.1:{}", target_port());
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let _ = client
            .get(url)
            .timeout(Duration::from_millis(500))
            .send()
            .await;
    }

    fn run_child(test: &str, target: u16, proxy: u16) {
        let exe = std::env::current_exe().unwrap();
        let proxy_url = format!("http://127.0.0.1:{proxy}");
        let status = std::process::Command::new(exe)
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("fu74_worker_target_port", target.to_string())
            .env("HTTP_PROXY", &proxy_url)
            .env("http_proxy", &proxy_url)
            .env("ALL_PROXY", &proxy_url)
            .env_remove("NO_PROXY")
            .env_remove("no_proxy")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn http_ml_worker_ignores_http_proxy() {
        let (tp, target) = thread_stub(health_ok_reply());
        let (pp, proxy) = thread_stub(health_ok_reply());
        run_child("tests::proxy_child_http_ml_worker", tp, pp);
        assert_eq!(
            proxy.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the proxy saw HttpMlWorker's request"
        );
        assert_eq!(target.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Positive control: the default client under the same env goes through
        // the proxy — proves HTTP_PROXY was actually live for the child.
        let (tp2, target2) = thread_stub(health_ok_reply());
        let (pp2, proxy2) = thread_stub(health_ok_reply());
        run_child("tests::proxy_child_default_client", tp2, pp2);
        assert_eq!(
            proxy2.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "measuring instrument: proxy env must take effect"
        );
        assert_eq!(target2.load(std::sync::atomic::Ordering::SeqCst), 0);
    }
}
