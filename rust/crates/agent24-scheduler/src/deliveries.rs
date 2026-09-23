//! ME4-1.3.1 — design `docs/design/ME4-S1-scheduler-callback.md` §4.3/§4.4/
//! §4.5/§5.4: the delivery state machine ([`apply_outcome`]) and the delivery
//! pump ([`DeliveryPump`]) that drives every module schedule's
//! `schedule_deliveries` row to a terminal state, or keeps retrying one that
//! is not there yet. `apply_outcome` is a pure function ported from the
//! frozen (v3.1) design's scratch check crate `src/delivery.rs`; the pump
//! itself (§5.4) has no scratch precedent — this is its first real
//! implementation.
//!
//! The pump is independent of the tick (§3.2/§5.4: "超时不阻塞 tick"): tick
//! never calls [`RunTrigger::trigger`] for a module row, only pre-advances and
//! records the fire (`Scheduler::fire_module`); this module is what actually
//! calls `trigger()` for a module row, one attempt at a time per schedule, up
//! to [`PER_OWNER_IN_FLIGHT`] per owner and [`GLOBAL_IN_FLIGHT`] overall.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use agent24_protocol::{EventBody, ScheduleDeliveredPayload, ScheduleDisabledPayload};
use agent24_store::{DeliveryOutcomeWrite, DueDelivery};
use chrono::{DateTime, Utc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::fire::FireId;
use crate::invocation::{
    FireOutcome, FireTrigger, InvocationTarget, ModuleScheduleKey, ScheduleInvocation,
};
use crate::next_fire::{fmt_iso, parse_iso};
use crate::{Clock, Scheduler};

/// Sent attempts per fire, including the first (design §4.4).
pub const MAX_SENT_ATTEMPTS: u32 = 3;
/// Wait before sent attempt 2 and 3. With [`DELIVERY_TIMEOUT`]'s 10s ceiling
/// per attempt, the worst case is 10 + 5 + 10 + 15 + 10 = 50s — under the 60s
/// minimum period a module row can have (design §6.3), so retries of one slot
/// end before the next slot is due (see the `worst_case_retry_span_is_under_
/// the_minimum_period` test below).
pub const RETRY_BACKOFF: [Duration; 2] = [Duration::from_secs(5), Duration::from_secs(15)];
/// How long the pump remembers "owner X answered Deferred" and leaves X's
/// rows out of its due query (design §5.4) — a dead module costs one probe
/// per this interval and ZERO writes (T4 does not write), and cannot fill the
/// per-round query window.
pub const DEFER_RECHECK: Duration = Duration::from_secs(2);
/// Pump cadence, plus an immediate wake after every recorded fire
/// (`Scheduler::delivery_notify`).
pub const PUMP_INTERVAL: Duration = Duration::from_secs(1);
/// Expiry-sweep cadence (design §4.5) — a write statement, so it runs far less
/// often than every pump pass.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(60);
/// Attempts in flight at once, per owner module (design §5.4).
pub const PER_OWNER_IN_FLIGHT: usize = 4;
/// Attempts in flight at once, in total (design §5.4). Independent of the
/// proxy's own `MAX_INFLIGHT_PER_MODULE` — a kernel-originated request never
/// goes through `ProxyState` at all (design §5.2/v2 L4).
pub const GLOBAL_IN_FLIGHT: usize = 16;
/// The largest module response body a delivery attempt reads (and discards) —
/// mirrors `agent24_os_proto::kernel_call::KernelLimits::max_response_bytes`
/// in production.
pub const MAX_FIRED_RESPONSE_BYTES: usize = 64 * 1024;
/// A non-terminal delivery row older than this (from `fired_at`) expires
/// (design §4.5) — from `fired_at`, never `scheduled_for`, so a skip-missed
/// fire recorded long after a daemon outage is not born already expired.
pub const DELIVERY_TTL: Duration = Duration::from_secs(24 * 3600);
/// One attempt's end-to-end ceiling: admit through response read. Mirrors
/// `agent24_os_proto::kernel_call::KernelLimits::total` in production; tests
/// inject a much shorter value (design v3, L-F).
pub const DELIVERY_TIMEOUT: Duration = Duration::from_secs(10);

/// design §4.3's five delivery-row states. `Pending`/`Deferred` are
/// non-terminal; the rest never move again once reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryStatus {
    Pending,
    Deferred,
    Delivered,
    Failed,
    Expired,
}

