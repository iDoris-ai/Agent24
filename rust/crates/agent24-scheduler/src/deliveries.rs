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

/// Review round 2, L-c: the FLOOR of the DB-write-error backoff — a single
/// failure gets this much; each consecutive failure for the SAME schedule
/// doubles it, capped at [`DB_WRITE_ERROR_RETRY_MAX`]. Same shape as
/// [`DEFER_RECHECK`], for the same reason: without it, a database erroring
/// on every write gets hammered every [`PUMP_INTERVAL`] for the exact
/// schedule that just failed, instead of backing off.
pub const DB_WRITE_ERROR_RETRY_AFTER: Duration = Duration::from_secs(2);
/// Review round 2, L-c: the ceiling the exponential backoff above saturates
/// at — a database that is down for a while must not make the pump wait
/// longer and longer forever; a minute is short enough that recovery is
/// still noticed quickly once the database comes back.
pub const DB_WRITE_ERROR_RETRY_MAX: Duration = Duration::from_secs(60);

/// Review round 2, **L-c**: `consecutive_db_failures` (1-indexed: the value
/// AFTER counting the failure that just happened) → how long this schedule
/// is left out of the due query. Doubles each time
/// (`DB_WRITE_ERROR_RETRY_AFTER * 2^(n-1)`), capped at
/// [`DB_WRITE_ERROR_RETRY_MAX`]. A pure function so the doubling/cap
/// arithmetic is directly testable without any database.
#[must_use]
fn db_write_backoff(consecutive_db_failures: u32) -> Duration {
    let shift = consecutive_db_failures.saturating_sub(1).min(6);
    std::cmp::min(
        DB_WRITE_ERROR_RETRY_AFTER * (1u32 << shift),
        DB_WRITE_ERROR_RETRY_MAX,
    )
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
    /// Review round 1, **M1** / round 2, **M-A**: `JoinSet::
    /// try_join_next_with_id`'s `Err` arm (a panicked attempt) only carries a
    /// [`tokio::task::Id`] — not the `DueDelivery` the panicking task was
    /// working on. Without this map, there is no way to find which
    /// schedule's slot to release (round 1's bug), AND no way to route the
    /// panic through [`apply_outcome`] as a real sent failure (round 2's
    /// fix: storing the id-keyed schedule/owner pair alone let a panic keep
    /// the row `pending` forever with no backoff and no failure count — see
    /// [`DeliveryPump::apply_joined`]'s doc comment). Populated the moment a
    /// task is spawned, removed the moment it is joined (`Ok` or `Err`).
    in_flight_task_rows: HashMap<tokio::task::Id, DueDelivery>,
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
    /// Review round 1, **L2** / round 2, **L-c**: a schedule whose LAST
    /// WRITE to `schedule_deliveries` itself failed (a storage error, not a
    /// delivery outcome) is left out of the due query — the `DateTime` is
    /// when it becomes eligible again, the `u32` is how many CONSECUTIVE
    /// write failures this schedule has had (reset to nothing the moment a
    /// write succeeds), which the backoff below doubles against, capped at
    /// [`DB_WRITE_ERROR_RETRY_MAX`] — a database erroring on every write for
    /// a while must not be hammered once per [`PUMP_INTERVAL`] forever.
    schedule_retry_after: HashMap<String, (DateTime<Utc>, u32)>,
    last_sweep: DateTime<Utc>,
}

impl PumpState {
    fn new(now: DateTime<Utc>) -> Self {
        Self {
            tasks: JoinSet::new(),
            in_flight_task_rows: HashMap::new(),
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

    /// Review round 2, **L-b**: drop entries whose window has already
    /// passed, instead of only ever ignoring them at read time — otherwise
    /// every owner/schedule that ever hit a transient defer or a transient
    /// DB error stays in these maps, doing nothing but taking up space, for
    /// as long as the pump runs.
    fn prune_stale_caches(&mut self, now: DateTime<Utc>) {
        self.owner_skip_until.retain(|_, until| *until > now);
        self.schedule_retry_after
            .retain(|_, (until, _)| *until > now);
    }

    fn begin_attempt(&mut self, schedule_id: &str, owner_module: &str) {
        self.in_flight_schedules.insert(schedule_id.to_owned());
        *self
            .in_flight_per_owner
            .entry(owner_module.to_owned())
            .or_insert(0) += 1;
    }

    /// Only the concurrency bookkeeping — never touches
    /// [`Self::owner_skip_until`]. Used directly (skipping [`Self::
    /// end_attempt`]) for a panicked attempt (review round 2, **M-A**): a
    /// kernel-side panic proves nothing about whether the MODULE is
    /// reachable, so it must neither arm a fresh skip window (as a real
    /// `Deferred` would) nor clear an EXISTING one set by a different
    /// schedule's genuine `Deferred` result on the same owner.
    fn release_slot(&mut self, schedule_id: &str, owner_module: &str) {
        self.in_flight_schedules.remove(schedule_id);
        if let Some(count) = self.in_flight_per_owner.get_mut(owner_module) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.in_flight_per_owner.remove(owner_module);
            }
        }
    }

