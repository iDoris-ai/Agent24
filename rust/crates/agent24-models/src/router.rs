//! ModelRouter (M-D / D2) — the first-class routing/health/cooldown layer that
//! sits ABOVE the minimal [`ModelProvider`](crate::ModelProvider) trait.
//!
//! It picks a provider per request from a [`TaskProfile`]:
//! - **privacy**: a `LocalOnly` task is fail-closed to local tiers — it is
//!   NEVER routed to a remote provider, even if every local provider is down
//!   (it errors instead of leaking sensitive data off-device).
//! - **complexity**: a `Simple` task prefers a fast local model; a `Complex`
//!   one prefers a more capable (usually remote) model, falling back to local.
//! - **health/cooldown**: a provider that returns `Unavailable` enters an
//!   exponential-backoff cooldown and is skipped until it expires; a success
//!   clears it. This is the closed feedback loop the bare trait deliberately
//!   omits.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::{CompletionRequest, CompletionResponse, ModelError, ModelProvider};

/// Routing tier. `Lora` is a locally-served fine-tune, so it counts as local
/// for privacy purposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Local,
    Remote,
    Lora,
}

impl Tier {
    /// Parse the open-enum tier string; anything unknown is treated as Remote
    /// (the conservative default — an unrecognised tier must never be assumed
    /// local and thus never satisfy a LocalOnly task).
    pub fn parse(s: &str) -> Tier {
        match s {
            "local" => Tier::Local,
            "lora" => Tier::Lora,
            _ => Tier::Remote,
        }
    }

    /// True for on-device tiers (Local + Lora) — the tiers a LocalOnly task may
    /// use.
    pub fn is_local(self) -> bool {
        matches!(self, Tier::Local | Tier::Lora)
    }
}

/// Privacy label. `LocalOnly` forbids any remote provider for this request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Privacy {
    #[default]
    Any,
    LocalOnly,
}

/// Task complexity — steers the tier preference order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Complexity {
    #[default]
    Simple,
    Complex,
}

/// What the router needs to know about a request to choose a provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct TaskProfile {
    pub privacy: Privacy,
    pub complexity: Complexity,
}

impl TaskProfile {
    /// The tier preference order for this profile. A LocalOnly task NEVER
    /// yields Remote — that is the privacy guarantee, enforced here.
    fn tier_order(&self) -> &'static [Tier] {
        match (self.privacy, self.complexity) {
            // Sensitive: local tiers only, never remote.
            (Privacy::LocalOnly, _) => &[Tier::Local, Tier::Lora],
            // Simple + shareable: prefer the fast local model, then remote.
            (Privacy::Any, Complexity::Simple) => &[Tier::Local, Tier::Lora, Tier::Remote],
            // Complex + shareable: prefer the capable remote model, fall back local.
            (Privacy::Any, Complexity::Complex) => &[Tier::Remote, Tier::Local, Tier::Lora],
        }
    }
}

/// Per-provider health for the cooldown feedback loop.
#[derive(Debug, Clone, Default)]
struct Health {
    consecutive_failures: u32,
    /// When set, the provider is skipped until this instant.
    cooldown_until: Option<Instant>,
}

struct Routed {
    provider: Arc<dyn ModelProvider>,
    tier: Tier,
}

/// Routes completions across tiered providers with a health/cooldown loop.
pub struct ModelRouter {
    providers: Vec<Routed>,
    health: Mutex<HashMap<String, Health>>,
    base_cooldown: Duration,
    max_cooldown: Duration,
}

/// Absolute ceiling on any cooldown, independent of the caller's `max_cooldown`
/// — keeps `now + backoff` far from `Instant`'s representable boundary so it
/// can never overflow (review D2).
const COOLDOWN_HARD_CAP: Duration = Duration::from_secs(24 * 3600);

impl ModelRouter {
    /// Build from `(provider, tier)` pairs. Cooldown grows exponentially from
    /// `base_cooldown`, capped at `max_cooldown`.
    ///
    /// PRIVACY CONTRACT: the [`Tier`] label states WHERE a provider runs and is
    /// the sole basis of the LocalOnly guarantee. `Tier::Local` / `Tier::Lora`
    /// MUST be on-device endpoints — the router cannot introspect a provider's
    /// URL, so mislabeling a remote endpoint as local would route sensitive
    /// (LocalOnly) traffic to it. [`from_env`](Self::from_env) labels correctly;
    /// any hand-built router must uphold this.
    pub fn new(
        providers: Vec<(Arc<dyn ModelProvider>, Tier)>,
        base_cooldown: Duration,
        max_cooldown: Duration,
    ) -> Self {
        // Clamp so exponential backoff can never approach the Instant boundary.
        let max_cooldown = max_cooldown.min(COOLDOWN_HARD_CAP);
        let base_cooldown = base_cooldown
            .min(max_cooldown)
            .max(Duration::from_millis(1));
        Self {
            providers: providers
                .into_iter()
                .map(|(provider, tier)| Routed { provider, tier })
                .collect(),
            health: Mutex::new(HashMap::new()),
            base_cooldown,
            max_cooldown,
        }
    }