impl DeliveryStatus {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Deferred => "deferred",
            Self::Delivered => "delivered",
            Self::Failed => "failed",
            Self::Expired => "expired",
        }
    }

    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed | Self::Expired)
    }

    /// `agent24_store::due_deliveries` only ever returns `"pending"` or
    /// `"deferred"` rows (its own `WHERE status IN (...)`) — anything else
    /// reaching here would be a store/pump mismatch, defensively treated as
    /// `Pending` rather than panicking a background loop over it.
    fn from_due_row(s: &str) -> Self {
        match s {
            "deferred" => Self::Deferred,
            _ => Self::Pending,
        }
    }
}

/// What applying one attempt's outcome does to the row and to its schedule
/// (design §4.3's transition table, as data).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Applied {
    pub status: DeliveryStatus,
    pub attempts: u32,
    /// `Some` iff `status` is non-terminal (mirrors the table's CHECK).
    pub next_attempt_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    /// `consecutive_failures += 1` on the schedule (and maybe system-disable).
    pub count_schedule_failure: bool,
    /// `consecutive_failures = 0` on the schedule.
    pub reset_schedule_failures: bool,
    /// Emit `schedule.delivered`.
    pub emit_delivered: bool,
}

/// Why [`apply_outcome`] did not produce an [`Applied`] — neither is an
/// error; both mean "nothing to write".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotApplied {
    /// `from` is already terminal: nothing moves it (T7/T8/T9/T10 may have
    /// already claimed the row between the pump's read and this call).
    AlreadyTerminal,
    /// Deferred again from `Deferred`: the row already says so (T4) — no
    /// write, no matter how many times this repeats.
    NoChange,
}

/// design §4.3's transition table, as a pure function: `from` (the status the
/// pump's `DueDelivery` row was read with) + this attempt's [`FireOutcome`] →
/// what to write. The caller ([`DeliveryPump`]) applies the result with a CAS
/// on `(fire_id, status IN (pending, deferred), attempts)`
/// (`agent24_store::apply_delivery_outcome`), so a result that lost a race
/// with expiry/supersede/delete is discarded rather than blindly written.
///
/// # Errors
/// [`NotApplied`].
pub fn apply_outcome(
    from: DeliveryStatus,
    attempts: u32,
    outcome: &FireOutcome,
    now: DateTime<Utc>,
) -> Result<Applied, NotApplied> {
    if from.is_terminal() {
        return Err(NotApplied::AlreadyTerminal);
    }
    if from == DeliveryStatus::Deferred && matches!(outcome, FireOutcome::Deferred { .. }) {
        return Err(NotApplied::NoChange);
    }
    let after = |d: Duration| {
        now + chrono::Duration::from_std(d).unwrap_or_else(|_| chrono::Duration::seconds(0))
    };
    Ok(match outcome {
        FireOutcome::ModuleDelivered { .. } => Applied {
            status: DeliveryStatus::Delivered,
            attempts: attempts.saturating_add(1),
            next_attempt_at: None,
            last_error: None,
            count_schedule_failure: false,
            reset_schedule_failures: true,
            emit_delivered: true,
        },
        FireOutcome::Deferred { reason } => Applied {
            status: DeliveryStatus::Deferred,
            // Nothing was sent: not an attempt (design §4.4).
            attempts,
            // Due at once; what actually holds it back is the pump's
            // owner-skip cache (§5.4), not this timestamp.
            next_attempt_at: Some(now),
            last_error: Some(reason.as_str().to_owned()),
            count_schedule_failure: false,
            reset_schedule_failures: false,
            emit_delivered: false,
        },
        // A module target's trigger answering `AgentRun` is a kernel bug
        // (wrong arm of `KernelTrigger`) — classified as a failed attempt
        // rather than trusted or panicked on.
        FireOutcome::Failed { .. } | FireOutcome::AgentRun { .. } => {
            let sent = attempts.saturating_add(1);
            let reason = match outcome {
                FireOutcome::Failed { reason } => reason.clone(),
                _ => "kernel bug: agent-run outcome for a module delivery".to_owned(),
            };
            if sent >= MAX_SENT_ATTEMPTS {
                Applied {
                    status: DeliveryStatus::Failed,
                    attempts: sent,
                    next_attempt_at: None,
                    last_error: Some(reason),
                    count_schedule_failure: true,
                    reset_schedule_failures: false,
                    emit_delivered: false,
                }
            } else {
                let wait = RETRY_BACKOFF
                    .get((sent - 1) as usize)
                    .copied()
                    .unwrap_or(RETRY_BACKOFF[RETRY_BACKOFF.len() - 1]);
                Applied {
                    status: DeliveryStatus::Pending,
                    attempts: sent,
                    next_attempt_at: Some(after(wait)),
                    last_error: Some(reason),
                    count_schedule_failure: false,
                    reset_schedule_failures: false,
                    emit_delivered: false,
                }
            }
        }
    })
}

