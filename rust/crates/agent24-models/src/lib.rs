//! Agent24 model gateway (B3 scope).
//!
//! Design rules (ADR-026 §6.5, openfang lessons):
//! - The [`ModelProvider`] trait stays MINIMAL — retry, cooldown, routing and
//!   health feedback live ABOVE the trait (M-D `ModelRouter`), never inside a
//!   provider.
//! - Every async call takes a [`CancellationToken`] — cancellation is a
//!   first-class citizen from the first line (never retrofit).
//! - Providers are registered in an ordered registry (no if/else factory).

use std::sync::Arc;
use std::time::Duration;

use agent24_protocol::{Model, Usage};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub mod router;
mod stream;

/// A tool advertised to the model (OpenAI function-calling wire shape is
/// produced by the adapter; this stays provider-neutral).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool input object
    pub parameters: Value,
}

/// A tool invocation the model asked for. `arguments` is the raw JSON string
/// exactly as the model produced it — parsing (and rejecting) it is the
/// caller's job, not the transport's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// Conversation message for the agent loop. Richer than the /chat wire
/// `ChatMessage` (which stays a plain role+content pair): assistant turns may
/// carry tool calls, and `role: "tool"` turns answer them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Msg {
    pub role: String,
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCallRequest>,
    /// Set on `role: "tool"` messages: which call this answers
    pub tool_call_id: Option<String>,
}

