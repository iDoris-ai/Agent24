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
//!
//! # Review round 1, L4: the real cost of a dead owner, and of a full window
//!
//! A dead (or merely unreachable) module owner is probed at most once per
//! [`DEFER_RECHECK`] (2s) — [`PumpState::owner_skip_until`] leaves it out of
//! every `due_deliveries` query in between. Each of those probes can pick up
//! at most [`PER_OWNER_IN_FLIGHT`] (4) of that owner's rows; the rest of its
//! backlog, however large, costs nothing until the next window. So a dead
//! owner with, say, 200 outstanding module schedules still costs the pump
//! only ONE query and at most 4 attempts every 2 seconds — never more, and
//! never a write for the ones it does not even look at (T4).
//!
//! The other side of the same knob: [`GLOBAL_IN_FLIGHT`] (16) is a SHARED
//! window across every owner. With, say, 6 busy owners each contributing up
//! to 4 in-flight attempts, the window is exactly full — a 7th owner's rows,
//! however old, wait behind it. The design accepts this as a bounded
//! (not eliminated) worst case, not a fairness guarantee: `due_deliveries`'s
//! own `ORDER BY scheduled_for, fire_id` (its own store-layer test, C1.10,
//! proves the per-owner partitioning is fair WITHIN one query) means an
//! owner whose rows are newest keeps losing the global LIMIT to older ones
//! until they clear — at [`DELIVERY_TIMEOUT`]'s 10s ceiling per attempt, a
//! full window of slow/stuck attempts can make a fresh arrival wait up to
//! roughly that long before its very first attempt starts.

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

/// Review round 1, L2: how long, after a write to `schedule_deliveries`
/// itself fails (a storage error, not a delivery outcome), the pump leaves
/// that ONE schedule out of its due query — the same shape as
/// [`DEFER_RECHECK`], for the same reason: without it, a database that is
/// erroring on every write gets hammered every [`PUMP_INTERVAL`] for the
/// exact schedule that just failed, instead of backing off briefly.
pub const DB_WRITE_ERROR_RETRY_AFTER: Duration = Duration::from_secs(2);

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
    /// Review round 1, **M1**: `JoinSet::try_join_next_with_id`'s `Err` arm
    /// (a panicked attempt) only carries a [`tokio::task::Id`] — not the
    /// `DueDelivery` the panicking task was working on. Without this map,
    /// there is no way to find which schedule's slot to release, and that
    /// schedule's `in_flight_schedules`/`in_flight_per_owner` entries would
    /// never be cleared: the row becomes permanently unfetchable (reviewer's
    /// reproduction: one crash on a schedule's first-ever attempt wedges it
    /// forever, and four crashes across one owner wedge the WHOLE owner,
    /// since `PER_OWNER_IN_FLIGHT` slots never free up either). Populated
    /// the moment a task is spawned, removed the moment it is joined
    /// (`Ok` or `Err`).
    in_flight_task_owners: HashMap<tokio::task::Id, (String, String)>,
    in_flight_per_owner: HashMap<String, usize>,
    /// design §5.4: "同一 schedule 同时至多一个在途尝试".
    in_flight_schedules: HashSet<String>,
    /// design §5.4: an owner that answered `Deferred` this recently is left
    /// out of the next `due_deliveries` query. Bounded, real cost of a dead
    /// module (review round 1, **L4**): a dead owner is probed at most once
    /// per [`DEFER_RECHECK`] (2s), and that one probe can pick up at most
    /// [`PER_OWNER_IN_FLIGHT`] (4) of its rows — the rest of that owner's
    /// backlog, however large, costs nothing between recheck windows.
    owner_skip_until: HashMap<String, DateTime<Utc>>,
    /// Review round 1, **L2**: a schedule whose LAST WRITE to
    /// `schedule_deliveries` itself failed (a storage error, not a delivery
    /// outcome) is left out of the due query for [`DB_WRITE_ERROR_RETRY_
    /// AFTER`] — otherwise a database erroring on every write gets hammered
    /// once per [`PUMP_INTERVAL`] for that exact schedule.
    schedule_retry_after: HashMap<String, DateTime<Utc>>,
    last_sweep: DateTime<Utc>,
}

