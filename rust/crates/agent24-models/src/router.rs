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
    /// On-device. v3.1 L-1: a provider carrying this label MUST be an
    /// `OpenAiCompatProvider` built with `loopback_only()` (or an equivalent
    /// that neither reads proxy variables nor follows redirects) pointing at a
    /// loopback address — the LocalOnly guarantee is exactly as good as this
    /// label. `from_env` does it; a hand-built router must too.
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

/// ME4-4.2.2a: a successful completion plus where it ran. `tier` is the
/// routing label of the provider that answered — the fact the per-module
/// usage ledger (`served_by`) and the LocalOnly tripwire both read.
#[derive(Debug, Clone)]
pub struct Served {
    pub provider: String,
    pub tier: Tier,
    pub response: CompletionResponse,
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

/// ME4-S2 H1: is `url` — parsed by the SAME parser the HTTP client uses
/// (`reqwest::Url`, WHATWG) — an on-device endpoint? IPv4/IPv6 literals:
/// `is_loopback()` (so `::ffff:127.0.0.1` and `0.0.0.0` are NOT loopback —
/// conservative: a Remote label on a host that happens to be local cannot
/// leak); a domain: only `localhost`, exactly; anything that fails to parse,
/// has no host, or is not http(s): not loopback.
fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return false;
    }
    let Some(host) = parsed.host_str() else {
        return false;
    };
    // `host_str` gives IPv6 literals in brackets.
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    match bare.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(v4)) => v4.is_loopback(),
        Ok(std::net::IpAddr::V6(v6)) => v6.is_loopback(),
        Err(_) => bare.eq_ignore_ascii_case("localhost"),
    }
}

/// The tier an env-configured "local" provider actually deserves: `Local`
/// only if the base URL AND every URL the adapter will actually request
/// (`{base}/v1/chat/completions`, `{base}/v1/models`) are loopback under the
/// client's own parser; else `Remote` (fail-safe).
fn env_local_tier(url: &str) -> Tier {
    let requested = [
        url.to_owned(),
        format!("{url}/v1/chat/completions"),
        format!("{url}/v1/models"),
    ];
    if requested.iter().all(|u| is_loopback_url(u)) {
        Tier::Local
    } else {
        tracing::warn!(
            "provider URL {url} is not loopback — labeling Remote so \
             LocalOnly tasks won't route to it"
        );
        Tier::Remote
    }
}

/// The open-enum tier string reported on `/models` for a judged tier.
fn tier_label(t: Tier) -> &'static str {
    match t {
        Tier::Local => "local",
        Tier::Lora => "lora",
        Tier::Remote => "remote",
    }
}

impl ModelRouter {
    /// Build from `(provider, tier)` pairs. Cooldown grows exponentially from
    /// `base_cooldown`, capped at `max_cooldown`.
    ///
    /// v3.1 L-1: every `Tier::Local`/`Tier::Lora` provider passed here must be
    /// built with `OpenAiCompatProvider::loopback_only()` (see [`Tier::Local`]).
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

    /// ME4-S2 H2: a router over the SAME providers (shared `Arc`s, same tier
    /// labels, same cooldown parameters) with its OWN, empty health table. A
    /// caller whose failures must not steer anyone else's routing — an
    /// out-of-process module, which can provoke 5xx/429 at will — routes
    /// through one of these, so the cooldowns it causes are visible only to
    /// itself; `/api/v1/chat`, the guardian and the session summarizer keep
    /// routing on the kernel's own table.
    #[must_use]
    pub fn with_separate_health(&self) -> Self {
        Self {
            providers: self
                .providers
                .iter()
                .map(|r| Routed {
                    provider: Arc::clone(&r.provider),
                    tier: r.tier,
                })
                .collect(),
            health: Mutex::new(HashMap::new()),
            base_cooldown: self.base_cooldown,
            max_cooldown: self.max_cooldown,
        }
    }

    /// Convenience default: 2s base, 60s cap. Same PRIVACY CONTRACT as
    /// [`Self::new`]: Local providers must be `loopback_only()`.
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
        // Validate locality before trusting the `Local` label: an OMLX_URL
        // pointed at a non-loopback address is treated as Remote so a
        // LocalOnly task never silently leaks to it (review D2).
        let omlx_tier = env_local_tier(&omlx_url);
        // ME4-S2 M5: overridable (was hard-coded). Labeled by the same rule
        // as OMLX_URL, so a non-loopback value is Remote.
        let ollama_url =
            std::env::var("OLLAMA_URL").unwrap_or_else(|_| "http://127.0.0.1:11434".to_owned());
        let ollama_tier = env_local_tier(&ollama_url);
        // v3 N1: a provider labeled Local talks ONLY to its loopback address —
        // no proxy, no redirect. v3 L-e: its reported tier string is the
        // judged tier, not a hard-coded "local".
        let build =
            |name: &str, url: String, key: Option<String>, tier: Tier| -> Arc<dyn ModelProvider> {
                let p = crate::OpenAiCompatProvider::new(
                    name,
                    url,
                    key,
                    tier_label(tier),
                    default_model.clone(),
                );
                Arc::new(if tier.is_local() {
                    p.loopback_only()
                } else {
                    p
                })
            };
        let omlx = build("omlx", omlx_url, Some(omlx_key), omlx_tier);
        let ollama = build("ollama", ollama_url, None, ollama_tier);
        Self::with_defaults(vec![(omlx, omlx_tier), (ollama, ollama_tier)])
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

