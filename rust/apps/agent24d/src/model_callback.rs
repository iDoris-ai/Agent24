//! `_a24/model/complete` (ME4-S2 v3.1, §5–§7). **4.2.2b1a**: constants,
//! admission (§5), the usage sink (§6.3), grants/deps (§5.1), and error
//! mapping (§7) — the parts of the callback that do not need the wire types
//! or the handler itself. **Not** in this file (see the split in
//! `docs/design/ME4-S2-model-callback.md` §10.2): wire types and
//! `ModelCompleteHandler` (4.2.2b1b), registration/`serve()` wiring
//! (4.2.2b2), and persisted usage (4.2.3).
//!
//! v3 N7: until 4.2.2b2 registers `_a24/model/complete`, most of this is
//! unreachable from a binary crate's point of view — `allow(dead_code)`
//! outside test builds; **removed by 4.2.2b2** together with the structural
//! test in `domain.rs` that pins "not registered yet" (J2).
#![cfg_attr(not(test), allow(dead_code))]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use agent24_domain::ModelAccess;
use agent24_models::ModelError;
use agent24_models::router::{ModelRouter, Privacy, Tier};
use agent24_os_proto::rpc::{ErrorKind, RpcError};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::events_emit::{Clock, RateLimiter};

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

    /// v2 L6: the same, with an injected clock for the bucket (tests).
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
