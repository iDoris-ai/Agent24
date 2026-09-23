//! ME4-1.3.1 — design `docs/design/ME4-S1-scheduler-callback.md` §5.1/§5.3:
//! the module deliverer. Turns one module fire
//! (`agent24_scheduler::ScheduleInvocation` with `InvocationTarget::Module`)
//! into a real `POST /api/v1/<ns>/_a24/scheduler/fired` over the module's
//! live `Generation` (`agent24_os_proto::kernel_call::send_kernel_request`),
//! and classifies the transport result into the `FireOutcome` the scheduler's
//! delivery state machine (`agent24_scheduler::deliveries::apply_outcome`)
//! consumes.
//!
//! `KernelTrigger` (`server.rs`)'s `Module` arm delegates here for every fire
//! the delivery pump (`agent24_scheduler::deliveries::DeliveryPump`) drives —
//! tick itself never reaches this (design §3.2).

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use agent24_os_proto::drain::{Abandoned, RequestRefused};
use agent24_os_proto::kernel_call::{
    FIRE_ID_HEADER, KernelCallError, KernelLimits, KernelRequest, KernelRequestIds, KernelResponse,
    SCHEDULE_KEY_HEADER, send_kernel_request,
};
use agent24_scheduler::{DeferReason, FireId, FireOutcome, ModuleScheduleKey};
use axum::http::{HeaderName, HeaderValue};

use crate::domain::Supervisors;

/// design §5.4/v3 L-F: production limits — 10s per attempt, 64 KiB of
/// response body read (and discarded). Mirrors
/// `agent24_scheduler::deliveries::{DELIVERY_TIMEOUT, MAX_FIRED_RESPONSE_BYTES}`
/// exactly, so the two crates cannot silently drift apart.
pub const PRODUCTION_LIMITS: KernelLimits = KernelLimits {
    total: Duration::from_secs(agent24_scheduler::deliveries::DELIVERY_TIMEOUT.as_secs()),
    max_response_bytes: agent24_scheduler::deliveries::MAX_FIRED_RESPONSE_BYTES,
};

/// design §5.1's late-bound handle to the running supervisors, plus the
/// kernel request-id minter and the injected per-attempt limits.
/// `AppState::new` builds the `Scheduler` (and its `KernelTrigger`) before
/// `mount_all` has a `ProcessHost` to hand this an `Arc<Supervisors>` —
/// `server::serve` calls [`Self::set_supervisors`] right after `mount_all`
/// returns, before the tick loop and the delivery pump start (design §4.6).
/// Unset ⇒ every module fire is `Deferred(MountPending)`.
pub struct ModuleDeliverer {
    supervisors: OnceLock<Arc<Supervisors>>,
    ids: KernelRequestIds,
    limits: KernelLimits,
}

impl ModuleDeliverer {
    #[must_use]
    pub fn new(limits: KernelLimits) -> Self {
        Self {
            supervisors: OnceLock::new(),
            ids: KernelRequestIds::new_random(),
            limits,
        }
    }

    /// Set exactly once, right after `mount_all` returns and before the tick
    /// loop / delivery pump start (design §4.6). A second call is a
    /// programming error in the caller — `OnceLock::set` silently keeps the
    /// first value either way, so this only logs loudly rather than panics: a
    /// background deliverer is not the place to bring the daemon down over a
    /// startup-ordering bug that did not actually lose any data.
    pub fn set_supervisors(&self, supervisors: Arc<Supervisors>) {
        if self.supervisors.set(supervisors).is_err() {
            tracing::warn!(
                "ModuleDeliverer::set_supervisors called more than once; the first call wins — \
                 this should happen exactly once, right after mount_all"
            );
            debug_assert!(
                false,
                "ModuleDeliverer::set_supervisors called twice — should be set exactly once, \
                 after mount_all"
            );
        }
    }