/// One attempt's inputs (from a fetched [`DueDelivery`]) and its outcome,
/// handed from the spawned attempt task back to the pump loop.
struct AttemptDone {
    row: DueDelivery,
    outcome: FireOutcome,
}

async fn run_attempt(trigger: Arc<dyn crate::RunTrigger>, row: DueDelivery) -> AttemptDone {
    let fire_trigger = if row.fire_trigger == FireTrigger::RunNow.as_str() {
        FireTrigger::RunNow
    } else {
        FireTrigger::Tick
    };
    let scheduled_for = parse_iso(&row.scheduled_for).unwrap_or_else(|err| {
        tracing::error!(
            "delivery pump: fire {} has an unparsable scheduled_for {:?}: {err}; using now",
            row.fire_id,
            row.scheduled_for
        );
        Utc::now()
    });
    let fired_at = parse_iso(&row.fired_at).unwrap_or_else(|err| {
        tracing::error!(
            "delivery pump: fire {} has an unparsable fired_at {:?}: {err}; using now",
            row.fire_id,
            row.fired_at
        );
        Utc::now()
    });
    let invocation = ScheduleInvocation {
        schedule_id: row.schedule_id.clone(),
        scheduled_for,
        fired_at,
        trigger: fire_trigger,
        target: InvocationTarget::Module {
            owner: ModuleScheduleKey {
                owner_module: row.owner_module.clone(),
                module_key: row.module_key.clone(),
            },
            fire_id: FireId::from_stored(row.fire_id.clone()),
        },
    };
    let outcome = trigger.trigger(&invocation).await;
    AttemptDone { row, outcome }
}

/// In-memory bookkeeping the pump carries ACROSS loop iterations — attempts
/// routinely outlive one `PUMP_INTERVAL` (design §5.4's whole point: a 10s
/// attempt must not block the 1s cadence), so the concurrency ceilings and
/// the owner-skip cache have to live here, not be recomputed each wake.
struct PumpState {
    tasks: JoinSet<AttemptDone>,
    in_flight_per_owner: HashMap<String, usize>,
    /// design §5.4: "同一 schedule 同时至多一个在途尝试".
    in_flight_schedules: HashSet<String>,
    /// design §5.4: an owner that answered `Deferred` this recently is left
    /// out of the next `due_deliveries` query.
    owner_skip_until: HashMap<String, DateTime<Utc>>,
    last_sweep: DateTime<Utc>,
}

impl PumpState {
    fn new(now: DateTime<Utc>) -> Self {
        Self {
            tasks: JoinSet::new(),
            in_flight_per_owner: HashMap::new(),
            in_flight_schedules: HashSet::new(),
            owner_skip_until: HashMap::new(),
            last_sweep: now,
        }
    }

