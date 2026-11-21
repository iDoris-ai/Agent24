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
use std::sync::{Mutex, RwLock};
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

type GatewaySnapshot = Option<Result<Arc<crate::IdorisGateway>, String>>;

/// Chat and embeddings have one exit. Shown when that exit is not published.
pub(crate) fn idoris_not_connected() -> &'static str {
    "iDoris is not connected. Agent24 sends chat and embeddings only through iDoris. Start the bundled model service, or for development set AGENT24_DEV_IDORIS_URL and AGENT24_DEV_IDORIS_KEY to one iDoris gateway."
}

/// Routes completions across tiered providers with a health/cooldown loop.
pub struct ModelRouter {
    idoris: Arc<RwLock<GatewaySnapshot>>,
    providers: Vec<Routed>,
    health: Mutex<HashMap<String, Health>>,
    base_cooldown: Duration,
    max_cooldown: Duration,
    /// The chat model used when a request names none. [`Self::from_env`]
    /// seeds it; the daemon may replace it at runtime ([`Self::set_default_model`]).
    /// Shared with [`Self::with_separate_health`] so a later update is visible
    /// on both. A hand-built router starts as `None`.
    default_model: Arc<Mutex<Option<String>>>,
    /// `from_env` has no direct model chain. An empty provider list then means
    /// iDoris is missing. [`Self::with_defaults`] stays false so in-process
    /// doubles keep the old empty-list behavior.
    idoris_only: bool,
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

/// How a direct local provider may be asked to skip a reasoning model's
/// thinking (`CompletionRequest::disable_thinking`). Production routing no
/// longer builds those providers; the mapping stays so its unit test can
/// still name the rule.
#[cfg(test)]
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
#[cfg(test)]
fn thinking_switch(backend: &str, tier: Tier) -> ThinkingSwitch {
    match backend {
        _ if !tier.is_local() => ThinkingSwitch::None,
        "omlx" => ThinkingSwitch::ChatTemplateKwargs,
        "ollama" => ThinkingSwitch::ReasoningEffort,
        _ => ThinkingSwitch::None,
    }
}

/// A user-chosen local OpenAI-compatible endpoint (model source setting):
/// `<data_dir>/model-source.json`, `{"url": "...", "model": "..."}`, written
/// by the desktop app. Only a loopback `url` is ever used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelSource {
    pub url: String,
    pub model: String,
}

/// Read and check `path`. `None` when it is not there (silently), or is not
/// a usable source (with a warning): the router is then built as before.
pub fn read_model_source(path: &std::path::Path) -> Option<ModelSource> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct File {
        url: String,
        model: String,
    }
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!("model source {}: unreadable ({e}); ignored", path.display());
            return None;
        }
    };
    let checked = serde_json::from_str::<File>(&text)
        .map_err(|_| "not {\"url\", \"model\"} JSON")
        .and_then(|f| check_model_source(f.url, f.model));
    match checked {
        Ok(source) => Some(source),
        Err(why) => {
            // Never the url itself: a rejected one may carry credentials.
            tracing::warn!("model source {}: {why}; ignored", path.display());
            None
        }
    }
}

const MODEL_SOURCE_MAX_MODEL: usize = 256;

/// The url is a bare origin (`scheme://host[:port]`: no credentials, path,
/// query or fragment; the provider adds `/v1/...`) that `env_local_tier`
/// judges Local, so it is labeled and built exactly as a loopback
/// local override is. Anything else is refused, never made Remote.
fn check_model_source(url: String, model: String) -> Result<ModelSource, &'static str> {
    let parsed = reqwest::Url::parse(&url).map_err(|_| "url does not parse")?;
    if url != parsed.origin().ascii_serialization() {
        return Err("url is not a bare scheme://host:port");
    }
    if env_local_tier(&url) != Tier::Local {
        return Err("url is not loopback");
    }
    let model = model.trim().to_owned();
    if model.is_empty()
        || model.len() > MODEL_SOURCE_MAX_MODEL
        || model.chars().any(char::is_control)
    {
        return Err("model name is empty, too long or has control characters");
    }
    Ok(ModelSource { url, model })
}