impl Msg {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".to_owned(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".to_owned(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: None,
        }
    }

    pub fn assistant(content: Option<String>, tool_calls: Vec<ToolCallRequest>) -> Self {
        Self {
            role: "assistant".to_owned(),
            content,
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool_result(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".to_owned(),
            content: Some(content.into()),
            tool_calls: vec![],
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionRequest {
    pub messages: Vec<Msg>,
    pub model: Option<String>,
    /// Empty = the model cannot call tools this turn
    pub tools: Vec<ToolSpec>,
    /// Constrained output. `None` = free text. The OpenAI-compatible adapter
    /// forwards it as `response_format`; providers that ignore it degrade to
    /// free text, so callers must still validate (Sin90's Local brain does).
    pub response_format: Option<ResponseFormat>,
    /// ME4-4.2.2a: upper bound on generated tokens. `None` = the field is NOT
    /// sent (today's behaviour, so `/api/v1/chat` is byte-for-byte unchanged).
    /// `NonZeroU32`: the lower bound (≥ 1) is a type fact; the upper bound is
    /// the CALLER's policy (`_a24/model/complete` caps at
    /// `MODEL_MAX_TOKENS_CEILING`), not this crate's — the adapter forwards
    /// the value verbatim and never clamps it silently.
    pub max_tokens: Option<std::num::NonZeroU32>,
    /// ME4-S2-thinking: ask the backend to skip a reasoning model's internal
    /// monologue (Qwen3-style `<think>…</think>`) for THIS request.
    /// [`OpenAiCompatProvider`] sends it only in the form its instance opted
    /// into: oMLX's `chat_template_kwargs: {"enable_thinking": false}`
    /// (confirmed against oMLX's own `mlx_lm.server`/`omlx.server`, which
    /// reads `chat_template_kwargs` off the request body top level and
    /// special-cases `enable_thinking`), or Ollama's `reasoning_effort:
    /// "none"` (ME4-S2c §4); one that opted into neither sends nothing. A
    /// backend that refuses the field with a 400 is asked again without it.
    /// The CALLER decides when to set this (`_a24/model/complete`'s handler,
    /// gated on `complexity: simple`) — this crate has no opinion on
    /// complexity.
    pub disable_thinking: bool,
    /// ME4-S2b: the sampling temperature, sent only when set. `None` leaves
    /// the body byte for byte as before (and Ollama's OpenAI layer then uses
    /// 1.0, over the model's own default). f64 end to end, so 0.7 stays 0.7.
    pub temperature: Option<f64>,
    /// ME4-S2a: read the answer as it is written, under these timers, instead
    /// of within one fixed `chat_timeout`. `None` = today's request, byte for
    /// byte. To be set only by `_a24/model/complete`; the router hands the same
    /// value to every provider it tries, so its deadline is the call's.
    /// Not with `tools`: a streamed answer carries no tool calls, so a request
    /// with both is refused rather than answered without them.
    pub progress: Option<Progress>,
}

/// How long a call may take while it makes progress (ME4-S2a §2.2): its first
/// chunk within `first_chunk`, then one within every `stall`, and all of it
/// by `deadline`, which is absolute and set once for the whole call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Progress {
    pub deadline: std::time::Instant,
    pub first_chunk: Duration,
    pub stall: Duration,
}

/// Structured-output request, mirroring the OpenAI `response_format` wire shape
/// so an OpenAI-compatible backend (oMLX, …) forwards it verbatim. Kept
/// provider-neutral: the adapter, not the router or Sin90, knows the wire.
#[derive(Debug, Clone, PartialEq)]
pub enum ResponseFormat {
    /// Force the model to emit JSON matching `schema` (OpenAI `json_schema`).
    JsonSchema {
        /// Schema name (OpenAI requires one).
        name: String,
        /// The JSON Schema the output must satisfy.
        schema: serde_json::Value,
        /// OpenAI `strict` mode — reject any output that doesn't match.
        strict: bool,
    },
}

impl ResponseFormat {
    /// The OpenAI `response_format` JSON body.
    fn to_wire(&self) -> Value {
        match self {
            ResponseFormat::JsonSchema {
                name,
                schema,
                strict,
            } => serde_json::json!({
                "type": "json_schema",
                "json_schema": { "name": name, "schema": schema, "strict": strict },
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResponse {
    pub message: Msg,
    pub usage: Usage,
    /// ME4-4.2.2a: the model id the PROVIDER reported serving this call
    /// (OpenAI `response.model`). `None` = the provider did not say — never
    /// back-filled with the requested name or the provider name, because
    /// that would state as fact something nobody observed.
    pub model_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// Provider unreachable / refused — the registry tries the next provider
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    /// Provider reachable but the call failed — NOT retried on another provider
    #[error("provider error: {0}")]
    Provider(String),
    /// ME4-S2 L3: the provider answered with a non-transient HTTP status
    /// (4xx other than 429). Same routing semantics and the SAME `Display`
    /// text as `Provider`, so `/api/v1/chat` and the agent loop say exactly
    /// what they said before; `status` lets a caller that must not quote
    /// provider text (the module callback) still tell "your request was
    /// refused" from "the backend is misconfigured".
    #[error("provider error: {message}")]
    Rejected { status: u16, message: String },
    #[error("cancelled")]
    Cancelled,
}

/// Minimal provider contract: request in, response out. No routing, no retry,
/// no health logic here.
#[async_trait]
pub trait ModelProvider: Send + Sync {
    fn name(&self) -> &str;

    async fn complete(
        &self,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError>;

    async fn models(&self, cancel: &CancellationToken) -> Result<Vec<Model>, ModelError>;
}

// ── OpenAI-compatible adapter (oMLX, Ollama, LM Studio, remote APIs) ─────────

pub struct OpenAiCompatProvider {
    name: String,
    base_url: String,
    api_key: Option<String>,
    /// Routing tier reported on /models (open enum: local | remote | lora)
    tier: String,
    default_model: String,
    client: reqwest::Client,
    /// Full-request budget for /chat/completions (local inference can be slow)
    chat_timeout: Duration,
    /// Full-request budget for cheap calls like /models
    quick_timeout: Duration,
    /// Review H2: whether THIS provider instance may be sent the
    /// oMLX/mlx-lm-server-specific `chat_template_kwargs` extension (the
    /// `enable_thinking: false` convention `disable_thinking` uses) — not
    /// part of the OpenAI-compatible contract every provider here claims to
    /// speak, so a hosted/managed OpenAI-compatible endpoint must never see
    /// it. `false` unless [`Self::supports_chat_template_kwargs`] was
    /// called — `new` never turns it on itself, and it is NOT derived from
    /// `tier`: being local does not imply being oMLX.
    supports_chat_template_kwargs: bool,
    /// ME4-S2c §4: whether `disable_thinking` may be sent to THIS instance as
    /// `reasoning_effort: "none"`. Same rule as the field above: `false`
    /// unless [`Self::supports_reasoning_effort`] was called.
    supports_reasoning_effort: bool,
}

/// v3.1 L-3: the settings every provider client shares; `new` and
/// `loopback_only` both start from here so they cannot drift.
fn base_client_builder() -> reqwest::ClientBuilder {
    // A provider that accepts TCP but never answers must not hang the daemon:
    // bounded connect (+ per-request timeouts at the call sites).
    reqwest::Client::builder().connect_timeout(Duration::from_secs(2))
}

impl OpenAiCompatProvider {
    pub fn new(
        name: impl Into<String>,
        base_url: impl Into<String>,
        api_key: Option<String>,
        tier: impl Into<String>,
        default_model: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            base_url: base_url.into(),
            api_key,
            tier: tier.into(),
            default_model: default_model.into(),
            // A provider that accepts TCP but never answers must not hang the
            // daemon: bounded connect + per-request timeouts, classified as
            // Unavailable so the registry can still fall through.
            client: base_client_builder().build().unwrap_or_default(),
            chat_timeout: Duration::from_secs(120),
            quick_timeout: Duration::from_secs(5),
            supports_chat_template_kwargs: false,
            supports_reasoning_effort: false,
        }
    }

    /// Review H2: opt this SPECIFIC provider instance into the
    /// oMLX/mlx-lm-server `chat_template_kwargs` extension. Callers must
    /// only do this for a provider they know is actually oMLX (or an
    /// mlx-lm-compatible server) — `router.rs::from_env` calls it on the
    /// `omlx`-named provider only, and only when it judged local (loopback).
    #[must_use]
    pub fn supports_chat_template_kwargs(mut self) -> Self {
        self.supports_chat_template_kwargs = true;
        self
    }

    /// ME4-S2c §4: opt this SPECIFIC provider instance into sending
    /// `disable_thinking` as `reasoning_effort: "none"`. Only `"none"` is ever
    /// sent: Ollama refuses a value asking for thought from a model that
    /// cannot think. `router.rs::from_env` calls it on the `ollama`-named
    /// provider only, and only when it judged local (loopback).
    #[must_use]
    pub fn supports_reasoning_effort(mut self) -> Self {
        self.supports_reasoning_effort = true;
        self
    }

    /// ME4-S2 v3 N1: make "the address judged" and "the address connected
    /// to" the same thing. The default client honours `HTTP_PROXY`/`ALL_PROXY`
    /// (and does NOT bypass loopback) and follows redirects — either one sends
    /// the request body somewhere other than `base_url`. A provider labeled
    /// `Tier::Local` must be built with this: no proxy, no redirect (a 3xx
    /// becomes a non-success status → `Rejected`).
    #[must_use]
    pub fn loopback_only(mut self) -> Self {
        // v3.1 L-2: fail closed. `unwrap_or_default()` here would silently fall
        // back to a client that DOES read proxy variables and follow redirects —
        // the exact thing this method exists to rule out.
        #[expect(
            clippy::expect_used,
            reason = "a Local provider must never fall back to a proxy-reading client"
        )]
        let client = base_client_builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("building the loopback-only HTTP client failed");
        self.client = client;
        self
    }

    /// Override request budgets (tests use tiny values against hanging servers)
    #[must_use]
    pub fn with_timeouts(mut self, chat: Duration, quick: Duration) -> Self {
        self.chat_timeout = chat;
        self.quick_timeout = quick;
        self
    }

    /// A whole (non-streamed) answer as a response.
    fn answer(&self, parsed: OaChatResponse) -> Result<CompletionResponse, ModelError> {
        let model_id = parsed.model.filter(|m| !m.is_empty());
        let choice =
            parsed.choices.into_iter().next().ok_or_else(|| {
                ModelError::Provider(format!("{} returned no choices", self.name))
            })?;
        Ok(CompletionResponse {
            message: choice.message.into(),
            usage: usage_of(parsed.usage),
            model_id,
        })
    }

    /// ME4-S2a §2.3: the answer read as it is written, under `p`'s timers. No
    /// total request timeout: the timers are all there is. A backend that
    /// ignored `stream` sends its whole answer at once; it is read by the
    /// deadline, but its headers come only when the answer is done, so they
    /// fall under the first-chunk timer.
    async fn complete_streamed(
        &self,
        body: &Value,
        p: &Progress,
        cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let started = std::time::Instant::now();
        let send = self
            .authed(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url)),
            )
            .json(body)
            .send();
        let (first, what) = if let Some(at) = started.checked_add(p.first_chunk)
            && at < p.deadline
        {
            (at, format!("no output within {}s", p.first_chunk.as_secs()))
        } else {
            (p.deadline, "past the call's deadline".to_owned())
        };
        let Some(sent) = stream::until(send, first, cancel).await? else {
            return Err(ModelError::Unavailable(format!("{}: {what}", self.name)));
        };
        let response = sent.map_err(|e| classify(&e))?;
        if !response.status().is_success() {
            let status = response.status();
            // Its error body too is read by the deadline: no request timeout bounds it here.
            let body = stream::until(read_error_body(response, cancel), p.deadline, cancel)
                .await?
                .unwrap_or_default();
            return Err(friendly_http_error(&self.name, status, &body));
        }
        let streamed = response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.starts_with("text/event-stream"));
        if !streamed {
            let whole = read_json_capped::<OaChatResponse>(
                response,
                MAX_CHAT_RESPONSE_BYTES,
                cancel,
                &self.name,
            );
            let Some(parsed) = stream::until(whole, p.deadline, cancel).await? else {
                return Err(ModelError::Unavailable(format!(
                    "{}: past the call's deadline",
                    self.name
                )));
            };
            return self.answer(parsed?);
        }
        let s = stream::read(response, p, started, cancel, &self.name).await?;
        Ok(CompletionResponse {
            message: Msg {
                role: "assistant".to_owned(),
                content: Some(s.content),
                tool_calls: vec![],
                tool_call_id: None,
            },
            usage: usage_of(s.usage),
            model_id: s.model,
        })
    }

    /// One request with `body` as it is. A streamed one shares the call's
    /// deadline with any request before it; its first-chunk timer is its own.
    async fn send(
        &self,
        body: &Value,
        progress: Option<&Progress>,
        cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        if let Some(p) = progress {
            return self.complete_streamed(body, p, cancel).await;
        }
        let fut = self
            .authed(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url)),
            )
            .timeout(self.chat_timeout)
            .json(body)
            .send();
        let response = tokio::select! {
            r = fut => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = read_error_body(response, cancel).await;
            return Err(friendly_http_error(&self.name, status, &body));
        }
        let parsed: OaChatResponse =
            read_json_capped(response, MAX_CHAT_RESPONSE_BYTES, cancel, &self.name).await?;
        self.answer(parsed)
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) if !key.is_empty() => rb.bearer_auth(key),
            _ => rb,
        }
    }
}

