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

use agent24_domain::{EgressDestination, EgressGate, EgressPurpose, EgressRequest, EgressResource};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;

use crate::{CompletionRequest, CompletionResponse, ModelError, ModelProvider};

/// The longest a provider stays cooled by default. A module waits as long
/// before it asks again after `no_provider` (ME4-S2a §2.6); the kernel's
/// tests hold the two equal.
pub const DEFAULT_MAX_COOLDOWN: Duration = Duration::from_secs(60);

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
    /// Validated gateway evidence; direct-provider calls have none.
    pub idoris: Option<crate::IdorisEvidence>,
    pub provider: String,
    pub tier: Tier,
    pub response: CompletionResponse,
}

/// Routes completions across tiered providers with a health/cooldown loop.
pub struct ModelRouter {
    idoris: Option<Result<Arc<crate::idoris::IdorisClient>, String>>,
    providers: Vec<Routed>,
    health: Mutex<HashMap<String, Health>>,
    base_cooldown: Duration,
    max_cooldown: Duration,
    /// ME4-desktop-model-ui: the `DEFAULT_MODEL` env var this router was
    /// built with, for a client (desktop's topbar) to ask "what does Agent24
    /// actually default to" instead of guessing from a locally-detected
    /// model list. Only [`Self::from_env`] sets it — a hand-built router
    /// (every other constructor, mostly tests and per-module
    /// `with_separate_health()` copies) has no single "the" default model to
    /// report, so this stays `None` there rather than a misleading guess.
    default_model: Option<String>,
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
pub(crate) fn is_loopback_url(url: &str) -> bool {
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

/// How a `from_env` provider may be asked to skip a reasoning model's
/// thinking (`CompletionRequest::disable_thinking`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThinkingSwitch {
    /// Never: the request body stays as it was before the switches existed.
    None,
    /// oMLX / mlx-lm-server's `chat_template_kwargs` extension.
    ChatTemplateKwargs,
    /// `reasoning_effort: "none"`, which Ollama's OpenAI layer maps to
    /// thinking off.
    ReasoningEffort,
}

/// Review H2: `chat_template_kwargs` is oMLX's own extension, so only oMLX
/// gets it — being local does not make a backend oMLX. ME4-S2c §4: Ollama gets
/// `reasoning_effort` by the same rule. Either only when judged local: behind
/// a remote URL may be any OpenAI-compatible service, which may refuse it.
/// A function of its own so a test can read it; `from_env` hands back
/// `dyn ModelProvider`s, which do not say.
fn thinking_switch(backend: &str, tier: Tier) -> ThinkingSwitch {
    match backend {
        _ if !tier.is_local() => ThinkingSwitch::None,
        "omlx" => ThinkingSwitch::ChatTemplateKwargs,
        "ollama" => ThinkingSwitch::ReasoningEffort,
        _ => ThinkingSwitch::None,
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
            idoris: None,
            providers: providers
                .into_iter()
                .map(|(provider, tier)| Routed { provider, tier })
                .collect(),
            health: Mutex::new(HashMap::new()),
            base_cooldown,
            max_cooldown,
            default_model: None,
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
            idoris: self.idoris.clone(),
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
            default_model: self.default_model.clone(),
        }
    }

    /// ME4-desktop-model-ui: the `DEFAULT_MODEL` this router was built with
    /// ([`Self::from_env`] only) — `None` for a hand-built router (tests,
    /// and every `with_separate_health()` copy of one that was never given
    /// one either). Never a guess: a caller that gets `None` back has no
    /// single "the default model" to show and must say so, not fall back to
    /// picking one itself.
    pub fn default_model(&self) -> Option<&str> {
        self.default_model.as_deref()
    }

    /// Convenience default: 2s base, 60s cap. Same PRIVACY CONTRACT as
    /// [`Self::new`]: Local providers must be `loopback_only()`. The cap is a
    /// contract with modules too (ME4-S2a §2.6): they wait it out after
    /// `no_provider`, so the daemon's router must keep [`DEFAULT_MAX_COOLDOWN`].
    pub fn with_defaults(providers: Vec<(Arc<dyn ModelProvider>, Tier)>) -> Self {
        Self::new(providers, Duration::from_secs(2), DEFAULT_MAX_COOLDOWN)
    }