    fn skip_owners_json(&self, now: DateTime<Utc>) -> String {
        let owners: Vec<&str> = self
            .owner_skip_until
            .iter()
            .filter(|(_, until)| **until > now)
            .map(|(owner, _)| owner.as_str())
            .collect();
        serde_json::to_string(&owners).unwrap_or_else(|_| "[]".to_owned())
    }

    fn begin_attempt(&mut self, row: &DueDelivery) {
        self.in_flight_schedules.insert(row.schedule_id.clone());
        *self
            .in_flight_per_owner
            .entry(row.owner_module.clone())
            .or_insert(0) += 1;
    }

    fn end_attempt(&mut self, row: &DueDelivery, now: DateTime<Utc>, deferred: bool) {
        self.in_flight_schedules.remove(&row.schedule_id);
        if let Some(count) = self.in_flight_per_owner.get_mut(&row.owner_module) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight_per_owner.remove(&row.owner_module);
            }
        }
        if deferred {
            self.owner_skip_until.insert(
                row.owner_module.clone(),
                now + chrono::Duration::from_std(DEFER_RECHECK).unwrap_or_default(),
            );
        } else {
            self.owner_skip_until.remove(&row.owner_module);
        }
    }
}

/// design §5.4: the delivery pump. Independent of the tick — its own
/// `CancellationToken`, its own cadence, driven by the SAME injectable
/// [`Clock`] the tick uses so tests never need a real sleep.
pub struct DeliveryPump {
    scheduler: Arc<Scheduler>,
}

impl DeliveryPump {
    #[must_use]
    pub fn new(scheduler: Arc<Scheduler>) -> Self {
        Self { scheduler }
    }

    /// Run until `cancel`. On cancellation the pump stops taking new rows;
    /// `PumpState::tasks` (a [`JoinSet`]) is dropped with `self` when this
    /// returns, which ABORTS every attempt still in flight — each aborted
    /// attempt's `InFlight` (inside `agent24-os-proto::kernel_call`) then
    /// leaves the generation's in-flight table via its own `Drop`, and the
    /// delivery row it was for is untouched: still `pending`/`deferred`,
    /// `attempts` unchanged, ready for the next start to pick up with the
    /// SAME `fire_id` (design §4.6/§5.4, judgement C4.12).
    pub async fn run(self, clock: Arc<dyn Clock>, cancel: CancellationToken) {
        tracing::info!("delivery pump started ({PUMP_INTERVAL:?} cadence)");
        let mut state = PumpState::new(clock.now());
        loop {
            tokio::select! {
                () = clock.sleep(PUMP_INTERVAL) => {}
                () = self.scheduler.delivery_notify().notified() => {}
                () = cancel.cancelled() => {
                    tracing::info!(
                        "delivery pump stopped; {} attempt(s) in flight will be aborted — their \
                         rows stay pending/deferred for the next start",
                        state.tasks.len()
                    );
                    return;
                }
            }
            let now = clock.now();
            while let Some(joined) = state.tasks.try_join_next() {
                match joined {
                    Ok(done) => self.apply_attempt(&mut state, done, now).await,
                    Err(join_err) => {
                        tracing::error!("delivery pump: an attempt task panicked: {join_err}");
                    }
                }
            }
            self.fetch_and_spawn(&mut state, now).await;
        }
    }

    async fn maybe_sweep(&self, state: &mut PumpState, now: DateTime<Utc>) {
        if now - state.last_sweep < chrono::Duration::from_std(SWEEP_INTERVAL).unwrap_or_default() {
            return;
        }
        state.last_sweep = now;
        if let Err(err) = self
            .scheduler
            .store()
            .expire_deliveries(&fmt_iso(now))
            .await
        {
            tracing::error!("delivery pump: the 24h expiry sweep failed: {err}");
        }
    }