// ── OpenAI wire shapes (serialize requests, deserialize responses) ──────────

#[derive(Serialize, Deserialize)]
struct OaFunctionCall {
    name: String,
    /// JSON-encoded string per the OpenAI protocol
    arguments: String,
}

#[derive(Serialize, Deserialize)]
struct OaToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: OaFunctionCall,
}

#[derive(Serialize, Deserialize)]
struct OaMessage {
    role: String,
    // Explicit null is how the wire says "tool-calls-only assistant turn"
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<OaToolCall>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

impl From<&Msg> for OaMessage {
    fn from(msg: &Msg) -> Self {
        Self {
            role: msg.role.clone(),
            content: msg.content.clone(),
            tool_calls: if msg.tool_calls.is_empty() {
                None
            } else {
                Some(
                    msg.tool_calls
                        .iter()
                        .map(|tc| OaToolCall {
                            id: tc.id.clone(),
                            kind: "function".to_owned(),
                            function: OaFunctionCall {
                                name: tc.name.clone(),
                                arguments: tc.arguments.clone(),
                            },
                        })
                        .collect(),
                )
            },
            tool_call_id: msg.tool_call_id.clone(),
        }
    }
}

impl From<OaMessage> for Msg {
    fn from(msg: OaMessage) -> Self {
        Self {
            role: msg.role,
            content: msg.content,
            tool_calls: msg
                .tool_calls
                .unwrap_or_default()
                .into_iter()
                .map(|tc| ToolCallRequest {
                    id: tc.id,
                    name: tc.function.name,
                    arguments: tc.function.arguments,
                })
                .collect(),
            tool_call_id: msg.tool_call_id,
        }
    }
}

#[derive(Deserialize)]
struct OaChoice {
    message: OaMessage,
}

#[derive(Debug, PartialEq, Deserialize)]
struct OaUsage {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
    #[serde(default)]
    total_tokens: u64,
}

#[derive(Deserialize)]
struct OaChatResponse {
    choices: Vec<OaChoice>,
    usage: Option<OaUsage>,
    /// ME4-4.2.2a: the served model id. Optional on the wire — some
    /// OpenAI-compatible servers omit it.
    #[serde(default)]
    model: Option<String>,
}

#[derive(Deserialize)]
struct OaModelEntry {
    id: String,
}

#[derive(Deserialize)]
struct OaModelsResponse {
    #[serde(default)]
    data: Vec<OaModelEntry>,
}

/// Response-body budgets — a misbehaving provider must not be able to
/// allocate unbounded memory in the daemon.
const MAX_CHAT_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const MAX_MODELS_RESPONSE_BYTES: usize = 1024 * 1024;
/// Error bodies are tiny (a JSON error object). Cap hard — the error path must
/// not become a second unbounded-allocation route.
const MAX_ERROR_BODY_BYTES: usize = 16 * 1024;

/// OpenAI-compatible error envelope: `{ "error": { "message", "code", "type" } }`.
#[derive(Deserialize)]
struct OaErrorBody {
    error: OaError,
}

#[derive(Deserialize)]
struct OaError {
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: Option<String>,
}

/// Pull the provider's own human-readable message out of an error body, if it
/// sent one in the OpenAI shape. Best-effort: a body we can't parse yields a
/// trimmed snippet rather than nothing, because even a raw HTML 502 page tells
/// a user more than a bare status code.
fn extract_provider_message(body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(parsed) = serde_json::from_str::<OaErrorBody>(trimmed) {
        let msg = parsed.error.message.trim();
        if !msg.is_empty() {
            return Some(match parsed.error.code {
                Some(code) if !code.is_empty() => format!("{msg} [{code}]"),
                _ => msg.to_owned(),
            });
        }
    }
    // Not the expected shape — surface a bounded snippet so the cause isn't lost.
    let snippet: String = trimmed.chars().take(200).collect();
    Some(snippet)
}

/// Turn a non-success HTTP status into something a user can act on (H12).
///
/// `provider returned HTTP 429` names a symptom nobody can fix; this names the
/// likely cause and the fix. The provider's own message (when present) is
/// appended, because it often carries the specific detail — which key, which
/// model, how long the rate-limit window is.
///
/// The [`ModelError`] variant is chosen so the router's fallthrough stays
/// sensible: a rate limit or a server-side 5xx is transient and provider-local,
/// so it is `Unavailable` and the next provider is tried; an auth/model/request
/// error is a configuration problem the next provider cannot paper over, so it
/// is `Provider` and stops here. (Before H12 every HTTP error was `Provider`;
/// letting 429/5xx fall through is the one behaviour change, and it is the
/// correct one — a rate-limited primary should yield to a healthy backup.)
fn friendly_http_error(provider: &str, status: reqwest::StatusCode, body: &str) -> ModelError {
    let code = status.as_u16();
    let cause = match code {
        401 => "authentication failed — the API key is missing, wrong, or expired",
        403 => "permission denied — this key may not be allowed to use this model",
        404 => "not found — the model id is likely wrong for this provider",
        408 | 409 => "the provider rejected the request",
        413 => "the request was too large for this provider",
        422 => "the provider rejected the request as invalid",
        429 => "rate limited or out of quota",
        400 => "the provider rejected the request as malformed",
        500..=599 => "the provider had a server-side error",
        _ => "unexpected provider response",
    };
    let detail = extract_provider_message(body);
    let msg = match detail {
        Some(d) => format!("{provider}: {cause} (HTTP {code}: {d})"),
        None => format!("{provider}: {cause} (HTTP {code})"),
    };
    if code == 429 || status.is_server_error() {
        ModelError::Unavailable(msg)
    } else {
        ModelError::Rejected {
            status: code,
            message: msg,
        }
    }
}

/// Read a bounded error body (cancellable) so [`friendly_http_error`] can quote
/// the provider's own message. A read failure degrades to an empty body — the
/// status-based cause line still stands on its own.
async fn read_error_body(response: reqwest::Response, cancel: &CancellationToken) -> String {
    let mut body = Vec::new();
    let mut response = response;
    loop {
        let chunk = tokio::select! {
            c = response.chunk() => c,
            () = cancel.cancelled() => break,
        };
        match chunk {
            Ok(Some(chunk)) => {
                let room = MAX_ERROR_BODY_BYTES.saturating_sub(body.len());
                if room == 0 {
                    break;
                }
                let take = room.min(chunk.len());
                body.extend_from_slice(&chunk[..take]);
                if take < chunk.len() {
                    break;
                }
            }
            Ok(None) => break,
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&body).into_owned()
}

async fn read_json_capped<T: serde::de::DeserializeOwned>(
    mut response: reqwest::Response,
    cap: usize,
    cancel: &CancellationToken,
    name: &str,
) -> Result<T, ModelError> {
    let mut body = Vec::new();
    // A body that breaks off is the backend going away, as in a stream
    // (`stream.rs`); one framed wrongly is not. reqwest reports both as
    // `is_decode`, so `classify` alone cannot tell them apart.
    let read_error = |e: reqwest::Error| {
        if hung_up(&e) {
            ModelError::Unavailable(format!("{name}: the answer broke off: {e}"))
        } else {
            classify(&e)
        }
    };
    loop {
        let chunk = tokio::select! {
            c = response.chunk() => c.map_err(read_error)?,
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
        };
        let Some(chunk) = chunk else { break };
        if body.len() + chunk.len() > cap {
            return Err(ModelError::Provider(format!(
                "{name} response exceeds {cap} bytes"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body)
        .map_err(|e| ModelError::Provider(format!("{name} returned invalid JSON: {e}")))
}

fn usage_of(u: Option<OaUsage>) -> Usage {
    u.map_or(
        Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
        },
        |u| Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            cost_usd: 0.0,
        },
    )
}

fn classify(err: &reqwest::Error) -> ModelError {
    if err.is_connect() || err.is_timeout() || (err.is_request() && hung_up(err)) {
        ModelError::Unavailable(err.to_string())
    } else {
        ModelError::Provider(err.to_string())
    }
}

/// A backend that hangs up after the request went out (Ollama stopped while
/// it reads the prompt, or an idle pooled connection it had closed) is going
/// away, like one that cannot be reached. An answer that is not HTTP is not
/// that: it stays the backend's fault.
///
/// hyper's three: `is_incomplete_message` is a close before the answer was
/// whole. `is_canceled` is also a request that was never sent: hyper cancels
/// one still queued when its connection fails or closes ("the message was
/// never even started", hyper 1.11 `proto/h1/dispatch.rs`), so trying again
/// later is as safe as after a refused connect. `is_closed` is the same kind:
/// the request was handed to a connection whose task had already ended
/// (hyper 1.11 `client/dispatch.rs` `Sender::poll_ready`; hyper-util 0.1
/// `legacy/client.rs`, a fresh connection that died before it was ready or a
/// pooled one that had closed), so it was never sent either.
fn hung_up(err: &reqwest::Error) -> bool {
    let mut source = std::error::Error::source(err);
    while let Some(e) = source {
        if let Some(h) = e.downcast_ref::<hyper::Error>()
            && (h.is_incomplete_message() || h.is_canceled() || h.is_closed())
        {
            return true;
        }
        if let Some(io) = e.downcast_ref::<std::io::Error>()
            && went_away(io.kind())
        {
            return true;
        }
        source = e.source();
    }
    false
}

/// The ways a socket says the other end went away. A backend that closes
/// while a large prompt is still being written shows up as `BrokenPipe` or
/// `ConnectionReset`, whichever the write meets first; `ConnectionAborted` is
/// what Windows reports where Unix says reset; `UnexpectedEof` is a close in
/// the middle of the answer.
fn went_away(kind: std::io::ErrorKind) -> bool {
    use std::io::ErrorKind::{BrokenPipe, ConnectionAborted, ConnectionReset, UnexpectedEof};
    matches!(
        kind,
        ConnectionReset | ConnectionAborted | BrokenPipe | UnexpectedEof
    )
}

#[async_trait]
impl ModelProvider for OpenAiCompatProvider {
    fn name(&self) -> &str {
        &self.name
    }

    async fn complete(
        &self,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let messages: Vec<OaMessage> = req.messages.iter().map(OaMessage::from).collect();
        let mut body = serde_json::json!({
            "model": req.model.as_deref().unwrap_or(&self.default_model),
            "messages": messages,
            "stream": false,
        });
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| {
                        serde_json::json!({
                            "type": "function",
                            "function": {
                                "name": t.name,
                                "description": t.description,
                                "parameters": t.parameters,
                            }
                        })
                    })
                    .collect(),
            );
        }
        if let Some(rf) = &req.response_format {
            body["response_format"] = rf.to_wire();
        }
        // ME4-4.2.2a: only when set — an absent key keeps /chat's body unchanged.
        if let Some(n) = req.max_tokens {
            body["max_tokens"] = Value::from(n.get());
        }
        // ME4-S2b: the same "only when set" rule.
        if let Some(t) = req.temperature {
            body["temperature"] = Value::from(t);
        }
        // ME4-S2-thinking / review H2: same "only when set" rule — an unset
        // flag, OR a provider that never opted into this oMLX-specific
        // extension, keeps the body byte-for-byte unchanged. Gating on
        // `self.supports_chat_template_kwargs` (not `self.tier`) means a
        // hosted/managed OpenAI-compatible endpoint — even one some future
        // config mislabels `local` — never receives a field outside the
        // OpenAI contract it actually speaks.
        if req.disable_thinking && self.supports_chat_template_kwargs {
            body["chat_template_kwargs"] = serde_json::json!({ "enable_thinking": false });
        }
        // ME4-S2c §4: the same gate, for Ollama.
        if req.disable_thinking && self.supports_reasoning_effort {
            body["reasoning_effort"] = Value::from("none");
        }
        if req.progress.is_some() {
            if !req.tools.is_empty() {
                return Err(ModelError::Provider(format!(
                    "{}: a streamed request cannot carry tools",
                    self.name
                )));
            }
            body["stream"] = Value::Bool(true);
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }
        // ME4-S2c §4: a backend that does not know the thinking switch (an
        // Ollama version or model that refuses `"none"`, an OMLX_URL that is
        // not oMLX) refuses the whole request. Asked once more without it, it
        // answers as it did before the switch existed, thinking and all,
        // rather than fail every `complexity: "simple"` call. A 400 for any
        // other reason costs one more refusal.
        const SWITCHES: [&str; 2] = ["chat_template_kwargs", "reasoning_effort"];
        let switched = SWITCHES.iter().any(|k| body.get(k).is_some());
        match self.send(&body, req.progress.as_ref(), cancel).await {
            Err(e @ ModelError::Rejected { status: 400, .. }) if switched => {
                // A refusal whose error body a cancel cut short is a cancel;
                // one the call's deadline cut short stays the refusal it is,
                // as there is no time left to ask again.
                if cancel.is_cancelled() {
                    return Err(ModelError::Cancelled);
                }
                if let Some(p) = &req.progress
                    && stream::left(p, std::time::Instant::now()).is_zero()
                {
                    return Err(e);
                }
                tracing::warn!("{e}; asking again without the thinking switch");
                if let Some(fields) = body.as_object_mut() {
                    for k in SWITCHES {
                        fields.remove(k);
                    }
                }
                self.send(&body, req.progress.as_ref(), cancel).await
            }
            answer => answer,
        }
    }

    async fn models(&self, cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
        let fut = self
            .authed(self.client.get(format!("{}/v1/models", self.base_url)))
            .timeout(self.quick_timeout)
            .send();
        let response = tokio::select! {
            r = fut => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
        };
        if !response.status().is_success() {
            let status = response.status();
            let body = read_error_body(response, cancel).await;
            return Err(friendly_http_error(&self.name, status, &body));
        }
        let parsed: OaModelsResponse =
            read_json_capped(response, MAX_MODELS_RESPONSE_BYTES, cancel, &self.name).await?;
        Ok(parsed
            .data
            .into_iter()
            .map(|m| Model {
                id: m.id,
                provider: self.name.clone(),
                tier: self.tier.clone(),
                loaded: true,
            })
            .collect())
    }
}

// ── Ordered registry (fallback chain; M-D replaces order with ModelRouter) ───

pub struct ProviderRegistry {
    providers: Vec<Arc<dyn ModelProvider>>,
}

impl ProviderRegistry {
    pub fn new(providers: Vec<Arc<dyn ModelProvider>>) -> Self {
        Self { providers }
    }