    /// `deferred`: whether this attempt's outcome was `Deferred` (bumps
    /// [`Self::owner_skip_until`]); otherwise any existing window is
    /// cleared — a REAL attempt (sent, or the module answered) proves the
    /// owner is reachable RIGHT NOW.
    fn end_attempt(
        &mut self,
        schedule_id: &str,
        owner_module: &str,
        now: DateTime<Utc>,
        deferred: bool,
    ) {
        self.release_slot(schedule_id, owner_module);
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
    /// Review round 1, **L1** / round 2, **L-a**: `biased;`, with the
    /// cancellation branch listed FIRST — a pending cancellation is honoured
    /// before a simultaneously-ready sleep/notify wakes the loop for
    /// another round; on cancellation, every attempt that ALREADY finished
    /// is drained and applied before returning, so a result that landed a
    /// moment before shutdown is not silently discarded (only attempts
    /// genuinely still running are left for the `JoinSet`'s `Drop` to
    /// abort). The SECOND branch applies a completed attempt's outcome the
    /// INSTANT it finishes, rather than waiting for the next sleep/notify
    /// wake — with a real `PUMP_INTERVAL` (1s) a result that is already
    /// known would otherwise sit unwritten for up to a second.
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
                Some(joined) = state.tasks.join_next_with_id(), if !state.tasks.is_empty() => {
                    self.apply_joined(&mut state, joined, clock.now()).await;
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
    /// or panicked. Review round 2, **M-A**: a panic used to just release
    /// the schedule's slot with NO further consequence — which the reviewer
    /// showed leaves a schedule that panics on every attempt retrying at
    /// roughly the pump's own cadence FOREVER: never reaching `failed`,
    /// never counting against `consecutive_failures`, and — if the panic
    /// happens after the bytes were dispatched — redelivering to the module
    /// every round too. That also contradicted this file's own rule
    /// (`apply_outcome`, `AgentRun` for a module target) that a kernel-side
    /// mistake is classified as a real sent failure, not trusted or
    /// silently retried. Fixed: a panic is now routed through
    /// [`Self::apply_attempt`] as `FireOutcome::Failed{reason: "kernel bug:
    /// attempt panicked"}` — subject to the exact same `MAX_SENT_ATTEMPTS`/
    /// backoff/disable rules as a real failure — with `touch_skip_window =
    /// false` (a panic says nothing about the MODULE's reachability, so it
    /// must not touch the owner's skip cache either way).
    async fn apply_joined(
        &self,
        state: &mut PumpState,
        joined: Result<(tokio::task::Id, AttemptDone), tokio::task::JoinError>,
        now: DateTime<Utc>,
    ) {
        match joined {
            Ok((id, done)) => {
                state.in_flight_task_rows.remove(&id);
                self.apply_attempt(state, done, now, true).await;
            }
            Err(join_err) => {
                let id = join_err.id();
                if let Some(row) = state.in_flight_task_rows.remove(&id) {
                    tracing::error!(
                        "delivery pump: attempt for schedule {} (owner {}) panicked: {join_err}; \
                         classified as a sent failure (kernel bug) — subject to the normal \
                         retry/failure budget, not retried forever",
                        row.schedule_id,
                        row.owner_module
                    );
                    let done = AttemptDone {
                        row,
                        outcome: FireOutcome::Failed {
                            reason: "kernel bug: attempt panicked".to_owned(),
                        },
                    };
                    self.apply_attempt(state, done, now, false).await;
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
        // Review round 2, L-b: drop expired skip/backoff entries before
        // using either map, not just at read time.
        state.prune_stale_caches(now);
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
            // Review round 1, L2 / round 2, L-c: a schedule whose last DB
            // write errored is left alone for an exponentially growing
            // while, independent of the owner cache above (a write can fail
            // for reasons that have nothing to do with the module being
            // unreachable).
            if state
                .schedule_retry_after
                .get(&row.schedule_id)
                .is_some_and(|(until, _)| *until > now)
            {
                continue;
            }
            let schedule_id = row.schedule_id.clone();
            let owner_module = row.owner_module.clone();
            state.begin_attempt(&schedule_id, &owner_module);
            let row_for_panic = row.clone();
            let trigger = Arc::clone(self.scheduler.trigger());
            let abort_handle = state.tasks.spawn(run_attempt(trigger, row));
            state
                .in_flight_task_rows
                .insert(abort_handle.id(), row_for_panic);
        }
    }

    /// `touch_skip_window`: `false` only for a panicked attempt (review
    /// round 2, **M-A** — see [`Self::apply_joined`]'s doc comment); `true`
    /// for every real attempt outcome.
    async fn apply_attempt(
        &self,
        state: &mut PumpState,
        done: AttemptDone,
        now: DateTime<Utc>,
        touch_skip_window: bool,
    ) {
        let AttemptDone { row, outcome } = done;
        if touch_skip_window {
            let deferred = matches!(outcome, FireOutcome::Deferred { .. });
            state.end_attempt(&row.schedule_id, &row.owner_module, now, deferred);
        } else {
            state.release_slot(&row.schedule_id, &row.owner_module);
        }

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
                // Review round 2, L-c: a write that lands resets this
                // schedule's consecutive-DB-failure count — the database is
                // demonstrably working for it again right now.
                state.schedule_retry_after.remove(&row.schedule_id);
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
                // Review round 2, L-c: an EXPONENTIALLY growing backoff for
                // THIS schedule (capped at `DB_WRITE_ERROR_RETRY_MAX`), not a
                // flat one — a database that stays down must not be hammered
                // at a constant rate forever. Note what this does NOT do:
                // `attempts` is not incremented and no delivery outcome is
                // recorded — the module-facing "3 sent attempts" budget is
                // untouched by a storage failure that never durably landed
                // anything; only the PUMP's own polling rate backs off.
                let count = {
                    let entry = state
                        .schedule_retry_after
                        .entry(row.schedule_id.clone())
                        .or_insert((now, 0));
                    entry.1 = entry.1.saturating_add(1);
                    entry.1
                };
                let backoff = db_write_backoff(count);
                let until = now + chrono::Duration::from_std(backoff).unwrap_or_default();
                state
                    .schedule_retry_after
                    .insert(row.schedule_id.clone(), (until, count));
                tracing::error!(
                    "delivery pump: could not apply the outcome for fire {} (consecutive DB \
                     write failure #{count} for this schedule; backing off {backoff:?}; \
                     attempts was NOT incremented — this outcome was never durably recorded): \
                     {err}",
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

    /// Review round 2, **L-c**: the DB-write-failure backoff doubles each
    /// consecutive failure and saturates at `DB_WRITE_ERROR_RETRY_MAX`,
    /// rather than staying flat at `DB_WRITE_ERROR_RETRY_AFTER` forever.
    #[test]
    fn db_write_backoff_doubles_and_saturates() {
        assert_eq!(db_write_backoff(1), DB_WRITE_ERROR_RETRY_AFTER);
        assert_eq!(db_write_backoff(2), DB_WRITE_ERROR_RETRY_AFTER * 2);
        assert_eq!(db_write_backoff(3), DB_WRITE_ERROR_RETRY_AFTER * 4);
        assert_eq!(db_write_backoff(4), DB_WRITE_ERROR_RETRY_AFTER * 8);
        // 2s * 2^5 = 64s > the 60s cap.
        assert_eq!(db_write_backoff(6), DB_WRITE_ERROR_RETRY_MAX);
        assert_eq!(db_write_backoff(100), DB_WRITE_ERROR_RETRY_MAX);
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

    /// Review round 2, **M-A**: panics on EVERY call — for proving a
    /// schedule that panics forever still gets bounded by the exact same
    /// `MAX_SENT_ATTEMPTS`/backoff/`failed` machinery a real sent failure
    /// does, not retried at ~1Hz forever (the reviewer's own reproduction of
    /// the bug the un-fixed code had).
    struct AlwaysPanics {
        calls: AtomicUsize,
    }

    impl AlwaysPanics {
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
    impl RunTrigger for AlwaysPanics {
        async fn trigger(&self, _invocation: &ScheduleInvocation) -> FireOutcome {
            self.calls.fetch_add(1, AtomicOrdering::SeqCst);
            panic!("AlwaysPanics: injected panic");
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

        // Review round 1, L6 / round 2, **M-D(1)**: `updated_at` must NOT
        // move across REPEATED Deferred rounds (T4: repeating Deferred from
        // an already-`deferred` row writes nothing). The original version of
        // this check was nearly vacuous: it compared two reads 30ms apart
        // while the virtual clock was FROZEN and the owner was still inside
        // its `DEFER_RECHECK` skip window, so the pump could not have
        // touched the row again either way, mutated or not. Fixed: capture
        // `updated_at` right after the FIRST write (the Pending→Deferred
        // transition just confirmed above, which DOES write), then drive
        // SEVERAL more `DEFER_RECHECK`-gated rounds — each one's `calls()`
        // increase is proof the PREVIOUS round already went all the way
        // through `apply_joined`/`apply_attempt` (this same schedule cannot
        // be re-spawned until its slot is released there), so by the time
        // the loop ends, every round up to the second-to-last is provably
        // applied; one extra round beyond the count this test cares about
        // is what proves the last one is too.
        let (_, updated_at_after_first_write) =
            fetch_attempts_and_updated_at(&store, &stable_fire_id).await;
        let mut seen = trigger.calls().len();
        for _ in 0..5 {
            clock.set(clock.now() + chrono::Duration::seconds(3)); // > DEFER_RECHECK (2s)
            wait_until(
                || async { trigger.calls().len() > seen },
                "the pump never re-polled after the skip window",
            )
            .await;
            seen = trigger.calls().len();
        }
        let (_, updated_at_now) = fetch_attempts_and_updated_at(&store, &stable_fire_id).await;
        assert_eq!(
            updated_at_after_first_write, updated_at_now,
            "repeated Deferred rounds after the first write must land NO further writes at all"
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

        // Review round 1, L6 / round 2, **M-D(2)**: a THIRD "restart" must
        // never call `trigger()` again — the row is `delivered`, a terminal
        // state `due_deliveries` never returns. The original version of this
        // check used a trigger that PANICKED if called and then asserted the
        // row was still `delivered` — but since round 2's own M-A fix, a
        // panic is now CAUGHT by `apply_joined` and routed through
        // `apply_attempt` rather than crashing the test, so that assertion
        // would hold trivially whether or not `trigger()` was ever actually
        // invoked (a caught panic changes nothing about the row's already-
        // terminal status either way). Fixed: count calls directly and
        // assert the count is exactly zero.
        struct CountIfCalled(AtomicUsize);
        #[async_trait]
        impl RunTrigger for CountIfCalled {
            async fn trigger(&self, _invocation: &ScheduleInvocation) -> FireOutcome {
                self.0.fetch_add(1, AtomicOrdering::SeqCst);
                FireOutcome::Failed {
                    reason: "a delivered (terminal) row must never be retried".to_owned(),
                }
            }
        }
        let count_if_called = Arc::new(CountIfCalled(AtomicUsize::new(0)));
        let (scheduler3, _events3) = scheduler_with(
            store.clone(),
            Arc::clone(&count_if_called) as Arc<dyn RunTrigger>,
        );
        let clock3 = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel3 = CancellationToken::new();
        let pump3 = DeliveryPump::new(Arc::clone(&scheduler3));
        let handle3 = tokio::spawn(pump3.run(clock3 as Arc<dyn Clock>, cancel3.child_token()));
        tokio::time::sleep(Duration::from_millis(50)).await;
        cancel3.cancel();
        handle3.await.unwrap();
        assert_eq!(
            count_if_called.0.load(AtomicOrdering::SeqCst),
            0,
            "a delivered (terminal) row must never be re-fetched, let alone retried"
        );
        let states = store.list_module_schedules("mod-d").await.unwrap();
        assert_eq!(
            states[0].last_fire.tick.as_ref().unwrap().status,
            "delivered",
            "still delivered — the row was never touched again"
        );
    }

    /// Review round 1, **M1** / round 2, **M-A** (the reviewer's own
    /// reproduction, and its fix): a schedule whose trigger panics on EVERY
    /// attempt must still (a) release its slot each time — the round 1 half,
    /// otherwise it is wedged after the FIRST panic — and (b), the round 2
    /// half, reach `failed` after exactly `MAX_SENT_ATTEMPTS` panicking
    /// attempts, with `attempts` counted and `consecutive_failures` charged
    /// exactly once — never retried at roughly the pump's own cadence
    /// forever (the pre-round-2 bug: a panic released the slot but recorded
    /// no outcome at all, so the row stayed `pending` with no backoff,
    /// `attempts` frozen at 0, and — had the panic happened after dispatch —
    /// the module would have been redelivered to every single round too).
    ///
    /// Mutation: revert `apply_joined`'s `Err` arm to release the slot
    /// WITHOUT routing it through `apply_attempt` (the pre-round-2 shape) —
    /// this test goes red: `trigger.calls()` keeps growing past 3 as the
    /// clock advances, `attempts` never reaches 3, and the row never reaches
    /// `failed`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_schedule_that_always_panics_still_reaches_failed_with_a_bounded_attempt_count() {
        let store = Store::open_memory().await.unwrap();
        let trigger = AlwaysPanics::new();
        let (scheduler, _events) =
            scheduler_with(store.clone(), trigger.clone() as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "mod-p", "k", now0).await;
        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        // Same clock choreography as `three_failures_...`: the panic route's
        // backoff is computed (inside `apply_attempt`, from whichever branch
        // of `run`'s `select!` observed the panic) using whatever the clock
        // holds AT THAT MOMENT — freeze it between rounds and only advance
        // once this test has proof the previous round already landed.
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

        wait_for_last_error(&store, "mod-p", "kernel bug: attempt panicked").await;
        let t1 = clock.now();
        clock.set(t1 + chrono::Duration::seconds(6)); // past the 5s backoff
        wait_until(
            || async { trigger.calls() >= 2 },
            "the second attempt never happened",
        )
        .await;
        let t2 = clock.now();
        clock.set(t2 + chrono::Duration::seconds(16)); // past the 15s backoff
        wait_until(
            || async { trigger.calls() >= 3 },
            "the third attempt never happened",
        )
        .await;

        wait_until(
            || async {
                let states = store.list_module_schedules("mod-p").await.unwrap();
                states[0]
                    .last_fire
                    .tick
                    .as_ref()
                    .is_some_and(|f| f.status == "failed")
            },
            "the schedule never reached failed after three panicking attempts",
        )
        .await;
        // A bounded wait to prove it does NOT keep retrying past three.
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(
            trigger.calls(),
            3,
            "a schedule that always panics must stop at MAX_SENT_ATTEMPTS, not retry forever"
        );

        let schedule = store.get_schedule(&schedule_id).await.unwrap().unwrap();
        assert_eq!(
            schedule.consecutive_failures, 1,
            "three panicking attempts of ONE fire must count as a single failure, not three"
        );
        let states = store.list_module_schedules("mod-p").await.unwrap();
        let fire_id = states[0].last_fire.tick.as_ref().unwrap().fire_id.clone();
        let (attempts, _) = fetch_attempts_and_updated_at(&store, &fire_id).await;
        assert_eq!(attempts, 3, "the failed row must record exactly 3 attempts");

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

    /// Review round 2, **M-B**: the "same schedule at most one in-flight
    /// attempt" rule (design §5.4) was NOT actually exercised by any
    /// existing test — every scenario that seeds one due row per schedule
    /// can only ever have zero or one in-flight attempt for it regardless of
    /// this check, so disabling it (`if false &&`) left every test green.
    /// This test constructs the state directly: `in_flight_schedules`
    /// already contains the one schedule that is due — `fetch_and_spawn`
    /// must not spawn a second attempt for it.
    ///
    /// Mutation: delete the `in_flight_schedules.contains(..)` check — this
    /// test goes red (a second task is spawned for the same schedule).
    #[tokio::test]
    async fn fetch_and_spawn_refuses_a_schedule_already_in_flight() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::Deferred {
            reason: DeferReason::NotRunning,
        }]);
        let (scheduler, _events) = scheduler_with(store.clone(), trigger as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        let schedule_id = seed_module_fire(&scheduler, &store, "owner-x", "k", now0).await;

        let now = now0 + chrono::Duration::seconds(65);
        let mut state = PumpState::new(now);
        // Stands in for "an attempt for this exact schedule is already
        // running from an earlier round" — no real task backs it, only the
        // bookkeeping `fetch_and_spawn` actually reads.
        state.in_flight_schedules.insert(schedule_id);
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        pump.fetch_and_spawn(&mut state, now).await;
        assert_eq!(
            state.tasks.len(),
            0,
            "a schedule already recorded as in-flight must not be spawned a second time"
        );
    }

    /// Review round 2, **M-B**: same blind spot as the test above, for the
    /// GLOBAL ceiling — `due_deliveries`'s own SQL `LIMIT` already caps ONE
    /// query's results at `GLOBAL_IN_FLIGHT`, which hid a disabled
    /// client-side check in every existing single-round test. This test
    /// pre-fills `state.tasks` with `GLOBAL_IN_FLIGHT` attempts that never
    /// resolve (standing in for a full window carried over from an earlier
    /// round) and proves a brand-new, otherwise-eligible schedule is not
    /// spawned on top of it.
    ///
    /// Mutation: delete the `state.tasks.len() >= GLOBAL_IN_FLIGHT` check at
    /// the TOP of `fetch_and_spawn` (the one that would otherwise return
    /// before even querying) — this test goes red (a 17th task is spawned).
    #[tokio::test]
    async fn fetch_and_spawn_refuses_when_the_global_ceiling_is_already_reached() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::Deferred {
            reason: DeferReason::NotRunning,
        }]);
        let (scheduler, _events) = scheduler_with(store.clone(), trigger as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        // THREE distinct, otherwise-eligible schedules (different owners, so
        // the per-owner cap never interferes) — `due_deliveries` will offer
        // all three in ONE query, since its own `LIMIT` is a flat
        // `GLOBAL_IN_FLIGHT`, not "however much room is left" (it has no way
        // to know that).
        for n in 0..3 {
            seed_module_fire(&scheduler, &store, &format!("owner-y{n}"), "k", now0).await;
        }

        let now = now0 + chrono::Duration::seconds(65);
        let mut state = PumpState::new(now);
        // `GLOBAL_IN_FLIGHT - 1`: one slot short of full — the TOP-of-
        // function early return alone would let this call proceed to query
        // and iterate; only the IN-LOOP check stops it from spawning more
        // than the one remaining slot.
        for _ in 0..GLOBAL_IN_FLIGHT - 1 {
            state.tasks.spawn(std::future::pending::<AttemptDone>());
        }
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        pump.fetch_and_spawn(&mut state, now).await;
        assert_eq!(
            state.tasks.len(),
            GLOBAL_IN_FLIGHT,
            "fetch_and_spawn must stop spawning the moment the global ceiling is reached, even \
             mid-batch — not spawn every eligible row a single query happened to return"
        );
    }

    /// Review round 2, **L-f** (the L1 half): an attempt that has ALREADY
    /// finished when cancellation fires must still have its outcome
    /// durably applied — not discarded because the `JoinSet`'s `Drop`
    /// (which aborts everything STILL running) raced ahead of it. Uses a
    /// trigger the test controls precisely: it signals "I have been called"
    /// and then returns immediately, so the test can release it and cancel
    /// the pump back-to-back, racing the real completion against the real
    /// cancellation on a genuine multi-threaded runtime.
    ///
    /// Mutation: make the cancellation branch of `run`'s `select!` return
    /// immediately instead of draining `try_join_next_with_id` first — this
    /// test goes red intermittently (the delivered write is sometimes lost).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_attempt_that_finishes_right_as_cancel_fires_still_lands() {
        struct SignalThenDeliver {
            called: tokio::sync::Notify,
        }
        #[async_trait]
        impl RunTrigger for SignalThenDeliver {
            async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
                self.called.notify_one();
                let InvocationTarget::Module { fire_id, .. } = &invocation.target else {
                    panic!("SignalThenDeliver is only exercised with Module targets");
                };
                FireOutcome::ModuleDelivered {
                    fire_id: fire_id.clone(),
                }
            }
        }
        let store = Store::open_memory().await.unwrap();
        let trigger = Arc::new(SignalThenDeliver {
            called: tokio::sync::Notify::new(),
        });
        let (scheduler, _events) =
            scheduler_with(store.clone(), Arc::clone(&trigger) as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        seed_module_fire(&scheduler, &store, "mod-race", "k", now0).await;
        let clock = TestClock::at(now0 + chrono::Duration::seconds(65));
        let cancel = CancellationToken::new();
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        let handle =
            tokio::spawn(pump.run(Arc::clone(&clock) as Arc<dyn Clock>, cancel.child_token()));

        tokio::time::timeout(Duration::from_secs(5), trigger.called.notified())
            .await
            .expect("the attempt never started");
        // The trigger has been called and is about to return `ModuleDelivered`
        // — cancel RIGHT NOW, racing the real task completion against the
        // real cancellation, exactly the window `run`'s cancel branch has to
        // cover by draining `try_join_next_with_id` before it returns.
        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), handle)
            .await
            .expect("the pump must stop promptly")
            .unwrap();

        let states = store.list_module_schedules("mod-race").await.unwrap();
        assert_eq!(
            states[0].last_fire.tick.as_ref().unwrap().status,
            "delivered",
            "an attempt that finished before (or exactly as) cancellation fired must still land"
        );
    }

    /// Review round 2, **L-f** (the L2 half): a GENUINE storage failure
    /// inside `apply_delivery_outcome`'s own `UPDATE` — using the same SQL
    /// fault-injection technique `agent24-store`'s own tests use (a `BEFORE
    /// UPDATE` trigger that aborts the write) — must back the schedule off
    /// (review round 2, L-c) rather than touch `attempts`/`status` at all,
    /// and must not crash the pump. Drives `fetch_and_spawn`/`apply_joined`
    /// directly (not the full `run()` loop) so the failure's effect can be
    /// inspected precisely, without racing a background task.
    #[tokio::test]
    async fn a_failing_db_write_backs_off_without_touching_the_row() {
        let store = Store::open_memory().await.unwrap();
        let trigger = ScriptedTrigger::new([FireOutcome::ModuleDelivered {
            fire_id: FireId::from_stored("placeholder".into()),
        }]);
        let (scheduler, _events) = scheduler_with(store.clone(), trigger as Arc<dyn RunTrigger>);
        let now0 = utc("2026-08-01T00:00:00Z");
        seed_module_fire(&scheduler, &store, "inject-fail-owner", "k", now0).await;

        // Fault injection: the SAME technique `agent24-store`'s own
        // `advance_and_record_fire` tests use (design §11, C1.7) — a
        // trigger that aborts the exact write `apply_delivery_outcome` is
        // about to attempt.
        sqlx::query(
            "CREATE TRIGGER inject_fail BEFORE UPDATE ON schedule_deliveries \
             WHEN OLD.owner_module = 'inject-fail-owner' \
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();

        let now = now0 + chrono::Duration::seconds(65);
        let mut state = PumpState::new(now);
        let pump = DeliveryPump::new(Arc::clone(&scheduler));
        pump.fetch_and_spawn(&mut state, now).await;
        let joined = state.tasks.join_next_with_id().await.unwrap();
        pump.apply_joined(&mut state, joined, now).await;

        // Nothing durable changed — the failed write never landed.
        let states = store
            .list_module_schedules("inject-fail-owner")
            .await
            .unwrap();
        let last_fire = states[0].last_fire.tick.as_ref().unwrap();
        assert_eq!(
            last_fire.status, "pending",
            "a failed DB write must not change the row's status"
        );
        let (attempts, _) = fetch_attempts_and_updated_at(&store, &last_fire.fire_id).await;
        assert_eq!(
            attempts, 0,
            "a failed DB write must not touch attempts either"
        );

        // The schedule is now backed off (L-c) — an IMMEDIATE second
        // `fetch_and_spawn` must not re-attempt it.
        pump.fetch_and_spawn(&mut state, now).await;
        assert_eq!(
            state.tasks.len(),
            0,
            "a schedule backing off from a DB write failure must not be re-fetched immediately"
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