    /// Convenience default: 2s base, 60s cap.
    pub fn with_defaults(providers: Vec<(Arc<dyn ModelProvider>, Tier)>) -> Self {
        Self::new(providers, Duration::from_secs(2), Duration::from_secs(60))
    }

    /// The default local chain as tiered providers: oMLX (8088) and Ollama
    /// (11434), both Local tier. Mirrors `ProviderRegistry::from_env` env vars
    /// (OMLX_URL / OMLX_API_KEY / DEFAULT_MODEL). A remote/lora provider is
    /// added by the daemon when configured; with only local providers, a
    /// Complex task simply falls back to them.
    pub fn from_env() -> Self {
        let omlx_url =
            std::env::var("OMLX_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned());
        let omlx_key = std::env::var("OMLX_API_KEY").unwrap_or_else(|_| "xiaobao8088".to_owned());
        let default_model =
            std::env::var("DEFAULT_MODEL").unwrap_or_else(|_| "Qwen3-8B-4bit".to_owned());
        let omlx: Arc<dyn ModelProvider> = Arc::new(crate::OpenAiCompatProvider::new(
            "omlx",
            omlx_url,
            Some(omlx_key),
            "local",
            default_model.clone(),
        ));
        let ollama: Arc<dyn ModelProvider> = Arc::new(crate::OpenAiCompatProvider::new(
            "ollama",
            "http://127.0.0.1:11434",
            None,
            "local",
            default_model,
        ));
        Self::with_defaults(vec![(omlx, Tier::Local), (ollama, Tier::Local)])
    }

    /// Provider indices to try, in order, for `profile` at `now`: tier
    /// preference (privacy-filtered) with cooled-down providers skipped.
    /// Pure of real time — `now` is supplied so routing is unit-testable.
    fn route(&self, profile: TaskProfile, now: Instant) -> Vec<usize> {
        let health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        let mut order = Vec::new();
        for &want in profile.tier_order() {
            for (idx, r) in self.providers.iter().enumerate() {
                if r.tier != want {
                    continue;
                }
                let cooling = health
                    .get(r.provider.name())
                    .and_then(|h| h.cooldown_until)
                    .is_some_and(|until| now < until);
                if !cooling {
                    order.push(idx);
                }
            }
        }
        order
    }

    fn record_failure(&self, name: &str, now: Instant) {
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        let entry = health.entry(name.to_owned()).or_default();
        entry.consecutive_failures = entry.consecutive_failures.saturating_add(1);
        // Exponential backoff: base * 2^(failures-1), capped.
        let shift = entry.consecutive_failures.saturating_sub(1).min(16);
        let backoff = self
            .base_cooldown
            .saturating_mul(1u32 << shift)
            .min(self.max_cooldown);
        // checked_add is belt-and-suspenders — max_cooldown is clamped to
        // COOLDOWN_HARD_CAP so this cannot realistically overflow.
        entry.cooldown_until = now.checked_add(backoff);
    }

    fn record_success(&self, name: &str) {
        let mut health = self.health.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(entry) = health.get_mut(name) {
            entry.consecutive_failures = 0;
            entry.cooldown_until = None;
        }
    }

    /// Route and complete. Only `Unavailable` falls through to the next routed
    /// provider (and records a cooldown); `Provider`/`Cancelled` errors stop
    /// immediately. A LocalOnly task with no available local provider errors
    /// rather than ever touching a remote one.
    pub async fn complete(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<(String, CompletionResponse), ModelError> {
        let route = self.route(profile, Instant::now());
        if route.is_empty() {
            return Err(ModelError::Unavailable(match profile.privacy {
                Privacy::LocalOnly => {
                    "no local provider available for a local-only task".to_owned()
                }
                Privacy::Any => "no provider available".to_owned(),
            }));
        }
        // Accumulate per-provider reasons so an all-unavailable error names
        // which providers were tried and why (review D2 minor).
        let mut tried: Vec<String> = Vec::new();
        for idx in route {
            let r = &self.providers[idx];
            match r.provider.complete(req, cancel).await {
                Ok(res) => {
                    self.record_success(r.provider.name());
                    return Ok((r.provider.name().to_owned(), res));
                }
                Err(ModelError::Unavailable(msg)) => {
                    tracing::debug!("provider {} unavailable: {msg}", r.provider.name());
                    self.record_failure(r.provider.name(), Instant::now());
                    tried.push(format!("{}: {msg}", r.provider.name()));
                }
                // A reachable-but-failed call or a cancellation is terminal —
                // never retried on another provider (mirrors ProviderRegistry).
                Err(other) => return Err(other),
            }
        }
        Err(ModelError::Unavailable(format!(
            "all routed providers unavailable [{}]",
            tried.join(", ")
        )))
    }

    /// Union of models from reachable providers (unreachable ones skipped).
    pub async fn models(&self, cancel: &CancellationToken) -> Vec<crate::Model> {
        let mut all = Vec::new();
        for r in &self.providers {
            match r.provider.models(cancel).await {
                Ok(mut models) => all.append(&mut models),
                Err(err) => {
                    tracing::debug!("models from {} failed: {err}", r.provider.name())
                }
            }
        }
        all
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::{Msg, Usage};
    use async_trait::async_trait;
    use std::sync::Mutex as StdMutex;

    /// A provider that records its calls and returns a scripted outcome.
    struct StubProvider {
        name: &'static str,
        /// true = Unavailable, false = Ok
        unavailable: StdMutex<bool>,
        calls: StdMutex<usize>,
    }

    impl StubProvider {
        fn ok(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                unavailable: StdMutex::new(false),
                calls: StdMutex::new(0),
            })
        }
        fn down(name: &'static str) -> Arc<Self> {
            Arc::new(Self {
                name,
                unavailable: StdMutex::new(true),
                calls: StdMutex::new(0),
            })
        }
        fn calls(&self) -> usize {
            *self.calls.lock().unwrap()
        }
    }

    #[async_trait]
    impl ModelProvider for StubProvider {
        fn name(&self) -> &str {
            self.name
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            *self.calls.lock().unwrap() += 1;
            if *self.unavailable.lock().unwrap() {
                Err(ModelError::Unavailable(format!("{} down", self.name)))
            } else {
                Ok(CompletionResponse {
                    message: Msg::assistant(Some(format!("from {}", self.name)), vec![]),
                    usage: Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        cost_usd: 0.0,
                    },
                })
            }
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<crate::Model>, ModelError> {
            Ok(vec![])
        }
    }

    fn req() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Msg {
                role: "user".to_owned(),
                content: Some("hi".to_owned()),
                tool_calls: vec![],
                tool_call_id: None,
            }],
            model: None,
            tools: vec![],
        }
    }

    fn router(providers: Vec<(Arc<dyn ModelProvider>, Tier)>) -> ModelRouter {
        ModelRouter::new(providers, Duration::from_secs(10), Duration::from_secs(60))
    }

    #[test]
    fn tier_order_respects_privacy_and_complexity() {
        assert_eq!(
            TaskProfile {
                privacy: Privacy::LocalOnly,
                complexity: Complexity::Complex
            }
            .tier_order(),
            &[Tier::Local, Tier::Lora]
        );
        assert_eq!(
            TaskProfile {
                privacy: Privacy::Any,
                complexity: Complexity::Simple
            }
            .tier_order(),
            &[Tier::Local, Tier::Lora, Tier::Remote]
        );
        assert_eq!(
            TaskProfile {
                privacy: Privacy::Any,
                complexity: Complexity::Complex
            }
            .tier_order(),
            &[Tier::Remote, Tier::Local, Tier::Lora]
        );
    }

    #[tokio::test]
    async fn simple_task_prefers_local() {
        let local = StubProvider::ok("local");
        let remote = StubProvider::ok("remote");
        let r = router(vec![
            (remote.clone(), Tier::Remote),
            (local.clone(), Tier::Local),
        ]);
        let (name, _) = r
            .complete(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Simple,
                },
                &req(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(name, "local");
        assert_eq!(remote.calls(), 0); // remote never touched
    }

    #[tokio::test]
    async fn complex_task_prefers_remote() {
        let local = StubProvider::ok("local");
        let remote = StubProvider::ok("remote");
        let r = router(vec![
            (local.clone(), Tier::Local),
            (remote.clone(), Tier::Remote),
        ]);
        let (name, _) = r
            .complete(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Complex,
                },
                &req(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(name, "remote");
        assert_eq!(local.calls(), 0);
    }

    #[tokio::test]
    async fn local_only_never_falls_back_to_remote() {
        // Only a remote provider is registered; a LocalOnly task must ERROR,
        // never leak to remote.
        let remote = StubProvider::ok("remote");
        let r = router(vec![(remote.clone(), Tier::Remote)]);
        let err = r
            .complete(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    complexity: Complexity::Complex,
                },
                &req(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::Unavailable(_)), "{err}");
        assert_eq!(
            remote.calls(),
            0,
            "remote must never be called for LocalOnly"
        );
    }

    #[tokio::test]
    async fn local_only_uses_lora_as_a_local_tier() {
        let lora = StubProvider::ok("lora");
        let remote = StubProvider::ok("remote");
        let r = router(vec![
            (remote.clone(), Tier::Remote),
            (lora.clone(), Tier::Lora),
        ]);
        let (name, _) = r
            .complete(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    complexity: Complexity::Simple,
                },
                &req(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(name, "lora");
        assert_eq!(remote.calls(), 0);
    }

    #[tokio::test]
    async fn unavailable_falls_through_and_records_cooldown() {
        let down = StubProvider::down("local");
        let up = StubProvider::ok("remote");
        let r = router(vec![
            (down.clone(), Tier::Local),
            (up.clone(), Tier::Remote),
        ]);
        // Simple prefers local (down) → falls through to remote (up)
        let (name, _) = r
            .complete(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Simple,
                },
                &req(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(name, "remote");
        assert_eq!(down.calls(), 1);
        // the failed local is now cooling down
        let now = Instant::now();
        let route = r.route(
            TaskProfile {
                privacy: Privacy::Any,
                complexity: Complexity::Simple,
            },
            now,
        );
        // local (idx 0) is skipped while cooling; only remote (idx 1) routed
        assert_eq!(route, vec![1]);
    }

    #[test]
    fn cooldown_expires_and_backs_off_exponentially() {
        let down = StubProvider::down("local");
        let up = StubProvider::ok("remote");
        let r = router(vec![
            (down.clone(), Tier::Local),
            (up.clone(), Tier::Remote),
        ]);
        let profile = TaskProfile {
            privacy: Privacy::Any,
            complexity: Complexity::Simple,
        };
        let t0 = Instant::now();
        // both available initially
        assert_eq!(r.route(profile, t0), vec![0, 1]);
        // first failure → 10s cooldown
        r.record_failure("local", t0);
        assert_eq!(r.route(profile, t0 + Duration::from_secs(5)), vec![1]);
        assert_eq!(r.route(profile, t0 + Duration::from_secs(11)), vec![0, 1]);
        // second consecutive failure → 20s cooldown (exponential)
        r.record_failure("local", t0);
        assert_eq!(r.route(profile, t0 + Duration::from_secs(15)), vec![1]);
        assert_eq!(r.route(profile, t0 + Duration::from_secs(21)), vec![0, 1]);
        // success clears it
        r.record_success("local");
        assert_eq!(r.route(profile, t0), vec![0, 1]);
    }

    #[test]
    fn cooldown_is_capped_at_max() {
        let r = router(vec![(
            StubProvider::down("p") as Arc<dyn ModelProvider>,
            Tier::Local,
        )]);
        let t0 = Instant::now();
        // many failures — backoff must not exceed max_cooldown (60s)
        for _ in 0..20 {
            r.record_failure("p", t0);
        }
        let profile = TaskProfile::default();
        // still cooling at 59s, available again by 61s (capped at 60s, not huge)
        assert_eq!(r.route(profile, t0 + Duration::from_secs(59)).len(), 0);
        assert_eq!(r.route(profile, t0 + Duration::from_secs(61)).len(), 1);
    }

    #[test]
    fn tier_parse_defaults_unknown_to_remote() {
        assert_eq!(Tier::parse("local"), Tier::Local);
        assert_eq!(Tier::parse("lora"), Tier::Lora);
        assert_eq!(Tier::parse("remote"), Tier::Remote);
        assert_eq!(Tier::parse("anything-else"), Tier::Remote); // conservative
        assert!(Tier::Local.is_local() && Tier::Lora.is_local());
        assert!(!Tier::Remote.is_local());
    }

    #[tokio::test]
    async fn cancelled_is_terminal_and_does_not_fall_through() {
        struct CancelledProvider;
        #[async_trait]
        impl ModelProvider for CancelledProvider {
            fn name(&self) -> &str {
                "cancelled"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                Err(ModelError::Cancelled)
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<crate::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let fallback = StubProvider::ok("remote");
        let r = router(vec![
            (Arc::new(CancelledProvider), Tier::Local),
            (fallback.clone(), Tier::Remote),
        ]);
        let err = r
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::Cancelled), "{err}");
        assert_eq!(fallback.calls(), 0, "cancellation must not fall through");
    }

    #[tokio::test]
    async fn all_unavailable_error_names_the_tried_providers() {
        let a = StubProvider::down("local");
        let b = StubProvider::down("remote");
        let r = router(vec![(a, Tier::Local), (b, Tier::Remote)]);
        let err = r
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("local: local down"), "{msg}");
        assert!(msg.contains("remote: remote down"), "{msg}");
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
                Err(ModelError::Provider("500".to_owned()))
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<crate::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let fallback = StubProvider::ok("remote");
        let r = router(vec![
            (Arc::new(BadProvider), Tier::Local),
            (fallback.clone(), Tier::Remote),
        ]);
        let err = r
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(matches!(err, ModelError::Provider(_)), "{err}");
        assert_eq!(fallback.calls(), 0, "Provider error must not fall through");
    }
}