    async fn fetch_and_spawn(&self, state: &mut PumpState, now: DateTime<Utc>) {
        self.maybe_sweep(state, now).await;
        if state.tasks.len() >= GLOBAL_IN_FLIGHT {
            return;
        }
        let now_str = fmt_iso(now);
        let skip_json = state.skip_owners_json(now);
        let due = match self
            .scheduler
            .store()
            .due_deliveries(
                &now_str,
                &skip_json,
                PER_OWNER_IN_FLIGHT as i64,
                GLOBAL_IN_FLIGHT as i64,
            )
            .await
        {
            Ok(rows) => rows,
            Err(err) => {
                tracing::error!("delivery pump: could not fetch due deliveries: {err}");
                return;
            }
        };
        for row in due {
            if state.tasks.len() >= GLOBAL_IN_FLIGHT {
                break;
            }
            if *state
                .in_flight_per_owner
                .get(&row.owner_module)
                .unwrap_or(&0)
                >= PER_OWNER_IN_FLIGHT
            {
                continue;
            }
            if state.in_flight_schedules.contains(&row.schedule_id) {
                continue;
            }
            state.begin_attempt(&row);
            let trigger = Arc::clone(self.scheduler.trigger());
            state.tasks.spawn(run_attempt(trigger, row));
        }
    }