/// The open-enum tier string reported on `/models` for a judged tier.
#[cfg(test)]
fn tier_label(t: Tier) -> &'static str {
    match t {
        Tier::Local => "local",
        Tier::Lora => "lora",
        Tier::Remote => "remote",
    }
}

impl ModelRouter {
    /// Build from memory credentials; uses exactly the environment-path checks.
    pub fn from_idoris(url: &str, key: zeroize::Zeroizing<String>) -> Result<Self, String> {
        let gateway = crate::IdorisGateway::from_credentials(url, key)?;
        let router = Self::with_defaults(Vec::new());
        router.replace_idoris(gateway);
        Ok(router)
    }

    /// Keep managed startup fail-closed until its supervisor and credentials are ready.
    pub fn idoris_unavailable(reason: &str) -> Self {
        let router = Self::with_defaults(Vec::new());
        router.block_idoris(reason);
        router
    }

    pub fn block_idoris(&self, reason: &str) {
        *self.idoris.write().unwrap_or_else(|p| p.into_inner()) = Some(Err(reason.into()));
    }

    /// The caller validates health before publishing. Readers retain their own Arc.
    pub fn replace_idoris(&self, gateway: crate::IdorisGateway) {
        *self.idoris.write().unwrap_or_else(|p| p.into_inner()) = Some(Ok(Arc::new(gateway)));
    }