impl PumpState {
    fn new(now: DateTime<Utc>) -> Self {
        Self {
            tasks: JoinSet::new(),
            in_flight_task_owners: HashMap::new(),
            in_flight_per_owner: HashMap::new(),
            in_flight_schedules: HashSet::new(),
            owner_skip_until: HashMap::new(),
            schedule_retry_after: HashMap::new(),
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

    fn begin_attempt(&mut self, schedule_id: &str, owner_module: &str) {
        self.in_flight_schedules.insert(schedule_id.to_owned());
        *self
            .in_flight_per_owner
            .entry(owner_module.to_owned())
            .or_insert(0) += 1;
    }

    /// `deferred`: whether this attempt's outcome was `Deferred` (bumps
    /// [`Self::owner_skip_until`]) — a panicked attempt (review round 1, M1)
    /// passes `false`, the same as a real sent outcome, since a kernel-side
    /// panic says nothing about whether the MODULE is reachable.
    fn end_attempt(
        &mut self,
        schedule_id: &str,
        owner_module: &str,
        now: DateTime<Utc>,
        deferred: bool,
    ) {
        self.in_flight_schedules.remove(schedule_id);
        if let Some(count) = self.in_flight_per_owner.get_mut(owner_module) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight_per_owner.remove(owner_module);
            }
        }
        if deferred {
            self.owner_skip_until.insert(
                owner_module.to_owned(),
                now + chrono::Duration::from_std(DEFER_RECHECK).unwrap_or_default(),
            );
        } else {
            self.owner_skip_until.remove(owner_module);
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
    ///
    /// Review round 1, **L1**: `biased;`, with the cancellation branch
    /// listed FIRST — a pending cancellation is honoured before a
    /// simultaneously-ready sleep/notify wakes the loop for another round —
    /// and, on cancellation, every attempt that ALREADY finished is drained
    /// and applied before returning, so a result that landed a moment
    /// before shutdown is not silently discarded; only attempts genuinely
    /// still running are left for the `JoinSet`'s `Drop` to abort.
    pub async fn run(self, clock: Arc<dyn Clock>, cancel: CancellationToken) {
        tracing::info!("delivery pump started ({PUMP_INTERVAL:?} cadence)");
        let mut state = PumpState::new(clock.now());
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => {
                    let now = clock.now();
                    while let Some(joined) = state.tasks.try_join_next_with_id() {
                        self.apply_joined(&mut state, joined, now).await;
                    }
                    tracing::info!(
                        "delivery pump stopped; {} attempt(s) still in flight will be aborted — \
                         their rows stay pending/deferred for the next start",
                        state.tasks.len()
                    );
                    return;
                }
                () = clock.sleep(PUMP_INTERVAL) => {}
                () = self.scheduler.delivery_notify().notified() => {}
            }
            let now = clock.now();
            while let Some(joined) = state.tasks.try_join_next_with_id() {
                self.apply_joined(&mut state, joined, now).await;
            }
            self.fetch_and_spawn(&mut state, now).await;
        }
    }

    /// One joined task's result — successful (apply its outcome as before)
    /// or panicked (review round 1, **M1**: release the schedule's slot so
    /// it is fetched again on the next round; the row itself is left
    /// untouched, exactly like a cancelled attempt — a panic inside
    /// `trigger()` is a kernel-side bug, not a real sent attempt against the
    /// module's own failure budget).
    async fn apply_joined(
        &self,
        state: &mut PumpState,
        joined: Result<(tokio::task::Id, AttemptDone), tokio::task::JoinError>,
        now: DateTime<Utc>,
    ) {
        match joined {
            Ok((id, done)) => {
                state.in_flight_task_owners.remove(&id);
                self.apply_attempt(state, done, now).await;
            }
            Err(join_err) => {
                let id = join_err.id();
                if let Some((schedule_id, owner_module)) = state.in_flight_task_owners.remove(&id) {
                    state.end_attempt(&schedule_id, &owner_module, now, false);
                    tracing::error!(
                        "delivery pump: attempt for schedule {schedule_id} (owner {owner_module}) \
                         panicked: {join_err}; its slot was released, the row is untouched and \
                         will be retried"
                    );
                } else {
                    tracing::error!(
                        "delivery pump: an attempt task panicked with no known schedule/owner \
                         ({join_err}) — this should be unreachable (every spawned task is \
                         recorded before it can panic)"
                    );
                }
            }
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
            // Review round 1, L2: a schedule whose last DB write errored is
            // left alone for a short while, independent of the owner cache
            // above (a write can fail for reasons that have nothing to do
            // with the module being unreachable).
            if state
                .schedule_retry_after
                .get(&row.schedule_id)
                .is_some_and(|until| *until > now)
            {
                continue;
            }
            let schedule_id = row.schedule_id.clone();
            let owner_module = row.owner_module.clone();
            state.begin_attempt(&schedule_id, &owner_module);
            let trigger = Arc::clone(self.scheduler.trigger());
            let abort_handle = state.tasks.spawn(run_attempt(trigger, row));
            state
                .in_flight_task_owners
                .insert(abort_handle.id(), (schedule_id, owner_module));
        }
    }

