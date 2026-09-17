//! `_a24/events/emit` — the first real callback method an out-of-process
//! module's connection serves (T7a / ME-3e). See
//! `docs/design/T7a-ME3e-grants-and-events.md`.
//!
//! Registered unconditionally on every out-of-process connection
//! (`crate::domain`'s `MethodsFor`); capability gating happens INSIDE
//! [`EventsEmitHandler::call`], not by conditionally registering the method —
//! that is what lets `forbidden` (a handler exists, this call isn't allowed)
//! stay distinct from `-32601` (no such method at all).

use std::sync::{Arc, Mutex};
use std::time::Instant;

use agent24_domain::{Capability, EventSink, Grants};
use agent24_os_proto::drain::{CallbackRefused, Generation};
use agent24_os_proto::rpc::{
    CallFuture, ErrorKind, Handler, ParamsBudget, RpcError, walk_params_budget,
};
use serde::Deserialize;
use serde_json::{Map, Value};

/// Token-bucket capacity for `_a24/events/emit`, per `Generation` (design §5).
pub(crate) const EVENTS_RATE_CAPACITY: f64 = 20.0;
/// Refill rate, tokens per second (design §5).
pub(crate) const EVENTS_RATE_REFILL_PER_SEC: f64 = 5.0;
/// Events-specific resident-cost cap on `payload` (design §5, revised after
/// code review round 1 High 1): bounding *serialized* bytes does not bound
/// what stays resident in the shared `EventsHub` ring, because the ring holds
/// the parsed `Map<String, Value>`, not the wire bytes. A payload shaped like
/// `{"a":[0,0,…,0]}` (~5,000 numbers) serializes to a few KiB but costs
/// ~150 KiB of `Value` nodes at rpc.rs's own ~32-bytes-per-node estimate — the
/// old byte-only check let it straight through. This caps the same
/// `(nodes, string_bytes)` shape `dispatch()`'s generic budget already
/// computes (`agent24_os_proto::rpc::ParamsBudget`), just tighter and
/// events-specific: 256 nodes × ~32 B ≈ 8 KiB of node overhead, plus up to
/// 8 KiB of string content, ≈ 16 KiB worst case per event — the same
/// per-event figure the design doc's `4096 × 16 KiB ≈ 64 MiB` ring bound
/// assumes, now actually measuring the thing that occupies that memory.
const EVENT_PAYLOAD_MAX_NODES: usize = 256;
/// See [`EVENT_PAYLOAD_MAX_NODES`].
const EVENT_PAYLOAD_MAX_STRING_BYTES: usize = 8 * 1024;

/// `payload`'s own budget: the object itself is one node (matching
/// `ParamsBudget`'s definition for `Value::Object`), each key's bytes count,
/// and each value is walked from depth 1. Takes `&Map` rather than wrapping it
/// in a `Value::Object` so the caller doesn't have to clone `payload` just to
/// measure it before consuming it in `sink.emit`.
fn payload_budget(payload: &Map<String, Value>) -> ParamsBudget {
    let mut budget = ParamsBudget {
        nodes: 1,
        ..ParamsBudget::default()
    };
    for (key, val) in payload {
        budget.string_bytes += key.len();
        walk_params_budget(val, 1, &mut budget);
    }
    budget
}

/// A monotonic clock, injectable so a test can freeze and advance time
/// instead of sleeping (design doc §5, Codex round 4 Medium 3: real `sleep`
/// makes the rate-limiter tests flaky). Precedent: `agent24_scheduler::Clock`
/// does the same for wall-clock time; this one is `Instant`-based because the
/// bucket's math must never observe time running backwards (a wall clock can
/// be adjusted; a monotonic clock cannot).
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The production clock.
#[derive(Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

struct Bucket {
    tokens: f64,
    last: Instant,
}