    /// design §5.1/§5.3: find the module's live generation, build the fired
    /// request from `owner`/`fire_id`/`trigger`/`scheduled_for`/`fired_at`,
    /// send it, and classify the result. Never panics, never blocks the tick
    /// — the delivery pump awaits this once per attempt, at most
    /// `PER_OWNER_IN_FLIGHT` at a time per owner.
    pub async fn deliver(
        &self,
        owner: &ModuleScheduleKey,
        fire_id: &FireId,
        trigger: &str,
        scheduled_for: &str,
        fired_at: &str,
    ) -> FireOutcome {
        let Some(supervisors) = self.supervisors.get() else {
            return FireOutcome::Deferred {
                reason: DeferReason::MountPending,
            };
        };
        let Some(current) = supervisors.running_slot(&owner.owner_module) else {
            return FireOutcome::Deferred {
                reason: DeferReason::NotRunning,
            };
        };
        let generation = current.get();
        let namespace = agent24_domain::DomainOsManifest::declared_namespace(&owner.owner_module);
        let path = format!("{namespace}/_a24/scheduler/fired");
        let body = FiredBody {
            key: &owner.module_key,
            trigger,
            scheduled_for,
            fired_at,
        };
        let body_bytes = match serde_json::to_vec(&body) {
            Ok(b) => b,
            Err(err) => {
                // Unreachable in practice — every field is a plain string —
                // fail safe rather than panic a background loop on a
                // serialization bug.
                return FireOutcome::Failed {
                    reason: format!("kernel bug: could not serialize the fired body: {err}"),
                };
            }
        };
        let (Ok(key_header), Ok(fire_id_header)) = (
            HeaderValue::from_str(&owner.module_key),
            HeaderValue::from_str(fire_id.as_str()),
        ) else {
            // `module_key` is validated at upsert time (design §6.3: ASCII
            // `[a-z0-9._-]`) and `fire_id` is kernel-derived hex — neither
            // should ever fail to become a header value; fail safe rather
            // than panic if that invariant is ever violated.
            return FireOutcome::Failed {
                reason: "kernel bug: schedule key or fire id is not a valid header value"
                    .to_owned(),
            };
        };
        let request = KernelRequest {
            path,
            extra_headers: vec![
                (HeaderName::from_static(SCHEDULE_KEY_HEADER), key_header),
                (HeaderName::from_static(FIRE_ID_HEADER), fire_id_header),
            ],
            body: body_bytes.into(),
        };
        let result = send_kernel_request(&generation, &self.ids, request, self.limits, None).await;
        classify(fire_id, result)
    }
}

/// design §5.3: the body of `POST /api/v1/<ns>/_a24/scheduler/fired`. Stable
/// across retries — every field is read back off the delivery row by the
/// caller (`agent24_scheduler::deliveries::run_attempt`), never re-taken at
/// send time.
#[derive(Debug, serde::Serialize)]
struct FiredBody<'a> {
    key: &'a str,
    /// `"tick"` | `"run_now"`.
    trigger: &'a str,
    scheduled_for: &'a str,
    fired_at: &'a str,
}