    /// Default local chain: oMLX (8088) → Ollama (11434). Env overrides:
    /// OMLX_URL / OMLX_API_KEY / DEFAULT_MODEL (mirrors the node daemon).
    pub fn from_env() -> Self {
        let omlx_url =
            std::env::var("OMLX_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned());
        let omlx_key = std::env::var("OMLX_API_KEY").unwrap_or_else(|_| "xiaobao8088".to_owned());
        let default_model =
            std::env::var("DEFAULT_MODEL").unwrap_or_else(|_| "Qwen3-8B-4bit".to_owned());
        Self::new(vec![
            Arc::new(OpenAiCompatProvider::new(
                "omlx",
                omlx_url,
                Some(omlx_key),
                "local",
                default_model.clone(),
            )),
            Arc::new(OpenAiCompatProvider::new(
                "ollama",
                "http://127.0.0.1:11434",
                None,
                "local",
                default_model,
            )),
        ])
    }

    /// Try providers in order; only `Unavailable` falls through to the next.
    pub async fn complete(
        &self,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<(String, CompletionResponse), ModelError> {
        let mut last = ModelError::Unavailable("no providers registered".to_owned());
        for provider in &self.providers {
            match provider.complete(req, cancel).await {
                Ok(res) => return Ok((provider.name().to_owned(), res)),
                Err(ModelError::Unavailable(msg)) => {
                    tracing::debug!("provider {} unavailable: {msg}", provider.name());
                    last = ModelError::Unavailable(msg);
                }
                Err(other) => return Err(other),
            }
        }
        Err(last)
    }

    /// Union of models from reachable providers (unreachable ones are skipped).
    pub async fn models(&self, cancel: &CancellationToken) -> Vec<Model> {
        let mut all = Vec::new();
        for provider in &self.providers {
            match provider.models(cancel).await {
                Ok(mut models) => all.append(&mut models),
                Err(err) => tracing::debug!("models from {} failed: {err}", provider.name()),
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::time::Duration;

    #[test]
    fn response_format_json_schema_wire_shape() {
        let rf = ResponseFormat::JsonSchema {
            name: "sin90_intent".to_owned(),
            schema: serde_json::json!({"type": "object"}),
            strict: true,
        };
        let wire = rf.to_wire();
        assert_eq!(wire["type"], "json_schema");
        assert_eq!(wire["json_schema"]["name"], "sin90_intent");
        assert_eq!(wire["json_schema"]["strict"], true);
        assert_eq!(wire["json_schema"]["schema"]["type"], "object");
    }

    // ── H12: friendly provider errors ────────────────────────────────────────

    #[test]
    fn extracts_the_openai_error_message_and_code() {
        let body = r#"{"error":{"message":"Incorrect API key provided","code":"invalid_api_key","type":"auth"}}"#;
        assert_eq!(
            extract_provider_message(body).as_deref(),
            Some("Incorrect API key provided [invalid_api_key]")
        );
    }

    #[test]
    fn falls_back_to_a_bounded_snippet_for_a_non_json_body() {
        let msg = extract_provider_message("<html>502 Bad Gateway</html>").unwrap();
        assert!(msg.contains("502"));
        assert!(msg.len() <= 200);
        assert_eq!(extract_provider_message("   "), None);
    }

    #[test]
    fn status_becomes_a_cause_a_user_can_act_on() {
        use reqwest::StatusCode;
        let auth = friendly_http_error("openai", StatusCode::UNAUTHORIZED, "");
        assert!(matches!(auth, ModelError::Rejected { status: 401, .. }));
        assert!(auth.to_string().contains("authentication failed"));

        let missing = friendly_http_error("ollama", StatusCode::NOT_FOUND, "");
        assert!(matches!(missing, ModelError::Rejected { status: 404, .. }));
        assert!(missing.to_string().contains("model id"));
    }

    /// The one behaviour change H12 makes: a rate limit or a 5xx is transient
    /// and provider-local, so it becomes `Unavailable` and the router tries the
    /// next provider — an auth/model error stays `Provider` and stops.
    #[test]
    fn transient_errors_allow_fallthrough_config_errors_do_not() {
        use reqwest::StatusCode;
        assert!(matches!(
            friendly_http_error("p", StatusCode::TOO_MANY_REQUESTS, ""),
            ModelError::Unavailable(_)
        ));
        assert!(matches!(
            friendly_http_error("p", StatusCode::BAD_GATEWAY, ""),
            ModelError::Unavailable(_)
        ));
        assert!(matches!(
            friendly_http_error("p", StatusCode::FORBIDDEN, ""),
            ModelError::Rejected { status: 403, .. }
        ));
    }

    #[test]
    fn the_providers_own_message_is_quoted_when_present() {
        use reqwest::StatusCode;
        let body = r#"{"error":{"message":"You exceeded your current quota"}}"#;
        let err = friendly_http_error("openai", StatusCode::TOO_MANY_REQUESTS, body);
        let s = err.to_string();
        assert!(s.contains("rate limited"), "{s}");
        assert!(s.contains("exceeded your current quota"), "{s}");
    }

    struct HangingProvider;

    #[async_trait]
    impl ModelProvider for HangingProvider {
        fn name(&self) -> &str {
            "hanging"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            cancel.cancelled().await;
            Err(ModelError::Cancelled)
        }
        async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct FixedProvider(&'static str);

    #[async_trait]
    impl ModelProvider for FixedProvider {
        fn name(&self) -> &str {
            self.0
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Ok(CompletionResponse {
                message: Msg::assistant(Some(format!("from {}", self.0)), vec![]),
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost_usd: 0.0,
                },
                model_id: None,
            })
        }
        async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Ok(vec![])
        }
    }

    struct DownProvider;

    #[async_trait]
    impl ModelProvider for DownProvider {
        fn name(&self) -> &str {
            "down"
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Err(ModelError::Unavailable("connection refused".to_owned()))
        }
        async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Err(ModelError::Unavailable("connection refused".to_owned()))
        }
    }

    fn req() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Msg::user("hi")],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
            disable_thinking: false,
            temperature: None,
            progress: None,
        }
    }

    // ── J4 (ME4-4.2.2a): max_tokens forwarded only when set; model_id parsed ──

    fn req_with_max_tokens(max: Option<u32>) -> CompletionRequest {
        CompletionRequest {
            max_tokens: max.and_then(std::num::NonZeroU32::new),
            ..req()
        }
    }

    async fn read_request(sock: &mut tokio::net::TcpStream) -> String {
        use tokio::io::AsyncReadExt;
        let mut buf = vec![0u8; 65536];
        let mut got = Vec::new();
        loop {
            let n = sock.read(&mut buf).await.unwrap();
            got.extend_from_slice(&buf[..n]);
            let text = String::from_utf8_lossy(&got).to_string();
            if let Some(idx) = text.find("\r\n\r\n") {
                let len: usize = text
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length: ")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                if got.len() >= idx + 4 + len {
                    return text[idx + 4..].to_string();
                }
            }
        }
    }