/// A per-`Generation` token bucket (design §5): capacity and refill rate are
/// fixed at construction, refill is computed lazily on each call, and the
/// check-and-decrement happens as ONE step under a single lock — never
/// check-then-decrement as two, which would let concurrent callers overdraw
/// the bucket (Codex round 4 Medium 3).
pub struct RateLimiter {
    capacity: f64,
    refill_per_sec: f64,
    clock: Arc<dyn Clock>,
    bucket: Mutex<Bucket>,
}

impl RateLimiter {
    /// The production limiter: a real, system clock.
    #[must_use]
    pub fn new(capacity: f64, refill_per_sec: f64) -> Self {
        Self::with_clock(capacity, refill_per_sec, Arc::new(SystemClock))
    }

    /// For tests: an injected clock, so the bucket's refill can be driven
    /// deterministically instead of by real elapsed time.
    #[must_use]
    pub fn with_clock(capacity: f64, refill_per_sec: f64, clock: Arc<dyn Clock>) -> Self {
        let last = clock.now();
        Self {
            capacity,
            refill_per_sec,
            clock,
            bucket: Mutex::new(Bucket {
                tokens: capacity,
                last,
            }),
        }
    }

    /// Refill lazily (`min(capacity, tokens + elapsed * refill_per_sec)`),
    /// then try to spend exactly one token, all under one lock. `true` if a
    /// token was spent, `false` if the bucket was empty.
    pub fn try_acquire(&self) -> bool {
        let mut bucket = self
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = self.clock.now();
        let elapsed = now.saturating_duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Wire params for `_a24/events/emit`. `deny_unknown_fields`: an unknown
/// field (including a self-reported `module`) is rejected outright rather
/// than silently ignored (Codex round 2 Medium 3) — there is deliberately no
/// field through which a module could claim to be a different module; the
/// event's `module` comes only from the manifest name closed over by the
/// handler.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsEmitParams {
    kind: String,
    payload: Map<String, Value>,
    #[serde(default)]
    request_id: Option<String>,
}

/// `_a24/events/emit`'s handler. One is built fresh every time this
/// generation's `MethodsFor` closure runs (`crate::domain`), so `generation`
/// and `limiter` are always this run's — never a previous or future one's.
pub struct EventsEmitHandler {
    pub generation: Arc<Generation>,
    pub name: String,
    pub granted: Grants,
    pub sink: Option<Arc<EventSink>>,
    pub limiter: Arc<RateLimiter>,
}

fn refused_error(refused: CallbackRefused) -> RpcError {
    match refused {
        CallbackRefused::NotReady => RpcError::application(
            ErrorKind::NotReady,
            "the module has not finished its handshake yet",
        ),
        CallbackRefused::DrainingWithoutRequest | CallbackRefused::DrainingUnknownRequest => {
            RpcError::application(
                ErrorKind::Draining,
                "the module is draining; only a callback tied to a still-in-flight \
                 request is admitted",
            )
        }
        CallbackRefused::Revoked => {
            RpcError::application(ErrorKind::Revoked, "this generation has been revoked")
        }
    }
}

impl Handler for EventsEmitHandler {
    fn check_params(&self, params: &Value) -> Result<(), String> {
        serde_json::from_value::<EventsEmitParams>(params.clone())
            .map(|_| ())
            .map_err(|e| e.to_string())
    }

