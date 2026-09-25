//! `_a24/model/complete` (ME4-S2 v3.1, §4–§7). **4.2.2b1a**: constants,
//! admission (§5), the usage sink (§6.3), grants/deps (§5.1), and error
//! mapping (§7). **4.2.2b1b**: the wire types (§4.2/§4.3) and
//! `ModelCompleteHandler` (§4.4). **4.2.2b2** (this increment):
//! [`model_grant`] — built once per mount, outside the `MethodsFor` closure,
//! same shape as `crate::os_memory::memory_grant_name` — and
//! `crate::domain` wires `KERNEL_OOP_GRANTS`, `CallbackDeps.models`,
//! `provides`, and registers `_a24/model/complete` unconditionally (design
//! §2.4). **Not** in this file (see the split in
//! `docs/design/ME4-S2-model-callback.md` §10.2): persisted usage (4.2.3).

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent24_domain::{Capability, Grants, ModelAccess};
use agent24_models::router::{Complexity, ModelRouter, Privacy, TaskProfile, Tier};
use agent24_models::{CompletionRequest, ModelError, Msg, ResponseFormat};
use agent24_os_proto::drain::{Generation, LifecycleTimeout, bind_to_lifecycle};
use agent24_os_proto::rpc::{CallFuture, ErrorKind, Handler, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;

#[cfg(test)]
use crate::events_emit::Clock;
use crate::events_emit::{RateLimiter, refused_error};

// ---- §5.2 numbers (⚖️ = chosen, not derived) ----

/// §3 decision M2: this method's own budget (== the provider's own
/// `chat_timeout`, `agent24-models/src/lib.rs:206`). Consumed by
/// `Handler::call_timeout` in 4.2.2b1b.
pub const MODEL_CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// ⚖️ §5: per-module in-flight ceiling (PLAN S2-5 suggests 2).
pub const MODEL_MAX_IN_FLIGHT_PER_MODULE: usize = 2;
/// ⚖️ §5: daemon-wide ceiling across ALL modules' model calls.
pub const MODEL_MAX_IN_FLIGHT_GLOBAL: usize = 4;
/// ⚖️ §5: per-module token bucket — burst 30 calls, 30 calls/minute sustained.
pub const MODEL_RATE_CAPACITY: f64 = 30.0;
pub const MODEL_RATE_REFILL_PER_SEC: f64 = 0.5;
/// ⚖️ §4.2: `max_tokens` bounds for a module call. Consumed by
/// `ModelCompleteParams::validate` in 4.2.2b1b.
pub const MODEL_MAX_TOKENS_CEILING: u32 = 4096;
pub const MODEL_DEFAULT_MAX_TOKENS: u32 = 1024;
/// ⚖️ §4.2: message count bound (string bytes are already bounded by
/// `dispatch()`'s 256 KiB params budget). Consumed by 4.2.2b1b.
pub const MODEL_MAX_MESSAGES: usize = 64;
/// §4.3 (v3 N4): the largest SERIALIZED result. The response line is
/// `{"jsonrpc":"2.0","id":<id>,"result":<this>}\n` and must fit the 1 MiB
/// frame. The id is a string of at most `MAX_ID_BYTES` (256) bytes, which
/// JSON-escapes to at most 6 × 256; 4 KiB covers it and the envelope. Checked
/// on the serialized bytes — raw text length undercounts escapes (a quote is
/// 2 bytes, a control character 6).
pub const RESULT_ENVELOPE_MARGIN: usize = 4096;
pub const MODEL_MAX_RESULT_BYTES: usize =
    agent24_os_proto::frame::MAX_FRAME_BYTES - RESULT_ENVELOPE_MARGIN;
const _: () = assert!(6 * agent24_os_proto::rpc::MAX_ID_BYTES + 64 <= RESULT_ENVELOPE_MARGIN);
/// v3 N4: a provider-reported model id longer than this is dropped (`None`),
/// not truncated — a truncated id would name a model that does not exist.
pub const MODEL_MAX_MODEL_ID_BYTES: usize = 256;

// ---- §4.2 params (deny_unknown_fields + _meta) ----

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ModelCompleteParams {
    messages: Vec<WireMessage>,
    #[serde(default)]
    response_format: Option<WireResponseFormat>,
    #[serde(default)]
    max_tokens: Option<u32>,
    #[serde(default)]
    complexity: Option<WireComplexity>,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    _meta: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireMessage {
    role: WireRole,
    content: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRole {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireComplexity {
    Simple,
    Complex,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum WireResponseFormat {
    JsonSchema { json_schema: WireJsonSchema },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireJsonSchema {
    name: String,
    schema: Map<String, Value>,
    #[serde(default)]
    strict: bool,
}

impl ModelCompleteParams {
    fn validate(&self) -> Result<(), String> {
        if self.messages.is_empty() || self.messages.len() > MODEL_MAX_MESSAGES {
            return Err(format!(
                "messages must hold 1..={MODEL_MAX_MESSAGES} entries"
            ));
        }
        if let Some(n) = self.max_tokens
            && !(1..=MODEL_MAX_TOKENS_CEILING).contains(&n)
        {
            return Err(format!(
                "max_tokens must be between 1 and {MODEL_MAX_TOKENS_CEILING}"
            ));
        }
        if let Some(WireResponseFormat::JsonSchema { json_schema }) = &self.response_format
            && json_schema.name.is_empty()
        {
            return Err("response_format.json_schema.name must not be empty".to_owned());
        }
        Ok(())
    }

    fn parse(params: Value) -> Result<Self, String> {
        let p: Self = serde_json::from_value(params).map_err(|e| e.to_string())?;
        p.validate()?;
        Ok(p)
    }

    fn into_request(self) -> (CompletionRequest, Complexity, Option<String>) {
        let max = self.max_tokens.unwrap_or(MODEL_DEFAULT_MAX_TOKENS);
        let request = CompletionRequest {
            messages: self
                .messages
                .into_iter()
                .map(|m| match m.role {
                    WireRole::System => Msg::system(m.content),
                    WireRole::User => Msg::user(m.content),
                    WireRole::Assistant => Msg::assistant(Some(m.content), vec![]),
                })
                .collect(),
            model: None,
            tools: vec![],
            response_format: self.response_format.map(
                |WireResponseFormat::JsonSchema { json_schema }| ResponseFormat::JsonSchema {
                    name: json_schema.name,
                    schema: Value::Object(json_schema.schema),
                    strict: json_schema.strict,
                },
            ),
            max_tokens: NonZeroU32::new(max),
        };
        let complexity = match self.complexity {
            Some(WireComplexity::Complex) => Complexity::Complex,
            Some(WireComplexity::Simple) | None => Complexity::Simple,
        };
        (request, complexity, self.request_id)
    }
}

// ---- §4.3 result ----

#[derive(Debug, Serialize)]
struct ModelCompleteResult {
    text: String,
    model_id: Option<String>,
    tier: &'static str,
    usage: ResultUsage,
}

#[derive(Debug, Serialize)]
struct ResultUsage {
    prompt_tokens: u64,
    completion_tokens: u64,
}

// ---- §6 usage: outcomes and the sink (4.2.2b1a). Storage is 4.2.3. ----

/// Which tier actually served a call — the fact the per-module usage ledger
/// (`served_by`) and the LocalOnly tripwire both read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Served {
    Local,
    Remote,
}

/// How one call that reached the router ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageOutcome {
    /// Tokens as the provider reported them (0 when it reported none).
    Ok {
        served: Served,
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    /// A provider answered but the KERNEL refused to pass the answer on
    /// (oversize text, LocalOnly tripwire): the tokens were spent.
    FailedAfterServe {
        served: Served,
        prompt_tokens: u64,
        completion_tokens: u64,
    },
    /// No provider produced a completion.
    Failed,
    Cancelled,
}

/// v2 M4: where outcomes go. Synchronous and non-blocking by contract — it
/// will be called from `Drop`, so it must never await or spawn.
pub trait UsageSink: Send + Sync {
    fn record(&self, module: &str, outcome: UsageOutcome);
}

/// 4.2.2b1's sink (and every handler test's, once 4.2.2b1b lands): an
/// in-memory list. 4.2.3 replaces it in production with `UsageRecorder`.
#[derive(Default)]
pub struct MemoryUsageSink(Mutex<Vec<(String, UsageOutcome)>>);

impl MemoryUsageSink {
    /// Test-only: production reads the sink through `UsageSink::record`
    /// alone (4.2.3 replaces this sink entirely). Not gated at the file
    /// level any more (4.2.2b2 removed that blanket allow, since most of
    /// this file is now reachable from `serve()`) — gated here instead, on
    /// the one method nothing in the production path calls.
    #[cfg(test)]
    pub fn take(&self) -> Vec<(String, UsageOutcome)> {
        std::mem::take(
            &mut *self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

impl UsageSink for MemoryUsageSink {
    fn record(&self, module: &str, outcome: UsageOutcome) {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((module.to_owned(), outcome));
    }
}

/// Exactly one outcome per call that reached the router: `finish` records it;
/// a drop without `finish` records `Cancelled`. Built by the handler
/// (4.2.2b1b) once it is about to route.
pub struct UsageTicket {
    sink: Arc<dyn UsageSink>,
    module: String,
    done: bool,
}

impl UsageTicket {
    pub fn new(sink: Arc<dyn UsageSink>, module: String) -> Self {
        Self {
            sink,
            module,
            done: false,
        }
    }

    pub fn finish(mut self, outcome: UsageOutcome) {
        self.done = true;
        self.sink.record(&self.module, outcome);
    }
}

impl Drop for UsageTicket {
    fn drop(&mut self) {
        if !self.done {
            self.sink.record(&self.module, UsageOutcome::Cancelled);
        }
    }
}

/// The tier a completed call was served from.
fn served_of(t: Tier) -> Served {
    if t.is_local() {
        Served::Local
    } else {
        Served::Remote
    }
}

// ---- §5 admission: per-module cap + a FAIR global cap (v2 M2 / v3 N3) ----

/// Daemon-level. One lock decides both caps, so the fairness rule is exact,
/// not a racy check-then-acquire:
/// - a module's FIRST in-flight call needs `total < GLOBAL`;
/// - any FURTHER call needs `total < GLOBAL - 1` (it may never take the last
///   free slot) and `mine < PER_MODULE`.
///
/// So second-and-later calls fill at most `GLOBAL - 1` slots: two modules can
/// never hold all four. v3 N3 (exact property): a new module's first call
/// finds a slot whenever at most `GLOBAL - 2` OTHER modules are active — with
/// GLOBAL = 4, "at most 2 others". With 3 others it may not (A1, A2, B1, C1 →
/// D busy; not guaranteed, see R3).
pub struct ModelAdmission {
    global: usize,
    per_module: usize,
    state: Mutex<(usize, HashMap<String, usize>)>,
}

pub struct AdmissionGuard {
    admission: Arc<ModelAdmission>,
    module: String,
}

impl ModelAdmission {
    pub fn new(global: usize, per_module: usize) -> Arc<Self> {
        Arc::new(Self {
            global,
            per_module,
            state: Mutex::new((0, HashMap::new())),
        })
    }

    pub fn try_admit(self: &Arc<Self>, module: &str) -> Option<AdmissionGuard> {
        let mut st = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let total = st.0;
        let mine = st.1.get(module).copied().unwrap_or(0);
        let ok = if mine == 0 {
            total < self.global
        } else {
            mine < self.per_module && total + 1 < self.global
        };
        if !ok {
            return None;
        }
        st.0 += 1;
        *st.1.entry(module.to_owned()).or_insert(0) += 1;
        Some(AdmissionGuard {
            admission: Arc::clone(self),
            module: module.to_owned(),
        })
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        let mut st = self
            .admission
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        st.0 = st.0.saturating_sub(1);
        if let Some(n) = st.1.get_mut(&self.module) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                st.1.remove(&self.module);
            }
        }
    }
}

// ---- §3.3 cancel root wiring (serve()) ----

/// §3.3/§3.4: the parent of every in-flight call's cancellation token.
/// `modules_cut_off` is `Shutdown::modules_cut_off()` in production — NOT the
/// start of shutdown, deliberately: a module's in-flight inference is allowed
/// to run to completion through its own drain, and only stops when the
/// module itself is about to be cut off (design §3.3 "为什么不在停机一开始就
/// 中止"). Spawns a task that waits for that future, then fires the token;
/// the returned token is what `ModelCallbackDeps::cancel_root` holds and every
/// call's `cancel_root.child_token()` (§4.4) descends from.
pub(crate) fn spawn_cancel_root(
    modules_cut_off: impl std::future::Future<Output = ()> + Send + 'static,
) -> CancellationToken {
    let root = CancellationToken::new();
    let fire = root.clone();
    tokio::spawn(async move {
        modules_cut_off.await;
        fire.cancel();
    });
    root
}

// ---- §5.1 grants and daemon-level deps ----

/// Daemon-level: built once in `serve()` (4.2.2b2), handed to `mount_all` BY
/// VALUE and dropped when it returns (so the only usage senders left live in
/// grants — §6.3's "writer exits when the channel closes").
#[derive(Clone)]
pub struct ModelCallbackDeps {
    /// `AppState.router` — the KERNEL's router. Never routed through directly
    /// by a module: each grant takes `with_separate_health()` of it (v2 H2).
    pub router: Arc<ModelRouter>,
    pub usage: Arc<dyn UsageSink>,
    /// v2 M1: cancelled at `Shutdown::modules_cut_off()`, not at the start of
    /// the shutdown — a module's in-flight inference lives exactly as long as
    /// the module's own drain allows.
    pub cancel_root: CancellationToken,
    pub admission: Arc<ModelAdmission>,
}

/// Mount-level: built once per mounted module (4.2.2b2's `mount_package`),
/// OUTSIDE the `MethodsFor` closure (SPEC §5 "限流桶不能被崩溃重置").
#[derive(Clone)]
pub struct ModelGrant {
    pub module: String,
    pub privacy: Privacy,
    /// v2 H2: this module's OWN health/cooldown table over the kernel's
    /// providers — the failures it provokes steer only its own routing.
    pub router: Arc<ModelRouter>,
    pub limiter: Arc<RateLimiter>,
    pub deps: ModelCallbackDeps,
}

impl ModelGrant {
    pub fn new(module: String, access: ModelAccess, deps: ModelCallbackDeps) -> Self {
        Self::build(
            module,
            access,
            deps,
            RateLimiter::new(MODEL_RATE_CAPACITY, MODEL_RATE_REFILL_PER_SEC),
        )
    }

    /// v2 L6: the same, with an injected clock for the bucket. Test-only —
    /// production always uses the wall clock via `new` (4.2.2b2 note: not
    /// gated at the file level, see `MemoryUsageSink::take`'s comment).
    #[cfg(test)]
    pub fn with_clock(
        module: String,
        access: ModelAccess,
        deps: ModelCallbackDeps,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::build(
            module,
            access,
            deps,
            RateLimiter::with_clock(MODEL_RATE_CAPACITY, MODEL_RATE_REFILL_PER_SEC, clock),
        )
    }

    fn build(
        module: String,
        access: ModelAccess,
        deps: ModelCallbackDeps,
        limiter: RateLimiter,
    ) -> Self {
        Self {
            module,
            privacy: match access {
                ModelAccess::RemoteAllowed => Privacy::Any,
                ModelAccess::LocalOnly => Privacy::LocalOnly,
            },
            router: Arc::new(deps.router.with_separate_health()),
            limiter: Arc::new(limiter),
            deps,
        }
    }
}

/// §2.4: built once per mount, OUTSIDE the `MethodsFor` closure — same shape
/// and reason as `crate::os_memory::memory_grant_name` (a restarted
/// generation must see the same limiter/health-table, not a fresh one).
/// `None` unless BOTH halves hold: the module actually requested and was
/// granted [`Capability::Models`], AND this daemon built model deps at all
/// (`deps` is `None` for a `CallbackDeps` with no router configured — no such
/// daemon exists in production, but tests that don't care about model
/// routing take that shortcut). Never lets a module appear in
/// `granted`/`provides` for a capability it does not actually hold
/// (invariant #134, the same rule `memory` follows).
pub(crate) fn model_grant(
    name: &str,
    access: ModelAccess,
    granted: &Grants,
    deps: Option<&ModelCallbackDeps>,
) -> Option<ModelGrant> {
    match (granted.has(Capability::Models), deps) {
        (true, Some(deps)) => Some(ModelGrant::new(name.to_owned(), access, deps.clone())),
        _ => None,
    }
}

// ---- §7 error mapping ----

/// v2 L3: `data.cause` is a closed set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableCause {
    NoProvider,
    RequestRejected,
    BackendConfig,
    ResponseTooLarge,
}

impl UnavailableCause {
    /// v3 L-b: the closed set, pinned against the SPEC sentence by a test.
    /// Test-only: nothing in the production path iterates the closed set
    /// (4.2.2b2 note: not gated at the file level, see
    /// `MemoryUsageSink::take`'s comment).
    #[cfg(test)]
    pub const ALL: [UnavailableCause; 4] = [
        Self::NoProvider,
        Self::RequestRejected,
        Self::BackendConfig,
        Self::ResponseTooLarge,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NoProvider => "no_provider",
            Self::RequestRejected => "request_rejected",
            Self::BackendConfig => "backend_config",
            Self::ResponseTooLarge => "response_too_large",
        }
    }

    fn retryable(self) -> bool {
        matches!(self, Self::NoProvider)
    }
}

fn unavailable(cause: UnavailableCause, message: &str) -> RpcError {
    RpcError::application(ErrorKind::Unavailable, message)
        .with_data("retryable", Value::Bool(cause.retryable()))
        .with_data("cause", Value::String(cause.as_str().to_owned()))
}

/// Default-deny: nothing from the provider's message reaches the module.
/// `unavailable(ResponseTooLarge, ..)` (the fourth cause) is raised directly
/// by the handler in 4.2.2b1b, once it has measured the serialized result
/// (§4.3, v3 N4) — this function only maps what `ModelRouter` can return.
pub fn map_model_error(module: &str, e: &ModelError) -> RpcError {
    match e {
        ModelError::Unavailable(detail) => {
            tracing::warn!(module, %detail, "model call: no permitted provider available");
            unavailable(
                UnavailableCause::NoProvider,
                "no model this module may use is available right now",
            )
        }
        ModelError::Rejected { status, message } => {
            tracing::warn!(module, status, %message, "model call: provider rejected the request");
            let cause = match status {
                400 | 408 | 409 | 413 | 422 => UnavailableCause::RequestRejected,
                _ => UnavailableCause::BackendConfig,
            };
            unavailable(cause, "the model backend refused this request")
        }
        ModelError::Provider(detail) => {
            tracing::warn!(module, %detail, "model call: backend failed");
            unavailable(
                UnavailableCause::BackendConfig,
                "the model backend failed this request",
            )
        }
        ModelError::Cancelled => {
            RpcError::application(ErrorKind::Cancelled, "the daemon is shutting down")
        }
    }
}

fn forbidden() -> RpcError {
    RpcError::application(
        ErrorKind::Forbidden,
        "this module was not granted model access",
    )
}

fn busy() -> RpcError {
    RpcError::application(
        ErrorKind::Busy,
        "too many model calls are already in flight",
    )
}

fn lifecycle_error(e: LifecycleTimeout) -> RpcError {
    match e {
        LifecycleTimeout::BudgetExhausted => RpcError::application(
            ErrorKind::Timeout,
            "this call's request-bound time budget was exhausted",
        ),
        LifecycleTimeout::RequestEnded => RpcError::application(
            ErrorKind::Timeout,
            "the request this call was bound to has already ended",
        ),
    }
}

// ---- §4.4 the handler ----

pub struct ModelCompleteHandler {
    pub generation: Arc<Generation>,
    /// `None` → `forbidden`; the method is registered unconditionally from
    /// 4.2.2b2 on regardless of whether this module holds a grant.
    pub grant: Option<ModelGrant>,
}

impl Handler for ModelCompleteHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        ModelCompleteParams::parse(params.clone()).map(|_| ())
    }

    fn call_timeout(&self) -> Option<Duration> {
        Some(MODEL_CALL_TIMEOUT)
    }

    fn call(&self, params: Value) -> CallFuture {
        let parsed = ModelCompleteParams::parse(params);
        let grant = self.grant.clone();
        let generation = self.generation.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params valid at check_params but not at call(): {e}"
                ))
            })?;
            let Some(grant) = grant else {
                return Err(forbidden());
            };
            let (request, complexity, request_id) = parsed.into_request();

            // One lock: admission + the bound request's lifecycle (W4b). Does
            // not repeat FU-70.
            let lifecycle = generation
                .admit_callback_bound(request_id.as_deref())
                .map_err(refused_error)?;
            if request_id.is_some() && lifecycle.is_none() {
                // §3.4: stricter than memory — an id that is not (or no
                // longer) in flight is refused, not silently run unbound.
                return Err(RpcError::application(
                    ErrorKind::Timeout,
                    "request_id is not (or no longer) in flight; send no request_id for background work",
                )
                .with_data("retryable", Value::Bool(false))); // v2 L4
            }

            // §5: fair admission first (no queueing: `busy`), THEN the token
            // bucket — a call refused as busy must not spend a token.
            let Some(_admitted) = grant.deps.admission.try_admit(&grant.module) else {
                return Err(busy());
            };
            if !grant.limiter.try_acquire() {
                return Err(RpcError::application(
                    ErrorKind::RateLimited,
                    "model call rate limit reached",
                ));
            }

            let profile = TaskProfile {
                privacy: grant.privacy,
                complexity,
            };
            let cancel = grant.deps.cancel_root.child_token(); // v2 M1
            let _cancel_on_drop = cancel.clone().drop_guard(); // §3.3
            let ticket = UsageTicket::new(grant.deps.usage.clone(), grant.module.clone()); // §6.3

            let served = match bind_to_lifecycle(
                lifecycle,
                grant.router.complete_served(profile, &request, &cancel), // v2 H2
            )
            .await
            {
                Err(lt) => return Err(lifecycle_error(lt)), // ticket drops → Cancelled
                Ok(Err(e)) => {
                    ticket.finish(match e {
                        ModelError::Cancelled => UsageOutcome::Cancelled,
                        _ => UsageOutcome::Failed,
                    });
                    return Err(map_model_error(&grant.module, &e)); // §7
                }
                Ok(Ok(served)) => served,
            };
            let u = &served.response.usage;
            let (p, c, s) = (u.prompt_tokens, u.completion_tokens, served_of(served.tier));
            // §2.2 tripwire — only catches a `tier_order` regression (L1):
            // it reads the same `Tier` label routing trusted, so it cannot
            // catch a mislabelled provider (that is §2.3/J16's job).
            if grant.privacy == Privacy::LocalOnly && !served.tier.is_local() {
                tracing::error!(
                    module = %grant.module,
                    provider = %served.provider,
                    "LocalOnly model call was served by a non-local tier — router invariant broken"
                );
                ticket.finish(UsageOutcome::FailedAfterServe {
                    served: s,
                    prompt_tokens: p,
                    completion_tokens: c,
                });
                return Err(RpcError::internal(
                    "the kernel routed this call incorrectly; the result is withheld",
                ));
            }
            let result = ModelCompleteResult {
                text: served.response.message.content.clone().unwrap_or_default(),
                model_id: served
                    .response
                    .model_id
                    .clone()
                    .filter(|m| m.len() <= MODEL_MAX_MODEL_ID_BYTES),
                tier: if served.tier.is_local() {
                    "local"
                } else {
                    "remote"
                },
                usage: ResultUsage {
                    prompt_tokens: p,
                    completion_tokens: c,
                },
            };
            // v3 N4: measure what will actually be written, THEN record —
            // metering never disagrees with what the module actually got.
            let value = serde_json::to_value(&result)
                .map_err(|e| RpcError::internal(format!("result not serialisable: {e}")))?;
            let size = serde_json::to_vec(&value)
                .map(|v| v.len())
                .unwrap_or(usize::MAX);
            if size > MODEL_MAX_RESULT_BYTES {
                tracing::warn!(
                    module = %grant.module,
                    bytes = size,
                    "model call: answer too large to return"
                );
                ticket.finish(UsageOutcome::FailedAfterServe {
                    served: s,
                    prompt_tokens: p,
                    completion_tokens: c,
                });
                return Err(unavailable(
                    UnavailableCause::ResponseTooLarge,
                    "the model's answer exceeds the size a callback result may carry; lower max_tokens",
                ));
            }
            ticket.finish(UsageOutcome::Ok {
                served: s,
                prompt_tokens: p,
                completion_tokens: c,
            });
            Ok(value)
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::sync::atomic::{AtomicUsize, Ordering};

    use agent24_models::router::{Complexity, TaskProfile};
    use agent24_models::{CompletionRequest, CompletionResponse, ModelProvider, Msg};
    use agent24_protocol::{Model, Usage};
    use serde_json::json;

    use super::*;

    /// §5.2: pin the frozen numbers table. `MODEL_CALL_TIMEOUT` feeds
    /// `Handler::call_timeout`, the other three feed
    /// `ModelCompleteParams::validate`/`into_request` — both in 4.2.2b1b,
    /// which does not exist yet in this increment.
    #[test]
    fn design_constants_match_the_frozen_numbers_table() {
        assert_eq!(MODEL_CALL_TIMEOUT, Duration::from_secs(120));
        assert_eq!(MODEL_MAX_TOKENS_CEILING, 4096);
        assert_eq!(MODEL_DEFAULT_MAX_TOKENS, 1024);
        assert_eq!(MODEL_MAX_MESSAGES, 64);
    }

    // ---- J13: error mapping never leaks provider detail; cause closed set ----

    #[test]
    fn map_model_error_never_leaks_provider_detail() {
        let e = map_model_error(
            "sin90",
            &ModelError::Unavailable("stub-SECRET HTTP 500 secret-host".into()),
        );
        assert_eq!(e.kind, Some(ErrorKind::Unavailable));
        let data = e.data.clone().unwrap();
        assert_eq!(
            (data["retryable"].clone(), data["cause"].clone()),
            (json!(true), json!("no_provider"))
        );
        assert!(!e.message.contains("SECRET") && !e.message.contains("secret-host"));

        let e = map_model_error(
            "sin90",
            &ModelError::Rejected {
                status: 400,
                message: "stub-SECRET: malformed schema".into(),
            },
        );
        let data = e.data.clone().unwrap();
        assert_eq!(
            (data["retryable"].clone(), data["cause"].clone()),
            (json!(false), json!("request_rejected"))
        );
        assert!(!e.message.contains("SECRET"));

        let e = map_model_error(
            "sin90",
            &ModelError::Rejected {
                status: 401,
                message: "stub-SECRET: bad api key".into(),
            },
        );
        let data = e.data.clone().unwrap();
        assert_eq!(
            (data["retryable"].clone(), data["cause"].clone()),
            (json!(false), json!("backend_config"))
        );
        assert!(!e.message.contains("SECRET"));

        let e = map_model_error(
            "sin90",
            &ModelError::Provider("stub-SECRET: bad json from secret-host".into()),
        );
        let data = e.data.clone().unwrap();
        assert_eq!(
            (data["retryable"].clone(), data["cause"].clone()),
            (json!(false), json!("backend_config"))
        );
        assert!(!e.message.contains("SECRET") && !e.message.contains("secret-host"));

        let e = map_model_error("sin90", &ModelError::Cancelled);
        assert_eq!(e.kind, Some(ErrorKind::Cancelled));
        assert_eq!(e.message, "the daemon is shutting down");
    }

    /// v3 L-b: `data.cause` is exactly the SPEC's closed set.
    #[test]
    fn unavailable_causes_are_exactly_specs_closed_set() {
        const SPEC: &str = "`unavailable` 的 `data` 固定为 `{retryable: bool, cause: no_provider|request_rejected|backend_config|response_too_large}`";
        let spec: std::collections::HashSet<&str> = SPEC
            .split("cause: ")
            .nth(1)
            .unwrap()
            .trim_end_matches(['}', '`'])
            .split('|')
            .collect();
        let ours: std::collections::HashSet<&str> =
            UnavailableCause::ALL.iter().map(|c| c.as_str()).collect();
        assert_eq!(ours, spec);
        assert_eq!(ours.len(), UnavailableCause::ALL.len());
    }

    // ---- §6.3: the sink's contract — finish records once, drop-without-finish records Cancelled ----

    #[test]
    fn usage_ticket_records_finish_or_drop_as_cancelled() {
        let sink = Arc::new(MemoryUsageSink::default());
        let t = UsageTicket::new(sink.clone(), "sin90".into());
        t.finish(UsageOutcome::Ok {
            served: Served::Local,
            prompt_tokens: 3,
            completion_tokens: 5,
        });
        assert_eq!(
            sink.take(),
            vec![(
                "sin90".to_owned(),
                UsageOutcome::Ok {
                    served: Served::Local,
                    prompt_tokens: 3,
                    completion_tokens: 5
                }
            )]
        );

        let t = UsageTicket::new(sink.clone(), "sin90".into());
        drop(t);
        assert_eq!(
            sink.take(),
            vec![("sin90".to_owned(), UsageOutcome::Cancelled)]
        );
    }

    /// §6.2: the two outcomes a provider-answered-but-kernel-refused call can
    /// record (oversize text, LocalOnly tripwire — both raised by the
    /// handler, 4.2.2b1b) and a router-refused call (`Failed`). Exercised
    /// directly here since b1a has no handler to raise them yet.
    #[test]
    fn failed_and_failed_after_serve_are_recorded_like_any_other_outcome() {
        let sink = Arc::new(MemoryUsageSink::default());
        UsageTicket::new(sink.clone(), "m".into()).finish(UsageOutcome::Failed);
        UsageTicket::new(sink.clone(), "m".into()).finish(UsageOutcome::FailedAfterServe {
            served: Served::Remote,
            prompt_tokens: 9,
            completion_tokens: 1,
        });
        assert_eq!(
            sink.take(),
            vec![
                ("m".to_owned(), UsageOutcome::Failed),
                (
                    "m".to_owned(),
                    UsageOutcome::FailedAfterServe {
                        served: Served::Remote,
                        prompt_tokens: 9,
                        completion_tokens: 1
                    }
                ),
            ]
        );
    }

    /// `served_of`: the tier → `Served` projection the handler (4.2.2b1b)
    /// will use to fill `UsageOutcome`.
    #[test]
    fn served_of_maps_tier_to_served() {
        assert_eq!(served_of(Tier::Local), Served::Local);
        assert_eq!(served_of(Tier::Lora), Served::Local);
        assert_eq!(served_of(Tier::Remote), Served::Remote);
    }

    // ---- J9: ModelAdmission — per-module cap, the fair global cap, and its exact boundary ----

    #[test]
    fn fair_global_admission() {
        let a = ModelAdmission::new(4, 2);
        let a1 = a.try_admit("A").unwrap();
        let a2 = a.try_admit("A").unwrap();
        assert!(a.try_admit("A").is_none(), "per-module cap");
        let b1 = a.try_admit("B").unwrap();
        assert!(
            a.try_admit("B").is_none(),
            "a second call may not take the last free slot"
        );
        let c1 = a.try_admit("C").unwrap();
        assert!(a.try_admit("D").is_none(), "global cap");
        drop((a1, a2, b1, c1));
        // Two modules can never hold all four.
        let x = [
            a.try_admit("A"),
            a.try_admit("A"),
            a.try_admit("B"),
            a.try_admit("B"),
        ];
        assert_eq!(x.iter().filter(|g| g.is_some()).count(), 3);
        assert!(a.try_admit("C").is_some());
    }

    /// v3 N3: the exact property and its boundary counterexample (A1, A2, B1,
    /// C1 → D's first call is `busy`).
    #[test]
    fn fairness_holds_with_two_others_and_not_with_three() {
        let a = ModelAdmission::new(4, 2);
        // Two other modules, as greedy as the rules allow → the newcomer still gets in.
        let g = [
            a.try_admit("A"),
            a.try_admit("A"),
            a.try_admit("B"),
            a.try_admit("B"),
        ];
        assert!(
            a.try_admit("C").is_some(),
            "<= 2 others active: first call admitted"
        );
        drop(g);
        // Three other modules (A twice, B, C) → D's first call is busy.
        let _g = [a.try_admit("A"), a.try_admit("A"), a.try_admit("B")];
        let _c = a.try_admit("C");
        assert!(_c.is_some());
        assert!(
            a.try_admit("D").is_none(),
            "3 others active: the property does not claim D"
        );
    }

    // ---- J17 (grant level): a module's provoked failures do not cool the ----
    // ---- kernel's own router down (v2 H2). Negative control: one shared   ----
    // ---- router DOES let a module's failure steer it.                    ----

    #[derive(Clone, Copy)]
    enum Behave {
        Ok,
        Fail500,
    }

    struct Stub {
        name: &'static str,
        calls: AtomicUsize,
        behave: Mutex<Behave>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for Stub {
        fn name(&self) -> &str {
            self.name
        }
        async fn complete(
            &self,
            _r: &CompletionRequest,
            _cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            match *self.behave.lock().unwrap() {
                Behave::Fail500 => Err(ModelError::Unavailable(format!("{} HTTP 500", self.name))),
                Behave::Ok => Ok(CompletionResponse {
                    message: Msg::assistant(Some("ok".into()), vec![]),
                    usage: Usage {
                        prompt_tokens: 3,
                        completion_tokens: 2,
                        total_tokens: 5,
                        cost_usd: 0.0,
                    },
                    model_id: Some("stub-actual-7b".into()),
                }),
            }
        }
        async fn models(&self, _c: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Ok(vec![])
        }
    }

    fn stub(name: &'static str, b: Behave) -> Arc<Stub> {
        Arc::new(Stub {
            name,
            calls: AtomicUsize::new(0),
            behave: Mutex::new(b),
        })
    }

    fn kernel_deps(router: Arc<ModelRouter>) -> ModelCallbackDeps {
        ModelCallbackDeps {
            router,
            usage: Arc::new(MemoryUsageSink::default()),
            cancel_root: CancellationToken::new(),
            admission: ModelAdmission::new(
                MODEL_MAX_IN_FLIGHT_GLOBAL,
                MODEL_MAX_IN_FLIGHT_PER_MODULE,
            ),
        }
    }

    fn req() -> CompletionRequest {
        CompletionRequest {
            messages: vec![Msg::user("hi")],
            model: None,
            tools: vec![],
            response_format: None,
            max_tokens: None,
        }
    }

    #[tokio::test]
    async fn module_failures_do_not_cool_down_the_kernels_router() {
        let a = stub("a", Behave::Fail500);
        let b = stub("b", Behave::Ok);
        let kernel = Arc::new(ModelRouter::with_defaults(vec![
            (a.clone() as Arc<dyn ModelProvider>, Tier::Local),
            (b.clone() as Arc<dyn ModelProvider>, Tier::Local),
        ]));
        let deps = kernel_deps(kernel.clone());
        let grant = ModelGrant::new("m".into(), ModelAccess::LocalOnly, deps);

        // Through the GRANT's own (separate) health table: a fails, falls through to b.
        let profile = TaskProfile {
            privacy: grant.privacy,
            complexity: Complexity::Simple,
        };
        grant
            .router
            .complete_served(profile, &req(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(a.calls.load(Ordering::SeqCst), 1);

        // `a` recovers; the KERNEL's own router (what /api/v1/chat uses) must
        // still try it next time — the module's failure did not cool it down there.
        *a.behave.lock().unwrap() = Behave::Ok;
        kernel
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            a.calls.load(Ordering::SeqCst),
            2,
            "the kernel's router must still try `a`"
        );

        // Negative control: the SAME sequence through one SHARED router — the
        // failure DOES cool `a` down, and the next call skips it.
        let a2 = stub("a", Behave::Fail500);
        let b2 = stub("b", Behave::Ok);
        let shared = ModelRouter::with_defaults(vec![
            (a2.clone() as Arc<dyn ModelProvider>, Tier::Local),
            (b2.clone() as Arc<dyn ModelProvider>, Tier::Local),
        ]);
        shared
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap();
        *a2.behave.lock().unwrap() = Behave::Ok;
        shared
            .complete(TaskProfile::default(), &req(), &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(
            a2.calls.load(Ordering::SeqCst),
            1,
            "shared health: `a` is skipped while cooling"
        );
    }

    // ---- ModelGrant::new / with_clock: privacy comes from ModelAccess, not the caller ----

    #[test]
    fn grant_privacy_comes_from_model_access() {
        let kernel = Arc::new(ModelRouter::with_defaults(vec![]));
        let deps = kernel_deps(kernel);
        let local = ModelGrant::new("m".into(), ModelAccess::LocalOnly, deps.clone());
        assert_eq!(local.privacy, Privacy::LocalOnly);
        let remote = ModelGrant::new("m".into(), ModelAccess::RemoteAllowed, deps.clone());
        assert_eq!(remote.privacy, Privacy::Any);

        struct Frozen(std::time::Instant);
        impl Clock for Frozen {
            fn now(&self) -> std::time::Instant {
                self.0
            }
        }
        let clocked = ModelGrant::with_clock(
            "m".into(),
            ModelAccess::LocalOnly,
            deps,
            Arc::new(Frozen(std::time::Instant::now())),
        );
        assert!(clocked.limiter.try_acquire());
    }

    /// §5.1: `ModelGrant` carries its module name and the daemon-level
    /// `deps` through untouched — `deps.usage`/`deps.cancel_root`/
    /// `deps.admission` are the SAME objects the caller built, not copies.
    /// The handler (4.2.2b1b) reads all three through `grant.deps`.
    #[tokio::test]
    async fn grant_carries_its_module_name_and_deps_through_untouched() {
        let sink = Arc::new(MemoryUsageSink::default());
        let admission =
            ModelAdmission::new(MODEL_MAX_IN_FLIGHT_GLOBAL, MODEL_MAX_IN_FLIGHT_PER_MODULE);
        let cancel_root = CancellationToken::new();
        let deps = ModelCallbackDeps {
            router: Arc::new(ModelRouter::with_defaults(vec![])),
            usage: sink.clone(),
            cancel_root: cancel_root.clone(),
            admission: admission.clone(),
        };
        let grant = ModelGrant::new("sin90".into(), ModelAccess::LocalOnly, deps);
        assert_eq!(grant.module, "sin90");

        // `deps.admission` is the SAME admission table (shared, not per-grant).
        assert!(Arc::ptr_eq(&grant.deps.admission, &admission));
        let g = grant.deps.admission.try_admit("sin90").unwrap();
        assert!(admission.try_admit("sin90").is_some());
        drop(g);

        // `deps.usage` reaches through to the same sink the caller holds.
        grant.deps.usage.record("sin90", UsageOutcome::Failed);
        assert_eq!(
            sink.take(),
            vec![("sin90".to_owned(), UsageOutcome::Failed)]
        );

        // `deps.cancel_root` is the same token: cancelling it externally is
        // visible through the grant (this is how `modules_cut_off()` reaches
        // an in-flight call, v2 M1 — wired up in 4.2.2b2).
        assert!(!grant.deps.cancel_root.is_cancelled());
        cancel_root.cancel();
        assert!(grant.deps.cancel_root.is_cancelled());
    }
}

/// 4.2.2b1b: the handler itself. J3, J6 (call_timeout only — see note below),
/// J7, J8, J9 (handler level), J10 (前半: single-generation rate limiting),
/// J15, J18.
#[cfg(test)]
mod handler_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use agent24_models::{CompletionResponse, ModelProvider};
    use agent24_protocol::{Model, Usage};
    use serde_json::json;

    use super::*;

    fn ok_params() -> Value {
        json!({"messages": [{"role": "user", "content": "hi"}]})
    }

    // ---- §4.2 wire validation ----

    #[test]
    fn params_shape() {
        assert!(ModelCompleteParams::parse(ok_params()).is_ok());
        // Nothing that could change privacy/model/provider exists at all.
        let mut p = ok_params();
        p["privacy"] = json!("any");
        assert!(
            ModelCompleteParams::parse(p)
                .unwrap_err()
                .contains("unknown field")
        );
        let mut p = ok_params();
        p["model"] = json!("gpt-remote");
        assert!(ModelCompleteParams::parse(p).is_err());
        // `_meta` is tolerated and never read.
        let mut p = ok_params();
        p["_meta"] = json!({"privacy": "any", "tier": "remote"});
        assert!(ModelCompleteParams::parse(p).is_ok());
        let mut p = ok_params();
        p["max_tokens"] = json!(0);
        assert!(
            ModelCompleteParams::parse(p)
                .unwrap_err()
                .contains("between 1 and 4096")
        );
        let mut p = ok_params();
        p["max_tokens"] = json!(4097);
        assert!(ModelCompleteParams::parse(p).is_err());
        let mut p = ok_params();
        p["max_tokens"] = json!(4096);
        assert!(ModelCompleteParams::parse(p).is_ok());
        assert!(ModelCompleteParams::parse(json!({"messages": []})).is_err());
        assert!(
            ModelCompleteParams::parse(json!({"messages": [{"role": "tool", "content": "x"}]}))
                .is_err()
        );
        let rf = json!({"type": "json_schema", "json_schema": {"name": "x", "schema": {"type": "object"}, "strict": true}});
        let mut p = ok_params();
        p["response_format"] = rf.clone();
        assert!(ModelCompleteParams::parse(p).is_ok());
        let mut p = ok_params();
        let mut bad = rf.clone();
        bad["extra"] = json!(1);
        p["response_format"] = bad;
        assert!(ModelCompleteParams::parse(p).is_err());
        let mut p = ok_params();
        let mut bad = rf;
        bad["json_schema"]["extra"] = json!(1);
        p["response_format"] = bad;
        assert!(ModelCompleteParams::parse(p).is_err());
        let mut p = ok_params();
        p["response_format"] = json!({"type": "json_object"});
        assert!(ModelCompleteParams::parse(p).is_err());
    }

    /// `Handler::call_timeout` reports the literal 120s (§5.2). The
    /// end-to-end proof that this OVERRIDES a real connection's 30s default
    /// (J6, `... sleeps 31s → success`) needs a real `serve()`/`Conn` on a
    /// duplex socket, which nothing in this crate's unit tests builds yet;
    /// left to the daemon-level wiring judgement (J19-style) once 4.2.2b2
    /// registers the method. `effective_call_timeout`'s own scaling
    /// (declared vs. connection-level vs. `MAX_METHOD_CALL_TIMEOUT`) is
    /// already pinned by J5 in `agent24-os-proto` (ME4-4.2.2-0).
    #[test]
    fn call_timeout_is_the_frozen_120s() {
        let h = ModelCompleteHandler {
            generation: running(),
            grant: None,
        };
        assert_eq!(h.call_timeout(), Some(MODEL_CALL_TIMEOUT));
    }

    #[derive(Clone, Copy)]
    enum Behave {
        Ok,
        Hang,
        /// L1 (Opus review round on top of `bb6fb0e`): unlike `Hang` (which
        /// never returns on its own — proving DROP-based cancellation, e.g.
        /// `$/cancelRequest`'s `handle.abort()`, reaches the provider), this
        /// mirrors the REAL `OpenAiCompatProvider::complete`'s own
        /// `tokio::select! { .., () = cancel.cancelled() => return
        /// Err(ModelError::Cancelled) }` (`agent24-models/src/lib.rs`): it
        /// resolves BY ITSELF once cancelled, with no external abort needed.
        HangUntilCancelled,
        Big(usize),
        Repeat(char, usize),
    }

    struct Stub {
        name: &'static str,
        calls: AtomicUsize,
        behave: Mutex<Behave>,
        saw_cancel: Arc<AtomicBool>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for Stub {
        fn name(&self) -> &str {
            self.name
        }

        async fn complete(
            &self,
            _r: &CompletionRequest,
            cancel: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let b = *self.behave.lock().unwrap();
            let text = match b {
                Behave::Hang => {
                    let c = cancel.clone();
                    let saw = self.saw_cancel.clone();
                    tokio::spawn(async move {
                        c.cancelled().await;
                        saw.store(true, Ordering::SeqCst);
                    });
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                Behave::HangUntilCancelled => {
                    cancel.cancelled().await;
                    self.saw_cancel.store(true, Ordering::SeqCst);
                    return Err(ModelError::Cancelled);
                }
                Behave::Ok => "ok".to_owned(),
                Behave::Big(n) => "x".repeat(n),
                Behave::Repeat(ch, n) => std::iter::repeat_n(ch, n).collect(),
            };
            Ok(CompletionResponse {
                message: Msg::assistant(Some(text), vec![]),
                usage: Usage {
                    prompt_tokens: 3,
                    completion_tokens: 2,
                    total_tokens: 5,
                    cost_usd: 0.0,
                },
                model_id: Some("stub-actual-7b".into()),
            })
        }

        async fn models(&self, _c: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Ok(vec![])
        }
    }

    fn stub(name: &'static str, b: Behave) -> Arc<Stub> {
        Arc::new(Stub {
            name,
            calls: AtomicUsize::new(0),
            behave: Mutex::new(b),
            saw_cancel: Arc::default(),
        })
    }

    fn deps(router: Arc<ModelRouter>) -> (ModelCallbackDeps, Arc<MemoryUsageSink>) {
        let sink = Arc::new(MemoryUsageSink::default());
        (
            ModelCallbackDeps {
                router,
                usage: sink.clone(),
                cancel_root: CancellationToken::new(),
                admission: ModelAdmission::new(
                    MODEL_MAX_IN_FLIGHT_GLOBAL,
                    MODEL_MAX_IN_FLIGHT_PER_MODULE,
                ),
            },
            sink,
        )
    }

    fn router(p: Vec<(Arc<Stub>, Tier)>) -> Arc<ModelRouter> {
        Arc::new(ModelRouter::with_defaults(
            p.into_iter()
                .map(|(s, t)| (s as Arc<dyn ModelProvider>, t))
                .collect(),
        ))
    }

    fn running() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/me4s2b1b-never-dialled.sock".into());
        assert!(g.ready());
        g
    }

    fn handler(grant: ModelGrant) -> Arc<ModelCompleteHandler> {
        Arc::new(ModelCompleteHandler {
            generation: running(),
            grant: Some(grant),
        })
    }

    // ---- J3: LocalOnly negative control / positive control ----

    #[tokio::test]
    async fn local_only_never_reaches_a_remote_provider_and_remote_allowed_does() {
        let remote = stub("stub-SECRET", Behave::Ok);
        let (d, sink) = deps(router(vec![(remote.clone(), Tier::Remote)]));
        let h = handler(ModelGrant::new(
            "sin90".into(),
            ModelAccess::LocalOnly,
            d.clone(),
        ));
        let e = h.call(ok_params()).await.unwrap_err();
        assert_eq!(e.kind, Some(ErrorKind::Unavailable));
        let data = e.data.clone().unwrap();
        assert_eq!(
            (data["retryable"].clone(), data["cause"].clone()),
            (json!(true), json!("no_provider"))
        );
        assert!(!e.message.contains("SECRET"));
        assert_eq!(
            remote.calls.load(Ordering::SeqCst),
            0,
            "remote stub must see ZERO requests"
        );
        // Fields that could change privacy do not exist — proven again here
        // against the SAME router, so a positive control is on record too.
        let mut p = ok_params();
        p["_meta"] = json!({"privacy": "any", "tier": "remote"});
        let e = h.call(p).await.unwrap_err();
        assert_eq!(e.kind, Some(ErrorKind::Unavailable));
        assert_eq!(remote.calls.load(Ordering::SeqCst), 0);

        let h = handler(ModelGrant::new(
            "sin90".into(),
            ModelAccess::RemoteAllowed,
            d,
        ));
        let v = h.call(ok_params()).await.unwrap();
        assert_eq!(
            (v["tier"].clone(), v["model_id"].clone()),
            (json!("remote"), json!("stub-actual-7b"))
        );
        assert_eq!(remote.calls.load(Ordering::SeqCst), 1);
        // Three records: the router itself is what refuses LocalOnly (empty
        // `tier_order`, §2.2) — a call still reaches it and is ticketed, it
        // just never reaches `remote`. So: Failed, Failed, then Ok.
        let recs = sink.take();
        assert_eq!(recs.len(), 3);
        assert!(matches!(recs[0].1, UsageOutcome::Failed));
        assert!(matches!(recs[1].1, UsageOutcome::Failed));
        assert!(matches!(recs[2].1, UsageOutcome::Ok { .. }));
    }

    // ---- J7: cancellation reaches the provider; recorded on `MemoryUsageSink` ----

    #[tokio::test]
    async fn dropping_the_call_future_cancels_the_provider_token_and_records_cancelled() {
        let local = stub("l", Behave::Hang);
        let (d, sink) = deps(router(vec![(local.clone(), Tier::Local)]));
        let h = handler(ModelGrant::new("sin90".into(), ModelAccess::LocalOnly, d));
        let r = tokio::time::timeout(Duration::from_millis(200), h.call(ok_params())).await;
        assert!(r.is_err(), "the call is still pending at 200ms");
        for _ in 0..50 {
            if local.saw_cancel.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(local.saw_cancel.load(Ordering::SeqCst));
        assert_eq!(
            sink.take(),
            vec![("sin90".to_owned(), UsageOutcome::Cancelled)]
        );
    }

    /// v2 M1 / J7(d): cancelling the ROOT (what `modules_cut_off` does in
    /// production, wired up in 4.2.2b2) reaches the provider and is recorded.
    #[tokio::test]
    async fn cancelling_the_root_reaches_the_provider_and_records_it() {
        let local = stub("l", Behave::Hang);
        let (d, sink) = deps(router(vec![(local.clone(), Tier::Local)]));
        let root = d.cancel_root.clone();
        let h = handler(ModelGrant::new("m".into(), ModelAccess::LocalOnly, d));
        let call = tokio::spawn(h.call(ok_params()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        root.cancel();
        for _ in 0..50 {
            if local.saw_cancel.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(local.saw_cancel.load(Ordering::SeqCst));
        call.abort();
        let _ = call.await;
        assert_eq!(sink.take(), vec![("m".to_owned(), UsageOutcome::Cancelled)]);
    }

    // ---- J19 (4.2.2b2, "取消部分"): `spawn_cancel_root` — the production
    // cut-off root `serve()` builds from `Shutdown::modules_cut_off()` — is
    // what `ModelCallbackDeps.cancel_root` actually is in production. The
    // test above already proves cancelling `deps.cancel_root` reaches the
    // provider; these two prove `spawn_cancel_root` itself turns "the future
    // resolved" into "the token is cancelled", and that the two compose
    // end-to-end through the real handler. Named with a `model_shutdown_wiring`
    // substring so `cargo test -p agent24d model_shutdown_wiring` (design §8
    // J19's own command) selects both. 4.2.3b upgrades the sink assertion to
    // a real `Store` row; this increment's assertion target is the
    // `MemoryUsageSink` (task scope: "J19 的取消部分，断言对象是内存 sink").

    #[tokio::test]
    async fn model_shutdown_wiring_spawn_cancel_root_only_fires_once_its_future_resolves() {
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let root = spawn_cancel_root(async move {
            let _ = rx.await;
        });
        assert!(!root.is_cancelled());
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !root.is_cancelled(),
            "must not fire before its future resolves"
        );
        tx.send(()).unwrap();
        for _ in 0..50 {
            if root.is_cancelled() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(root.is_cancelled());
    }

    /// J19: the SAME wiring `serve()` uses (`spawn_cancel_root` feeding
    /// `ModelCallbackDeps.cancel_root`) reaches a real in-flight call's
    /// provider and lands a `Cancelled` outcome — proving "the daemon cuts
    /// off in-flight model calls at `modules_cut_off()`" end to end, not just
    /// piece by piece.
    #[tokio::test]
    async fn model_shutdown_wiring_cuts_off_an_in_flight_call_and_records_cancelled() {
        let local = stub("l", Behave::HangUntilCancelled);
        let (mut d, sink) = deps(router(vec![(local.clone(), Tier::Local)]));
        let (cut_off_tx, cut_off_rx) = tokio::sync::oneshot::channel::<()>();
        d.cancel_root = spawn_cancel_root(async move {
            let _ = cut_off_rx.await;
        });
        let h = handler(ModelGrant::new("m".into(), ModelAccess::LocalOnly, d));
        let call = tokio::spawn(h.call(ok_params()));
        for _ in 0..50 {
            if local.calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            local.calls.load(Ordering::SeqCst),
            1,
            "the call must have reached the provider before cut-off"
        );
        cut_off_tx
            .send(())
            .expect("spawn_cancel_root's task must still be waiting on this");
        for _ in 0..50 {
            if local.saw_cancel.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            local.saw_cancel.load(Ordering::SeqCst),
            "the provider must observe the cancellation once modules_cut_off resolves"
        );
        // L1 (Opus review round on top of `bb6fb0e`): `call.abort()` here
        // would make the `Cancelled` outcome near-tautological — the task
        // gets torn down by the abort regardless of what the cancel root
        // did. Instead, let the call's own future resolve on its own terms
        // (bounded, so a regression hangs the test rather than passing it)
        // and assert what it actually returned.
        let outcome = tokio::time::timeout(Duration::from_secs(2), call)
            .await
            .expect("the call must resolve on its own once the cancel root fires")
            .expect("the spawned task must not panic");
        let err = outcome.expect_err("a cut-off call must not succeed");
        assert_eq!(err.kind, Some(ErrorKind::Cancelled), "{err:?}");
        assert_eq!(sink.take(), vec![("m".to_owned(), UsageOutcome::Cancelled)]);
    }

    // ---- J8: lifecycle binding ----

    #[tokio::test]
    async fn an_unknown_request_id_is_refused_not_run_unbound() {
        let local = stub("l", Behave::Ok);
        let (d, _s) = deps(router(vec![(local.clone(), Tier::Local)]));
        let h = handler(ModelGrant::new("m".into(), ModelAccess::LocalOnly, d));
        let mut p = ok_params();
        p["request_id"] = json!("req_not_in_flight");
        let e = h.call(p).await.unwrap_err();
        assert_eq!(e.kind, Some(ErrorKind::Timeout));
        assert_eq!(e.data.unwrap()["retryable"], false);
        assert_eq!(
            local.calls.load(Ordering::SeqCst),
            0,
            "never reached the router"
        );
        // Positive control: no `request_id` at all runs fine.
        assert!(h.call(ok_params()).await.is_ok());
    }

    #[tokio::test]
    async fn a_just_finished_request_id_is_refused_the_same_way() {
        let local = stub("l", Behave::Ok);
        let (d, _s) = deps(router(vec![(local.clone(), Tier::Local)]));
        let generation = running();
        let in_flight = generation
            .admit_request(
                "req_already_finished".into(),
                [0u8; 32],
                std::time::Instant::now(),
                Duration::from_secs(30),
            )
            .unwrap();
        in_flight.finish().unwrap();
        let h = Arc::new(ModelCompleteHandler {
            generation,
            grant: Some(ModelGrant::new("m".into(), ModelAccess::LocalOnly, d)),
        });
        let mut p = ok_params();
        p["request_id"] = json!("req_already_finished");
        let e = h.call(p).await.unwrap_err();
        assert_eq!(e.kind, Some(ErrorKind::Timeout));
        assert_eq!(e.data.unwrap()["retryable"], false);
        assert_eq!(local.calls.load(Ordering::SeqCst), 0);
    }

    // ---- J9 (handler level): busy spends no token ----

    #[tokio::test]
    async fn busy_spends_no_token() {
        struct Frozen(std::time::Instant);
        impl Clock for Frozen {
            fn now(&self) -> std::time::Instant {
                self.0
            }
        }
        let local = stub("l", Behave::Hang);
        let (mut d, _s) = deps(router(vec![(local.clone(), Tier::Local)]));
        d.admission = ModelAdmission::new(8, 2);
        let clock = Arc::new(Frozen(std::time::Instant::now()));
        let mut grant = ModelGrant::with_clock("m".into(), ModelAccess::LocalOnly, d, clock);
        grant.limiter = Arc::new(RateLimiter::with_clock(
            3.0,
            0.0,
            Arc::new(Frozen(std::time::Instant::now())),
        ));
        let h = handler(grant);
        let c1 = tokio::spawn(h.call(ok_params()));
        let c2 = tokio::spawn(h.call(ok_params()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            h.call(ok_params()).await.unwrap_err().kind,
            Some(ErrorKind::Busy),
            "the module's own per-module cap (2) is already at 2 in-flight"
        );
        c1.abort();
        let _ = c1.await;
        *local.behave.lock().unwrap() = Behave::Ok;
        assert!(
            h.call(ok_params()).await.is_ok(),
            "the busy call above must not have spent the 3rd (and last) token"
        );
        c2.abort();
    }

    // ---- J10 (前半): the token bucket itself, and its refill ----

    #[tokio::test]
    async fn the_31st_call_is_rate_limited_and_refill_lets_the_32nd_through() {
        struct Movable(Mutex<std::time::Instant>);
        impl Clock for Movable {
            fn now(&self) -> std::time::Instant {
                *self.0.lock().unwrap()
            }
        }
        let local = stub("l", Behave::Ok);
        let (d, _s) = deps(router(vec![(local.clone(), Tier::Local)]));
        let clock = Arc::new(Movable(Mutex::new(std::time::Instant::now())));
        let grant = ModelGrant::with_clock("m".into(), ModelAccess::LocalOnly, d, clock.clone());
        let h = handler(grant);
        for i in 0..30 {
            assert!(
                h.call(ok_params()).await.is_ok(),
                "call {i} of 30 (capacity)"
            );
        }
        assert_eq!(
            h.call(ok_params()).await.unwrap_err().kind,
            Some(ErrorKind::RateLimited),
            "the 31st call exceeds the burst capacity"
        );
        // MODEL_RATE_REFILL_PER_SEC = 0.5: +2s refills exactly one token.
        *clock.0.lock().unwrap() += Duration::from_secs(2);
        assert!(
            h.call(ok_params()).await.is_ok(),
            "one token refilled after 2s"
        );
    }

    // J15 (the LocalOnly tripwire, `grant.privacy == LocalOnly &&
    // !served.tier.is_local()` in `call()`) cannot be exercised by a normal
    // test: a real LocalOnly `tier_order` never yields `Remote` (that
    // guarantee is §2.3/J16, in `agent24-models`), and J3 above already
    // pins the routing-level behaviour this tripwire backs up. J15 is a
    // `docs/agent/mutate.sh` judgement — widen `tier_order` for
    // `Privacy::LocalOnly` to include `Tier::Remote` and confirm J3's
    // "remote stub must see ZERO requests" assertion turns red AND the
    // module gets `-32603` — not a standing test in this file.

    // ---- J18: result size, judged after serialization ----

    async fn size_case(b: Behave) -> (Result<Value, RpcError>, Vec<(String, UsageOutcome)>) {
        let (d, sink) = deps(router(vec![(stub("l", b), Tier::Local)]));
        let r = handler(ModelGrant::new("m".into(), ModelAccess::LocalOnly, d))
            .call(ok_params())
            .await;
        (r, sink.take())
    }

    #[tokio::test]
    async fn result_size_is_judged_after_serialization() {
        // 2 MiB of plain text → too large, recorded as FailedAfterServe with tokens.
        let (r, recs) = size_case(Behave::Big(2 * 1024 * 1024)).await;
        let e = r.unwrap_err();
        assert_eq!(
            (e.kind, e.data.unwrap()["cause"].clone()),
            (Some(ErrorKind::Unavailable), json!("response_too_large"))
        );
        assert!(matches!(
            recs[0].1,
            UsageOutcome::FailedAfterServe {
                prompt_tokens: 3,
                ..
            }
        ));

        // 400 KiB of `"` → 800 KiB serialized: fits, succeeds (positive control).
        let (r, recs) = size_case(Behave::Repeat('"', 400 * 1024)).await;
        assert!(r.is_ok());
        assert!(matches!(recs[0].1, UsageOutcome::Ok { .. }));

        // 200 KiB of U+0001 → 1.2 MiB serialized: a raw-length check (v2)
        // would have passed it and the frame limit would have turned it into
        // -32603 instead.
        let (r, recs) = size_case(Behave::Repeat('\u{1}', 200 * 1024)).await;
        assert_eq!(r.unwrap_err().data.unwrap()["cause"], "response_too_large");
        assert!(matches!(recs[0].1, UsageOutcome::FailedAfterServe { .. }));

        // The boundary itself, measured on the real serializer.
        let overhead = serde_json::to_vec(
            &json!({"text":"","model_id":"stub-actual-7b","tier":"local",
            "usage":{"prompt_tokens":3,"completion_tokens":2}}),
        )
        .unwrap()
        .len();
        let (r, _) = size_case(Behave::Big(MODEL_MAX_RESULT_BYTES - overhead)).await;
        assert!(r.is_ok(), "exactly at the limit passes");
        let (r, _) = size_case(Behave::Big(MODEL_MAX_RESULT_BYTES - overhead + 1)).await;
        assert!(r.is_err(), "one byte over fails");
    }

    #[tokio::test]
    async fn an_overlong_model_id_is_dropped_not_truncated() {
        struct LongId;
        #[async_trait::async_trait]
        impl ModelProvider for LongId {
            fn name(&self) -> &str {
                "l"
            }
            async fn complete(
                &self,
                _r: &CompletionRequest,
                _c: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                Ok(CompletionResponse {
                    message: Msg::assistant(Some("x".into()), vec![]),
                    usage: Usage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                        total_tokens: 2,
                        cost_usd: 0.0,
                    },
                    model_id: Some("m".repeat(MODEL_MAX_MODEL_ID_BYTES + 1)),
                })
            }
            async fn models(&self, _c: &CancellationToken) -> Result<Vec<Model>, ModelError> {
                Ok(vec![])
            }
        }
        let r = Arc::new(ModelRouter::with_defaults(vec![(
            Arc::new(LongId) as Arc<dyn ModelProvider>,
            Tier::Local,
        )]));
        let (d, _s) = deps(r);
        let v = handler(ModelGrant::new("m".into(), ModelAccess::LocalOnly, d))
            .call(ok_params())
            .await
            .unwrap();
        assert!(v["model_id"].is_null());
    }

    // ---- forbidden ----

    #[tokio::test]
    async fn no_grant_is_forbidden() {
        let h = ModelCompleteHandler {
            generation: running(),
            grant: None,
        };
        assert_eq!(
            h.call(ok_params()).await.unwrap_err().kind,
            Some(ErrorKind::Forbidden)
        );
    }
}