    #[tokio::test]
    async fn max_tokens_is_forwarded_only_when_set_and_model_id_is_parsed() {
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut bodies = vec![];
            for _ in 0..2 {
                let (mut s, _) = l.accept().await.unwrap();
                bodies.push(read_request(&mut s).await);
                let body = r#"{"model":"stub-actual-7b","choices":[{"message":{"role":"assistant","content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).await.unwrap();
            }
            bodies
        });
        let p = OpenAiCompatProvider::new("omlx", url, None, "local", "Qwen3-8B-4bit");
        let c = CancellationToken::new();
        // model_id: parsed from the provider's own report, not back-filled
        // from the provider name ("omlx") or the requested model name.
        let r = p
            .complete(&req_with_max_tokens(Some(77)), &c)
            .await
            .unwrap();
        assert_eq!(
            r.model_id.as_deref(),
            Some("stub-actual-7b"),
            "not the provider name, not the requested name"
        );
        // Positive control: max_tokens absent → key absent from the body,
        // which is what keeps /api/v1/chat byte-for-byte unchanged.
        let _ = p.complete(&req_with_max_tokens(None), &c).await.unwrap();
        let bodies = server.await.unwrap();
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
        assert_eq!(first["max_tokens"], 77);
        assert!(
            second.get("max_tokens").is_none(),
            "absent → not sent (/chat unchanged)"
        );
    }

    // ── ME4-S2b J1, J5, J6: temperature sent only when set, by value ─────────

    #[tokio::test]
    async fn temperature_is_sent_only_when_set_and_no_penalty_ever_is() {
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut bodies = vec![];
            for _ in 0..3 {
                let (mut s, _) = l.accept().await.unwrap();
                bodies.push(read_request(&mut s).await);
                let body = r#"{"model":"stub","choices":[{"message":{"role":"assistant","content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).await.unwrap();
            }
            bodies
        });
        let p = OpenAiCompatProvider::new("ollama", url, None, "local", "qwen2.5:7b");
        let c = CancellationToken::new();
        let at = |t: Option<f64>| CompletionRequest {
            temperature: t,
            ..req()
        };
        for t in [Some(0.7), Some(0.0), None] {
            let _ = p.complete(&at(t), &c).await.unwrap();
        }
        let bodies: Vec<Value> = server
            .await
            .unwrap()
            .iter()
            .map(|b| serde_json::from_str(b).unwrap())
            .collect();
        // By value: 0.7 is not 0.699999988…, and 0 is written as 0.0.
        assert_eq!(bodies[0]["temperature"].as_f64(), Some(0.7));
        assert_eq!(bodies[1]["temperature"].as_f64(), Some(0.0));
        assert!(
            bodies[2].get("temperature").is_none(),
            "absent → not sent: every other caller's body is unchanged"
        );
        for b in &bodies {
            for k in ["frequency_penalty", "presence_penalty", "repeat_penalty"] {
                assert!(b.get(k).is_none(), "{k} in {b}");
            }
        }
        // J5: requests that differ only in temperature are not equal.
        assert_ne!(at(Some(0.0)), at(None));
        assert_eq!(at(Some(0.0)), at(Some(0.0)));
    }

    // ── ME4-S2-thinking: disable_thinking → chat_template_kwargs, gated ──────

    #[tokio::test]
    async fn disable_thinking_sets_chat_template_kwargs_only_when_true() {
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let mut bodies = vec![];
            for _ in 0..2 {
                let (mut s, _) = l.accept().await.unwrap();
                bodies.push(read_request(&mut s).await);
                let body = r#"{"model":"stub","choices":[{"message":{"role":"assistant","content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                s.write_all(resp.as_bytes()).await.unwrap();
            }
            bodies
        });
        let p = OpenAiCompatProvider::new("omlx", url, None, "local", "Qwen3-8B-4bit")
            .supports_chat_template_kwargs();
        let c = CancellationToken::new();
        let simple = CompletionRequest {
            disable_thinking: true,
            ..req()
        };
        let complex = CompletionRequest {
            disable_thinking: false,
            ..req()
        };
        let _ = p.complete(&simple, &c).await.unwrap();
        let _ = p.complete(&complex, &c).await.unwrap();
        let bodies = server.await.unwrap();
        let first: serde_json::Value = serde_json::from_str(&bodies[0]).unwrap();
        let second: serde_json::Value = serde_json::from_str(&bodies[1]).unwrap();
        assert_eq!(
            first["chat_template_kwargs"],
            serde_json::json!({ "enable_thinking": false }),
            "simple → thinking disabled in the wire body"
        );
        assert!(
            second.get("chat_template_kwargs").is_none(),
            "complex/unset → key absent, body unchanged from before this feature"
        );
    }

    /// Review H2: `chat_template_kwargs` is an oMLX/mlx-lm-server extension,
    /// not part of the OpenAI-compatible contract every provider here claims
    /// to speak. A provider that never opted in (a hosted/managed remote
    /// endpoint, or even a local one that isn't actually oMLX) must NEVER
    /// receive it, no matter what `disable_thinking` says — this is the
    /// negative control `disable_thinking_sets_chat_template_kwargs_only_when_true`
    /// doesn't cover (that test's provider always opts in).
    #[tokio::test]
    async fn a_provider_that_never_opted_in_never_sends_chat_template_kwargs_even_with_disable_thinking()
     {
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let body = read_request(&mut s).await;
            let resp_body = r#"{"model":"stub","choices":[{"message":{"role":"assistant","content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{resp_body}",
                resp_body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            body
        });
        // Deliberately NOT `.supports_chat_template_kwargs()` — this is the
        // default every remote/hosted provider (and Ollama) gets from
        // `router.rs::from_env`.
        let p = OpenAiCompatProvider::new("openrouter", url, None, "remote", "gpt-x");
        let c = CancellationToken::new();
        let simple = CompletionRequest {
            disable_thinking: true,
            ..req()
        };
        let _ = p.complete(&simple, &c).await.unwrap();
        let body: serde_json::Value = serde_json::from_str(&server.await.unwrap()).unwrap();
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "a provider that never opted in must never see this field, even when disable_thinking is true"
        );
    }

    #[test]
    fn the_ways_a_socket_says_the_backend_went_away() {
        use std::io::ErrorKind::*;
        for k in [
            BrokenPipe,
            ConnectionReset,
            ConnectionAborted,
            UnexpectedEof,
        ] {
            assert!(went_away(k), "{k:?}");
        }
        // Controls: a framing or data error is the backend's fault.
        for k in [InvalidData, InvalidInput, TimedOut, Other] {
            assert!(!went_away(k), "{k:?}");
        }
    }

    // ── ME4-S2c §4: disable_thinking → reasoning_effort, and the 400 retry ──

    /// A whole HTTP answer: `status`, then `body` as JSON.
    fn reply(status: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn ok_whole() -> String {
        reply(
            "200 OK",
            r#"{"model":"stub","choices":[{"message":{"role":"assistant","content":"x"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
        )
    }

    fn ok_streamed() -> String {
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\ndata: {\"choices\":[{\"delta\":{\"content\":\"x\"}}]}\n\ndata: [DONE]\n\n".to_owned()
    }

    fn refused(status: &str) -> String {
        reply(status, r#"{"error":{"message":"bad field"}}"#)
    }

    /// A backend that answers its n-th request with `replies[n]` (the last one
    /// once they run out). Every body it was sent, in order.
    async fn answers_in_turn(
        replies: Vec<String>,
    ) -> (String, Arc<std::sync::Mutex<Vec<serde_json::Value>>>) {
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let asked = Arc::new(std::sync::Mutex::new(vec![]));
        let seen = Arc::clone(&asked);
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                let body = read_request(&mut s).await;
                let n = {
                    let mut seen = seen.lock().unwrap();
                    seen.push(serde_json::from_str(&body).unwrap());
                    seen.len() - 1
                };
                let reply = &replies[n.min(replies.len() - 1)];
                s.write_all(reply.as_bytes()).await.unwrap();
            }
        });
        (url, asked)
    }

    /// What this thread logs from here on, as text.
    #[derive(Clone, Default)]
    struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
        type Writer = Logs;
        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    impl Logs {
        fn capture() -> (Logs, tracing::subscriber::DefaultGuard) {
            let logs = Logs::default();
            let subscriber = tracing_subscriber::fmt()
                .with_writer(logs.clone())
                .with_ansi(false)
                .finish();
            (logs, tracing::subscriber::set_default(subscriber))
        }

        fn retries(&self) -> usize {
            String::from_utf8_lossy(&self.0.lock().unwrap())
                .matches("asking again without the thinking switch")
                .count()
        }
    }

    fn thinking_off(progress: Option<Progress>) -> CompletionRequest {
        CompletionRequest {
            disable_thinking: true,
            progress,
            ..req()
        }
    }

    fn streamed() -> Option<Progress> {
        Some(Progress {
            deadline: std::time::Instant::now() + Duration::from_secs(5),
            first_chunk: Duration::from_secs(2),
            stall: Duration::from_secs(2),
        })
    }

    #[tokio::test]
    async fn disable_thinking_sets_reasoning_effort_only_on_a_provider_that_opted_in() {
        // J1: opted in → "none" when asked, absent otherwise.
        let (url, asked) = answers_in_turn(vec![ok_whole()]).await;
        let p = OpenAiCompatProvider::new("ollama", url, None, "local", "qwen3")
            .supports_reasoning_effort();
        let c = CancellationToken::new();
        p.complete(&thinking_off(None), &c).await.unwrap();
        p.complete(&req(), &c).await.unwrap();
        // J2: oMLX's switch and none at all never carry it.
        let (url, omlx) = answers_in_turn(vec![ok_whole()]).await;
        OpenAiCompatProvider::new("omlx", url, None, "local", "qwen3")
            .supports_chat_template_kwargs()
            .complete(&thinking_off(None), &c)
            .await
            .unwrap();
        let (url, plain) = answers_in_turn(vec![ok_whole()]).await;
        OpenAiCompatProvider::new("remote", url, None, "remote", "gpt-x")
            .complete(&thinking_off(None), &c)
            .await
            .unwrap();
        let asked = asked.lock().unwrap();
        assert_eq!(asked[0]["reasoning_effort"], "none");
        assert!(
            asked[0].get("chat_template_kwargs").is_none(),
            "{}",
            asked[0]
        );
        assert!(asked[1].get("reasoning_effort").is_none(), "{}", asked[1]);
        let omlx = &omlx.lock().unwrap()[0];
        assert!(omlx.get("reasoning_effort").is_none(), "{omlx}");
        assert!(omlx.get("chat_template_kwargs").is_some(), "{omlx}");
        let plain = &plain.lock().unwrap()[0];
        assert!(plain.get("reasoning_effort").is_none(), "{plain}");
    }

    #[tokio::test]
    async fn a_refused_thinking_switch_is_dropped_and_the_request_asked_once_more() {
        // J2b, on both paths: a 400 → the same request without the switch.
        let c = CancellationToken::new();
        let ollama = |url| {
            OpenAiCompatProvider::new("ollama", url, None, "local", "qwen3")
                .supports_reasoning_effort()
        };
        let omlx = |url| {
            OpenAiCompatProvider::new("omlx", url, None, "local", "qwen3")
                .supports_chat_template_kwargs()
        };
        for (progress, ok) in [(None, ok_whole()), (streamed(), ok_streamed())] {
            for (key, build) in [
                (
                    "reasoning_effort",
                    &ollama as &dyn Fn(String) -> OpenAiCompatProvider,
                ),
                ("chat_template_kwargs", &omlx),
            ] {
                let (url, asked) =
                    answers_in_turn(vec![refused("400 Bad Request"), ok.clone()]).await;
                let (logs, _guard) = Logs::capture();
                let got = build(url).complete(&thinking_off(progress), &c).await;
                assert_eq!(got.unwrap().message.content.as_deref(), Some("x"));
                assert_eq!(logs.retries(), 1, "{key}: the retry is logged");
                let asked = asked.lock().unwrap().clone();
                assert_eq!(asked.len(), 2, "{key}: {asked:?}");
                assert!(asked[0].get(key).is_some(), "{key}: {}", asked[0]);
                let mut without = asked[0].clone();
                without.as_object_mut().unwrap().remove(key);
                assert_eq!(asked[1], without, "{key}: only the switch is dropped");
                // Refused again: that refusal stands, after one retry only.
                let (url, again) = answers_in_turn(vec![refused("400 Bad Request")]).await;
                let got = build(url).complete(&thinking_off(progress), &c).await;
                assert!(
                    matches!(got, Err(ModelError::Rejected { status: 400, .. })),
                    "{key}: {got:?}"
                );
                assert_eq!(again.lock().unwrap().len(), 2, "{key}");
            }
            // Controls: no switch in the request, or a refusal that is not a
            // 400 → no retry.
            let (url, asked) = answers_in_turn(vec![refused("400 Bad Request")]).await;
            let got = ollama(url)
                .complete(&CompletionRequest { progress, ..req() }, &c)
                .await;
            assert!(
                matches!(got, Err(ModelError::Rejected { status: 400, .. })),
                "{got:?}"
            );
            assert_eq!(asked.lock().unwrap().len(), 1);
            let (url, asked) = answers_in_turn(vec![refused("422 Unprocessable Entity")]).await;
            let got = ollama(url).complete(&thinking_off(progress), &c).await;
            assert!(
                matches!(got, Err(ModelError::Rejected { status: 422, .. })),
                "{got:?}"
            );
            assert_eq!(asked.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn a_provider_with_both_switches_drops_both_when_refused() {
        let (url, asked) = answers_in_turn(vec![refused("400 Bad Request"), ok_whole()]).await;
        let p = OpenAiCompatProvider::new("p", url, None, "local", "qwen3")
            .supports_chat_template_kwargs()
            .supports_reasoning_effort();
        let c = CancellationToken::new();
        p.complete(&thinking_off(None), &c).await.unwrap();
        let asked = asked.lock().unwrap().clone();
        assert_eq!(asked.len(), 2);
        assert!(asked[0].get("reasoning_effort").is_some(), "{}", asked[0]);
        assert!(
            asked[0].get("chat_template_kwargs").is_some(),
            "{}",
            asked[0]
        );
        for k in ["reasoning_effort", "chat_template_kwargs"] {
            assert!(asked[1].get(k).is_none(), "{k}: {}", asked[1]);
        }
    }

    #[tokio::test]
    async fn a_refusal_cut_short_by_a_cancel_is_not_asked_again() {
        // The whole path reads a refusal's body until cancelled, then still
        // reports the refusal: it must not be taken for one to retry.
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((mut s, _)) = l.accept().await {
                let _ = read_request(&mut s).await;
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let head = "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"error\":";
                s.write_all(head.as_bytes()).await.unwrap();
                held.push(s);
            }
        });
        let p = OpenAiCompatProvider::new("ollama", url, None, "local", "qwen3")
            .supports_reasoning_effort();
        let c = CancellationToken::new();
        let later = c.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            later.cancel();
        });
        let (logs, _guard) = Logs::capture();
        let got = p.complete(&thinking_off(None), &c).await;
        assert!(matches!(got, Err(ModelError::Cancelled)), "{got:?}");
        assert_eq!(logs.retries(), 0, "a cancel is no refusal to retry");
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_refusal_cut_short_by_the_deadline_is_not_asked_again() {
        // Its error body never ends; the call's deadline comes first. That is
        // still the refusal, not a backend gone away.
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = Arc::clone(&count);
        tokio::spawn(async move {
            let mut held = vec![];
            while let Ok((mut s, _)) = l.accept().await {
                let _ = read_request(&mut s).await;
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let head = "HTTP/1.1 400 Bad Request\r\ncontent-type: application/json\r\ncontent-length: 100\r\n\r\n{\"error\":";
                s.write_all(head.as_bytes()).await.unwrap();
                held.push(s);
            }
        });
        let p = OpenAiCompatProvider::new("ollama", url, None, "local", "qwen3")
            .supports_reasoning_effort();
        let progress = Some(Progress {
            deadline: std::time::Instant::now() + Duration::from_millis(300),
            first_chunk: Duration::from_millis(200),
            stall: Duration::from_millis(200),
        });
        let (logs, _guard) = Logs::capture();
        let got = p
            .complete(&thinking_off(progress), &CancellationToken::new())
            .await;
        assert!(
            matches!(got, Err(ModelError::Rejected { status: 400, .. })),
            "{got:?}"
        );
        assert_eq!(logs.retries(), 0);
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The answer of a whole response: what a streamed one's end and the
    /// non-streaming fallback both go through.
    #[test]
    fn a_whole_answer_takes_its_first_choice_a_named_model_and_its_usage() {
        let p = OpenAiCompatProvider::new("ollama", "http://127.0.0.1:1", None, "local", "m");
        let parsed = |v: Value| serde_json::from_value::<OaChatResponse>(v).unwrap();
        let msg = serde_json::json!({ "role": "assistant", "content": "x" });
        // An empty model is no model.
        let r = p
            .answer(parsed(
                serde_json::json!({ "model": "", "choices": [{ "message": msg }] }),
            ))
            .unwrap();
        assert_eq!(r.model_id, None);
        // No choices is an error, named for the provider.
        match p.answer(parsed(serde_json::json!({ "model": "m", "choices": [] }))) {
            Err(ModelError::Provider(m)) => assert_eq!(m, "ollama returned no choices"),
            other => panic!("{other:?}"),
        }
        // No usage is zeros, and a cost is never claimed.
        let zero = usage_of(None);
        assert_eq!(
            (
                zero.prompt_tokens,
                zero.completion_tokens,
                zero.total_tokens,
                zero.cost_usd
            ),
            (0, 0, 0, 0.0)
        );
        let r = p.answer(parsed(serde_json::json!({ "choices": [{ "message": msg }], "usage": { "prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7 } }))).unwrap();
        assert_eq!(
            (
                r.usage.prompt_tokens,
                r.usage.completion_tokens,
                r.usage.total_tokens,
                r.usage.cost_usd
            ),
            (3, 4, 7, 0.0)
        );
    }

    #[tokio::test]
    async fn a_provider_that_omits_the_model_field_reports_no_model_id() {
        use tokio::io::AsyncWriteExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let _ = read_request(&mut s).await;
            let body = r#"{"choices":[{"message":{"role":"assistant","content":"x"}}]}"#;
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
        });
        let p = OpenAiCompatProvider::new("omlx", url, None, "local", "m");
        let r = p
            .complete(&req_with_max_tokens(None), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(r.model_id, None);
    }

    #[tokio::test]
    async fn hanging_tcp_server_times_out_as_unavailable() {
        // Real socket that accepts connections but never responds — the
        // request timeout (not cooperation) must bound the call.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    break;
                };
                // hold the socket open, never write
                tokio::spawn(async move {
                    let _sock = sock;
                    tokio::time::sleep(Duration::from_secs(60)).await;
                });
            }
        });
        let provider =
            OpenAiCompatProvider::new("hang", format!("http://{addr}"), None, "local", "m")
                .with_timeouts(Duration::from_millis(200), Duration::from_millis(200));
        let cancel = CancellationToken::new();
        let started = std::time::Instant::now();
        let chat = provider.complete(&req(), &cancel).await;
        assert!(matches!(chat, Err(ModelError::Unavailable(_))), "{chat:?}");
        let models = provider.models(&cancel).await;
        assert!(
            matches!(models, Err(ModelError::Unavailable(_))),
            "{models:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "timeout not bounded"
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_a_hanging_provider_promptly() {
        let registry = ProviderRegistry::new(vec![Arc::new(HangingProvider)]);
        let cancel = CancellationToken::new();
        let canceller = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            canceller.cancel();
        });
        let started = std::time::Instant::now();
        let result = registry.complete(&req(), &cancel).await;
        assert!(matches!(result, Err(ModelError::Cancelled)));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancel was not prompt"
        );
    }

    #[tokio::test]
    async fn unavailable_falls_through_to_next_provider() {
        let registry = ProviderRegistry::new(vec![
            Arc::new(DownProvider),
            Arc::new(FixedProvider("second")),
        ]);
        let cancel = CancellationToken::new();
        let (name, res) = registry.complete(&req(), &cancel).await.unwrap();
        assert_eq!(name, "second");
        assert_eq!(res.message.content.as_deref(), Some("from second"));
    }

    #[tokio::test]
    async fn all_unavailable_surfaces_unavailable() {
        let registry = ProviderRegistry::new(vec![Arc::new(DownProvider)]);
        let cancel = CancellationToken::new();
        let result = registry.complete(&req(), &cancel).await;
        assert!(matches!(result, Err(ModelError::Unavailable(_))));
    }

    #[tokio::test]
    async fn provider_error_does_not_fall_through() {
        struct BadProvider;
        #[async_trait]
        impl ModelProvider for BadProvider {
            fn name(&self) -> &str {
                "bad"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                Err(ModelError::Provider("500 from upstream".to_owned()))
            }
            async fn models(&self, _cancel: &CancellationToken) -> Result<Vec<Model>, ModelError> {
                Ok(vec![])
            }
        }
        let registry = ProviderRegistry::new(vec![
            Arc::new(BadProvider),
            Arc::new(FixedProvider("later")),
        ]);
        let cancel = CancellationToken::new();
        let result = registry.complete(&req(), &cancel).await;
        assert!(matches!(result, Err(ModelError::Provider(_))));
    }
}