    async fn apply_attempt(&self, state: &mut PumpState, done: AttemptDone, now: DateTime<Utc>) {
        let AttemptDone { row, outcome } = done;
        let deferred = matches!(outcome, FireOutcome::Deferred { .. });
        state.end_attempt(&row.schedule_id, &row.owner_module, now, deferred);

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
                // Review round 1, L2: a short in-memory backoff for THIS
                // schedule, so a database erroring on every write is not
                // hammered once per `PUMP_INTERVAL` for the exact row that
                // just failed.
                state.schedule_retry_after.insert(
                    row.schedule_id.clone(),
                    now + chrono::Duration::from_std(DB_WRITE_ERROR_RETRY_AFTER)
                        .unwrap_or_default(),
                );
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

/// design §11 C4, review round 1 M1/M4/L6: the delivery pump
/// (`DeliveryPump`) driven end to end against a REAL `agent24_store::Store`
/// and scripted `RunTrigger`s — the pump's own concurrency/retry/
/// cancellation/panic-recovery behaviour, independent of the transport
/// (`kernel_call`'s revoke races are covered in `agent24-os-proto`; the
/// `ModuleDeliverer`/`classify` mapping and the C4.1 end-to-end shape are
/// covered in `agent24d::scheduler_deliver`).
///
/// Review round 1 relocated these tests here from `agent24d::scheduler_
/// deliver::pump_tests`: `DeliveryPump` is entirely this crate's own type,
/// and nothing below needs a real `Supervisors`/`Generation`/UDS module —
/// only a scripted `RunTrigger`, which this crate can build without agent24d
/// at all.
#[cfg(test)]
mod pump_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::next_fire::next_fire;
    use crate::{DeferReason, RunTrigger};
    use agent24_store::{ModuleScheduleDesired, Store};
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};
    use std::time::Instant;

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
                    reason: DeferReason::NotRunning,
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

    /// Review round 1, **M1**: panics on its FIRST call, delivers on every
    /// call after that.
    struct PanicOnce {
        calls: AtomicUsize,
    }

