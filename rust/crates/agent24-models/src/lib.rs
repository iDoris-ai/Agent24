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

use agent24_protocol::{ChatMessage, Model, Usage};
use async_trait::async_trait;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionRequest {
    pub messages: Vec<ChatMessage>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CompletionResponse {
    pub message: ChatMessage,
    pub usage: Usage,
}

#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// Provider unreachable / refused — the registry tries the next provider
    #[error("provider unavailable: {0}")]
    Unavailable(String),
    /// Provider reachable but the call failed — NOT retried on another provider
    #[error("provider error: {0}")]
    Provider(String),
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
            client: reqwest::Client::builder()
                .connect_timeout(Duration::from_secs(2))
                .build()
                .unwrap_or_default(),
            chat_timeout: Duration::from_secs(120),
            quick_timeout: Duration::from_secs(5),
        }
    }

    /// Override request budgets (tests use tiny values against hanging servers)
    #[must_use]
    pub fn with_timeouts(mut self, chat: Duration, quick: Duration) -> Self {
        self.chat_timeout = chat;
        self.quick_timeout = quick;
        self
    }

    fn authed(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) if !key.is_empty() => rb.bearer_auth(key),
            _ => rb,
        }
    }
}

#[derive(Deserialize)]
struct OaChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
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

fn classify(err: &reqwest::Error) -> ModelError {
    if err.is_connect() || err.is_timeout() {
        ModelError::Unavailable(err.to_string())
    } else {
        ModelError::Provider(err.to_string())
    }
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
        let body = serde_json::json!({
            "model": req.model.as_deref().unwrap_or(&self.default_model),
            "messages": req.messages,
            "stream": false,
        });
        let fut = self
            .authed(
                self.client
                    .post(format!("{}/v1/chat/completions", self.base_url)),
            )
            .timeout(self.chat_timeout)
            .json(&body)
            .send();
        let response = tokio::select! {
            r = fut => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
        };
        if !response.status().is_success() {
            return Err(ModelError::Provider(format!(
                "{} returned HTTP {}",
                self.name,
                response.status()
            )));
        }
        let parsed: OaChatResponse = tokio::select! {
            r = response.json() => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
        };
        let choice =
            parsed.choices.into_iter().next().ok_or_else(|| {
                ModelError::Provider(format!("{} returned no choices", self.name))
            })?;
        let usage = parsed.usage.map_or(
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
        );
        Ok(CompletionResponse {
            message: choice.message,
            usage,
        })
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
            return Err(ModelError::Provider(format!(
                "{} returned HTTP {}",
                self.name,
                response.status()
            )));
        }
        let parsed: OaModelsResponse = tokio::select! {
            r = response.json() => r.map_err(|e| classify(&e))?,
            () = cancel.cancelled() => return Err(ModelError::Cancelled),
        };
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
                message: ChatMessage {
                    role: "assistant".to_owned(),
                    content: format!("from {}", self.0),
                },
                usage: Usage {
                    prompt_tokens: 1,
                    completion_tokens: 1,
                    total_tokens: 2,
                    cost_usd: 0.0,
                },
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
            messages: vec![ChatMessage {
                role: "user".to_owned(),
                content: "hi".to_owned(),
            }],
            model: None,
        }
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
        assert_eq!(res.message.content, "from second");
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