    fn gateway_snapshot(&self) -> GatewaySnapshot {
        self.idoris
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    pub fn idoris_gateway(&self) -> Result<Arc<crate::IdorisGateway>, ModelError> {
        self.gateway_snapshot()
            .ok_or_else(|| ModelError::Unavailable(idoris_not_connected().into()))?
            .map_err(ModelError::Provider)
    }

    async fn wait_idoris(
        &self,
        cancel: &CancellationToken,
        limit: Duration,
        probe: bool,
    ) -> Result<Arc<crate::IdorisGateway>, ModelError> {
        let wait = async {
            loop {
                match self.gateway_snapshot() {
                    Some(Ok(gateway)) => {
                        if !probe || gateway.check_health(cancel).await.is_ok() {
                            return Ok(gateway);
                        }
                    }
                    Some(Err(reason)) if reason.contains("circuit_open") => {
                        return Err(ModelError::Unavailable(
                            "idoris_unavailable: circuit_open; retry_after=60".into(),
                        ));
                    }
                    Some(Err(reason))
                        if reason.contains("not ready")
                            || reason.contains("starting")
                            || reason.contains("waiting") => {}
                    Some(Err(reason)) => {
                        return Err(ModelError::Unavailable(format!(
                            "idoris_unavailable: {reason}"
                        )));
                    }
                    None => return Err(ModelError::Unavailable("idoris_unavailable".into())),
                }
                tokio::select! {
                    _ = cancel.cancelled() => return Err(ModelError::Cancelled),
                    _ = tokio::time::sleep(Duration::from_millis(50)) => {},
                }
            }
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ModelError::Cancelled),
            result = tokio::time::timeout(limit, wait) => result.unwrap_or_else(|_| Err(ModelError::Unavailable("idoris_unavailable: readiness_timeout; retry_after=5".into()))),
        }
    }

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
            idoris: Arc::new(RwLock::new(None)),
            providers: providers
                .into_iter()
                .map(|(provider, tier)| Routed { provider, tier })
                .collect(),
            health: Mutex::new(HashMap::new()),
            base_cooldown,
            max_cooldown,
            default_model: Arc::new(Mutex::new(None)),
            idoris_only: false,
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
            default_model: Arc::clone(&self.default_model),
            idoris_only: self.idoris_only,
        }
    }

    /// Current default chat model, if one has been set. `None` means the
    /// caller has nothing to display and must not invent one.
    pub fn default_model(&self) -> Option<String> {
        self.default_model
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// Replace the default chat model. Takes effect on the next local
    /// completion that does not name its own model, including on a
    /// [`Self::with_separate_health`] copy.
    pub fn set_default_model(&self, model: impl Into<String>) {
        *self
            .default_model
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(model.into());
    }

    /// Convenience default: 2s base, 60s cap. Same PRIVACY CONTRACT as
    /// [`Self::new`]: Local providers must be `loopback_only()`. The cap is a
    /// contract with modules too (ME4-S2a §2.6): they wait it out after
    /// `no_provider`, so the daemon's router must keep [`DEFAULT_MAX_COOLDOWN`].
    pub fn with_defaults(providers: Vec<(Arc<dyn ModelProvider>, Tier)>) -> Self {
        Self::new(providers, Duration::from_secs(2), DEFAULT_MAX_COOLDOWN)
    }

    /// The only production model exit is the iDoris slot.
    ///
    /// A non-empty `DEFAULT_MODEL` is the saved chat model. It is written onto
    /// a request that does not name one, including through the gateway. Unset,
    /// the gateway sends the iDoris role name.
    ///
    /// `AGENT24_DEV_IDORIS_URL` and `AGENT24_DEV_IDORIS_KEY` publish that one
    /// gateway into the same slot a managed iDoris uses. There is no direct
    /// provider and no fallback when the slot is empty.
    pub fn from_env() -> Self {
        let mut router = Self::with_defaults(Vec::new());
        router.idoris_only = true;
        if let Ok(model) = std::env::var("DEFAULT_MODEL") {
            let model = model.trim();
            if !model.is_empty() {
                router.set_default_model(model);
            }
        }
        match crate::IdorisGateway::from_dev_env() {
            Some(Ok(gateway)) => router.replace_idoris(gateway),
            Some(Err(reason)) => router.block_idoris(&reason),
            None => {}
        }
        router
    }

    /// [`Self::from_env`]. `model-source.json` is not a provider.
    pub fn from_env_with_source(source: Option<ModelSource>) -> Self {
        if source.is_some() {
            tracing::warn!("model-source.json is ignored; models go through iDoris only");
        }
        Self::from_env()
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
        if self.gateway_snapshot().is_some() {
            let gateway = self
                .wait_idoris(cancel, Duration::from_secs(30), false)
                .await?;
            let result = gateway.complete(profile, req, cancel, request_id).await;
            if matches!(&result, Err(ModelError::Unavailable(reason)) if reason == "idoris_connection_lost")
            {
                let gateway = self
                    .wait_idoris(cancel, Duration::from_secs(30), true)
                    .await?;
                return gateway.complete(profile, req, cancel, request_id).await.map_err(|error| {
                    if matches!(&error, ModelError::Unavailable(reason) if reason == "idoris_connection_lost") {
                        ModelError::Unavailable("idoris_unavailable: connection_lost; retry_after=5".into())
                    } else { error }
                });
            }
            return result;
        }
        if self.idoris_only {
            return Err(ModelError::Unavailable(idoris_not_connected().into()));
        }
        // iDoris data keys are role-scoped: a concrete model name on the wire
        // is a role-less request and is refused (403). The gateway therefore
        // always sends the role alias; the saved default reaches iDoris as the
        // daily role's binding, not as a per-request model. Only the non-iDoris
        // path below stamps it.
        let stamped;
        let req = match (req.model.is_none(), self.default_model()) {
            (true, Some(model)) => {
                stamped = CompletionRequest {
                    model: Some(model),
                    ..req.clone()
                };
                &stamped
            }
            _ => req,
        };
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
        if let Some(gateway) = self.gateway_snapshot() {
            let result = match &gateway {
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
        if self.idoris_only {
            return ModelInventory {
                models: Vec::new(),
                failures: vec![format!("idoris-local: {}", idoris_not_connected())],
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
            idoris_headroom_confirmed: false,
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
            "http://127.0.0.1:9",
            "http://localhost:9",
            "http://LOCALHOST:9",
            "https://[::1]:8443",
            "http://user:pw@127.0.0.1:9", // real userinfo: host IS 127.0.0.1
            "http://evil.example%2F@127.0.0.1:9", // %2F stays in userinfo
            "http://0x7f000001:9",        // WHATWG normalises to 127.0.0.1
            "http://loc\talhost:9",       // tab stripped → localhost (reqwest agrees)
            "http://127.0.0.1:9?@evil.example", // `?` ends the authority → 127.0.0.1
        ] {
            assert_eq!(env_local_tier(u), Tier::Local, "{u:?}");
        }
        // Must be Remote.
        for u in [
            "http://evil.example\\@127.0.0.1:9", // H1: `\` ends the authority → evil.example
            "http://evil.example:80\\@localhost/",
            "http://127.0.0.1\t.evil.example:9", // tab stripped → a domain
            "http://[::ffff:127.0.0.1]:9",       // mapped v6: not is_loopback()
            "http://0.0.0.0:9",
            "http://localhost.:9",
            "http://192.168.1.50:9",
            "https://inference.example.com",
            "not a url",
            "file:///tmp/x",
            "",
        ] {
            assert_eq!(env_local_tier(u), Tier::Remote, "{u:?}");
        }
        // The reviewer's exact H1 vector, checked against the parser reqwest uses.
        let h1 = "http://evil.example\\@127.0.0.1:9";
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
            idoris_headroom_confirmed: false,
            temperature: None,
            progress: None,
        }
    }

    /// Child of `from_env_without_idoris_errors`. The parent strips the dev
    /// gateway so this process cannot publish a slot or inherit a laptop URL.
    #[tokio::test]
    #[ignore = "child process of from_env_without_idoris_errors"]
    async fn child_from_env_without_idoris() {
        let router = ModelRouter::from_env();
        assert!(router.providers.is_empty());
        let err = router
            .complete(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    ..Default::default()
                },
                &secret_request(),
                &CancellationToken::new(),
            )
            .await
            .unwrap_err();
        let message = err.to_string();
        assert!(message.contains("iDoris"), "{message}");
        assert!(matches!(err, ModelError::Unavailable(_)), "{err:?}");
        let inventory = router.models_detailed(&CancellationToken::new()).await;
        assert!(inventory.models.is_empty());
        assert!(!inventory.is_complete());
        assert!(
            inventory
                .failures
                .iter()
                .any(|line| line.contains("iDoris")),
            "{inventory:?}"
        );
    }

    #[test]
    fn from_env_without_idoris_errors() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "router::tests::child_from_env_without_idoris",
                "--ignored",
                "--nocapture",
            ])
            .env_remove("AGENT24_DEV_IDORIS_URL")
            .env_remove("AGENT24_DEV_IDORIS_KEY")
            .env_remove("DEFAULT_MODEL")
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn idoris_health_body() -> String {
        let version = agent24_idoris::IdorisLock::embedded()
            .tested_contract_version
            .as_str();
        format!(
            r#"{{"service":"idoris","version":"0.5.0","contract_version":"{version}","instance_id":"fixture","dev_no_key_enabled":false}}"#
        )
    }

    /// One loopback iDoris: health, then chat. Returns the chat body.
    fn idoris_chat_stub() -> (String, std::sync::mpsc::Receiver<String>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        let health = idoris_health_body();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut got = Vec::new();
                let mut buf = [0u8; 65536];
                while let Ok(n) = stream.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    got.extend_from_slice(&buf[..n]);
                    if got.windows(4).any(|w| w == b"\r\n\r\n") {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&got);
                let line = text.lines().next().unwrap_or("");
                if line.starts_with("GET /health") {
                    let raw = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{health}",
                        health.len()
                    );
                    let _ = stream.write_all(raw.as_bytes());
                    continue;
                }
                let body = text
                    .split_once("\r\n\r\n")
                    .map(|(_, body)| body.to_owned())
                    .unwrap_or_default();
                let _ = tx.send(body);
                let payload = r#"{"model":"served","choices":[{"message":{"role":"assistant","content":"ok"}}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
                let raw = format!(
                    "HTTP/1.1 200 OK\r\nx-idoris-served-locality: loopback\r\nx-idoris-record-id: rec_default\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{payload}",
                    payload.len()
                );
                let _ = stream.write_all(raw.as_bytes());
            }
        });
        (format!("http://127.0.0.1:{port}/"), rx)
    }

    #[tokio::test]
    async fn the_idoris_gateway_sends_the_role_alias_not_the_saved_default() {
        let (url, bodies) = idoris_chat_stub();
        let router = ModelRouter::with_defaults(vec![]);
        router.replace_idoris(
            crate::IdorisGateway::from_credentials(
                &url,
                zeroize::Zeroizing::new("idk_default_model_fixture".to_owned()),
            )
            .unwrap(),
        );
        router.set_default_model("Qwen3-30B");
        let profile = TaskProfile {
            privacy: Privacy::LocalOnly,
            ..Default::default()
        };
        router
            .complete(profile, &secret_request(), &CancellationToken::new())
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&bodies.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        // A concrete name would be a role-less request and a role-scoped key
        // gets 403; the default reaches iDoris as the daily binding instead.
        assert_eq!(body["model"], "idoris/daily", "{body}");

        let explicit = CompletionRequest {
            model: Some("named-by-caller".to_owned()),
            ..secret_request()
        };
        router
            .complete(profile, &explicit, &CancellationToken::new())
            .await
            .unwrap();
        let body: serde_json::Value =
            serde_json::from_str(&bodies.recv_timeout(Duration::from_secs(5)).unwrap()).unwrap();
        // Even an explicitly named model goes out as the role alias.
        assert_eq!(body["model"], "idoris/daily", "{body}");
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
            idoris_headroom_confirmed: false,
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

    // ── Model source setting (M1) ──────────────────────────────────────────
    fn source_file(body: &str) -> (std::path::PathBuf, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("a24-model-source-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("model-source.json");
        std::fs::write(&file, body).unwrap();
        (dir, file)
    }

    fn read_source(url: &str, model: &str) -> Option<ModelSource> {
        let body = serde_json::json!({ "url": url, "model": model }).to_string();
        let (dir, file) = source_file(&body);
        let got = read_model_source(&file);
        std::fs::remove_dir_all(dir).unwrap();
        got
    }

    #[test]
    fn a_model_source_must_be_loopback() {
        for url in [
            "http://127.0.0.1:1234",
            "http://localhost:1234",
            "http://[::1]:1234",
            "http://127.0.0.2:1234",
        ] {
            assert_eq!(
                read_source(url, "qwen"),
                Some(ModelSource {
                    url: url.to_owned(),
                    model: "qwen".to_owned()
                }),
                "{url}"
            );
        }
        for url in [
            "http://localhost.evil:1234",
            "http://localhost.evil.com",
            "http://127.0.0.1.nip.io:1234",
            "http://127.0.0.1@evil.com:1234",
            "http://user:pw@127.0.0.1:1234",
            "http://evil@localhost:1234",
            "http://0.0.0.0:1234",
            "http://[::ffff:127.0.0.1]:1234",
            "http://10.0.0.5:1234",
            "http://192.168.1.2:1234",
            "https://api.openai.com",
            "ftp://127.0.0.1:1234",
            "127.0.0.1:1234",
            "http://127.0.0.1:1234?x=1",
            "http://127.0.0.1:1234#frag",
            "http://127.0.0.1:1234/v1",
            "",
        ] {
            assert_eq!(read_source(url, "qwen"), None, "{url}");
        }
    }

    #[test]
    fn a_model_source_needs_a_model_name_and_a_well_formed_file() {
        assert_eq!(read_source("http://127.0.0.1:1234", ""), None);
        assert_eq!(read_source("http://127.0.0.1:1234", "   "), None);
        assert_eq!(read_source("http://127.0.0.1:1234", "a\nb"), None);
        assert_eq!(read_source("http://127.0.0.1:1234", &"m".repeat(300)), None);
        for body in [
            "",
            "{",
            "[]",
            r#"{"url":"http://127.0.0.1:1234"}"#,
            r#"{"model":"m"}"#,
        ] {
            let (dir, file) = source_file(body);
            assert_eq!(read_model_source(&file), None, "{body}");
            std::fs::remove_dir_all(dir).unwrap();
        }
        let missing = std::env::temp_dir().join(format!(
            "a24-none-{}/model-source.json",
            uuid::Uuid::new_v4()
        ));
        assert_eq!(read_model_source(&missing), None);
    }

    #[test]
    fn without_a_model_source_the_chain_is_unchanged() {
        let a = ModelRouter::from_env();
        let b = ModelRouter::from_env_with_source(None);
        let names = |r: &ModelRouter| {
            r.providers
                .iter()
                .map(|p| (p.provider.name().to_owned(), p.tier))
                .collect::<Vec<_>>()
        };
        assert_eq!(names(&a), names(&b));
        assert!(names(&b).is_empty());
        assert_eq!(a.default_model(), b.default_model());
    }

    #[tokio::test]
    async fn a_model_source_goes_first_with_its_model_and_no_thinking_switch() {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel::<String>();
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
                let _ = tx.send(
                    text.split_once("\r\n\r\n")
                        .map(|(_, b)| b.to_owned())
                        .unwrap_or_default(),
                );
                let _ = s.write_all(http_ok().as_bytes());
            }
        });
        let source = ModelSource {
            url: format!("http://127.0.0.1:{port}"),
            model: "lm-qwen".to_owned(),
        };
        let r = ModelRouter::from_env_with_source(Some(source));
        assert!(r.providers.is_empty());
        let err = r
            .complete(
                TaskProfile {
                    privacy: Privacy::LocalOnly,
                    ..Default::default()
                },
                &secret_request(),
                &CancellationToken::new(),
            )
            .await;
        // A developer machine may already publish a dev gateway. Either way
        // the model-source listener is not a provider.
        match err {
            Ok(_) => assert!(r.idoris_gateway().is_ok()),
            Err(err) => {
                let message = err.to_string();
                assert!(message.contains("iDoris"), "{message}");
            }
        }
        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "model-source.json must not be dialed"
        );
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
        let local = env_local_tier("http://127.0.0.1:9");
        let remote = env_local_tier("http://10.0.0.5:9");
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

    #[tokio::test]
    async fn set_default_model_is_what_a_model_less_request_sends() {
        struct Seen(StdMutex<Option<String>>);
        #[async_trait]
        impl ModelProvider for Seen {
            fn name(&self) -> &str {
                "seen"
            }
            async fn complete(
                &self,
                req: &CompletionRequest,
                _cancel: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                *self.0.lock().unwrap() = req.model.clone();
                Ok(CompletionResponse {
                    message: Msg::assistant(Some("ok".to_owned()), vec![]),
                    usage: Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        cost_usd: 0.0,
                    },
                    model_id: None,
                })
            }
            async fn models(
                &self,
                _cancel: &CancellationToken,
            ) -> Result<Vec<crate::Model>, ModelError> {
                Ok(vec![])
            }
        }
        let seen = Arc::new(Seen(StdMutex::new(None)));
        let router = router(vec![(seen.clone(), Tier::Local)]);
        router.set_default_model("Qwen3-30B");
        assert_eq!(
            router.with_separate_health().default_model().as_deref(),
            Some("Qwen3-30B")
        );
        router
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(seen.0.lock().unwrap().as_deref(), Some("Qwen3-30B"));
        let mut explicit = req();
        explicit.model = Some("kept".to_owned());
        router
            .complete(TaskProfile::default(), &explicit, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(seen.0.lock().unwrap().as_deref(), Some("kept"));
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

#[cfg(test)]
mod ic3a_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    #[tokio::test]
    async fn slot_wait_is_bounded_and_circuit_fails_immediately() {
        let router = ModelRouter::idoris_unavailable("managed iDoris not ready");
        let error = router
            .wait_idoris(&CancellationToken::new(), Duration::from_millis(15), false)
            .await
            .err()
            .unwrap();
        assert!(
            matches!(error, ModelError::Unavailable(reason) if reason.contains("readiness_timeout"))
        );
        router.block_idoris("idoris_unavailable: circuit_open");
        let error = tokio::time::timeout(
            Duration::from_millis(100),
            router.wait_idoris(&CancellationToken::new(), Duration::from_secs(30), false),
        )
        .await
        .unwrap()
        .err()
        .unwrap();
        assert!(
            matches!(error, ModelError::Unavailable(reason) if reason.contains("circuit_open"))
        );
    }

    #[tokio::test]
    async fn waiting_request_observes_replacement_and_cancellation() {
        let router = Arc::new(ModelRouter::idoris_unavailable("starting"));
        let waiting = router.clone();
        let task = tokio::spawn(async move {
            waiting
                .wait_idoris(&CancellationToken::new(), Duration::from_secs(1), false)
                .await
        });
        tokio::time::sleep(Duration::from_millis(10)).await;
        router.replace_idoris(
            crate::IdorisGateway::from_credentials(
                "http://127.0.0.1:9999",
                zeroize::Zeroizing::new("idk_key".into()),
            )
            .unwrap(),
        );
        assert!(task.await.unwrap().is_ok());
        router.block_idoris("starting");
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert!(matches!(
            router
                .wait_idoris(&cancel, Duration::from_secs(30), false)
                .await,
            Err(ModelError::Cancelled)
        ));
    }
    #[tokio::test]
    async fn nonstream_connection_close_retries_exactly_once() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for recover in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let chats = Arc::new(AtomicUsize::new(0));
            let seen = chats.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut bytes = Vec::new();
                    let mut buf = [0; 4096];
                    loop {
                        let n = socket.read(&mut buf).await.unwrap();
                        if n == 0 {
                            break;
                        }
                        bytes.extend_from_slice(&buf[..n]);
                        if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&bytes[..end]);
                            let size = headers
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length: ")
                                        .and_then(|s| s.parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if bytes.len() >= end + 4 + size {
                                break;
                            }
                        }
                    }
                    let health = bytes.starts_with(b"GET /health ");
                    let body = if health {
                        serde_json::json!({"service":"idoris","version":"0.5.0","contract_version":"1.2.0","instance_id":"fixture","dev_no_key_enabled":false}).to_string()
                    } else {
                        let call = seen.fetch_add(1, Ordering::SeqCst);
                        if call == 0 || !recover {
                            continue;
                        }
                        serde_json::json!({"model":"fixture","choices":[{"message":{"role":"assistant","content":"recovered"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}).to_string()
                    };
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nx-idoris-served-locality: loopback\r\nx-idoris-record-id: recovered\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    socket.write_all(reply.as_bytes()).await.unwrap();
                }
            });
            let router =
                ModelRouter::from_idoris(&url, zeroize::Zeroizing::new("idk_retry".into()))
                    .unwrap();
            let request = CompletionRequest {
                messages: vec![crate::Msg::user("test")],
                model: None,
                tools: vec![],
                response_format: None,
                max_tokens: None,
                temperature: None,
                disable_thinking: false,
                idoris_headroom_confirmed: false,
                progress: None,
            };
            let result = tokio::time::timeout(
                Duration::from_secs(5),
                router.complete(TaskProfile::default(), &request, &CancellationToken::new()),
            )
            .await
            .unwrap();
            assert_eq!(result.is_ok(), recover);
            assert_eq!(chats.load(Ordering::SeqCst), 2);
            server.abort();
        }
    }
}