    async fn apply_attempt(&self, state: &mut PumpState, done: AttemptDone, now: DateTime<Utc>) {
        let AttemptDone { row, outcome } = done;
        let deferred = matches!(outcome, FireOutcome::Deferred { .. });
        state.end_attempt(&row, now, deferred);

        let from = DeliveryStatus::from_due_row(&row.status);
        let applied = match apply_outcome(from, row.attempts as u32, &outcome, now) {
            Ok(applied) => applied,
            Err(_not_applied) => return, // AlreadyTerminal or NoChange: nothing to write.
        };
        let next_attempt_at_str = applied.next_attempt_at.map(fmt_iso);
        let write = DeliveryOutcomeWrite {
            status: applied.status.as_str(),
            attempts: i64::from(applied.attempts),
            next_attempt_at: next_attempt_at_str.as_deref(),
            last_error: applied.last_error.as_deref(),
            reset_schedule_failures: applied.reset_schedule_failures,
            count_schedule_failure: applied.count_schedule_failure,
        };
        let disable_at = i64::from(agent24_core::transitions::MAX_CONSECUTIVE_FAILURES);
        match self
            .scheduler
            .store()
            .apply_delivery_outcome(
                &row.fire_id,
                row.attempts,
                &write,
                &fmt_iso(now),
                disable_at,
            )
            .await
        {
            Ok((landed, disabled)) => {
                if landed && applied.emit_delivered {
                    self.scheduler.emit_event(EventBody::ScheduleDelivered(
                        ScheduleDeliveredPayload {
                            schedule_id: row.schedule_id.clone(),
                            module: row.owner_module.clone(),
                            key: row.module_key.clone(),
                            fire_id: row.fire_id.clone(),
                            scheduled_for: row.scheduled_for.clone(),
                        },
                    ));
                }
                if landed && disabled {
                    self.scheduler.emit_event(EventBody::ScheduleDisabled(
                        ScheduleDisabledPayload {
                            schedule_id: row.schedule_id.clone(),
                            reason: "consecutive_failures".to_owned(),
                        },
                    ));
                }
            }
            Err(err) => {
                tracing::error!(
                    "delivery pump: could not apply the outcome for fire {}: {err}",
                    row.fire_id
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;
    use crate::invocation::DeferReason;

    fn t0() -> DateTime<Utc> {
        parse_iso("2026-09-23T09:00:00Z").unwrap()
    }
    fn failed() -> FireOutcome {
        FireOutcome::Failed {
            reason: "HTTP 500".into(),
        }
    }

    /// design §4.3/§4.4: three sent failures fail ONCE (not three times) and
    /// back off 5s then 15s.
    #[test]
    fn three_sent_failures_fail_once_and_count_once() {
        let a = apply_outcome(DeliveryStatus::Pending, 0, &failed(), t0()).unwrap();
        assert_eq!((a.status, a.attempts), (DeliveryStatus::Pending, 1));
        assert_eq!(a.next_attempt_at, Some(t0() + chrono::Duration::seconds(5)));
        assert!(!a.count_schedule_failure);
        let b = apply_outcome(a.status, a.attempts, &failed(), t0()).unwrap();
        assert_eq!((b.status, b.attempts), (DeliveryStatus::Pending, 2));
        assert_eq!(
            b.next_attempt_at,
            Some(t0() + chrono::Duration::seconds(15))
        );
        let c = apply_outcome(b.status, b.attempts, &failed(), t0()).unwrap();
        assert_eq!((c.status, c.attempts), (DeliveryStatus::Failed, 3));
        assert!(c.count_schedule_failure && c.next_attempt_at.is_none());
    }

    /// design §4.4: a `Deferred` outcome is never an attempt, never counted,
    /// and repeating it from `Deferred` writes nothing (T4).
    #[test]
    fn deferral_is_not_an_attempt_and_repeating_it_writes_nothing() {
        let defer = FireOutcome::Deferred {
            reason: DeferReason::Draining,
        };
        let a = apply_outcome(DeliveryStatus::Pending, 0, &defer, t0()).unwrap();
        assert!(!a.count_schedule_failure);
        let s = (a.status, a.attempts);
        assert_eq!(s, (DeliveryStatus::Deferred, 0));
        for _ in 0..50 {
            assert_eq!(
                apply_outcome(s.0, s.1, &defer, t0()),
                Err(NotApplied::NoChange)
            );
        }
        // A pending row already mid-backoff that finds its owner gone keeps
        // its attempt count.
        let b = apply_outcome(DeliveryStatus::Pending, 2, &defer, t0()).unwrap();
        assert_eq!((b.status, b.attempts), (DeliveryStatus::Deferred, 2));
        // Positive control: from Deferred, a delivery lands and resets.
        let d = apply_outcome(
            s.0,
            s.1,
            &FireOutcome::ModuleDelivered {
                fire_id: FireId::from_stored("fire_x".into()),
            },
            t0(),
        )
        .unwrap();
        assert_eq!(d.status, DeliveryStatus::Delivered);
        assert!(d.reset_schedule_failures && d.emit_delivered);
    }

    #[test]
    fn terminal_rows_never_move_again() {
        for s in [
            DeliveryStatus::Delivered,
            DeliveryStatus::Failed,
            DeliveryStatus::Expired,
        ] {
            assert_eq!(
                apply_outcome(s, 1, &failed(), t0()),
                Err(NotApplied::AlreadyTerminal)
            );
        }
    }

    /// design §4.4's own arithmetic: total retry span stays under the
    /// shortest period a module row can have (§6.3's 60s floor).
    #[test]
    fn worst_case_retry_span_is_under_the_minimum_period() {
        let span = DELIVERY_TIMEOUT * MAX_SENT_ATTEMPTS + RETRY_BACKOFF[0] + RETRY_BACKOFF[1];
        assert!(span < Duration::from_secs(60), "{span:?}");
    }

    /// A module target answering `AgentRun` (kernel bug) is treated as a
    /// failed attempt, never trusted and never a panic.
    #[test]
    fn an_agent_run_outcome_for_a_module_target_is_a_failed_attempt() {
        let a = apply_outcome(
            DeliveryStatus::Pending,
            0,
            &FireOutcome::AgentRun {
                run_id: "run_x".into(),
            },
            t0(),
        )
        .unwrap();
        assert_eq!(a.status, DeliveryStatus::Pending);
        assert!(
            a.last_error
                .as_deref()
                .unwrap_or_default()
                .contains("kernel bug")
        );
    }

    #[test]
    fn skip_owners_json_only_lists_owners_still_within_the_recheck_window() {
        let now = t0();
        let mut state = PumpState::new(now);
        state
            .owner_skip_until
            .insert("stale".into(), now - chrono::Duration::seconds(1));
        state
            .owner_skip_until
            .insert("fresh".into(), now + chrono::Duration::seconds(1));
        let json = state.skip_owners_json(now);
        let parsed: Vec<String> = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, vec!["fresh".to_owned()]);
    }
}