    fn call(&self, params: Value) -> CallFuture {
        // `check_params` (run by `dispatch()` before `call()` is ever reached)
        // already proved `params` deserializes; a failure here would be a
        // kernel bug, not a caller error, so it is `internal` rather than
        // re-litigated as `invalid_params`.
        let parsed = serde_json::from_value::<EventsEmitParams>(params);
        let granted = self.granted.clone();
        let generation = self.generation.clone();
        let limiter = self.limiter.clone();
        let sink = self.sink.clone();
        let module = self.name.clone();
        Box::pin(async move {
            let parsed = parsed.map_err(|e| {
                RpcError::internal(format!(
                    "params for module {module:?} were valid at check_params but not at call(): {e}"
                ))
            })?;

            // 1. Capability: no grant, no quota charged, no event recorded.
            if !granted.has(Capability::Events) {
                return Err(RpcError::application(
                    ErrorKind::Forbidden,
                    "the `events` capability was not granted to this module",
                ));
            }

            // 2. Lifecycle admission: reuses `Generation::admit_callback` as-is
            // (ME-3b-5). A rejection here does not touch the rate limiter
            // either — a draining module with a bad/missing `request_id`
            // must not be able to spend a legitimate call's quota.
            if let Err(refused) = generation.admit_callback(parsed.request_id.as_deref()) {
                return Err(refused_error(refused));
            }

            // 3. Rate limit: only a call that passed 1-2 is charged.
            if !limiter.try_acquire() {
                return Err(RpcError::application(
                    ErrorKind::RateLimited,
                    "`_a24/events/emit` rate limit exceeded for this module",
                ));
            }

            // Sink existence mirrors `granted` by construction (`crate::domain`
            // builds it iff `granted.has(Capability::Events)`) — `None` here
            // would be a kernel wiring bug, not a caller error.
            let Some(sink) = sink.as_ref() else {
                return Err(RpcError::internal(
                    "the `events` capability was granted with no event sink (kernel bug)",
                ));
            };

            // 4. Events-specific resident-cost cap (design §5) — distinct
            // from, and tighter than, `dispatch()`'s generic budget: bounds
            // the shared `EventsHub`'s worst-case resident footprint by
            // measuring the same (nodes, string_bytes) shape that budget
            // uses, not serialized wire bytes (code review round 1 High 1 —
            // serialized size does not bound resident `Value` tree cost).
            // Checked BEFORE `EventSink::emit`, whose own validation (kind
            // length/syntax) is `-32602`, a different failure class from
            // `payload_too_large`.
            let budget = payload_budget(&parsed.payload);
            if budget.nodes > EVENT_PAYLOAD_MAX_NODES
                || budget.string_bytes > EVENT_PAYLOAD_MAX_STRING_BYTES
            {
                return Err(RpcError::application(
                    ErrorKind::PayloadTooLarge,
                    format!(
                        "event payload has {} nodes / {} string bytes, over the \
                         {EVENT_PAYLOAD_MAX_NODES}-node / {EVENT_PAYLOAD_MAX_STRING_BYTES}-byte \
                         limit for `_a24/events/emit`",
                        budget.nodes, budget.string_bytes
                    ),
                ));
            }

            // 5. `EventSink::emit`'s own validation (kind length/syntax,
            // enforced identically for in-process and out-of-process
            // modules) — a failure here is `invalid_params`, not a
            // capability or lifecycle failure.
            sink.emit(&parsed.kind, parsed.payload)
                .map_err(|e| RpcError::invalid_params(e.to_string()))?;

            Ok(Value::Object(Map::new()))
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_domain::DomainOsManifest;
    use agent24_os_proto::rpc::{Methods, code};
    use serde_json::json;
    use std::sync::Mutex as StdMutex;
    use std::time::Duration;

    /// A minimal out-of-process manifest — this module never spawns anything
    /// for these tests, `EventSink::new` only reads the name back out of it.
    fn manifest(name: &str) -> DomainOsManifest {
        DomainOsManifest::from_yaml(&format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: python3\n  args: []\n"
        ))
        .unwrap()
    }

    #[derive(Default, Clone)]
    struct RecordingBroadcast {
        sent: Arc<StdMutex<Vec<agent24_protocol::EventBody>>>,
    }
    impl agent24_domain::EventBroadcast for RecordingBroadcast {
        fn send(&self, body: agent24_protocol::EventBody) {
            self.sent.lock().unwrap().push(body);
        }
    }

    fn events(name: &str) -> (RecordingBroadcast, Arc<EventSink>) {
        let bus = RecordingBroadcast::default();
        let sink = Arc::new(EventSink::new(
            &manifest(name),
            Arc::new(bus.clone()) as Arc<dyn agent24_domain::EventBroadcast>,
        ));
        (bus, sink)
    }

    fn module_events(bus: &RecordingBroadcast) -> Vec<agent24_protocol::ModuleEventPayload> {
        bus.sent
            .lock()
            .unwrap()
            .iter()
            .filter_map(|b| match b {
                agent24_protocol::EventBody::Module(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    fn granted(has_events: bool) -> Grants {
        if has_events {
            Grants::granting(&[Capability::Events], &[Capability::Events])
        } else {
            Grants::granting(&[], &[Capability::Events])
        }
    }

    fn running_generation() -> Arc<Generation> {
        let g = Generation::serving_at("/tmp/does-not-need-to-exist".into());
        assert!(g.ready(), "a freshly-serving generation must become Ready");
        g
    }

    /// Generous enough that no test not specifically about rate limiting can
    /// ever trip it.
    fn generous_limiter() -> Arc<RateLimiter> {
        Arc::new(RateLimiter::new(1_000_000.0, 1_000_000.0))
    }

    fn good_params() -> Value {
        json!({"kind": "task.transitioned", "payload": {"x": 1}})
    }

    fn handler(
        generation: Arc<Generation>,
        has_events: bool,
        sink: Option<Arc<EventSink>>,
        limiter: Arc<RateLimiter>,
    ) -> EventsEmitHandler {
        EventsEmitHandler {
            generation,
            name: "probe".to_owned(),
            granted: granted(has_events),
            sink,
            limiter,
        }
    }

    // ── judgement 1: capability gating ────────────────────────────────

    #[tokio::test]
    async fn ungranted_module_is_forbidden_and_emits_nothing() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), false, Some(sink), generous_limiter());
        let err = h.call(good_params()).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Forbidden));
        assert!(module_events(&bus).is_empty());
    }

    #[tokio::test]
    async fn granted_module_can_call_and_the_event_lands() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let result = h.call(good_params()).await.unwrap();
        // judgement 15: the success result is EXACTLY `{}`.
        assert_eq!(result, json!({}));
        let sent = module_events(&bus);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].module, "probe");
        assert_eq!(sent[0].kind, "task.transitioned");
    }

    // ── judgement 2: unknown fields (deny_unknown_fields) ─────────────

    #[test]
    fn an_unknown_field_is_rejected_at_check_params() {
        let (_, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let params = json!({"kind": "task.transitioned", "payload": {}, "module": "sneaky"});
        let err = h.check_params(&params).expect_err("module is not a field");
        assert!(err.contains("module") || err.contains("unknown"), "{err}");
    }

    #[test]
    fn a_legitimate_call_passes_check_params() {
        let (_, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        h.check_params(&good_params()).expect("this shape is legal");
        h.check_params(&json!({"kind": "task.transitioned", "payload": {}}))
            .expect("request_id is optional");
    }

    // ── judgement 3: attribution comes from the closure, never from params ──

    #[tokio::test]
    async fn a_payload_key_named_module_does_not_override_the_envelope() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let params = json!({
            "kind": "task.transitioned",
            "payload": {"module": "someone-else", "x": 1},
        });
        h.call(params).await.unwrap();
        let sent = module_events(&bus);
        assert_eq!(
            sent[0].module, "probe",
            "the envelope must come from the closure"
        );
        assert_eq!(
            sent[0].payload.get("module").and_then(Value::as_str),
            Some("someone-else"),
            "the payload's own `module` key passes through untouched"
        );
    }

    // ── judgement 4/5: EventSink::emit's own validation, reused as-is ──

    #[tokio::test]
    async fn a_malformed_kind_is_invalid_params_not_forbidden_or_lifecycle() {
        let (_, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let err = h
            .call(json!({"kind": "not-dotted", "payload": {}}))
            .await
            .unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        assert!(err.kind.is_none(), "invalid_params carries no `kind`");
    }

    #[tokio::test]
    async fn a_non_object_payload_is_rejected_at_deserialization_not_at_emit() {
        let (_, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let params = json!({"kind": "task.transitioned", "payload": "not an object"});
        h.check_params(&params)
            .expect_err("payload must deserialize as a Map, not a Value");
    }

    // ── judgement 6/7/8: lifecycle admission (reuses `admit_callback`) ──

    #[tokio::test]
    async fn starting_is_not_ready_regardless_of_request_id() {
        let (_, sink) = events("probe");
        let starting = Generation::starting();
        for params in [
            json!({"kind": "task.transitioned", "payload": {}}),
            json!({"kind": "task.transitioned", "payload": {}, "request_id": "r1"}),
        ] {
            let h = handler(
                starting.clone(),
                true,
                Some(sink.clone()),
                generous_limiter(),
            );
            let err = h.call(params).await.unwrap_err();
            assert_eq!(err.kind, Some(ErrorKind::NotReady));
        }
    }

    #[tokio::test]
    async fn running_without_a_request_id_is_a_background_event() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        h.call(good_params()).await.unwrap();
        assert_eq!(module_events(&bus).len(), 1);
    }

    #[tokio::test]
    async fn draining_without_a_request_id_is_refused_as_draining() {
        let (_, sink) = events("probe");
        let g = running_generation();
        assert!(g.begin_drain(Instant::now(), Duration::from_secs(30)));
        let h = handler(g, true, Some(sink), generous_limiter());
        let err = h.call(good_params()).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Draining));
    }

    #[tokio::test]
    async fn draining_with_an_unknown_request_id_is_refused_as_draining() {
        let (_, sink) = events("probe");
        let g = running_generation();
        assert!(g.begin_drain(Instant::now(), Duration::from_secs(30)));
        let h = handler(g, true, Some(sink), generous_limiter());
        let params = json!({"kind": "task.transitioned", "payload": {}, "request_id": "ghost"});
        let err = h.call(params).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::Draining));
    }

    #[tokio::test]
    async fn draining_with_a_still_in_flight_request_id_still_lands() {
        let (bus, sink) = events("probe");
        let g = running_generation();
        // Admitted while Running, and kept alive (not dropped) so the id stays
        // in `in_flight` through the drain — this is exactly the "still-live
        // request" case `admit_callback` is meant to allow through.
        let in_flight = g.admit_request("req-1".to_owned(), [0u8; 32]).unwrap();
        assert!(g.begin_drain(Instant::now(), Duration::from_secs(30)));
        let h = handler(g, true, Some(sink), generous_limiter());
        let params = json!({"kind": "task.transitioned", "payload": {}, "request_id": "req-1"});
        let result = h.call(params).await.unwrap();
        assert_eq!(result, json!({}));
        assert_eq!(module_events(&bus).len(), 1);
        drop(in_flight);
    }

    /// `CallbackRefused`'s mapping to wire kinds, pinned directly — including
    /// `Revoked`, which this crate has no public way to reach on a real
    /// `Generation` (`Generation::revoke` is `pub(crate)` to
    /// `agent24-os-proto`); `agent24-os-proto::drain`'s own tests cover the
    /// state machine that produces `CallbackRefused::Revoked` in the first
    /// place (design doc: "不改的东西" — the state machine is reused as-is).
    #[test]
    fn callback_refused_maps_to_the_designed_wire_kinds() {
        assert_eq!(
            refused_error(CallbackRefused::NotReady).kind,
            Some(ErrorKind::NotReady)
        );
        assert_eq!(
            refused_error(CallbackRefused::DrainingWithoutRequest).kind,
            Some(ErrorKind::Draining)
        );
        assert_eq!(
            refused_error(CallbackRefused::DrainingUnknownRequest).kind,
            Some(ErrorKind::Draining)
        );
        assert_eq!(
            refused_error(CallbackRefused::Revoked).kind,
            Some(ErrorKind::Revoked)
        );
    }

    // ── judgement 13: the events-specific resident-cost cap ───────────

    #[tokio::test]
    async fn a_payload_with_a_huge_string_is_payload_too_large_and_not_emitted() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let big = "x".repeat(20_000); // well under the 256 KiB generic cap
        let params = json!({"kind": "task.transitioned", "payload": {"blob": big}});
        let err = h.call(params).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::PayloadTooLarge));
        assert!(module_events(&bus).is_empty());
    }

    /// Code review round 1 High 1: a serialized-bytes-only cap lets this
    /// through — `{"a":[0,0,…]}` with ~5,000 short numbers serializes to a
    /// few KiB, but costs thousands of resident `Value` nodes. The cap must
    /// be measured on `(nodes, string_bytes)`, not wire bytes, to catch it.
    #[tokio::test]
    async fn a_payload_with_many_short_numbers_is_payload_too_large_despite_tiny_wire_size() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let many_numbers: Vec<Value> = (0..1_000).map(|_| json!(0)).collect();
        let payload = json!({"a": many_numbers});
        // Confirm the premise: this is tiny on the wire.
        assert!(serde_json::to_vec(&payload).unwrap().len() < 16 * 1024);
        let params = json!({"kind": "task.transitioned", "payload": payload});
        let err = h.call(params).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::PayloadTooLarge));
        assert!(module_events(&bus).is_empty());
    }

    #[tokio::test]
    async fn a_small_payload_lands_normally() {
        let (bus, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), generous_limiter());
        let small = "x".repeat(1_000);
        let params = json!({"kind": "task.transitioned", "payload": {"blob": small}});
        h.call(params).await.unwrap();
        assert_eq!(module_events(&bus).len(), 1);
    }

    // ── judgement 14: the per-generation token bucket ─────────────────

    /// A clock a test can freeze and advance instead of sleeping.
    #[derive(Clone)]
    struct TestClock(Arc<StdMutex<Instant>>);
    impl TestClock {
        fn frozen_at(now: Instant) -> Self {
            Self(Arc::new(StdMutex::new(now)))
        }
        fn advance(&self, by: Duration) {
            let mut t = self.0.lock().unwrap();
            *t += by;
        }
    }
    impl Clock for TestClock {
        fn now(&self) -> Instant {
            *self.0.lock().unwrap()
        }
    }

    #[test]
    fn a_fresh_limiter_never_inherits_another_ones_exhaustion() {
        let clock = Arc::new(TestClock::frozen_at(Instant::now()));
        let a = RateLimiter::with_clock(1.0, 5.0, clock.clone());
        assert!(a.try_acquire());
        assert!(!a.try_acquire(), "a's one token is spent");
        // A brand new limiter — as `MethodsFor` builds on every restart — is
        // full regardless of what `a` did (Codex round 3 Medium 2).
        let b = RateLimiter::with_clock(1.0, 5.0, clock);
        assert!(b.try_acquire());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 8)]
    async fn sixty_four_concurrent_calls_yield_exactly_twenty_successes() {
        let clock = Arc::new(TestClock::frozen_at(Instant::now()));
        let limiter = Arc::new(RateLimiter::with_clock(
            EVENTS_RATE_CAPACITY,
            EVENTS_RATE_REFILL_PER_SEC,
            clock.clone(),
        ));
        let (bus, sink) = events("probe");
        let handler = Arc::new(handler(
            running_generation(),
            true,
            Some(sink),
            limiter.clone(),
        ));
        let mut tasks = Vec::new();
        for _ in 0..64 {
            let h = handler.clone();
            tasks.push(tokio::spawn(async move { h.call(good_params()).await }));
        }
        let mut ok = 0;
        let mut limited = 0;
        for t in tasks {
            match t.await.unwrap() {
                Ok(_) => ok += 1,
                Err(e) => {
                    assert_eq!(e.kind, Some(ErrorKind::RateLimited));
                    limited += 1;
                }
            }
        }
        assert_eq!(ok, 20, "exactly capacity, not more, not fewer");
        assert_eq!(limited, 44);
        assert_eq!(module_events(&bus).len(), 20);

        // The clock never moved during the burst: a frozen clock refills
        // nothing, which is what makes the count above deterministic rather
        // than a race against real time.
        assert!(handler.call(good_params()).await.is_err());

        // Advance by exactly one refill period (200ms at 5/sec) — exactly one
        // more token becomes available, not more.
        clock.advance(Duration::from_millis(200));
        handler
            .call(good_params())
            .await
            .expect("one token refilled");
        let err = handler.call(good_params()).await.unwrap_err();
        assert_eq!(err.kind, Some(ErrorKind::RateLimited));
    }

    #[tokio::test]
    async fn forbidden_and_lifecycle_rejections_do_not_spend_a_token() {
        let clock = Arc::new(TestClock::frozen_at(Instant::now()));
        let limiter = Arc::new(RateLimiter::with_clock(1.0, 5.0, clock));
        let (_, sink) = events("probe");

        // Neither an ungranted module...
        let forbidden = handler(
            running_generation(),
            false,
            Some(sink.clone()),
            limiter.clone(),
        );
        for _ in 0..5 {
            assert_eq!(
                forbidden.call(good_params()).await.unwrap_err().kind,
                Some(ErrorKind::Forbidden)
            );
        }
        // ...nor a not-yet-ready generation...
        let not_ready = handler(
            Generation::starting(),
            true,
            Some(sink.clone()),
            limiter.clone(),
        );
        for _ in 0..5 {
            assert_eq!(
                not_ready.call(good_params()).await.unwrap_err().kind,
                Some(ErrorKind::NotReady)
            );
        }
        // ...ever touches the shared limiter: it still has its one token.
        let ok = handler(running_generation(), true, Some(sink), limiter);
        ok.call(good_params())
            .await
            .expect("the token is untouched");
    }

    #[tokio::test]
    async fn a_validation_failure_at_emit_still_spends_a_token() {
        // Deliberate asymmetry (design doc §5): letting a bad `kind` be free
        // would let a caller probe arbitrary kind strings without limit.
        let clock = Arc::new(TestClock::frozen_at(Instant::now()));
        let limiter = Arc::new(RateLimiter::with_clock(1.0, 5.0, clock));
        let (_, sink) = events("probe");
        let h = handler(running_generation(), true, Some(sink), limiter);
        let err = h
            .call(json!({"kind": "not-dotted", "payload": {}}))
            .await
            .unwrap_err();
        assert_eq!(err.code, code::INVALID_PARAMS);
        // The one token is gone even though the call "failed":
        let err2 = h.call(good_params()).await.unwrap_err();
        assert_eq!(err2.kind, Some(ErrorKind::RateLimited));
    }

    // ── judgement 16: forbidden + malformed params → -32602, not forbidden ──

    #[test]
    fn ungranted_plus_malformed_params_is_invalid_params_via_dispatch() {
        let (_, sink) = events("probe");
        let h = Arc::new(handler(
            running_generation(),
            false,
            Some(sink),
            generous_limiter(),
        ));
        let methods = Methods::none().with("_a24/events/emit", h);
        let frame = serde_json::to_vec(&json!({
            "jsonrpc": "2.0",
            "id": "1",
            "method": "_a24/events/emit",
            "params": {"kind": "task.transitioned", "payload": {}, "module": "sneaky"},
        }))
        .unwrap();
        let agent24_os_proto::rpc::Dispatch::Respond(r) =
            agent24_os_proto::rpc::dispatch(&frame, &methods, &|_| false)
        else {
            panic!("check_params must reject this before call() ever runs");
        };
        let err = r.outcome.unwrap_err();
        assert_eq!(
            err.code,
            code::INVALID_PARAMS,
            "check_params runs before call(): forbidden never gets a chance to fire"
        );
    }
}