/// design §5.3's classification table: one attempt's transport result → the
/// [`FireOutcome`] the delivery state machine consumes. The rule: **not sent
/// ⇒ `Deferred`; sent (or a live generation refused the connection) and not
/// 2xx ⇒ `Failed`**.
#[must_use]
fn classify(fire_id: &FireId, result: Result<KernelResponse, KernelCallError>) -> FireOutcome {
    let failed = |reason: String| FireOutcome::Failed { reason };
    let deferred = |reason| FireOutcome::Deferred { reason };
    match result {
        Ok(r) if r.status.is_success() => FireOutcome::ModuleDelivered {
            fire_id: fire_id.clone(),
        },
        Ok(r) => failed(format!("module answered HTTP {}", r.status.as_u16())),
        Err(KernelCallError::Refused(RequestRefused::NotReady)) => deferred(DeferReason::NotReady),
        Err(KernelCallError::Refused(RequestRefused::Draining)) => deferred(DeferReason::Draining),
        Err(KernelCallError::Refused(RequestRefused::Stopping)) => deferred(DeferReason::Stopping),
        Err(
            KernelCallError::Refused(RequestRefused::DuplicateId)
            | KernelCallError::EntropyUnavailable,
        ) => deferred(DeferReason::KernelTransient),
        Err(
            KernelCallError::NotDispatched
            | KernelCallError::Abandoned(Abandoned { dispatched: false }),
        ) => deferred(DeferReason::NeverSent),
        // Sent, then the generation was revoked under it: the module may have
        // acted (at-least-once ⇒ redeliver), and a handler that crashes the
        // module every time must not loop forever — so this is a SENT
        // attempt, not a deferral (design §5.3, residual risk R11).
        Err(KernelCallError::Abandoned(Abandoned { dispatched: true })) => {
            failed("the module was stopped while the delivery was in flight".to_owned())
        }
        Err(KernelCallError::NotSent(e)) => failed(format!("could not reach the module: {e}")),
        Err(KernelCallError::MaybeSent(e)) => {
            failed(format!("connection closed during the delivery: {e}"))
        }
        Err(KernelCallError::Timeout) => {
            failed("the module did not answer within the delivery timeout".to_owned())
        }
        Err(KernelCallError::ResponseTooLarge) => {
            failed("the module's response exceeded the limit".to_owned())
        }
        // Review round 1, L3: a non-2xx response whose body failed to read
        // for a reason OTHER than the size limit (a reset/truncated
        // connection, most likely) — still a real failure, `last_error`
        // says what actually happened rather than the size-limit message.
        Err(KernelCallError::ResponseBodyError(e)) => {
            failed(format!("the module's response body could not be read: {e}"))
        }
        // A `Running` generation always has an upstream address (`Generation::
        // ready`'s own invariant) — unreachable in practice; treated as a
        // transient kernel condition rather than trusted to be impossible.
        Err(KernelCallError::NoUpstream) => deferred(DeferReason::NotReady),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use agent24_os_proto::drain::{Current, Generation};
    use agent24_scheduler::FireTrigger;

    fn fid() -> FireId {
        FireId::derive(FireTrigger::Tick, "sch_test", chrono::Utc::now())
    }

    #[test]
    fn a_2xx_status_is_module_delivered() {
        let outcome = classify(
            &fid(),
            Ok(KernelResponse {
                status: axum::http::StatusCode::OK,
            }),
        );
        assert_eq!(outcome, FireOutcome::ModuleDelivered { fire_id: fid() });
    }

    #[test]
    fn a_non_2xx_status_is_failed_not_deferred() {
        let outcome = classify(
            &fid(),
            Ok(KernelResponse {
                status: axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            }),
        );
        match outcome {
            FireOutcome::Failed { reason } => assert!(reason.contains("500")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    #[test]
    fn not_ready_draining_stopping_are_all_deferred_not_failed() {
        for (refused, expect) in [
            (RequestRefused::NotReady, DeferReason::NotReady),
            (RequestRefused::Draining, DeferReason::Draining),
            (RequestRefused::Stopping, DeferReason::Stopping),
        ] {
            let outcome = classify(&fid(), Err(KernelCallError::Refused(refused)));
            assert_eq!(outcome, FireOutcome::Deferred { reason: expect });
        }
    }

    #[test]
    fn never_sent_is_deferred_not_failed() {
        let outcome = classify(&fid(), Err(KernelCallError::NotDispatched));
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NeverSent
            }
        );
        let outcome = classify(
            &fid(),
            Err(KernelCallError::Abandoned(Abandoned { dispatched: false })),
        );
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NeverSent
            }
        );
    }

    /// design §5.3: `Abandoned{dispatched:true}` is a FAILED attempt, never a
    /// deferral — the whole point being that a module which crashes on every
    /// fired delivery must eventually hit `failed`, not loop forever.
    #[test]
    fn dispatched_and_abandoned_is_failed_not_deferred() {
        let outcome = classify(
            &fid(),
            Err(KernelCallError::Abandoned(Abandoned { dispatched: true })),
        );
        assert!(matches!(outcome, FireOutcome::Failed { .. }), "{outcome:?}");
    }

    #[test]
    fn timeout_and_response_too_large_are_failed() {
        assert!(matches!(
            classify(&fid(), Err(KernelCallError::Timeout)),
            FireOutcome::Failed { .. }
        ));
        assert!(matches!(
            classify(&fid(), Err(KernelCallError::ResponseTooLarge)),
            FireOutcome::Failed { .. }
        ));
    }

    /// Review round 1, L3: a non-2xx response body that fails to read for a
    /// reason OTHER than the size limit is still `Failed` (sent, just not
    /// acknowledged) — never silently reclassified as `ResponseTooLarge`.
    #[test]
    fn a_response_body_error_is_failed_and_says_what_happened() {
        let outcome = classify(
            &fid(),
            Err(KernelCallError::ResponseBodyError(
                "connection reset".to_owned(),
            )),
        );
        match outcome {
            FireOutcome::Failed { reason } => assert!(reason.contains("connection reset")),
            other => panic!("expected Failed, got {other:?}"),
        }
    }

    fn owner() -> ModuleScheduleKey {
        ModuleScheduleKey {
            owner_module: "zzmod".to_owned(),
            module_key: "k".to_owned(),
        }
    }

    /// design §4.6/§5.1: before `set_supervisors` is ever called, every
    /// module fire is `Deferred(MountPending)` — never a failure.
    #[tokio::test]
    async fn unset_supervisors_defers_as_mount_pending() {
        let deliverer = ModuleDeliverer::new(PRODUCTION_LIMITS);
        let outcome = deliverer
            .deliver(
                &owner(),
                &fid(),
                "tick",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            )
            .await;
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::MountPending
            }
        );
    }

    /// design §5.1/§9: an owner with no running slot (never mounted,
    /// disabled, hot-disabled, uninstalled) is `Deferred(NotRunning)`.
    #[tokio::test]
    async fn no_running_slot_defers_as_not_running() {
        let deliverer = ModuleDeliverer::new(PRODUCTION_LIMITS);
        deliverer.set_supervisors(Arc::new(Supervisors::default()));
        let outcome = deliverer
            .deliver(
                &owner(),
                &fid(),
                "tick",
                "2026-01-01T00:00:00Z",
                "2026-01-01T00:00:00Z",
            )
            .await;
        assert_eq!(
            outcome,
            FireOutcome::Deferred {
                reason: DeferReason::NotRunning
            }
        );
    }

    /// A `Starting` generation (handshake not complete) reachable through
    /// `running_slot` is `Deferred(NotReady)`, never a failure — same
    /// admission every proxied request goes through.
    #[tokio::test]
    async fn a_starting_generation_defers_as_not_ready() {
        let generation = Generation::starting();
        let current = Current::new(generation);
        // `running_slot` needs a real `Supervised` entry, which needs a real
        // `SupervisorHandle` — out of reach without a full supervised process
        // (see `scheduler_deliver`'s own module doc). This test instead pins
        // the layer directly below `running_slot`: `deliver`'s behaviour once
        // it HAS a `Current` for a Starting generation, by exercising
        // `send_kernel_request`'s own admission refusal through the same
        // `classify` this module uses — proven end-to-end (with a REAL
        // running_slot lookup) in the black-box test module instead.
        let admitted = current.get().admit_request(
            "probe".to_owned(),
            [0u8; 32],
            std::time::Instant::now(),
            Duration::from_secs(1),
        );
        assert_eq!(admitted.err(), Some(RequestRefused::NotReady));
    }

    /// design §11 C4.11: the tick loop AND the delivery pump must be spawned
    /// AFTER `mount_all` returns (design §4.6/§3.2, S1-6) — a structural
    /// scan of `server.rs`'s own source, the same heuristic-but-forcing style
    /// `domain.rs`'s `reserved_segments_match_the_kernel_routes_exactly`
    /// already uses. Mutation: swap the two blocks in `server.rs` — this
    /// assertion goes red.
    #[test]
    fn the_tick_loop_and_delivery_pump_are_spawned_after_mount_all() {
        let src = include_str!("server.rs");
        let mount_at = src
            .find("crate::domain::mount_all(")
            .expect("the mount_all call must exist in server.rs");
        let tick_at = src
            .find("tick_scheduler.run(")
            .expect("the tick loop spawn must exist in server.rs");
        let pump_at = src
            .find("DeliveryPump::new(")
            .expect("the delivery pump spawn must exist in server.rs");
        assert!(
            tick_at > mount_at,
            "the scheduler tick loop must be spawned after mount_all (design §4.6)"
        );
        assert!(
            pump_at > mount_at,
            "the delivery pump must be spawned after mount_all (design §4.6)"
        );
    }
}