    /// Route and complete — unchanged signature and behaviour (`/api/v1/chat`
    /// and the agent loop call this). Now a thin projection of
    /// [`Self::complete_served`].
    pub async fn complete(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<(String, CompletionResponse), ModelError> {
        self.complete_served(profile, req, cancel)
            .await
            .map(|s| (s.provider, s.response))
    }

    /// ME4-4.2.2a: route and complete, and say WHICH TIER served it. Only
    /// `Unavailable` falls through to the next routed provider (and records a
    /// cooldown); `Provider`/`Cancelled` errors stop immediately. A LocalOnly
    /// task with no available local provider errors rather than ever touching
    /// a remote one — `route()` never yields a remote index for it.
    pub async fn complete_served(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
    ) -> Result<Served, ModelError> {
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
                Ok(response) => {
                    self.record_success(r.provider.name());
                    return Ok(Served {
                        provider: r.provider.name().to_owned(),
                        tier: r.tier,
                        response,
                    });
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

    /// Union of models from every provider that yielded a usable list; the rest
    /// are silently skipped. Use [`Self::models_detailed`] if an ABSENCE has to
    /// mean something.
    pub async fn models(&self, cancel: &CancellationToken) -> Vec<crate::Model> {
        self.models_detailed(cancel).await.models
    }

    /// Like [`Self::models`], but says which providers did NOT answer.
    ///
    /// The plain list cannot distinguish "this model does not exist" from "the
    /// provider that has it did not yield a list", because a partial failure still
    /// returns a non-empty union. Any caller that draws a CONCLUSION from a model's absence
    /// needs that difference: telling a user to install a model they already have,
    /// because their provider was briefly unreachable, is a confident wrong answer
    /// (ME-2's mount-time resource check).
    pub async fn models_detailed(&self, cancel: &CancellationToken) -> ModelInventory {
        let mut inv = ModelInventory::default();
        for r in &self.providers {
            match r.provider.models(cancel).await {
                Ok(mut models) => inv.models.append(&mut models),
                Err(err) => {
                    tracing::debug!("models from {} failed: {err}", r.provider.name());
                    inv.failures.push(format!("{}: {err}", r.provider.name()));
                }
            }
        }
        inv
    }
}

/// What a model enumeration actually found, including what it could not reach.
#[derive(Debug, Default, Clone)]
pub struct ModelInventory {
    /// Models from every provider that answered.
    pub models: Vec<crate::Model>,
    /// `provider: error` for each provider that did not yield a usable list, so a
    /// caller can tell an absent model from an unseen provider. Named for the
    /// OUTCOME, not a cause: unreachable, cancelled, auth-rejected and
    /// parse-failed all land here, alike in the only way that matters — this sweep
    /// did not see what that provider has.
    pub failures: Vec<String>,
}

impl ModelInventory {
    /// Whether every configured provider yielded a list. `false` means an absence
    /// proves nothing.
    pub fn is_complete(&self) -> bool {
        self.failures.is_empty()
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
                    model_id: None,
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
            response_format: None,
            max_tokens: None,
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
    async fn local_only_with_cooled_down_local_still_never_uses_remote() {
        // Privacy × health: even when the ONLY local provider is in cooldown, a
        // LocalOnly task must fail closed — never fall back to the remote one.
        let local = StubProvider::down("local"); // Unavailable → enters cooldown
        let remote = StubProvider::ok("remote");
        let r = router(vec![
            (local.clone(), Tier::Local),
            (remote.clone(), Tier::Remote),
        ]);
        // First call routes to local, which is down and so cools down. Remote is
        // off-limits for LocalOnly, so the call still errors.
        let first = r
            .complete(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    complexity: Complexity::Simple,
                },
                &req(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert!(matches!(first, ModelError::Unavailable(_)), "{first}");
        // Second call: local is now cooling → route is empty → still Unavailable,
        // and the remote provider is never touched.
        let second = r
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
        assert!(matches!(second, ModelError::Unavailable(_)), "{second}");
        assert_eq!(
            remote.calls(),
            0,
            "remote must never be called for LocalOnly, even with local cooling"
        );
        assert_eq!(
            local.calls(),
            1,
            "local tried once, then skipped while cooling"
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
    fn env_local_tier_uses_the_http_clients_own_parser() {
        // Positive controls.
        for u in [
            "http://127.0.0.1:8088",
            "http://localhost:8088",
            "http://LOCALHOST:8088",
            "https://[::1]:8443",
            "http://user:pw@127.0.0.1:8088", // real userinfo: host IS 127.0.0.1
            "http://evil.example%2F@127.0.0.1:8088", // %2F stays in userinfo
            "http://0x7f000001:8088",        // WHATWG normalises to 127.0.0.1
            "http://loc\talhost:8088",       // tab stripped → localhost (reqwest agrees)
            "http://127.0.0.1:8088?@evil.example", // `?` ends the authority → 127.0.0.1
        ] {
            assert_eq!(env_local_tier(u), Tier::Local, "{u:?}");
        }
        // Must be Remote.
        for u in [
            "http://evil.example\\@127.0.0.1:8088", // H1: `\` ends the authority → evil.example
            "http://evil.example:80\\@localhost/",
            "http://127.0.0.1\t.evil.example:8088", // tab stripped → a domain
            "http://[::ffff:127.0.0.1]:8088",       // mapped v6: not is_loopback()
            "http://0.0.0.0:8088",
            "http://localhost.:8088",
            "http://192.168.1.50:8088",
            "https://inference.example.com",
            "not a url",
            "file:///tmp/x",
            "",
        ] {
            assert_eq!(env_local_tier(u), Tier::Remote, "{u:?}");
        }
        // The reviewer's exact H1 vector, checked against the parser reqwest uses.
        let h1 = "http://evil.example\\@127.0.0.1:8088";
        assert_eq!(
            reqwest::Url::parse(h1).unwrap().host_str(),
            Some("evil.example")
        );
    }

    // ---- v3 N1: proxy / redirect must not move a Local provider's traffic ----

    /// A blocking stub on its own thread: counts connections, answers `reply`.
    fn thread_stub(reply: String) -> (u16, Arc<std::sync::atomic::AtomicUsize>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let n = Arc::new(std::sync::atomic::AtomicUsize::new(0));
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

    fn http_ok() -> String {
        let body = r#"{"model":"m","choices":[{"message":{"role":"assistant","content":"x"}}]}"#;
        format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn secret_request() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Msg::user("SECRET")],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
        }
    }

    // The two CHILD tests below run only inside a fresh process started by
    // `from_env_local_providers_ignore_http_proxy` (edition 2024 makes
    // `set_var` unsafe and the workspace forbids unsafe, so the env is given
    // to a new process). They are selected by exact name, not by an env var:
    // `agent24-cli`'s PASSTHROUGH_VARS scanner collects every environment read
    // under crates/, and a test-only variable would trip it.

    /// Child: the production path — from_env labels OMLX_URL Local → loopback_only.
    #[tokio::test]
    #[ignore = "child process of from_env_local_providers_ignore_http_proxy"]
    async fn proxy_child_from_env() {
        let r = ModelRouter::from_env();
        let profile = TaskProfile {
            privacy: Privacy::LocalOnly,
            ..Default::default()
        };
        let _ = r
            .complete(profile, &secret_request(), &CancellationToken::new())
            .await;
    }

    /// Child: positive control — the default client, same URL, same env.
    #[tokio::test]
    #[ignore = "child process of from_env_local_providers_ignore_http_proxy"]
    async fn proxy_child_default_client() {
        let url = std::env::var("OMLX_URL").unwrap_or_default(); // already in PASSTHROUGH_VARS
        let p = crate::OpenAiCompatProvider::new("omlx", url, None, "local", "m");
        let _ = p
            .complete(&secret_request(), &CancellationToken::new())
            .await;
    }

    fn run_child(test: &str, target: u16, proxy: u16) {
        let exe = std::env::current_exe().unwrap();
        let proxy_url = format!("http://127.0.0.1:{proxy}");
        let status = std::process::Command::new(exe)
            .args(["--exact", test, "--ignored", "--nocapture"])
            .env("OMLX_URL", format!("http://127.0.0.1:{target}"))
            .env("OLLAMA_URL", "http://127.0.0.1:1")
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
    fn from_env_local_providers_ignore_http_proxy() {
        let (tp, target) = thread_stub(http_ok());
        let (pp, proxy) = thread_stub(http_ok());
        run_child("router::tests::proxy_child_from_env", tp, pp);
        assert_eq!(
            proxy.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the proxy saw a Local provider's request"
        );
        assert_eq!(target.load(std::sync::atomic::Ordering::SeqCst), 1);
        // Positive control: the default client under the same env goes through the proxy.
        let (tp2, target2) = thread_stub(http_ok());
        let (pp2, proxy2) = thread_stub(http_ok());
        run_child("router::tests::proxy_child_default_client", tp2, pp2);
        assert_eq!(
            proxy2.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "measuring instrument: proxy env must take effect"
        );
        assert_eq!(target2.load(std::sync::atomic::Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn a_loopback_only_provider_does_not_follow_redirects() {
        let (rp, redirected) = thread_stub(http_ok());
        let redirect = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nlocation: http://[::ffff:127.0.0.1]:{rp}/v1/chat/completions\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
        );
        let req = CompletionRequest {
            messages: vec![Msg::user("SECRET")],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
        };
        let c = CancellationToken::new();
        let (tp, _) = thread_stub(redirect.clone());
        let p = crate::OpenAiCompatProvider::new(
            "omlx",
            format!("http://127.0.0.1:{tp}"),
            None,
            "local",
            "m",
        )
        .loopback_only();
        let e = p.complete(&req, &c).await.unwrap_err();
        assert!(matches!(e, ModelError::Rejected { status: 307, .. }), "{e}");
        assert_eq!(redirected.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Positive control: the default client follows the 307 with the body.
        let (tp2, _) = thread_stub(redirect);
        let p = crate::OpenAiCompatProvider::new(
            "omlx",
            format!("http://127.0.0.1:{tp2}"),
            None,
            "local",
            "m",
        );
        let _ = p.complete(&req, &c).await;
        assert_eq!(redirected.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn from_env_reports_the_judged_tier() {
        assert_eq!(tier_label(env_local_tier("http://0.0.0.0:1")), "remote");
        assert_eq!(tier_label(env_local_tier("http://127.0.0.1:1")), "local");
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

    /// Enumeration-only stubs: `StubProvider::models` always returns an empty
    /// list, so it cannot express the case these tests are about — a sweep where
    /// one provider CONTRIBUTES models and another fails.
    struct Lister(&'static str, Vec<&'static str>);
    struct Dead(&'static str);

    #[async_trait]
    impl ModelProvider for Lister {
        fn name(&self) -> &str {
            self.0
        }
        async fn complete(
            &self,
            _r: &CompletionRequest,
            _c: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Err(ModelError::Unavailable("not used".into()))
        }
        async fn models(&self, _c: &CancellationToken) -> Result<Vec<crate::Model>, ModelError> {
            Ok(self
                .1
                .iter()
                .map(|id| crate::Model {
                    id: (*id).to_owned(),
                    provider: self.0.to_owned(),
                    tier: "local".to_owned(),
                    loaded: true,
                })
                .collect())
        }
    }

    #[async_trait]
    impl ModelProvider for Dead {
        fn name(&self) -> &str {
            self.0
        }
        async fn complete(
            &self,
            _r: &CompletionRequest,
            _c: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            Err(ModelError::Unavailable("down".into()))
        }
        async fn models(&self, _c: &CancellationToken) -> Result<Vec<crate::Model>, ModelError> {
            Err(ModelError::Unavailable("down".into()))
        }
    }

    #[tokio::test]
    async fn no_providers_is_a_complete_and_empty_inventory() {
        // Vacuously complete: nobody failed to answer, so an absent model really is
        // absent. Calling this "cannot tell" would leave a daemon with no provider
        // configured permanently unable to report anything.
        let router = ModelRouter::with_defaults(vec![]);
        let inv = router.models_detailed(&CancellationToken::new()).await;
        assert!(inv.is_complete());
        assert!(inv.models.is_empty());
    }

    #[tokio::test]
    async fn a_partial_sweep_is_incomplete_even_though_it_returned_models() {
        // The bug this type exists to prevent: the union is NON-EMPTY, so a caller
        // looking only at the list would conclude anything absent is missing —
        // including the models held by the provider that never answered.
        let router = ModelRouter::with_defaults(vec![
            (
                Arc::new(Lister("up", vec!["m1"])) as Arc<dyn ModelProvider>,
                Tier::Local,
            ),
            (
                Arc::new(Dead("down")) as Arc<dyn ModelProvider>,
                Tier::Remote,
            ),
        ]);
        let inv = router.models_detailed(&CancellationToken::new()).await;
        assert_eq!(inv.models.len(), 1, "the union is non-empty");
        assert!(
            !inv.is_complete(),
            "yet it proves nothing about what is missing"
        );
        assert!(inv.failures[0].contains("down"), "{:?}", inv.failures);
        // And the plain `models()` view still hides all of that, which is exactly
        // why a caller drawing conclusions must not use it.
        assert_eq!(router.models(&CancellationToken::new()).await.len(), 1);
    }
}