    impl PanicOnce {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
            })
        }
        fn calls(&self) -> usize {
            self.calls.load(AtomicOrdering::SeqCst)
        }
    }

    #[async_trait]
    impl RunTrigger for PanicOnce {
        async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
            let k = self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            if k == 0 {
                panic!("PanicOnce: injected panic on the first attempt");
            }
            let InvocationTarget::Module { fire_id, .. } = &invocation.target else {
                panic!("PanicOnce is only exercised with Module targets in these tests");
            };
            FireOutcome::ModuleDelivered {
                fire_id: fire_id.clone(),
            }
        }
    }

    /// Review round 1, **M4**: records, per call, how many attempts are
    /// concurrently inside `trigger()` — globally and per owner — and blocks
    /// there until [`Self::release`] is called, so a test can observe the
    /// PEAK concurrency the pump actually reaches before anything completes.
    struct ConcurrencyProbe {
        global_in_flight: AtomicUsize,
        global_peak: AtomicUsize,
        per_owner: StdMutex<HashMap<String, (usize, usize)>>, // (current, peak)
        hold: AtomicBool,
    }

    impl ConcurrencyProbe {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                global_in_flight: AtomicUsize::new(0),
                global_peak: AtomicUsize::new(0),
                per_owner: StdMutex::new(HashMap::new()),
                hold: AtomicBool::new(true),
            })
        }
        fn release(&self) {
            self.hold.store(false, AtomicOrdering::SeqCst);
        }
        fn current_global(&self) -> usize {
            self.global_in_flight.load(AtomicOrdering::SeqCst)
        }
        fn global_peak(&self) -> usize {
            self.global_peak.load(AtomicOrdering::SeqCst)
        }
        fn owner_peak(&self, owner: &str) -> usize {
            self.per_owner
                .lock()
                .unwrap()
                .get(owner)
                .map_or(0, |(_, peak)| *peak)
        }
    }

    #[async_trait]
    impl RunTrigger for ConcurrencyProbe {
        async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
            let InvocationTarget::Module { owner, fire_id } = &invocation.target else {
                panic!("ConcurrencyProbe is only exercised with Module targets in these tests");
            };
            let g = self.global_in_flight.fetch_add(1, AtomicOrdering::SeqCst) + 1;
            self.global_peak.fetch_max(g, AtomicOrdering::SeqCst);
            {
                let mut owners = self.per_owner.lock().unwrap();
                let entry = owners.entry(owner.owner_module.clone()).or_insert((0, 0));
                entry.0 += 1;
                entry.1 = entry.1.max(entry.0);
            }
            while self.hold.load(AtomicOrdering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            self.global_in_flight.fetch_sub(1, AtomicOrdering::SeqCst);
            if let Some(entry) = self.per_owner.lock().unwrap().get_mut(&owner.owner_module) {
                entry.0 = entry.0.saturating_sub(1);
            }
            FireOutcome::ModuleDelivered {
                fire_id: fire_id.clone(),
            }
        }
    }

    /// Review round 1, C4.5 (second half): blocks ONLY the one named
    /// schedule (standing in for a slow/timing-out delivery); every other
    /// schedule delivers immediately.
    struct SelectiveBlock {
        blocked_schedule: String,
        hold: AtomicBool,
        blocked_started: tokio::sync::Notify,
    }

    impl SelectiveBlock {
        fn new(blocked_schedule: String) -> Arc<Self> {
            Arc::new(Self {
                blocked_schedule,
                hold: AtomicBool::new(true),
                blocked_started: tokio::sync::Notify::new(),
            })
        }
        fn release(&self) {
            self.hold.store(false, AtomicOrdering::SeqCst);
        }
    }

    #[async_trait]
    impl RunTrigger for SelectiveBlock {
        async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
            let InvocationTarget::Module { fire_id, .. } = &invocation.target else {
                panic!("SelectiveBlock is only exercised with Module targets in these tests");
            };
            if invocation.schedule_id == self.blocked_schedule {
                self.blocked_started.notify_one();
                while self.hold.load(AtomicOrdering::SeqCst) {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                return FireOutcome::Failed {
                    reason: "simulated timeout".to_owned(),
                };
            }
            FireOutcome::ModuleDelivered {
                fire_id: fire_id.clone(),
            }
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

    /// Review round 1, **L7**: an async CONDITION (not `futures::executor::
    /// block_on` inside a sync closure) — bounded polling, never a real sleep
    /// the test's own correctness depends on: `condition` is awaited
    /// immediately and then at a short real interval (irrelevant to
    /// `TestClock`'s virtual time) until `deadline` is hit, at which point
    /// this panics with `on_timeout`'s message.
    async fn wait_until<F, Fut>(mut condition: F, on_timeout: &str)
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if condition().await {
                return;
            }
            assert!(Instant::now() < deadline, "{on_timeout}");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Review round 1, **L6**: `attempts`/`updated_at` are not part of
    /// `ModuleScheduleState`'s public read model (`list_module_schedules`
    /// only exposes `fire_id`/`scheduled_for`/`status`/`last_error`) — read
    /// them through `agent24_store::test_hooks::pool`, the crate's own
    /// documented "test-only escape hatch" for exactly this.
    async fn fetch_attempts_and_updated_at(store: &Store, fire_id: &str) -> (i64, String) {
        let pool = agent24_store::test_hooks::pool(store);
        sqlx::query_as::<_, (i64, String)>(
            "SELECT attempts, updated_at FROM schedule_deliveries WHERE fire_id = ?",
        )
        .bind(fire_id)
        .fetch_one(pool)
        .await
        .unwrap()
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
            fire_id: FireId::from_stored("placeholder".into()),
        }]);
        let (scheduler, events) = scheduler_with(store.clone(), trigger as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-a", "k", now0).await;

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle = tokio::spawn(pump.run(clock as Arc<dyn Clock>, cancel.child_token()));

        wait_until(
            || async {
                let states = store.list_module_schedules("mod-a").await.unwrap();
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
    /// different `fire_id`) is asserted in the same test. Review round 1,
    /// **L6**: also asserts `attempts == 3` directly off the row (not just
    /// inferred from `status == "failed"`).
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
                fire_id: FireId::from_stored("must-not-be-reached".into()),
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
                || async {
                    let states = store.list_module_schedules(owner).await.unwrap();
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
            || async { trigger.calls().len() >= 3 },
            "the third attempt never happened",
        )
        .await;

        wait_until(
            || async {
                let states = store.list_module_schedules("mod-b").await.unwrap();
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
        let fire_id = calls[0].fire_id.clone();
        assert!(
            calls.iter().all(|c| c.fire_id == fire_id),
            "every retry of one fire must carry the exact same fire_id: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .all(|c| c.scheduled_for == calls[0].scheduled_for
                    && c.fired_at == calls[0].fired_at),
            "every retry of one fire must carry the exact same scheduled_for/fired_at: {calls:?}"
        );

        // Review round 1, L6: attempts is 3, read directly off the row.
        let (attempts, _) = fetch_attempts_and_updated_at(&store, &fire_id).await;
        assert_eq!(attempts, 3, "the failed row must record exactly 3 attempts");

        cancel.cancel();
        handle.await.unwrap();

        // Positive control: a later, DIFFERENT slot gets a different fire_id.
        let trigger2 = ScriptedTrigger::new([FireOutcome::ModuleDelivered {
            fire_id: FireId::from_stored("placeholder".into()),
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
            || async { !trigger2.calls().is_empty() },
            "the next slot's fire never attempted",
        )
        .await;
        cancel2.cancel();
        handle2.await.unwrap();
        assert_ne!(
            trigger2.calls()[0].fire_id,
            fire_id,
            "a different slot must never reuse the same fire_id"
        );
    }

    /// Review round 1, **L6**: the OTHER half of C4.3 — a schedule that
    /// already carries a NON-ZERO `consecutive_failures` (from an earlier,
    /// fully-failed fire) is reset back to 0 the moment a LATER fire's
    /// second attempt lands 2xx. This is the positive control the original
    /// three-failures test never exercised (it only ever drove one fire to
    /// `failed`, on a schedule that started at 0).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_later_fires_second_attempt_success_resets_a_nonzero_failure_count() {
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
        ]);
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-reset", "k", now0).await;
        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        async fn wait_for_last_error(store: &Store, owner: &str, expected: &str) {
            wait_until(
                || async {
                    let states = store.list_module_schedules(owner).await.unwrap();
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
        wait_for_last_error(&store, "mod-reset", "boom 1").await;
        clock.set(clock.now() + chrono::Duration::seconds(6));
        wait_for_last_error(&store, "mod-reset", "boom 2").await;
        clock.set(clock.now() + chrono::Duration::seconds(16));
        wait_until(
            || async {
                let states = store.list_module_schedules("mod-reset").await.unwrap();
                states[0]
                    .last_fire
                    .tick
                    .as_ref()
                    .is_some_and(|f| f.status == "failed")
            },
            "the first fire never reached failed",
        )
        .await;
        // Pre-condition: `consecutive_failures` is genuinely non-zero now.
        let before = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(
            before.consecutive_failures, 1,
            "the pre-condition itself failed"
        );
        cancel.cancel();
        handle.await.unwrap();

        // A LATER fire: fails once, then succeeds on the second attempt.
        let trigger2 = ScriptedTrigger::new([
            FireOutcome::Failed {
                reason: "boom again".into(),
            },
            FireOutcome::ModuleDelivered {
                fire_id: FireId::from_stored("placeholder".into()),
            },
        ]);
        let (scheduler2, _events2) = scheduler_with(store.clone(), trigger2 as Arc<dyn RunTrigger>);
        let now1 = now0 + chrono::Duration::seconds(200);
        assert_eq!(scheduler2.tick(now1).await.unwrap(), 1);
        let clock2 = TestClock::at(now1);
        let cancel2 = CancellationToken::new();
        let pump2 = DeliveryPump::new(Arc::clone(&scheduler2));
        let handle2 =
            tokio::spawn(pump2.run(Arc::clone(&clock2) as Arc<dyn Clock>, cancel2.child_token()));
        wait_for_last_error(&store, "mod-reset", "boom again").await;
        clock2.set(clock2.now() + chrono::Duration::seconds(6));
        wait_until(
            || async {
                let states = store.list_module_schedules("mod-reset").await.unwrap();
                states[0]
                    .last_fire
                    .tick
                    .as_ref()
                    .is_some_and(|f| f.status == "delivered")
            },
            "the second fire's retry never delivered",
        )
        .await;
        cancel2.cancel();
        handle2.await.unwrap();

        let after = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(
            after.consecutive_failures, 0,
            "a delivered fire must reset a PRE-EXISTING non-zero failure count back to 0"
        );
    }

    /// design §4.1/§5.3/§9, judgement **C4.4**: the module being unreachable
    /// (`Deferred`) never counts as a failure and never stops — the row is
    /// picked up again once the module comes back, still carrying the SAME
    /// `fire_id`. Also stands in for **C4.10** (the pure `apply_outcome`
    /// "repeating Deferred writes nothing" contract is unit-tested directly
    /// in `apply_outcome`'s own tests above; here the pump-level effect —
    /// `consecutive_failures` never moves while the module stays
    /// unavailable, however many rounds it takes — is what's under test).
    /// Review round 1, **L6**: also asserts `updated_at` is UNCHANGED across
    /// the repeated-Deferred rounds (T4 really writes nothing).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unavailable_module_never_counts_as_a_failure_and_recovers_with_the_same_fire_id() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::Deferred {
            reason: DeferReason::NotRunning,
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

        // Review round 1, L6: `updated_at` must NOT have moved across those
        // repeated Deferred rounds (T4: repeating Deferred from Deferred
        // writes nothing at all).
        let (_, updated_at_before) = fetch_attempts_and_updated_at(&store, &stable_fire_id).await;
        tokio::time::sleep(Duration::from_millis(30)).await;
        let (_, updated_at_after) = fetch_attempts_and_updated_at(&store, &stable_fire_id).await;
        assert_eq!(
            updated_at_before, updated_at_after,
            "repeated Deferred rounds must not touch updated_at"
        );

        // The module "comes back": swap in a trigger that delivers.
        trigger.outcomes.lock().unwrap().clear();
        trigger
            .outcomes
            .lock()
            .unwrap()
            .push_back(FireOutcome::ModuleDelivered {
                fire_id: FireId::from_stored(stable_fire_id.clone()),
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
    /// redelivers with the EXACT SAME `fire_id`, and it succeeds. Review
    /// round 1, **L6**: a THIRD "restart" (a trigger that panics if ever
    /// called) proves the now-`delivered` row is NOT retried again.
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
            fire_id: FireId::from_stored(fire_id_before.clone()),
        }]);
        let (scheduler2, _events2) = scheduler_with(store.clone(), trigger2 as Arc<dyn RunTrigger>);
        let clock2 = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel2 = CancellationToken::new();
        let pump2 = DeliveryPump::new(Arc::clone(&scheduler2));
        let handle2 = tokio::spawn(pump2.run(clock2 as Arc<dyn Clock>, cancel2.child_token()));
        wait_until(
            || async {
                let states = store.list_module_schedules("mod-d").await.unwrap();
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

        // Review round 1, L6: a THIRD "restart" must never call trigger()
        // again — the row is `delivered`, a terminal state.
        struct PanicIfCalled;
        #[async_trait]
        impl RunTrigger for PanicIfCalled {
            async fn trigger(&self, _invocation: &ScheduleInvocation) -> FireOutcome {
                panic!("a delivered (terminal) row must never be retried");
            }
        }
        let (scheduler3, _events3) = scheduler_with(
            store.clone(),
            Arc::new(PanicIfCalled) as Arc<dyn RunTrigger>,
        );
        let clock3 = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel3 = CancellationToken::new();
        let pump3 = DeliveryPump::new(Arc::clone(&scheduler3));
        let handle3 = tokio::spawn(pump3.run(clock3 as Arc<dyn Clock>, cancel3.child_token()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel3.cancel();
        handle3.await.unwrap();
        let states = store.list_module_schedules("mod-d").await.unwrap();
        assert_eq!(
            states[0].last_fire.tick.as_ref().unwrap().status,
            "delivered",
            "still delivered — the panic-if-called trigger was never reached"
        );
    }

    /// Review round 1, **M1** (the reviewer's own reproduction): an attempt
    /// that PANICS must release its schedule's in-flight slot — otherwise
    /// the schedule (and, after enough panics across one owner, the whole
    /// owner) is wedged forever, since the slot the panicking task held is
    /// never freed. Mutation: revert `apply_joined`'s `Err` arm to only log
    /// (no `end_attempt`) — this test times out (the schedule never gets a
    /// second attempt).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_panicking_attempt_releases_its_schedule_slot_for_the_next_try() {
        let store = Store::open_memory().await.unwrap();
        let trigger = PanicOnce::new();
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        seed_module_fire(&scheduler, &store, "mod-p", "k", now0).await;
        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let states = store.list_module_schedules("mod-p").await.unwrap();
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
                "the schedule never recovered after the panic — its slot must have leaked"
            );
            clock.set(clock.now() + chrono::Duration::seconds(1));
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            trigger.calls() >= 2,
            "the panicking attempt must not have been the only one"
        );
        cancel.cancel();
        handle.await.unwrap();
    }

    /// Review round 1, **M4**: 6 owners × 5 schedules each, all due at once,
    /// against a trigger that blocks every attempt until released — proves
    /// the pump's own concurrency ceilings from the ACTUAL peak reached, not
    /// from reasoning about the code: global in-flight never exceeds
    /// [`GLOBAL_IN_FLIGHT`] even with 30 eligible rows, and no single
    /// owner's in-flight count ever exceeds [`PER_OWNER_IN_FLIGHT`].
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrency_ceilings_hold_across_thirty_schedules_and_six_owners() {
        let store = Store::open_memory().await.unwrap();
        let probe = ConcurrencyProbe::new();
        let (scheduler, _events) =
            scheduler_with(store.clone(), probe.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        const OWNERS: usize = 6;
        const SCHEDULES_PER_OWNER: usize = 5;
        for owner_n in 0..OWNERS {
            for key_n in 0..SCHEDULES_PER_OWNER {
                seed_module_fire(
                    &scheduler,
                    &store,
                    &format!("owner-{owner_n}"),
                    &format!("k{key_n}"),
                    now0,
                )
                .await;
            }
        }
        let total = OWNERS * SCHEDULES_PER_OWNER;
        assert_eq!(total, 30);

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        let expected_ceiling = GLOBAL_IN_FLIGHT.min(total);
        wait_until(
            || {
                let current = probe.current_global();
                async move { current == expected_ceiling }
            },
            "the pump never reached the expected global concurrency ceiling",
        )
        .await;
        // A moment more, to let any (wrong) extra spawn show up before the
        // assertions below.
        tokio::time::sleep(Duration::from_millis(20)).await;

        assert_eq!(
            probe.current_global(),
            expected_ceiling,
            "in-flight must not exceed the global ceiling"
        );
        assert!(
            probe.global_peak() <= GLOBAL_IN_FLIGHT,
            "peak global in-flight ({}) must never exceed GLOBAL_IN_FLIGHT ({GLOBAL_IN_FLIGHT})",
            probe.global_peak()
        );
        for owner_n in 0..OWNERS {
            let owner = format!("owner-{owner_n}");
            assert!(
                probe.owner_peak(&owner) <= PER_OWNER_IN_FLIGHT,
                "owner {owner}'s peak in-flight ({}) must never exceed PER_OWNER_IN_FLIGHT \
                 ({PER_OWNER_IN_FLIGHT})",
                probe.owner_peak(&owner)
            );
        }

        probe.release();
        cancel.cancel();
        handle.await.unwrap();
    }

    /// Review round 1, **M4**, the surgical half: the concurrency test above
    /// proves the pump never OBSERVABLY exceeds the ceilings across many
    /// real rounds, but with a single simultaneous batch the SQL's own
    /// per-owner `ROW_NUMBER` cap (`due_deliveries`'s `?3` parameter) happens
    /// to enforce the same limit on its own for a first fetch, which would
    /// let a removed CLIENT-SIDE per-owner check hide behind it. This test
    /// isolates the client-side check directly: `fetch_and_spawn` is called
    /// with `PumpState.in_flight_per_owner` PRE-SET to the cap (as if 4
    /// attempts from an EARLIER round are still unresolved) — a live
    /// invariant `due_deliveries` cannot see on its own, since an in-flight
    /// row's status/`next_attempt_at` are untouched in the DB until its
    /// outcome is applied. A 5th, brand-new schedule for the SAME owner must
    /// not be spawned.
    ///
    /// Mutation: delete the `in_flight_per_owner` cap check in
    /// `fetch_and_spawn` — this test goes red (the 5th schedule IS spawned).
    #[tokio::test]
    async fn fetch_and_spawn_refuses_a_new_schedule_when_the_owner_is_already_at_its_cap() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::Deferred {
            reason: DeferReason::NotRunning,
        }]);
        let (scheduler, _events) = scheduler_with(store.clone(), trigger as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        // Only ONE schedule for this owner is real; `in_flight_per_owner`
        // below stands in for four others already in flight from an
        // earlier round.
        seed_module_fire(&scheduler, &store, "owner-stale", "k-new", now0).await;

        let now = now0 + chrono::Duration::seconds(65);
        let mut state = PumpState::new(now);
        state
            .in_flight_per_owner
            .insert("owner-stale".to_owned(), PER_OWNER_IN_FLIGHT);
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        pump.fetch_and_spawn(&mut state, now).await;
        assert_eq!(
            state.tasks.len(),
            0,
            "a schedule must not be spawned for an owner already at its per-owner cap, even when \
             none of that owner's OTHER in-flight attempts are represented as real rows"
        );
    }

    /// Review round 1, **C4.5 (second half)**: one schedule's slow delivery
    /// must never block another's — proven by holding one schedule's
    /// attempt open while a SECOND, unrelated schedule (same owner, so they
    /// also share the per-owner slot pool) delivers normally in the
    /// meantime.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_schedules_slow_delivery_does_not_block_another() {
        let store = Store::open_memory().await.unwrap();
        let now0 = utc("2026-08-01T00:00:00Z");
        // Seeded first so `seed_module_fire`'s own `SelectiveBlock::new`
        // below can name it before the scheduler exists.
        let slow_id = "sch_owner-mixed_slow".to_owned();
        let trigger = SelectiveBlock::new(slow_id.clone());
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let seeded_slow_id =
            seed_module_fire(&scheduler, &store, "owner-mixed", "slow", now0).await;
        assert_eq!(
            seeded_slow_id, slow_id,
            "the predicted schedule id must match the real one"
        );
        seed_module_fire(&scheduler, &store, "owner-mixed", "fast", now0).await;

        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        tokio::time::timeout(Duration::from_secs(5), trigger.blocked_started.notified())
            .await
            .expect("the slow schedule's attempt never started");

        // The fast schedule must deliver WHILE the slow one is still stuck.
        wait_until(
            || async {
                let states = store.list_module_schedules("owner-mixed").await.unwrap();
                states
                    .iter()
                    .find(|s| s.key == "fast")
                    .and_then(|s| s.last_fire.tick.as_ref())
                    .is_some_and(|f| f.status == "delivered")
            },
            "the fast schedule never delivered while the slow one was blocked",
        )
        .await;
        let states = store.list_module_schedules("owner-mixed").await.unwrap();
        let slow_status = states
            .iter()
            .find(|s| s.key == "slow")
            .and_then(|s| s.last_fire.tick.clone())
            .unwrap()
            .status;
        assert_eq!(
            slow_status, "pending",
            "the slow schedule must still be pending (its attempt is still blocked)"
        );

        trigger.release();
        wait_until(
            || async {
                let states = store.list_module_schedules("owner-mixed").await.unwrap();
                states
                    .iter()
                    .find(|s| s.key == "slow")
                    .and_then(|s| s.last_fire.tick.as_ref())
                    .and_then(|f| f.last_error.as_deref())
                    == Some("simulated timeout")
            },
            "the slow schedule's attempt never completed once released",
        )
        .await;

        cancel.cancel();
        handle.await.unwrap();
    }
}