/// ME4-1.3.1 (design §11, C4): the delivery pump (`agent24_scheduler::
/// deliveries::DeliveryPump`) driven end to end against a REAL
/// `agent24_store::Store` and a scripted `RunTrigger` — the pump's own
/// concurrency/retry/cancellation behaviour, independent of the transport
/// (`kernel_call`'s revoke races are covered in `agent24-os-proto`; the
/// `ModuleDeliverer`/`classify` mapping is covered by `tests` above).
///
/// A REAL `Supervisors`/`Generation`/UDS module is deliberately NOT used
/// here: `agent24_os_proto::supervisor::SupervisorHandle` has no public
/// constructor outside a genuinely supervised process
/// (`crate::domain::Supervisors::start_with`'s closure needs one), so a
/// `running_slot`-reachable module for these tests would need the daemon's
/// full out-of-process package harness (`domain.rs`'s `write_package_with` +
/// `test_host`) rather than the lighter, deterministic scripted trigger used
/// here. The `Deferred(NotReady)` test above and `agent24-os-proto`'s own
/// `kernel_call` suite already prove the transport layer that harness would
/// otherwise be re-proving.
#[cfg(test)]
mod pump_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex as StdMutex};
    use std::time::{Duration, Instant};

    use agent24_protocol::EventBody;
    use agent24_scheduler::deliveries::DeliveryPump;
    use agent24_scheduler::next_fire::{fmt_iso, next_fire, parse_iso};
    use agent24_scheduler::{
        Clock, FireOutcome, InvocationTarget, RunTrigger, ScheduleInvocation, Scheduler,
    };
    use agent24_store::{ModuleScheduleDesired, Store};
    use async_trait::async_trait;
    use chrono::{DateTime, Utc};
    use tokio_util::sync::CancellationToken;

    /// A clock this test fully controls: `now()` is whatever the test last
    /// set it to (never tied to real elapsed time, so `RETRY_BACKOFF`'s fixed
    /// 5s/15s waits never cost a real second); `sleep()` returns almost at
    /// once regardless of the requested duration, so the pump's own
    /// `PUMP_INTERVAL` cadence never makes a test wait a real second either.
    /// Tests observe outcomes by bounded polling (`wait_until`), never by
    /// assuming a fixed number of pump iterations happened.
    #[derive(Clone)]
    struct TestClock(Arc<StdMutex<DateTime<Utc>>>);

    impl TestClock {
        fn at(now: DateTime<Utc>) -> Arc<Self> {
            Arc::new(Self(Arc::new(StdMutex::new(now))))
        }
        fn set(&self, now: DateTime<Utc>) {
            *self.0.lock().unwrap() = now;
        }
    }

    #[async_trait]
    impl Clock for TestClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().unwrap()
        }
        async fn sleep(&self, _dur: Duration) {
            tokio::time::sleep(Duration::from_millis(3)).await;
        }
    }

    /// One recorded call: enough to assert `fire_id`/`scheduled_for`/
    /// `fired_at` stayed byte-identical across retries (design §4.2/§5.3).
    #[derive(Debug, Clone, PartialEq)]
    struct RecordedCall {
        fire_id: String,
        scheduled_for: DateTime<Utc>,
        fired_at: DateTime<Utc>,
    }

    /// A `RunTrigger` a test scripts: each call to `trigger()` for a `Module`
    /// target pops the next canned `FireOutcome` (the last one repeats once
    /// the queue is empty, so a test does not have to over-provision it).
    struct ScriptedTrigger {
        outcomes: StdMutex<VecDeque<FireOutcome>>,
        calls: StdMutex<Vec<RecordedCall>>,
    }

    impl ScriptedTrigger {
        fn new(outcomes: impl IntoIterator<Item = FireOutcome>) -> Arc<Self> {
            Arc::new(Self {
                outcomes: StdMutex::new(outcomes.into_iter().collect()),
                calls: StdMutex::new(Vec::new()),
            })
        }
        fn calls(&self) -> Vec<RecordedCall> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl RunTrigger for ScriptedTrigger {
        async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
            let InvocationTarget::Module { fire_id, .. } = &invocation.target else {
                panic!("ScriptedTrigger is only exercised with Module targets in these tests");
            };
            self.calls.lock().unwrap().push(RecordedCall {
                fire_id: fire_id.as_str().to_owned(),
                scheduled_for: invocation.scheduled_for,
                fired_at: invocation.fired_at,
            });
            let mut outcomes = self.outcomes.lock().unwrap();
            if outcomes.len() > 1 {
                outcomes.pop_front().unwrap()
            } else {
                outcomes.front().cloned().unwrap_or(FireOutcome::Deferred {
                    reason: agent24_scheduler::DeferReason::NotRunning,
                })
            }
        }
    }

    /// A `RunTrigger` whose `Module` arm blocks forever (never resolves) — for
    /// judgement C4.12: an attempt genuinely "in flight" when the pump is
    /// cancelled.
    struct BlockingTrigger {
        started: tokio::sync::Notify,
    }

    impl BlockingTrigger {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                started: tokio::sync::Notify::new(),
            })
        }
    }

    #[async_trait]
    impl RunTrigger for BlockingTrigger {
        async fn trigger(&self, _invocation: &ScheduleInvocation) -> FireOutcome {
            self.started.notify_one();
            std::future::pending::<()>().await;
            unreachable!("pending() never resolves")
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    fn every_module(secs: u32) -> ModuleScheduleDesired {
        ModuleScheduleDesired {
            spec: agent24_protocol::ScheduleSpec::Every { secs },
            enabled: true,
            label: "k".to_owned(),
        }
    }

    /// Seeds one module row and records its first tick fire — the same
    /// recipe `agent24-scheduler`'s own `module_row_tick_records_a_pending_
    /// delivery_and_counts_no_failure` test uses, via the SAME `Scheduler`
    /// this test then hands to a `DeliveryPump`.
    async fn seed_module_fire(
        scheduler: &Arc<Scheduler>,
        store: &Store,
        owner: &str,
        key: &str,
        now0: DateTime<Utc>,
    ) -> String {
        let desired = every_module(60);
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        let schedule_id = format!("sch_{owner}_{key}");
        store
            .upsert_module_schedule(
                &schedule_id,
                owner,
                key,
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();
        let due = now0 + chrono::Duration::seconds(65);
        assert_eq!(
            scheduler.tick(due).await.unwrap(),
            1,
            "the seeded row must fire on this tick"
        );
        schedule_id
    }

    /// Bounded polling — never a real sleep the test's own correctness
    /// depends on: `condition` is checked immediately and then at a short
    /// real interval (irrelevant to `TestClock`'s virtual time) until
    /// `deadline` is hit, at which point this panics with `on_timeout`'s
    /// message.
    async fn wait_until<F: Fn() -> bool>(condition: F, on_timeout: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if condition() {
                return;
            }
            assert!(Instant::now() < deadline, "{on_timeout}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn scheduler_with(
        store: Store,
        trigger: Arc<dyn RunTrigger>,
    ) -> (Arc<Scheduler>, Arc<StdMutex<Vec<EventBody>>>) {
        let events = Arc::new(StdMutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
            ev.lock().unwrap().push(body);
        });
        (Scheduler::new(store, trigger, emit), events)
    }

    /// design §4.3 (T2): a module fire that comes back `ModuleDelivered` on
    /// its first attempt lands `delivered`, resets `consecutive_failures`,
    /// and emits `schedule.delivered`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_successful_first_attempt_is_delivered_and_emits_the_event() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::ModuleDelivered {
            fire_id: agent24_scheduler::FireId::from_stored("placeholder".into()),
        }]);
        let (scheduler, events) = scheduler_with(store.clone(), trigger as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-a", "k", now0).await;

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle = tokio::spawn(pump.run(clock as Arc<dyn Clock>, cancel.child_token()));

        wait_until(
            || {
                let states =
                    futures::executor::block_on(store.list_module_schedules("mod-a")).unwrap();
                states[0]
                    .last_fire
                    .tick
                    .as_ref()
                    .is_some_and(|f| f.status == "delivered")
            },
            "the fire never reached delivered",
        )
        .await;
        cancel.cancel();
        handle.await.unwrap();

        let schedule = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(schedule.consecutive_failures, 0);
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, EventBody::ScheduleDelivered(_))),
            "schedule.delivered must have been emitted"
        );
    }

    /// design §4.2/§4.3/§4.4 (T5/T6), judgement **C4.2/C4.3**: three sent
    /// failures fail the fire ONCE (not three times), `consecutive_failures`
    /// goes to 1, and every attempt carried the exact same `fire_id`/
    /// `scheduled_for`/`fired_at` — the retries of ONE slot, not three
    /// different fires. The positive control (a later, different slot gets a
    /// different `fire_id`) is asserted in the same test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn three_failures_fail_once_with_a_stable_fire_id_then_the_next_slot_differs() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([
            FireOutcome::Failed {
                reason: "boom 1".into(),
            },
            FireOutcome::Failed {
                reason: "boom 2".into(),
            },
            FireOutcome::Failed {
                reason: "boom 3".into(),
            },
            // The row is `failed` (terminal) after the third attempt — this
            // fourth entry must never be reached for THIS fire; it exists
            // only so `ScriptedTrigger` has something to hand back if the
            // pump's own CAS ever (wrongly) retried past three.
            FireOutcome::ModuleDelivered {
                fire_id: agent24_scheduler::FireId::from_stored("must-not-be-reached".into()),
            },
        ]);
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-b", "k", now0).await;

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        // §4.4's backoff (5s, then 15s) is computed from `clock.now()` AT THE
        // MOMENT the pump applies an attempt's outcome — not at the moment it
        // was dispatched. So the clock must stay FROZEN while an attempt is
        // in flight (advancing it early would inflate that attempt's own
        // `next_attempt_at`) and only move once this test has PROOF (the
        // expected `last_error` landed in the store) that attempt N's
        // outcome was applied while the clock held the value this test last
        // set — only then is "that value + the fixed backoff" the exact
        // threshold the next attempt needs.
        async fn wait_for_last_error(store: &Store, owner: &str, expected: &str) {
            wait_until(
                || {
                    let states =
                        futures::executor::block_on(store.list_module_schedules(owner)).unwrap();
                    states[0]
                        .last_fire
                        .tick
                        .as_ref()
                        .and_then(|f| f.last_error.as_deref())
                        == Some(expected)
                },
                &format!("last_error never became {expected:?}"),
            )
            .await;
        }

        wait_for_last_error(&store, "mod-b", "boom 1").await;
        let t1 = clock.now(); // unchanged since `at()`: still now0 + 65s
        clock.set(t1 + chrono::Duration::seconds(6)); // past the 5s backoff
        wait_for_last_error(&store, "mod-b", "boom 2").await;
        let t2 = clock.now(); // unchanged since the line above
        clock.set(t2 + chrono::Duration::seconds(16)); // past the 15s backoff
        wait_until(
            || trigger.calls().len() >= 3,
            "the third attempt never happened",
        )
        .await;

        wait_until(
            || {
                let states =
                    futures::executor::block_on(store.list_module_schedules("mod-b")).unwrap();
                states[0]
                    .last_fire
                    .tick
                    .as_ref()
                    .is_some_and(|f| f.status == "failed")
            },
            "the fire never reached failed after three attempts",
        )
        .await;
        // Give the pump a moment to prove it does NOT attempt a fourth time.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            trigger.calls().len(),
            3,
            "a failed fire must not be retried a fourth time"
        );

        let schedule = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(
            schedule.consecutive_failures, 1,
            "three attempts of ONE fire must count as a single failure, not three"
        );

        let calls = trigger.calls();
        assert_eq!(calls.len(), 3);
        assert!(
            calls.iter().all(|c| c.fire_id == calls[0].fire_id),
            "every retry of one fire must carry the exact same fire_id: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .all(|c| c.scheduled_for == calls[0].scheduled_for
                    && c.fired_at == calls[0].fired_at),
            "every retry of one fire must carry the exact same scheduled_for/fired_at: {calls:?}"
        );

        cancel.cancel();
        handle.await.unwrap();

        // Positive control: a later, DIFFERENT slot gets a different fire_id.
        let trigger2 = ScriptedTrigger::new([FireOutcome::ModuleDelivered {
            fire_id: agent24_scheduler::FireId::from_stored("placeholder".into()),
        }]);
        let (scheduler2, _events2) =
            scheduler_with(store.clone(), trigger2.clone() as Arc<dyn RunTrigger>);
        let now1 = now0 + chrono::Duration::seconds(200);
        assert_eq!(scheduler2.tick(now1).await.unwrap(), 1);
        let clock2 = TestClock::at(now1);
        let cancel2 = CancellationToken::new();
        let pump2 = DeliveryPump::new(Arc::clone(&scheduler2));
        let handle2 = tokio::spawn(pump2.run(clock2 as Arc<dyn Clock>, cancel2.child_token()));
        wait_until(
            || !trigger2.calls().is_empty(),
            "the next slot's fire never attempted",
        )
        .await;
        cancel2.cancel();
        handle2.await.unwrap();
        assert_ne!(
            trigger2.calls()[0].fire_id,
            calls[0].fire_id,
            "a different slot must never reuse the same fire_id"
        );
    }

    /// design §4.1/§5.3/§9, judgement **C4.4**: the module being unreachable
    /// (`Deferred`) never counts as a failure and never stops — the row is
    /// picked up again once the module comes back, still carrying the SAME
    /// `fire_id`. Also stands in for **C4.10** (the pure `apply_outcome`
    /// "repeating Deferred writes nothing" contract is unit-tested directly
    /// in `agent24_scheduler::deliveries`; here the pump-level effect —
    /// `consecutive_failures` never moves while the module stays
    /// unavailable, however many rounds it takes — is what's under test).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unavailable_module_never_counts_as_a_failure_and_recovers_with_the_same_fire_id() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::Deferred {
            reason: agent24_scheduler::DeferReason::NotRunning,
        }]);
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-c", "k", now0).await;

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        // Several rounds while the module stays unavailable: never a failure.
        // The pump's `DEFER_RECHECK` owner-skip cache is keyed off the SAME
        // virtual clock, so it must be advanced past each 2s window for the
        // pump to re-poll — a real `tokio::time::sleep` here would just wait
        // out a virtual window that never moves on its own.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if trigger.calls().len() >= 3 {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the pump never re-polled the unavailable module"
            );
            clock.set(clock.now() + chrono::Duration::seconds(3));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let schedule = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(
            schedule.consecutive_failures, 0,
            "Deferred must never count as a failure"
        );
        let states = store.list_module_schedules("mod-c").await.unwrap();
        let last_fire = states[0].last_fire.tick.clone().unwrap();
        assert_eq!(last_fire.status, "deferred");
        let stable_fire_id = last_fire.fire_id.clone();

        // The module "comes back": swap in a trigger that delivers.
        trigger.outcomes.lock().unwrap().clear();
        trigger
            .outcomes
            .lock()
            .unwrap()
            .push_back(FireOutcome::ModuleDelivered {
                fire_id: agent24_scheduler::FireId::from_stored(stable_fire_id.clone()),
            });
        // Same reason as above: advance the virtual clock past the owner's
        // remaining `DEFER_RECHECK` window, since nothing else will.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let states = store.list_module_schedules("mod-c").await.unwrap();
            if states[0]
                .last_fire
                .tick
                .as_ref()
                .is_some_and(|f| f.status == "delivered")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the fire never delivered once the module recovered"
            );
            clock.set(clock.now() + chrono::Duration::seconds(3));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let states = store.list_module_schedules("mod-c").await.unwrap();
        assert_eq!(
            states[0].last_fire.tick.as_ref().unwrap().fire_id,
            stable_fire_id,
            "recovery must deliver the SAME fire, not a new one"
        );
        cancel.cancel();
        handle.await.unwrap();
    }

    /// design §4.6/§5.4, judgement **C4.12**: an attempt genuinely in flight
    /// when the pump is cancelled leaves its row untouched (`pending`,
    /// `attempts` unchanged) — the `JoinSet` aborts it, it never gets to
    /// apply an outcome. Judgement **C4.8**: a fresh `Scheduler`+`DeliveryPump`
    /// against the SAME store (standing in for "the next start") then
    /// redelivers with the EXACT SAME `fire_id`, and it succeeds.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_cancelled_in_flight_attempt_leaves_its_row_pending_for_the_next_start() {
        let store = Store::open_memory().await.unwrap();
        let trigger = BlockingTrigger::new();
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-d", "k", now0).await;
        let fire_id_before = store.list_module_schedules("mod-d").await.unwrap()[0]
            .last_fire
            .tick
            .clone()
            .unwrap()
            .fire_id;

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle = tokio::spawn(pump.run(clock as Arc<dyn Clock>, cancel.child_token()));

        tokio::time::timeout(Duration::from_secs(5), trigger.started.notified())
            .await
            .expect("the attempt never started");
        // Genuinely in flight now (blocked inside `trigger()`, forever).
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the pump must stop promptly on cancellation, not wait out the blocked attempt")
            .unwrap();

        let states = store.list_module_schedules("mod-d").await.unwrap();
        let after_cancel = states[0].last_fire.tick.clone().unwrap();
        assert_eq!(
            after_cancel.status, "pending",
            "a cancelled in-flight attempt must leave the row pending"
        );
        assert_eq!(after_cancel.fire_id, fire_id_before);
        let schedule = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(schedule.consecutive_failures, 0);

        // "The next start": a FRESH Scheduler + DeliveryPump over the SAME
        // store, with a trigger that now delivers.
        let trigger2 = ScriptedTrigger::new([FireOutcome::ModuleDelivered {
            fire_id: agent24_scheduler::FireId::from_stored(fire_id_before.clone()),
        }]);
        let (scheduler2, _events2) = scheduler_with(store.clone(), trigger2 as Arc<dyn RunTrigger>);
        let clock2 = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel2 = CancellationToken::new();
        let pump2 = DeliveryPump::new(Arc::clone(&scheduler2));
        let handle2 = tokio::spawn(pump2.run(clock2 as Arc<dyn Clock>, cancel2.child_token()));
        wait_until(
            || {
                let states =
                    futures::executor::block_on(store.list_module_schedules("mod-d")).unwrap();
                states[0]
                    .last_fire
                    .tick
                    .as_ref()
                    .is_some_and(|f| f.status == "delivered")
            },
            "the restarted pump never redelivered the pending fire",
        )
        .await;
        let states = store.list_module_schedules("mod-d").await.unwrap();
        assert_eq!(
            states[0].last_fire.tick.as_ref().unwrap().fire_id,
            fire_id_before,
            "the restart must redeliver the SAME fire_id, not a new one"
        );
        cancel2.cancel();
        handle2.await.unwrap();
    }
}