    /// The default local chain as tiered providers: oMLX (8088) and Ollama
    /// (11434), both Local tier. Mirrors `ProviderRegistry::from_env` env vars
    /// (OMLX_URL / OMLX_API_KEY / DEFAULT_MODEL). A remote/lora provider is
    /// added by the daemon when configured; with only local providers, a
    /// Complex task simply falls back to them.
    pub fn from_env() -> Self {
        if std::env::var_os("IDORIS_URL").is_some() {
            let mut router = Self::with_defaults(Vec::new());
            router.idoris = Some(crate::idoris::IdorisClient::from_env().map(Arc::new));
            return router;
        }
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
        // Review H2, ME4-S2c §4: which thinking switch each one may be sent
        // is decided per PROVIDER INSTANCE by `thinking_switch`, not by tier.
        let build =
            |name: &str, url: String, key: Option<String>, tier: Tier| -> Arc<dyn ModelProvider> {
                let p = crate::OpenAiCompatProvider::new(
                    name,
                    url,
                    key,
                    tier_label(tier),
                    default_model.clone(),
                );
                let p = if tier.is_local() {
                    p.loopback_only()
                } else {
                    p
                };
                let p = match thinking_switch(name, tier) {
                    ThinkingSwitch::ChatTemplateKwargs => p.supports_chat_template_kwargs(),
                    ThinkingSwitch::ReasoningEffort => p.supports_reasoning_effort(),
                    ThinkingSwitch::None => p,
                };
                Arc::new(p)
            };
        let omlx = build("omlx", omlx_url, Some(omlx_key), omlx_tier);
        let ollama = build("ollama", ollama_url, None, ollama_tier);
        let mut router = Self::with_defaults(vec![(omlx, omlx_tier), (ollama, ollama_tier)]);
        router.default_model = Some(default_model);
        router
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

    /// Host-authorized completion variant for callers carrying a live source snapshot.
    pub async fn complete_with_egress(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
        gate: std::sync::Arc<dyn EgressGate>,
        resources: Vec<EgressResource>,
        authorization_generation: u64,
    ) -> Result<(String, CompletionResponse), ModelError> {
        self.complete_served_with_egress(
            profile,
            req,
            cancel,
            gate,
            resources,
            authorization_generation,
        )
        .await
        .map(|served| (served.provider, served.response))
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
        self.complete_served_with_request_id(profile, req, cancel, None)
            .await
    }

    /// Carry an admitted lifecycle id on `X-iDoris-Trace-Id` when iDoris accepts it.
    /// `X-iDoris-Request-Id` is a fresh idempotency key on every call, with or
    /// without that id. A missing or rejected id omits the trace header.
    /// Remote providers stay closed (deny-all egress); the iDoris gateway is
    /// loopback-only, so it never needs an egress grant.
    pub async fn complete_served_with_request_id(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
        request_id: Option<&str>,
    ) -> Result<Served, ModelError> {
        self.complete_served_inner(
            profile,
            req,
            cancel,
            request_id,
            <dyn EgressGate>::deny_all(),
            Vec::new(),
            0,
        )
        .await
    }

    /// Host-authorized variant. The live gate is called immediately before
    /// each remote provider attempt, so a grant revoked during routing is denied.
    pub async fn complete_served_with_egress(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
        gate: std::sync::Arc<dyn EgressGate>,
        resources: Vec<EgressResource>,
        authorization_generation: u64,
    ) -> Result<Served, ModelError> {
        self.complete_served_inner(
            profile,
            req,
            cancel,
            None,
            gate,
            resources,
            authorization_generation,
        )
        .await
    }

    #[allow(clippy::too_many_arguments)]
    async fn complete_served_inner(
        &self,
        profile: TaskProfile,
        req: &CompletionRequest,
        cancel: &CancellationToken,
        request_id: Option<&str>,
        gate: std::sync::Arc<dyn EgressGate>,
        resources: Vec<EgressResource>,
        authorization_generation: u64,
    ) -> Result<Served, ModelError> {
        if let Some(gateway) = &self.idoris {
            return gateway
                .as_ref()
                .map_err(|e| ModelError::Provider(e.clone()))?
                .complete(profile, req, cancel, request_id)
                .await;
        }
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
        for (n, idx) in route.into_iter().enumerate() {
            // ME4-S2a: the providers share the call's deadline. With less of it
            // left than one stall, the next would load its model only to be cut.
            if let Some(p) = &req.progress
                && n > 0
                && crate::stream::left(p, Instant::now()) < p.stall
            {
                tried.push("no time left for another provider".to_owned());
                break;
            }
            let r = &self.providers[idx];
            if r.tier == Tier::Remote {
                let request = EgressRequest::remote(
                    resources.clone(),
                    EgressPurpose::ModelInference,
                    r.provider
                        .outbound_destination()
                        .map_or_else(EgressDestination::unknown, EgressDestination::exact),
                    authorization_generation,
                );
                if request.authorize(gate.as_ref()).await.is_err() {
                    tried.push(format!(
                        "{}: outbound policy denied or unavailable",
                        r.provider.name()
                    ));
                    continue;
                }
            }
            match r.provider.complete(req, cancel).await {
                Ok(response) => {
                    self.record_success(r.provider.name());
                    return Ok(Served {
                        idoris: None,
                        provider: r.provider.name().to_owned(),
                        tier: r.tier,
                        response,
                    });
                }
                Err(ModelError::Unavailable(msg)) => {
                    tracing::debug!("provider {} unavailable: {msg}", r.provider.name());
                    // Cut at the call's shared deadline is no fault of the provider: no cooldown.
                    let cut = req
                        .progress
                        .is_some_and(|p| crate::stream::left(&p, Instant::now()).is_zero());
                    if !cut {
                        self.record_failure(r.provider.name(), Instant::now());
                    }
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
        self.models_detailed_with_egress(cancel, <dyn EgressGate>::deny_all(), Vec::new(), 0)
            .await
    }

    /// Host-authorized inventory. Remote `/models` requests use the same
    /// exact destination/source gate as inference requests.
    pub async fn models_detailed_with_egress(
        &self,
        cancel: &CancellationToken,
        gate: std::sync::Arc<dyn EgressGate>,
        resources: Vec<EgressResource>,
        authorization_generation: u64,
    ) -> ModelInventory {
        if let Some(gateway) = &self.idoris {
            let result = match gateway {
                Ok(gateway) => gateway.models(cancel).await,
                Err(error) => Err(ModelError::Provider(error.clone())),
            };
            return match result {
                Ok(models) => ModelInventory {
                    models,
                    failures: Vec::new(),
                },
                Err(error) => ModelInventory {
                    models: Vec::new(),
                    failures: vec![format!("idoris-local: {error}")],
                },
            };
        }
        let mut inv = ModelInventory::default();
        for r in &self.providers {
            if r.tier == Tier::Remote {
                let request = EgressRequest::remote(
                    resources.clone(),
                    EgressPurpose::ModelDiscovery,
                    r.provider
                        .outbound_destination()
                        .map_or_else(EgressDestination::unknown, EgressDestination::exact),
                    authorization_generation,
                );
                if request.authorize(gate.as_ref()).await.is_err() {
                    inv.failures.push(format!(
                        "{}: outbound policy denied or unavailable",
                        r.provider.name()
                    ));
                    continue;
                }
            }
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
        fn outbound_destination(&self) -> Option<String> {
            Some(format!("provider:{}", self.name))
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
            disable_thinking: false,
            temperature: None,
            progress: None,
        }
    }

    fn router(providers: Vec<(Arc<dyn ModelProvider>, Tier)>) -> ModelRouter {
        ModelRouter::new(providers, Duration::from_secs(10), Duration::from_secs(60))
    }

    struct TestGrant;
    #[async_trait]
    impl EgressGate for TestGrant {
        async fn check(
            &self,
            _request: &EgressRequest,
        ) -> Result<(), agent24_domain::EgressDecision> {
            Ok(())
        }
    }

    fn authorized_test_scope() -> (Arc<dyn EgressGate>, Vec<EgressResource>) {
        (
            Arc::new(TestGrant),
            vec![
                EgressResource::cloud_authorized("test-resource", "test-revision", 1)
                    .with_authorization_ref("test-grant"),
            ],
        )
    }

    struct TestDeny;
    #[async_trait]
    impl EgressGate for TestDeny {
        async fn check(
            &self,
            _request: &EgressRequest,
        ) -> Result<(), agent24_domain::EgressDecision> {
            Err(agent24_domain::EgressDecision)
        }
    }

    /// A local backend that takes requests and never answers; how many it took.
    async fn silent_backend() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let taken = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let t = taken.clone();
        tokio::spawn(async move {
            while let Ok((s, _)) = l.accept().await {
                t.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async move {
                    let _held = s;
                    tokio::time::sleep(Duration::from_secs(30)).await;
                });
            }
        });
        (url, taken)
    }

    fn progressing(first_ms: u64, stall_ms: u64, deadline_ms: u64) -> CompletionRequest {
        CompletionRequest {
            temperature: None,
            progress: Some(crate::Progress {
                deadline: Instant::now() + Duration::from_millis(deadline_ms),
                first_chunk: Duration::from_millis(first_ms),
                stall: Duration::from_millis(stall_ms),
            }),
            ..req()
        }
    }

    async fn two_silent(
        r: &CompletionRequest,
    ) -> (Duration, Result<Served, ModelError>, usize, Vec<usize>) {
        let ((a, _), (b, taken_b)) = (silent_backend().await, silent_backend().await);
        let providers: Vec<(Arc<dyn ModelProvider>, Tier)> = vec![
            (
                Arc::new(crate::OpenAiCompatProvider::new(
                    "omlx", a, None, "local", "m",
                )),
                Tier::Local,
            ),
            (
                Arc::new(crate::OpenAiCompatProvider::new(
                    "ollama", b, None, "local", "m",
                )),
                Tier::Local,
            ),
        ];
        let started = Instant::now();
        let profile = TaskProfile {
            privacy: Privacy::LocalOnly,
            complexity: Complexity::Simple,
        };
        let rt = router(providers);
        let out = rt
            .complete_served(profile, r, &CancellationToken::new())
            .await;
        (
            started.elapsed(),
            out,
            taken_b.load(std::sync::atomic::Ordering::SeqCst),
            rt.route(profile, Instant::now()),
        )
    }

    #[tokio::test]
    async fn the_providers_share_the_calls_deadline() {
        // The first stalls at its 400ms first chunk; the second has only what is left of 800ms.
        let (took, out, second, _) = two_silent(&progressing(400, 50, 800)).await;
        assert!(matches!(out, Err(ModelError::Unavailable(_))));
        assert_eq!(second, 1, "the second was tried");
        assert!(
            took >= Duration::from_millis(750) && took < Duration::from_millis(1100),
            "{took:?}"
        );
        // The first failed on its own (no first chunk) and cools down; the second
        // was cut at the shared deadline, which is no fault of its own.
        let (_, _, _, after) = two_silent(&progressing(400, 50, 800)).await;
        assert_eq!(
            after,
            vec![1],
            "only the one cut at the deadline is routed again"
        );
    }

    #[tokio::test]
    async fn no_provider_is_tried_with_less_left_than_one_stall() {
        // 50ms left after the first's 400ms: less than the 100ms stall.
        let (took, out, second, _) = two_silent(&progressing(400, 100, 450)).await;
        match out {
            Err(ModelError::Unavailable(m)) => assert!(m.contains("no time left"), "{m}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(second, 0, "the second was not tried");
        assert!(took < Duration::from_millis(600), "{took:?}");
        // Positive control: with time left, it is.
        let (_, _, second, _) = two_silent(&progressing(400, 100, 700)).await;
        assert_eq!(second, 1);
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
        let (gate, resources) = authorized_test_scope();
        let (name, _) = r
            .complete_with_egress(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Simple,
                },
                &req(),
                &CancellationToken::new(),
                gate,
                resources,
                1,
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
        let (gate, resources) = authorized_test_scope();
        let (name, _) = r
            .complete_with_egress(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Complex,
                },
                &req(),
                &CancellationToken::new(),
                gate,
                resources,
                1,
            )
            .await
            .unwrap();
        assert_eq!(name, "remote");
        assert_eq!(local.calls(), 0);
    }

    /// K1-6b.5d: a run's model override is a model id, never a tier. `route`
    /// reads only the profile and provider health, so an override naming
    /// either provider leaves the routed tier as the profile chose it, and a
    /// LocalOnly run never reaches the remote one.
    #[tokio::test]
    async fn a_model_override_never_changes_the_routed_tier() {
        let cases = [
            (Privacy::Any, Complexity::Simple, "local"),
            (Privacy::Any, Complexity::Complex, "remote"),
            (Privacy::LocalOnly, Complexity::Complex, "local"),
        ];
        for (privacy, complexity, expected) in cases {
            for model in ["local", "remote"] {
                let local = StubProvider::ok("local");
                let remote = StubProvider::ok("remote");
                let r = router(vec![
                    (local.clone(), Tier::Local),
                    (remote.clone(), Tier::Remote),
                ]);
                let (gate, resources) = authorized_test_scope();
                let mut request = req();
                request.model = Some(model.to_owned());
                let (name, _) = r
                    .complete_with_egress(
                        TaskProfile {
                            privacy,
                            complexity,
                        },
                        &request,
                        &CancellationToken::new(),
                        gate,
                        resources,
                        1,
                    )
                    .await
                    .unwrap();
                let case = format!("{privacy:?}/{complexity:?}, override {model}");
                assert_eq!(name, expected, "{case}");
                let unused = if expected == "local" { &remote } else { &local };
                assert_eq!(unused.calls(), 0, "{case}");
            }
        }
    }

    #[tokio::test]
    async fn remote_provider_is_not_called_without_a_host_egress_gate() {
        let remote = StubProvider::ok("remote");
        let r = router(vec![(remote.clone(), Tier::Remote)]);
        assert!(
            r.complete(TaskProfile::default(), &req(), &CancellationToken::new())
                .await
                .is_err()
        );
        assert_eq!(remote.calls(), 0);
    }

    #[tokio::test]
    async fn denied_remote_falls_back_to_healthy_local_provider() {
        let remote = StubProvider::ok("remote");
        let local = StubProvider::ok("local");
        let r = router(vec![
            (remote.clone(), Tier::Remote),
            (local.clone(), Tier::Local),
        ]);
        let resources = vec![
            EgressResource::cloud_authorized("test-resource", "test-revision", 1)
                .with_authorization_ref("test-grant"),
        ];
        let served = r
            .complete_served_with_egress(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Complex,
                },
                &req(),
                &CancellationToken::new(),
                Arc::new(TestDeny),
                resources,
                1,
            )
            .await
            .unwrap();

        assert_eq!(served.provider, "local");
        assert_eq!(
            remote.calls(),
            0,
            "a denied remote provider is never called"
        );
        assert_eq!(local.calls(), 1);
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

    /// Completes under a gate that allows every request, with a valid cloud
    /// grant: nothing but the LocalOnly exclusion in `tier_order` stands
    /// between a task and a remote provider (K1-6b.5e prerequisite, #1209
    /// PR-Daemon).
    async fn complete_granted(
        r: &ModelRouter,
        privacy: Privacy,
        complexity: Complexity,
    ) -> Result<(String, CompletionResponse), ModelError> {
        let (gate, resources) = authorized_test_scope();
        r.complete_with_egress(
            TaskProfile {
                privacy,
                complexity,
            },
            &req(),
            &CancellationToken::new(),
            gate,
            resources,
            1,
        )
        .await
    }

    /// With only a remote provider, the granting gate serves a task with no
    /// privacy label (the control) and never a LocalOnly one.
    #[tokio::test]
    async fn a_granting_gate_never_serves_local_only_from_a_lone_remote() {
        for complexity in [Complexity::Simple, Complexity::Complex] {
            let remote = StubProvider::ok("remote");
            let r = router(vec![(remote.clone(), Tier::Remote)]);
            let (name, _) = complete_granted(&r, Privacy::Any, complexity)
                .await
                .unwrap();
            assert_eq!(name, "remote", "{complexity:?}");
            assert_eq!(remote.calls(), 1, "{complexity:?}");

            let err = complete_granted(&r, Privacy::LocalOnly, complexity)
                .await
                .unwrap_err();
            assert!(
                matches!(err, ModelError::Unavailable(_)),
                "{complexity:?}: {err}"
            );
            assert_eq!(remote.calls(), 1, "{complexity:?}");
        }
    }

    /// A local provider that fails does not hand a LocalOnly task to the
    /// remote one the gate would let through.
    #[tokio::test]
    async fn a_granting_gate_never_serves_local_only_when_local_fails() {
        for complexity in [Complexity::Simple, Complexity::Complex] {
            let local = StubProvider::down("local");
            let remote = StubProvider::ok("remote");
            let r = router(vec![
                (local.clone(), Tier::Local),
                (remote.clone(), Tier::Remote),
            ]);
            let err = complete_granted(&r, Privacy::LocalOnly, complexity)
                .await
                .unwrap_err();
            assert!(
                matches!(err, ModelError::Unavailable(_)),
                "{complexity:?}: {err}"
            );
            assert_eq!(local.calls(), 1, "{complexity:?}");
            assert_eq!(remote.calls(), 0, "{complexity:?}");
        }
    }

    /// Nor does one cooling down after a failure, when no local provider is
    /// left to try at all.
    #[tokio::test]
    async fn a_granting_gate_never_serves_local_only_while_local_cools_down() {
        for complexity in [Complexity::Simple, Complexity::Complex] {
            let local = StubProvider::down("local");
            let remote = StubProvider::ok("remote");
            let r = router(vec![
                (local.clone(), Tier::Local),
                (remote.clone(), Tier::Remote),
            ]);
            complete_granted(&r, Privacy::LocalOnly, complexity)
                .await
                .unwrap_err();
            let err = complete_granted(&r, Privacy::LocalOnly, complexity)
                .await
                .unwrap_err();
            assert!(
                matches!(err, ModelError::Unavailable(_)),
                "{complexity:?}: {err}"
            );
            assert_eq!(local.calls(), 1, "{complexity:?}: cooling, not retried");
            assert_eq!(remote.calls(), 0, "{complexity:?}");
        }
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
        let (gate, resources) = authorized_test_scope();
        let (name, _) = r
            .complete_with_egress(
                TaskProfile {
                    privacy: Privacy::Any,
                    complexity: Complexity::Simple,
                },
                &req(),
                &CancellationToken::new(),
                gate,
                resources,
                1,
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

    /// ME4-S2a §2.6 (PR-Daemon on #969): the daemon's router, built by
    /// `with_defaults`, cools a provider for at most `DEFAULT_MAX_COOLDOWN`, the
    /// number modules wait after `no_provider`; waiting exactly that is enough.
    #[test]
    fn the_default_router_cools_for_at_most_the_shared_cap() {
        let r = ModelRouter::with_defaults(vec![(
            StubProvider::down("p") as Arc<dyn ModelProvider>,
            Tier::Local,
        )]);
        let t0 = Instant::now();
        for _ in 0..20 {
            r.record_failure("p", t0);
        }
        let profile = TaskProfile::default();
        let just_before = DEFAULT_MAX_COOLDOWN - Duration::from_millis(1);
        assert!(r.route(profile, t0 + just_before).is_empty());
        assert_eq!(r.route(profile, t0 + DEFAULT_MAX_COOLDOWN).len(), 1);
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
            disable_thinking: false,
            temperature: None,
            progress: None,
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

    /// Child: `from_env`'s providers, each asked once with thinking off.
    #[tokio::test]
    #[ignore = "child process of from_env_sends_each_backend_its_own_thinking_switch"]
    async fn switch_child_from_env() {
        let r = ModelRouter::from_env();
        let profile = TaskProfile {
            privacy: Privacy::LocalOnly,
            ..Default::default()
        };
        let req = CompletionRequest {
            disable_thinking: true,
            ..secret_request()
        };
        let _ = r.complete(profile, &req, &CancellationToken::new()).await;
    }

    #[test]
    fn from_env_sends_each_backend_its_own_thinking_switch() {
        // J3's wiring: oMLX refuses for now (503), so the router goes on to
        // Ollama; each body is read as it came.
        use std::io::{Read, Write};
        fn stub(reply: String) -> (u16, std::sync::mpsc::Receiver<String>) {
            let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let (tx, rx) = std::sync::mpsc::channel();
            std::thread::spawn(move || {
                for s in l.incoming() {
                    let Ok(mut s) = s else { continue };
                    let _ = s.set_read_timeout(Some(Duration::from_millis(300)));
                    let mut got = Vec::new();
                    let mut buf = [0u8; 65536];
                    while let Ok(n) = s.read(&mut buf) {
                        if n == 0 {
                            break;
                        }
                        got.extend_from_slice(&buf[..n]);
                    }
                    let text = String::from_utf8_lossy(&got).to_string();
                    let body = text.split_once("\r\n\r\n").map(|(_, b)| b.to_owned());
                    let _ = tx.send(body.unwrap_or_default());
                    let _ = s.write_all(reply.as_bytes());
                }
            });
            (port, rx)
        }
        let busy =
            "HTTP/1.1 503 Service Unavailable\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
        let (omlx_port, omlx) = stub(busy.to_owned());
        let (ollama_port, ollama) = stub(http_ok());
        let exe = std::env::current_exe().unwrap();
        let status = std::process::Command::new(exe)
            .args([
                "--exact",
                "router::tests::switch_child_from_env",
                "--ignored",
                "--nocapture",
            ])
            .env("OMLX_URL", format!("http://127.0.0.1:{omlx_port}"))
            .env("OLLAMA_URL", format!("http://127.0.0.1:{ollama_port}"))
            .status()
            .unwrap();
        assert!(status.success());
        let wait = Duration::from_secs(5);
        let omlx: serde_json::Value =
            serde_json::from_str(&omlx.recv_timeout(wait).unwrap()).unwrap();
        let ollama: serde_json::Value =
            serde_json::from_str(&ollama.recv_timeout(wait).unwrap()).unwrap();
        assert!(omlx.get("chat_template_kwargs").is_some(), "{omlx}");
        assert!(omlx.get("reasoning_effort").is_none(), "{omlx}");
        assert_eq!(ollama["reasoning_effort"], "none", "{ollama}");
        assert!(ollama.get("chat_template_kwargs").is_none(), "{ollama}");
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
            disable_thinking: false,
            temperature: None,
            progress: None,
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
    fn only_a_local_ollama_is_sent_reasoning_effort() {
        // J3 (ME4-S2c): a local Ollama gets `reasoning_effort`; Ollama
        // judged remote and oMLX never do.
        let local = env_local_tier("http://127.0.0.1:11434");
        let remote = env_local_tier("http://10.0.0.5:11434");
        assert_eq!(
            thinking_switch("ollama", local),
            ThinkingSwitch::ReasoningEffort
        );
        assert_eq!(thinking_switch("ollama", remote), ThinkingSwitch::None);
        assert_eq!(
            thinking_switch("omlx", local),
            ThinkingSwitch::ChatTemplateKwargs
        );
        assert_eq!(thinking_switch("omlx", remote), ThinkingSwitch::None);
        assert_eq!(thinking_switch("lmstudio", local), ThinkingSwitch::None);
    }

    /// `from_env()`'s own env-var manipulation is exercised only via the
    /// child-process tests above (edition 2024 forbids `set_var` outside
    /// `unsafe`, which this workspace forbids outright) — this just pins the
    /// getter's default for every OTHER constructor, so a client asking a
    /// hand-built or per-module router "what's the default model" gets
    /// `None`, never a stale or made-up guess.
    #[test]
    fn default_model_is_none_unless_built_from_env() {
        let r = ModelRouter::with_defaults(vec![]);
        assert_eq!(r.default_model(), None);
        assert_eq!(r.with_separate_health().default_model(), None);
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
        let (gate, resources) = authorized_test_scope();
        let err = r
            .complete_with_egress(
                TaskProfile::default(),
                &req(),
                &CancellationToken::new(),
                gate,
                resources,
                1,
            )
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("local: local down"), "{msg}");
        assert!(msg.contains("remote: remote down"), "{msg}");
    }

    #[tokio::test]
    async fn rejected_502_upstream_auth_failed_does_not_fall_through() {
        struct AuthFailed;
        #[async_trait]
        impl ModelProvider for AuthFailed {
            fn name(&self) -> &str {
                "idoris"
            }
            async fn complete(
                &self,
                _req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                Err(ModelError::Rejected {
                    status: 502,
                    message: "upstream_auth_failed".to_owned(),
                })
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
            (Arc::new(AuthFailed), Tier::Local),
            (fallback.clone(), Tier::Remote),
        ]);
        let err = r
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap_err();
        assert!(
            matches!(err, ModelError::Rejected { status: 502, .. }),
            "{err}"
        );
        assert_eq!(fallback.calls(), 0, "Rejected must not fall through");
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

    // ── K1-6b.5b (ADR-K1-02 §2.4, §5): the remote model egress matrix ──────

    /// A remote provider that counts both inference and discovery calls, with
    /// a destination the test chooses (`None`: it cannot say where it sends).
    struct CountingRemote {
        destination: Option<&'static str>,
        completions: StdMutex<usize>,
        listings: StdMutex<usize>,
    }

    impl CountingRemote {
        fn at(destination: Option<&'static str>) -> Arc<Self> {
            Arc::new(Self {
                destination,
                completions: StdMutex::new(0),
                listings: StdMutex::new(0),
            })
        }
        fn completions(&self) -> usize {
            *self.completions.lock().unwrap()
        }
        fn listings(&self) -> usize {
            *self.listings.lock().unwrap()
        }
    }

    #[async_trait]
    impl ModelProvider for CountingRemote {
        fn name(&self) -> &str {
            "counting-remote"
        }
        fn outbound_destination(&self) -> Option<String> {
            self.destination.map(str::to_owned)
        }
        async fn complete(
            &self,
            _req: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            *self.completions.lock().unwrap() += 1;
            Ok(CompletionResponse {
                message: Msg::assistant(Some("from remote".to_owned()), vec![]),
                usage: Usage::default(),
                model_id: None,
            })
        }
        async fn models(
            &self,
            _cancel: &CancellationToken,
        ) -> Result<Vec<crate::Model>, ModelError> {
            *self.listings.lock().unwrap() += 1;
            Ok(vec![])
        }
    }

    /// A gate that allows exactly one request and nothing else.
    struct ExactGate(EgressRequest);

    #[async_trait]
    impl EgressGate for ExactGate {
        async fn check(
            &self,
            request: &EgressRequest,
        ) -> Result<(), agent24_domain::EgressDecision> {
            if *request == self.0 {
                Ok(())
            } else {
                Err(agent24_domain::EgressDecision)
            }
        }
    }

    const REMOTE_AT: &str = "https://models.example/v1";

    fn granted_resource() -> EgressResource {
        EgressResource::cloud_authorized("doc-1", "rev-1", 3).with_authorization_ref("grant-1")
    }

    /// The one request the exact gate allows: this resource, inference, this
    /// destination, generation 7.
    fn granted(purpose: EgressPurpose) -> EgressRequest {
        EgressRequest::remote(
            vec![granted_resource()],
            purpose,
            EgressDestination::exact(REMOTE_AT),
            7,
        )
    }

    /// Each element of the granted scope changed on its own, everything else
    /// as granted: (what changed, resources, authorization generation).
    fn mismatched_scopes() -> Vec<(&'static str, Vec<EgressResource>, u64)> {
        let other = |f: fn(&mut EgressResource)| {
            let mut resource = granted_resource();
            f(&mut resource);
            vec![resource]
        };
        vec![
            ("resource", other(|r| r.resource_id = "doc-2".into()), 7),
            ("revision", other(|r| r.revision = Some("rev-2".into())), 7),
            (
                "authorization",
                other(|r| r.authorization_ref = Some("grant-2".into())),
                7,
            ),
            ("policy version", other(|r| r.policy_version = 4), 7),
            ("generation", vec![granted_resource()], 8),
        ]
    }

    fn any_privacy() -> TaskProfile {
        TaskProfile {
            privacy: Privacy::Any,
            complexity: Complexity::Simple,
        }
    }

    #[tokio::test]
    async fn a_remote_provider_that_cannot_name_its_destination_is_never_called() {
        let remote = CountingRemote::at(None);
        let router = router(vec![(remote.clone(), Tier::Remote)]);
        let (gate, resources) = authorized_test_scope();
        let result = router
            .complete_with_egress(
                any_privacy(),
                &req(),
                &CancellationToken::new(),
                gate.clone(),
                resources.clone(),
                1,
            )
            .await;
        assert!(result.is_err());
        let inventory = router
            .models_detailed_with_egress(&CancellationToken::new(), gate, resources, 1)
            .await;
        assert!(
            inventory.failures[0].contains("outbound policy denied"),
            "{:?}",
            inventory.failures
        );
        assert_eq!((remote.completions(), remote.listings()), (0, 0));
    }

    #[tokio::test]
    async fn an_exact_grant_lets_the_remote_provider_serve_only_that_request() {
        let remote = CountingRemote::at(Some(REMOTE_AT));
        let elsewhere = CountingRemote::at(Some("https://elsewhere.example/v1"));
        let elsewhere_router = router(vec![(elsewhere.clone(), Tier::Remote)]);
        let router = router(vec![(remote.clone(), Tier::Remote)]);
        let gate: Arc<dyn EgressGate> = Arc::new(ExactGate(granted(EgressPurpose::ModelInference)));
        let served = router
            .complete_served_with_egress(
                any_privacy(),
                &req(),
                &CancellationToken::new(),
                gate.clone(),
                vec![granted_resource()],
                7,
            )
            .await
            .unwrap();
        assert_eq!(served.tier, Tier::Remote);
        assert_eq!(remote.completions(), 1);

        // Every other scope: the remote provider is never called again.
        for (what, resources, generation) in mismatched_scopes() {
            let result = router
                .complete_served_with_egress(
                    any_privacy(),
                    &req(),
                    &CancellationToken::new(),
                    gate.clone(),
                    resources,
                    generation,
                )
                .await;
            assert!(result.is_err(), "{what}");
            assert_eq!(remote.completions(), 1, "{what}");
        }

        // Another destination, and a grant for another purpose.
        let discovery_only: Arc<dyn EgressGate> =
            Arc::new(ExactGate(granted(EgressPurpose::ModelDiscovery)));
        for (what, router, gate) in [
            ("destination", &elsewhere_router, gate.clone()),
            ("purpose", &router, discovery_only),
        ] {
            let result = router
                .complete_served_with_egress(
                    any_privacy(),
                    &req(),
                    &CancellationToken::new(),
                    gate,
                    vec![granted_resource()],
                    7,
                )
                .await;
            assert!(result.is_err(), "{what}");
        }
        assert_eq!((remote.completions(), elsewhere.completions()), (1, 0));
    }

    #[tokio::test]
    async fn model_discovery_never_lists_a_remote_provider_through_the_default_gate() {
        let remote = CountingRemote::at(Some(REMOTE_AT));
        let local = Lister("local", vec!["llama"]);
        let router = router(vec![
            (remote.clone(), Tier::Remote),
            (Arc::new(local), Tier::Local),
        ]);
        let inventory = router.models_detailed(&CancellationToken::new()).await;
        assert_eq!(remote.listings(), 0);
        assert_eq!(inventory.models.len(), 1);
        assert!(
            inventory
                .failures
                .iter()
                .any(|f| f.contains("counting-remote: outbound policy denied")),
            "{:?}",
            inventory.failures
        );
    }

    #[tokio::test]
    async fn model_discovery_needs_a_grant_for_the_discovery_purpose() {
        let remote = CountingRemote::at(Some(REMOTE_AT));
        let elsewhere = CountingRemote::at(Some("https://elsewhere.example/v1"));
        let elsewhere_router = router(vec![(elsewhere.clone(), Tier::Remote)]);
        let router = router(vec![(remote.clone(), Tier::Remote)]);
        let inference_only: Arc<dyn EgressGate> =
            Arc::new(ExactGate(granted(EgressPurpose::ModelInference)));
        let _ = router
            .models_detailed_with_egress(
                &CancellationToken::new(),
                inference_only,
                vec![granted_resource()],
                7,
            )
            .await;
        assert_eq!(remote.listings(), 0);

        let discovery: Arc<dyn EgressGate> =
            Arc::new(ExactGate(granted(EgressPurpose::ModelDiscovery)));
        for (what, resources, generation) in mismatched_scopes() {
            let _ = router
                .models_detailed_with_egress(
                    &CancellationToken::new(),
                    discovery.clone(),
                    resources,
                    generation,
                )
                .await;
            assert_eq!(remote.listings(), 0, "{what}");
        }
        let _ = elsewhere_router
            .models_detailed_with_egress(
                &CancellationToken::new(),
                discovery.clone(),
                vec![granted_resource()],
                7,
            )
            .await;
        assert_eq!(elsewhere.listings(), 0, "destination");

        let inventory = router
            .models_detailed_with_egress(
                &CancellationToken::new(),
                discovery,
                vec![granted_resource()],
                7,
            )
            .await;
        assert!(inventory.failures.is_empty(), "{:?}", inventory.failures);
        assert_eq!(remote.listings(), 1);
    }
}
