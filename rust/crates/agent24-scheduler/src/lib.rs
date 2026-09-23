//! Agent24 wall-clock scheduler (C5).
//!
//! The product soul: cron / every / at schedules that fire agent runs, and
//! (ME4-1.2.2b, in progress) module-owned schedules that fire callback
//! deliveries. Design constraints (SPEC-002 §1.5, ADR-026 hard constraint #7):
//! - **pre-advance**: a due schedule's `next_run_at` is recomputed and
//!   persisted BEFORE the run is triggered, so a crash mid-fire cannot double
//!   fire (openfang's cron lesson).
//! - **skip-missed**: the new `next_run_at` is computed from *now*, so a
//!   schedule that lay due while the daemon was down fires once and jumps to
//!   its next future slot — never a replay burst (`MissedTickBehavior::Skip`).
//! - **fail-safe disable**: `MAX_CONSECUTIVE_FAILURES` trigger failures in a
//!   row disable the schedule and emit `schedule.disabled`.
//! - **revision CAS** (`docs/design/ME4-S1-scheduler-callback.md` §2.3): the
//!   pre-advance write is conditioned on the revision AND slot the tick read.
//!   A concurrent spec change (or a racing tick) loses the write and the CAS
//!   is discarded — the whole event never happens for that tick.
//!
//! `tick(now)` is a single pass driven by an injected instant, so the whole
//! engine is testable with a mock clock and NO real sleeps.
//!
//! ME4-1.2.2b2 rewired `Scheduler` onto [`invocation::RunTrigger`] (design
//! §3) for the AgentRun path (`fire_agent_run`, `run_now`'s `AgentRun` arm),
//! byte-identical behaviour on the non-racing path (C2.7).
//!
//! ME4-1.2.2b3 (this cut) adds the module row's own tick branch
//! (`fire_module`) and `run_now`'s module arm (design §3.2/§4.2/§4.7):
//! `tick()` now dispatches through [`Scheduler::fire`] rather than skipping
//! module rows outright. A module row's pre-advance AND its delivery record
//! land in the SAME CAS'd transaction (`advance_and_record_fire`) — but,
//! unlike an AgentRun row, `tick()` never calls `trigger()` for it; that is
//! the delivery pump's job (ME4-1.3.1). [`InstalledOwners`] gates whether a
//! fire is recorded at all (v2, M5): an owner absent from this run's
//! catalogue still gets its `next_run_at` advanced, but no delivery row.
//! See `docs/design/ME4-S1-scheduler-callback.md` §3/§13.
//!
//! Review fixes folded into ME4-1.2.2b3 (Opus review of the pre-split
//! 4f64743, continued from b2's M1/M2/H1/L1):
//! - **M2**: `fire_module`'s pre-advance CAS also pins the RAW
//!   `next_run_at` string (same fix as `fire_agent_run`'s); the delivery
//!   row's OWN `scheduled_for` column stays canonical (`fmt_iso(due)`,
//!   matching what `FireId::derive` hashes) — only the CAS comparison
//!   argument changes.
//! - **L3**: the delivery-pump wake uses `Notify::notify_one` (a permit is
//!   never lost if nothing is waiting yet — no pump exists in this repo
//!   until ME4-1.3.1), not `notify_waiters` (which only wakes CURRENTLY
//!   waiting tasks and drops the signal on the floor otherwise).
//! - **L4**: `run_now`'s module arm no longer binds an unused `owner` — it
//!   only needs to know the row IS module-owned; `record_run_now_fire`
//!   looks the owner up itself.
//! - The tick/no-failure module test's second half (manually calling
//!   `RecordingTrigger::trigger` and re-asserting ITS OWN hardcoded
//!   `Deferred(MountPending)` return) was a tautology — removed here; the
//!   equivalent assertion against the REAL `KernelTrigger` lands in the
//!   top-level (agent24d) cut.
//! - L2 (module-row `next_fire` error log rate-limiting) is left as a
//!   follow-up (FU) rather than implemented — see the comment at the log
//!   call site.

pub mod fire;
pub mod installed_owners;
pub mod invocation;
pub mod next_fire;

use std::sync::Arc;
use std::time::Duration;

use agent24_core::record_schedule_result;
use agent24_core::util::ulid;
use agent24_protocol::{
    EventBody, Schedule, ScheduleCreate, ScheduleDisabledPayload, ScheduleFiredPayload,
    ScheduleUpdate,
};
use agent24_store::{Advance, NewFire, ScheduleRecord, Store, StoreError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

pub use fire::FireId;
pub use installed_owners::InstalledOwners;
pub use invocation::{
    DeferReason, FireOutcome, FireTrigger, InvocationTarget, ModuleScheduleKey, RunNowOutcome,
    RunTrigger, ScheduleInvocation, agent_run_result,
};

use next_fire::{SpecError, fmt_iso, next_fire, validate};

#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    #[error("schedule not found: {0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<SpecError> for ScheduleError {
    fn from(err: SpecError) -> Self {
        ScheduleError::Invalid(err.to_string())
    }
}

/// Injectable clock so the background loop can be driven without real time.
#[async_trait]
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
    async fn sleep(&self, dur: Duration);
}

/// Production clock: system time + tokio sleep.
pub struct SystemClock;

#[async_trait]
impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
    async fn sleep(&self, dur: Duration) {
        tokio::time::sleep(dur).await;
    }
}

pub struct Scheduler {
    store: Store,
    trigger: Arc<dyn RunTrigger>,
    emit: Arc<dyn Fn(EventBody) + Send + Sync>,
    /// §4.2/v2 M5: which module owners this run's catalogue has. Unset ⇒
    /// record for everyone (ME4-1.3.1 sets this once, after `mount_all`).
    installed_owners: InstalledOwners,
    /// §5.4: wakes the (ME4-1.3.1) delivery pump the moment a module fire is
    /// recorded, rather than making it wait out its poll interval. Nothing
    /// subscribes to this yet in this cut — tick/`run_now` notify
    /// unconditionally so the interface is exercised end to end.
    delivery_notify: tokio::sync::Notify,
}

impl Scheduler {
    pub fn new(
        store: Store,
        trigger: Arc<dyn RunTrigger>,
        emit: Arc<dyn Fn(EventBody) + Send + Sync>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            trigger,
            emit,
            installed_owners: InstalledOwners::new(),
            delivery_notify: tokio::sync::Notify::new(),
        })
    }

    /// The module catalogue gate for the tick's delivery-recording branch
    /// (§4.2/v2 M5). ME4-1.3.1's daemon wiring calls `.set(..)` on this once,
    /// right after `mount_all` returns and before the tick loop starts.
    #[must_use]
    pub fn installed_owners(&self) -> &InstalledOwners {
        &self.installed_owners
    }

    /// The delivery pump's wake signal (§5.4), for ME4-1.3.1 to `.notified()`
    /// on.
    #[must_use]
    pub fn delivery_notify(&self) -> &tokio::sync::Notify {
        &self.delivery_notify
    }

    // ── CRUD ─────────────────────────────────────────────────────────────────

    /// Create a schedule; `next_run_at` is computed from `now` (None if the
    /// spec is a one-shot already in the past, or the schedule is disabled).
    pub async fn create(
        &self,
        create: ScheduleCreate,
        now: DateTime<Utc>,
    ) -> Result<Schedule, ScheduleError> {
        validate(&create.spec)?;
        let next_run_at = if create.enabled {
            next_fire(&create.spec, now)?.map(fmt_iso)
        } else {
            None
        };
        let schedule = Schedule {
            id: format!("sch_{}", ulid()),
            name: create.name,
            enabled: create.enabled,
            spec: create.spec,
            // ME4-1.2.2a: `Schedule.action` is `Option<ScheduleAction>` now
            // (module rows), but `ScheduleCreate`'s isn't — REST can only
            // ever create a user (AgentRun) row (S1-2), so this is always
            // `Some`.
            action: Some(create.action),
            delivery: create.delivery,
            last_run_at: None,
            next_run_at,
            consecutive_failures: 0,
            // This crate only ever constructs USER rows (module rows are
            // created by `Store::upsert_module_schedule`, ME4-1.2.1a) — the
            // five view fields are all at their "nothing is blocking it"
            // defaults, mirroring a fresh user row's real values.
            owner: None,
            user_suspended: false,
            system_disabled_reason: None,
            effective_enabled: create.enabled,
            disabled_by: (!create.enabled).then_some(agent24_protocol::DisabledBy::User),
        };
        self.store.upsert_schedule(&schedule).await?;
        Ok(schedule)
    }

    pub async fn get(&self, id: &str) -> Result<Schedule, ScheduleError> {
        self.store
            .get_schedule(id)
            .await?
            .ok_or_else(|| ScheduleError::NotFound(id.to_owned()))
    }

    pub async fn list(&self) -> Result<Vec<Schedule>, ScheduleError> {
        Ok(self.store.list_schedules().await?)
    }

    /// Apply a partial update. Changing `spec`, or toggling `enabled`,
    /// recomputes `next_run_at`; disabling clears it.
    pub async fn update(
        &self,
        id: &str,
        update: ScheduleUpdate,
        now: DateTime<Utc>,
    ) -> Result<Schedule, ScheduleError> {
        let mut schedule = self.get(id).await?;
        let mut recompute = false;
        if let Some(name) = update.name {
            schedule.name = name;
        }
        if let Some(spec) = update.spec {
            validate(&spec)?;
            schedule.spec = spec;
            recompute = true;
        }
        if let Some(action) = update.action {
            schedule.action = Some(action);
        }
        if let Some(delivery) = update.delivery {
            schedule.delivery = delivery;
        }
        if let Some(enabled) = update.enabled {
            if enabled != schedule.enabled {
                recompute = true;
            }
            schedule.enabled = enabled;
        }
        // Keep the view fields consistent with a changed `enabled`. This
        // crate only ever legitimately handles USER rows today (module-row
        // PATCH guardrails are ME4-1.2.2c's job, not wired up yet) — on one,
        // `user_suspended`/`system_disabled_reason` are always
        // false/None (migration 0007's CHECK), so `!enabled` is the only
        // thing `disabled_by` can be tracking. Leave a module row's view
        // fields alone rather than guess at them.
        if schedule.owner.is_none()
            && !schedule.user_suspended
            && schedule.system_disabled_reason.is_none()
        {
            schedule.effective_enabled = schedule.enabled;
            schedule.disabled_by =
                (!schedule.enabled).then_some(agent24_protocol::DisabledBy::User);
        }
        if recompute {
            schedule.next_run_at = if schedule.enabled {
                next_fire(&schedule.spec, now)?.map(fmt_iso)
            } else {
                None
            };
            // A manual re-enable / spec change is a fresh start
            schedule.consecutive_failures = 0;
        }
        self.store.upsert_schedule(&schedule).await?;
        Ok(schedule)
    }

    pub async fn delete(&self, id: &str) -> Result<(), ScheduleError> {
        if self.store.delete_schedule(id).await? {
            Ok(())
        } else {
            Err(ScheduleError::NotFound(id.to_owned()))
        }
    }

    /// Fire immediately without touching `next_run_at` (manual "run now",
    /// design §4.7). A user (AgentRun) row triggers synchronously, exactly as
    /// before (rewired onto [`ScheduleInvocation`]/[`agent_run_result`], same
    /// observable behaviour).
    ///
    /// A module row's `run_now` records a fire (own `fire_id`, `scheduled_for
    /// = fired_at = now`, own `fire_trigger = "run_now"`) WITHOUT touching
    /// `next_run_at` and WITHOUT retiring the tick source's own outstanding
    /// fire (v2, H3 — tick and run_now supersede only within their own
    /// trigger source, never each other); it does not call `trigger()`
    /// synchronously either — the delivery pump (ME4-1.3.1) picks this row
    /// up exactly like a tick-recorded one. Same-second double calls are
    /// idempotent on `fire_id` (`record_run_now_fire`'s own `INSERT … ON
    /// CONFLICT DO NOTHING`): the second call returns the SAME id, not a
    /// second row. Review L4: this arm only needs to know the row IS
    /// module-owned — `record_run_now_fire` looks up `owner_module`/
    /// `module_key` itself, so there is nothing here worth binding out of
    /// `schedule.owner`.
    pub async fn run_now(
        &self,
        id: &str,
        now: DateTime<Utc>,
    ) -> Result<RunNowOutcome, ScheduleError> {
        let schedule = self.get(id).await?;
        if schedule.owner.is_some() {
            let fire_id = FireId::derive(FireTrigger::RunNow, &schedule.id, now);
            let now_str = fmt_iso(now);
            let expires_at = fmt_iso(now + chrono::Duration::hours(24));
            let recorded = self
                .store
                .record_run_now_fire(&schedule.id, fire_id.as_str(), &now_str, &expires_at)
                .await?;
            if !recorded {
                // Raced away between `get()` and here (deleted, or somehow
                // no longer module-owned) — nothing this call can still do.
                return Err(ScheduleError::NotFound(schedule.id));
            }
            self.delivery_notify.notify_one();
            return Ok(RunNowOutcome::Fire { fire_id });
        }
        let Some(action) = schedule.action.clone() else {
            return Err(ScheduleError::Invalid(format!(
                "schedule {} has neither an owner nor an action (kernel bug)",
                schedule.id
            )));
        };
        let invocation = ScheduleInvocation {
            schedule_id: schedule.id.clone(),
            scheduled_for: now,
            fired_at: now,
            trigger: FireTrigger::RunNow,
            target: InvocationTarget::AgentRun(action),
        };
        let outcome = self.trigger.trigger(&invocation).await;
        agent_run_result(outcome)
            .map(|run_id| RunNowOutcome::Run { run_id })
            .map_err(ScheduleError::Invalid)
    }

    // ── the tick ─────────────────────────────────────────────────────────────

    /// Process every schedule due at `now`. Returns how many fired.
    ///
    /// Review L1 (behaviour note, not new — this is what the CAS'd rewrite
    /// makes explicit): a slot whose pre-advance CAS LOST — the row was
    /// deleted, or its revision/slot moved — since this tick read it does
    /// NOT count, even though the pre-1.2.2b code's non-CAS'd version used to
    /// count "the row was gone by the time we tried to write" as a fire (it
    /// returned `Ok(())` either way). Nothing was actually fired in that
    /// case, so the new count is the more honest one; nothing today reads
    /// this return value expecting the old, slightly-off number.
    ///
    /// Free of real time — the caller supplies `now`, so tests drive it
    /// directly.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<usize, ScheduleError> {
        // Every row (user AND module); a single corrupt row is skipped and
        // logged rather than wedging every future tick (`list_schedules_for_
        // tick`'s own doc comment).
        let records = self.store.list_schedules_for_tick().await?;
        let mut fired = 0;
        for record in records {
            let ScheduleRecord { schedule, revision } = record;
            let Some(next_run_at) = schedule.next_run_at.clone() else {
                continue;
            };
            let due = match next_fire::parse_iso(&next_run_at) {
                Ok(due) => due,
                Err(err) => {
                    tracing::error!("schedule {} has unparsable next_run_at: {err}", schedule.id);
                    continue;
                }
            };
            if due > now {
                continue;
            }
            // Isolate per-schedule failures: a genuine StoreError while
            // firing ONE schedule (e.g. a transient SQLITE_BUSY under
            // concurrent approval/tool/audit writes to the same file) must
            // not abort the whole batch and starve every other healthy due
            // schedule (review #39). The failed schedule keeps its persisted
            // next_run_at and is retried next tick.
            let id = schedule.id.clone();
            match self.fire(schedule, revision, &next_run_at, due, now).await {
                Ok(did_fire) => {
                    if did_fire {
                        fired += 1;
                    }
                }
                Err(err) => tracing::error!("schedule {id} fire failed: {err}; skipping this tick"),
            }
        }
        Ok(fired)
    }

    /// Dispatch a due row to its kind's fire path (design §8.1: `owner` is
    /// `Some` exactly for a module row, `action` is `Some` exactly for a user
    /// row — the two are mutually exclusive by construction). `raw_next_run_at`
    /// is passed straight through to whichever path fires (review M2).
    async fn fire(
        &self,
        schedule: Schedule,
        revision: i64,
        raw_next_run_at: &str,
        due: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, ScheduleError> {
        if schedule.owner.is_some() {
            self.fire_module(schedule, revision, raw_next_run_at, due, now)
                .await
        } else if schedule.action.is_some() {
            self.fire_agent_run(schedule, revision, raw_next_run_at, due, now)
                .await
        } else {
            // Invariant violation (§8.1): a row with neither. Unreadable by
            // this crate's model — skip defensively rather than panic.
            tracing::error!(
                "schedule {} has neither an owner nor an action; skipping",
                schedule.id
            );
            Ok(false)
        }
    }

    /// Pre-advance THEN trigger, for a user (AgentRun) row — unchanged
    /// behaviour from before ME4-1.2.2b on the non-racing path (design
    /// §3.3/C2.7), just re-plumbed onto the CAS'd store calls:
    /// `advance_and_record_fire` (§2.3's revision CAS; `fire: None` — a user
    /// row has no delivery table) for the pre-advance,
    /// `update_schedule_runtime_cas` (§2.3's closing rule: the AgentRun
    /// failure-counter/disable write is CAS'd too) for the post-fire
    /// counters. `raw_next_run_at` is the LITERAL string this tick read off
    /// the row (review M2) — the pre-advance CAS pins the DB column to that
    /// exact spelling, never a re-`fmt_iso`'d `due`, so a legacy row with a
    /// non-canonical-but-valid RFC-3339 `next_run_at` (milliseconds, a
    /// `+00:00` offset) still fires; `due` (parsed from it) is only used for
    /// the invocation's `scheduled_for` and for computing the next slot.
    ///
    /// Event ordering note: `schedule.fired` is emitted the moment the run is
    /// created (queued); the run's own `run.started` is emitted later from
    /// its execution task and may interleave. Clients that need the causal
    /// link read `schedule_id` off `RunStartedPayload` rather than relying on
    /// the relative order of the two events on the broadcast bus.
    async fn fire_agent_run(
        &self,
        schedule: Schedule,
        revision: i64,
        raw_next_run_at: &str,
        due: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, ScheduleError> {
        self.fire_agent_run_inner(schedule, revision, raw_next_run_at, due, now, || async {})
            .await
    }

    /// The actual implementation, with a test seam (review M1):
    /// `between_advance_and_write` runs AFTER the pre-advance CAS lands and
    /// BEFORE the post-trigger counter/disable CAS — exactly the window a
    /// concurrent PATCH could land in (the pre-advance does not bump
    /// `revision`, so a PATCH racing THIS window is invisible to the first
    /// CAS but not the second). Production always calls [`fire_agent_run`],
    /// which passes a no-op; only tests reach for this directly.
    async fn fire_agent_run_inner<F, Fut>(
        &self,
        schedule: Schedule,
        revision: i64,
        raw_next_run_at: &str,
        due: DateTime<Utc>,
        now: DateTime<Utc>,
        between_advance_and_write: F,
    ) -> Result<bool, ScheduleError>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        let now_str = fmt_iso(now);
        // Skip-missed: next slot is computed from `now`, not the stale due time.
        let advanced = match next_fire(&schedule.spec, now) {
            Ok(next) => next.map(fmt_iso),
            Err(err) => {
                // A spec that no longer computes (shouldn't happen
                // post-validate) disables the schedule rather than looping
                // forever. No pre-advance has happened yet, so the CAS here
                // is on what the tick's list read straight off the row.
                tracing::error!(
                    "schedule {} next_fire failed: {err}; disabling",
                    schedule.id
                );
                let landed = self
                    .store
                    .update_schedule_runtime_cas(
                        &schedule.id,
                        revision,
                        Some(raw_next_run_at),
                        schedule.last_run_at.as_deref(),
                        i64::from(schedule.consecutive_failures),
                        false,
                        true,
                    )
                    .await?;
                // Review M1: a lost CAS here means nothing actually changed —
                // announcing "disabled" for a schedule that is not, in fact,
                // newly disabled would be a lie on the event bus.
                if landed {
                    self.emit_disabled(&schedule.id, "next_fire_error");
                }
                return Ok(false);
            }
        };
        let Some(action) = schedule.action.clone() else {
            tracing::error!(
                "schedule {} fire_agent_run called with no action (module row?); skipping",
                schedule.id
            );
            return Ok(false);
        };
        // Pre-advance, CAS'd on the revision and slot this tick read (§2.3):
        // if the row's revision moved (a concurrent PATCH) or this slot was
        // already advanced past (a racing tick), the whole transaction rolls
        // back — nothing is written, this tick skips the row.
        let advance = self
            .store
            .advance_and_record_fire(
                &schedule.id,
                revision,
                raw_next_run_at,
                advanced.as_deref(),
                &now_str,
                None,
            )
            .await?;
        if matches!(advance, Advance::Lost) {
            tracing::debug!(
                "schedule {} tick CAS lost (revision/slot moved concurrently); skipping this tick",
                schedule.id
            );
            return Ok(false);
        }

        between_advance_and_write().await;

        let invocation = ScheduleInvocation {
            schedule_id: schedule.id.clone(),
            scheduled_for: due,
            fired_at: now,
            trigger: FireTrigger::Tick,
            target: InvocationTarget::AgentRun(action),
        };
        match agent_run_result(self.trigger.trigger(&invocation).await) {
            Ok(run_id) => {
                if schedule.consecutive_failures != 0 {
                    // The row's next_run_at/last_run_at were just set by the
                    // pre-advance above — that is what this CAS must pin. A
                    // lost CAS here only costs observability (the counter
                    // stays at its old value until the next successful
                    // write) — there is no event tied to a mere reset, so
                    // nothing further to gate on `landed`.
                    self.store
                        .update_schedule_runtime_cas(
                            &schedule.id,
                            revision,
                            advanced.as_deref(),
                            Some(&now_str),
                            0,
                            true,
                            false,
                        )
                        .await?;
                }
                self.emit.as_ref()(EventBody::ScheduleFired(ScheduleFiredPayload {
                    schedule_id: schedule.id.clone(),
                    run_id,
                }));
            }
            Err(err) => {
                tracing::warn!("schedule {} trigger failed: {err}", schedule.id);
                let mut failures = schedule.consecutive_failures;
                let health = record_schedule_result(&mut failures, false);
                let disable = health == agent24_core::ScheduleHealth::MustDisable;
                let landed = self
                    .store
                    .update_schedule_runtime_cas(
                        &schedule.id,
                        revision,
                        advanced.as_deref(),
                        Some(&now_str),
                        i64::from(failures),
                        !disable,
                        disable,
                    )
                    .await?;
                // Review M1: only announce a disable that actually landed —
                // a lost CAS (a concurrent PATCH raced this exact write)
                // means the row was NOT disabled by this call, whatever this
                // call locally computed.
                if disable && landed {
                    self.emit_disabled(&schedule.id, "consecutive_failures");
                }
            }
        }
        Ok(true)
    }

    /// The module row's tick branch (design §3.2/§4.2, ME4-1.2.2b3):
    /// pre-advance AND record the delivery row in ONE CAS'd transaction
    /// (`advance_and_record_fire`), then STOP — no synchronous `trigger()`
    /// call. Unlike an AgentRun row, the pump (ME4-1.3.1) is what eventually
    /// calls `trigger()` for this fire; a slow module must never make the
    /// tick loop itself block. `raw_next_run_at` (review M2) is what the
    /// pre-advance CAS pins the DB column to; the delivery row's OWN
    /// `scheduled_for` column, and `FireId::derive`, both use the CANONICAL
    /// `fmt_iso(due)` instead — only the CAS comparison needs the exact
    /// stored spelling, the delivery row's content should be the one
    /// consistent spelling regardless of how `next_run_at` happened to be
    /// written.
    async fn fire_module(
        &self,
        schedule: Schedule,
        revision: i64,
        raw_next_run_at: &str,
        due: DateTime<Utc>,
        now: DateTime<Utc>,
    ) -> Result<bool, ScheduleError> {
        let Some(owner) = schedule.owner.clone() else {
            tracing::error!(
                "schedule {} fire_module called without an owner; skipping",
                schedule.id
            );
            return Ok(false);
        };
        let now_str = fmt_iso(now);
        let due_str = fmt_iso(due);
        let advanced = match next_fire(&schedule.spec, now) {
            Ok(next) => next.map(fmt_iso),
            Err(err) => {
                // Module spec validation (design §6.3) restricts cron/every
                // up front specifically so this can't happen; fail safe
                // rather than write a value the migration's CHECK (system_
                // disabled_reason only meaningful with a dedicated writer)
                // does not cover from this path.
                //
                // Review L2 (FU, not implemented here): this `error!` has no
                // rate limit. In production it can only repeat once per this
                // schedule's own period (≥ 60s, §6.3), and only for a spec
                // that should be unreachable given upfront validation — a
                // low-risk, low-volume FU rather than something this cut
                // needs to solve.
                tracing::error!(
                    "module schedule {} ({}:{}) next_fire failed: {err}; skipping this tick",
                    schedule.id,
                    owner.module,
                    owner.key,
                );
                return Ok(false);
            }
        };
        let should_record = self.installed_owners.may_record(&owner.module);
        let fire_id = FireId::derive(FireTrigger::Tick, &schedule.id, due);
        let fire_id_str = fire_id.as_str().to_owned();
        let expires_at_str = fmt_iso(now + chrono::Duration::hours(24));
        let new_fire = should_record.then(|| NewFire {
            fire_id: &fire_id_str,
            owner_module: &owner.module,
            module_key: &owner.key,
            scheduled_for: &due_str,
            fired_at: &now_str,
            trigger: FireTrigger::Tick,
            expires_at: &expires_at_str,
        });
        let advance = self
            .store
            .advance_and_record_fire(
                &schedule.id,
                revision,
                raw_next_run_at,
                advanced.as_deref(),
                &now_str,
                new_fire,
            )
            .await?;
        match advance {
            Advance::Lost => {
                tracing::debug!(
                    "module schedule {} tick CAS lost (revision/slot moved concurrently); \
                     skipping this tick, no delivery row written",
                    schedule.id
                );
                Ok(false)
            }
            Advance::Advanced => {
                if should_record {
                    // Review L3: `notify_one`, not `notify_waiters` — a
                    // permit is banked even if nothing is `.notified().await`
                    // yet (no pump exists in this repo until ME4-1.3.1), so
                    // the first waiter to arrive picks it straight up instead
                    // of the signal being silently dropped.
                    self.delivery_notify.notify_one();
                }
                Ok(true)
            }
        }
    }

    fn emit_disabled(&self, schedule_id: &str, reason: &str) {
        self.emit.as_ref()(EventBody::ScheduleDisabled(ScheduleDisabledPayload {
            schedule_id: schedule_id.to_owned(),
            reason: reason.to_owned(),
        }));
    }

    // ── background loop ──────────────────────────────────────────────────────

    /// Run the tick loop until `cancel`. `tick_interval` is the poll cadence;
    /// finest schedule granularity is a minute, so a few seconds is ample.
    pub async fn run(
        self: Arc<Self>,
        clock: Arc<dyn Clock>,
        tick_interval: Duration,
        cancel: CancellationToken,
    ) {
        tracing::info!("scheduler loop started (tick {tick_interval:?})");
        loop {
            tokio::select! {
                () = clock.sleep(tick_interval) => {
                    let now = clock.now();
                    if let Err(err) = self.tick(now).await {
                        tracing::error!("scheduler tick failed: {err}");
                    }
                }
                () = cancel.cancelled() => {
                    tracing::info!("scheduler loop stopped");
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_protocol::ScheduleAction;
    use agent24_protocol::ScheduleSpec;
    use agent24_store::{ModuleScheduleDesired, UpsertOutcome};
    use next_fire::parse_iso;
    use std::collections::HashSet;
    use std::sync::Mutex;

    /// Records every AgentRun trigger; optionally fails the first N calls. A
    /// Module target is answered the way the daemon's real trigger answers
    /// one until ME4-1.2.2b3 wires module-row tick support and ME4-1.3.1
    /// wires a real deliverer (design §3.3): `Deferred(MountPending)`, never
    /// a failure. Nothing in THIS cut's tests constructs a Module target yet
    /// — the arm exists purely so the match stays exhaustive against the new
    /// trait.
    struct RecordingTrigger {
        calls: Mutex<Vec<String>>,
        fail_until: Mutex<usize>,
    }

    impl RecordingTrigger {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(vec![]),
                fail_until: Mutex::new(0),
            })
        }
        fn always_failing() -> Arc<Self> {
            Arc::new(Self {
                calls: Mutex::new(vec![]),
                fail_until: Mutex::new(usize::MAX),
            })
        }
        fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    #[async_trait]
    impl RunTrigger for RecordingTrigger {
        async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
            match &invocation.target {
                InvocationTarget::AgentRun(_action) => {
                    {
                        let mut fail_until = self.fail_until.lock().unwrap();
                        if *fail_until > 0 {
                            *fail_until = fail_until.saturating_sub(1);
                            return FireOutcome::Failed {
                                reason: "trigger boom".to_owned(),
                            };
                        }
                    }
                    let mut calls = self.calls.lock().unwrap();
                    calls.push(invocation.schedule_id.clone());
                    FireOutcome::AgentRun {
                        run_id: format!("run_{}", calls.len()),
                    }
                }
                InvocationTarget::Module { .. } => FireOutcome::Deferred {
                    reason: DeferReason::MountPending,
                },
            }
        }
    }

    fn utc(s: &str) -> DateTime<Utc> {
        parse_iso(s).unwrap()
    }

    async fn scheduler_with(
        trigger: Arc<dyn RunTrigger>,
    ) -> (Arc<Scheduler>, Arc<Mutex<Vec<String>>>, Store) {
        let store = Store::open_memory().await.unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = Arc::clone(&events);
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(move |body: EventBody| {
            ev.lock().unwrap().push(body.wire_type().to_owned());
        });
        (Scheduler::new(store.clone(), trigger, emit), events, store)
    }

    fn every_create(secs: u32) -> ScheduleCreate {
        ScheduleCreate {
            name: "test".to_owned(),
            enabled: true,
            spec: ScheduleSpec::Every { secs },
            action: ScheduleAction::AgentRun {
                prompt: "do it".to_owned(),
                session_id: None,
                model_override: None,
            },
            delivery: vec![],
        }
    }

    fn every_module(secs: u32) -> ModuleScheduleDesired {
        ModuleScheduleDesired {
            spec: ScheduleSpec::Every { secs },
            enabled: true,
            label: "k".to_owned(),
        }
    }

    #[tokio::test]
    async fn create_computes_next_run_at() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, _store) = scheduler_with(trig).await;
        let s = sched
            .create(every_create(3600), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        assert_eq!(s.next_run_at.as_deref(), Some("2026-07-24T11:00:00Z"));
        assert_eq!(s.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn every_secs_below_minimum_is_rejected() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, _store) = scheduler_with(trig).await;
        let err = sched
            .create(every_create(30), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap_err();
        assert!(matches!(err, ScheduleError::Invalid(_)), "{err}");
    }

    #[tokio::test]
    async fn tick_fires_due_schedule_and_pre_advances() {
        let trig = RecordingTrigger::new();
        let (sched, events, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        // next_run_at = 10:01:00. Not due at 10:00:30.
        assert_eq!(sched.tick(utc("2026-07-24T10:00:30Z")).await.unwrap(), 0);
        assert_eq!(trig.count(), 0);
        // Due at 10:01:05 → fires once, advances to 10:02:05 (from `now`)
        assert_eq!(sched.tick(utc("2026-07-24T10:01:05Z")).await.unwrap(), 1);
        assert_eq!(trig.count(), 1);
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert_eq!(after.next_run_at.as_deref(), Some("2026-07-24T10:02:05Z"));
        assert_eq!(after.last_run_at.as_deref(), Some("2026-07-24T10:01:05Z"));
        assert_eq!(events.lock().unwrap().clone(), vec!["schedule.fired"]);
    }

    #[tokio::test]
    async fn tick_fires_every_due_schedule_in_the_batch() {
        // The per-schedule loop must not stop early — every due schedule fires
        // in one tick (review #39: one schedule's failure can't starve others).
        let trig = RecordingTrigger::new();
        let (sched, _ev, _store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        for _ in 0..3 {
            sched
                .create(every_create(60), utc("2026-07-24T10:00:00Z"))
                .await
                .unwrap();
        }
        // all three are due at 10:01:05
        assert_eq!(sched.tick(utc("2026-07-24T10:01:05Z")).await.unwrap(), 3);
        assert_eq!(trig.count(), 3);
    }

    #[tokio::test]
    async fn missed_slots_fire_once_not_a_burst() {
        // Daemon "down" for an hour: an every-60s schedule due long ago must
        // fire exactly once and jump forward, not replay 60 times.
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        // Way past due:
        assert_eq!(sched.tick(utc("2026-07-24T11:00:00Z")).await.unwrap(), 1);
        assert_eq!(trig.count(), 1);
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        // advanced from now (11:00), not from the stale 10:01
        assert_eq!(after.next_run_at.as_deref(), Some("2026-07-24T11:01:00Z"));
    }

    #[tokio::test]
    async fn one_shot_at_fires_once_and_clears_next_run() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let create = ScheduleCreate {
            name: "once".to_owned(),
            enabled: true,
            spec: ScheduleSpec::At {
                ts: "2026-07-24T12:00:00Z".to_owned(),
            },
            action: ScheduleAction::AgentRun {
                prompt: "once".to_owned(),
                session_id: None,
                model_override: None,
            },
            delivery: vec![],
        };
        let created = sched
            .create(create, utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        assert_eq!(created.next_run_at.as_deref(), Some("2026-07-24T12:00:00Z"));
        // Fire at 12:00:01
        assert_eq!(sched.tick(utc("2026-07-24T12:00:01Z")).await.unwrap(), 1);
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert_eq!(after.next_run_at, None);
        assert!(after.enabled); // still enabled, just nothing more to fire
        // A later tick does nothing
        assert_eq!(sched.tick(utc("2026-07-24T13:00:00Z")).await.unwrap(), 0);
        assert_eq!(trig.count(), 1);
    }

    #[tokio::test]
    async fn five_consecutive_failures_disable_the_schedule() {
        let trig = RecordingTrigger::always_failing();
        let (sched, events, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        // Drive five due ticks a minute apart
        let mut t = utc("2026-07-24T10:01:05Z");
        for _ in 0..5 {
            sched.tick(t).await.unwrap();
            t += chrono::Duration::seconds(60);
        }
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert!(!after.enabled, "schedule should be disabled");
        assert_eq!(after.next_run_at, None);
        assert_eq!(after.consecutive_failures, 5);
        let seen = events.lock().unwrap().clone();
        assert_eq!(seen.iter().filter(|e| *e == "schedule.disabled").count(), 1);
        // Disabled → subsequent ticks are no-ops
        assert_eq!(sched.tick(t).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_success_resets_the_failure_counter() {
        // Fail 2, then succeed → counter back to 0, stays enabled
        let trig = Arc::new(RecordingTrigger {
            calls: Mutex::new(vec![]),
            fail_until: Mutex::new(2),
        });
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        let mut t = utc("2026-07-24T10:01:05Z");
        for _ in 0..3 {
            sched.tick(t).await.unwrap();
            t += chrono::Duration::seconds(60);
        }
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert!(after.enabled);
        assert_eq!(after.consecutive_failures, 0);
        assert_eq!(trig.count(), 1);
    }

    #[tokio::test]
    async fn update_spec_recomputes_next_run_at() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, _store) = scheduler_with(trig).await;
        let created = sched
            .create(every_create(3600), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        let updated = sched
            .update(
                &created.id,
                ScheduleUpdate {
                    spec: Some(ScheduleSpec::Every { secs: 120 }),
                    ..Default::default()
                },
                utc("2026-07-24T10:30:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(updated.next_run_at.as_deref(), Some("2026-07-24T10:32:00Z"));
    }

    #[tokio::test]
    async fn disabling_clears_next_run_and_reenable_recomputes() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, _store) = scheduler_with(trig).await;
        let created = sched
            .create(every_create(3600), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        let disabled = sched
            .update(
                &created.id,
                ScheduleUpdate {
                    enabled: Some(false),
                    ..Default::default()
                },
                utc("2026-07-24T10:30:00Z"),
            )
            .await
            .unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.next_run_at, None);
        let reenabled = sched
            .update(
                &created.id,
                ScheduleUpdate {
                    enabled: Some(true),
                    ..Default::default()
                },
                utc("2026-07-24T10:45:00Z"),
            )
            .await
            .unwrap();
        assert_eq!(
            reenabled.next_run_at.as_deref(),
            Some("2026-07-24T11:45:00Z")
        );
    }

    #[tokio::test]
    async fn run_now_fires_without_touching_next_run_at() {
        let trig = RecordingTrigger::new();
        let (sched, events, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(3600), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        let outcome = sched
            .run_now(&created.id, utc("2026-07-24T10:05:00Z"))
            .await
            .unwrap();
        let RunNowOutcome::Run { run_id } = outcome else {
            panic!("expected a user row to answer Run, got {outcome:?}");
        };
        assert!(run_id.starts_with("run_"));
        assert_eq!(trig.count(), 1);
        // next_run_at unchanged; no schedule.fired event (that's tick-only)
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert_eq!(after.next_run_at, created.next_run_at);
        assert!(events.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_delete_during_fire_is_not_resurrected() {
        // A trigger that deletes its own schedule then fails: the post-trigger
        // failure write must be an UPDATE-if-exists, never a resurrecting
        // upsert (review C5).
        struct DeletingTrigger {
            store: Store,
        }
        #[async_trait]
        impl RunTrigger for DeletingTrigger {
            async fn trigger(&self, invocation: &ScheduleInvocation) -> FireOutcome {
                self.store
                    .delete_schedule(&invocation.schedule_id)
                    .await
                    .unwrap();
                FireOutcome::Failed {
                    reason: "boom after delete".to_owned(),
                }
            }
        }
        let store = Store::open_memory().await.unwrap();
        let emit: Arc<dyn Fn(EventBody) + Send + Sync> = Arc::new(|_| {});
        let sched = Scheduler::new(
            store.clone(),
            Arc::new(DeletingTrigger {
                store: store.clone(),
            }),
            emit,
        );
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        // due → fire → trigger deletes the row → failure write must not re-add
        sched.tick(utc("2026-07-24T10:01:05Z")).await.unwrap();
        assert!(
            store.get_schedule(&created.id).await.unwrap().is_none(),
            "deleted schedule was resurrected"
        );
    }

    #[tokio::test]
    async fn tick_survives_a_corrupt_schedule_row() {
        // A row with unreadable JSON must be skipped, not abort the tick so
        // that healthy schedules still fire (review C5).
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let good = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        // Inject a corrupt row directly (invalid spec JSON)
        agent24_store::test_hooks::insert_raw_schedule(
            &store,
            "sch_corrupt",
            "not-json",
            "2026-07-24T10:00:00Z",
        )
        .await
        .unwrap();
        // Healthy schedule still fires despite the corrupt neighbour
        assert_eq!(sched.tick(utc("2026-07-24T10:01:05Z")).await.unwrap(), 1);
        assert_eq!(trig.count(), 1);
        assert!(store.get_schedule(&good.id).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn run_now_fires_even_a_disabled_schedule() {
        // run_now is a manual override — it triggers regardless of `enabled`
        // and never touches next_run_at, so a user can run a paused schedule
        // on demand without re-enabling it.
        let trig = RecordingTrigger::new();
        let (sched, _events, store) =
            scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(3600), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        let disabled = sched
            .update(
                &created.id,
                ScheduleUpdate {
                    enabled: Some(false),
                    ..Default::default()
                },
                utc("2026-07-24T10:05:00Z"),
            )
            .await
            .unwrap();
        assert!(!disabled.enabled);
        assert_eq!(disabled.next_run_at, None);

        let outcome = sched
            .run_now(&created.id, utc("2026-07-24T10:06:00Z"))
            .await
            .unwrap();
        let RunNowOutcome::Run { run_id } = outcome else {
            panic!("expected a user row to answer Run, got {outcome:?}");
        };
        assert!(run_id.starts_with("run_"));
        assert_eq!(trig.count(), 1);
        // still disabled, next_run_at still None — run_now changed neither
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert!(!after.enabled);
        assert_eq!(after.next_run_at, None);
    }

    #[tokio::test]
    async fn update_and_delete_unknown_id_is_not_found() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, _store) = scheduler_with(trig).await;
        assert!(matches!(
            sched.get("sch_nope").await.unwrap_err(),
            ScheduleError::NotFound(_)
        ));
        assert!(matches!(
            sched.delete("sch_nope").await.unwrap_err(),
            ScheduleError::NotFound(_)
        ));
        assert!(matches!(
            sched
                .update(
                    "sch_nope",
                    ScheduleUpdate::default(),
                    utc("2026-07-24T10:00:00Z")
                )
                .await
                .unwrap_err(),
            ScheduleError::NotFound(_)
        ));
    }

    // ── ME4-1.2.2b2 review fixes (H1/M1/M2) ──────────────────────────────────

    #[tokio::test]
    async fn a_stale_agentrun_fire_loses_its_cas_and_the_next_real_tick_fires_once() {
        // Review H1: a concurrent revision bump between what a tick read and
        // what it would write must make that tick's fire a complete no-op
        // (no trigger call, no event) — and the SLOT is still due, so the
        // very next real tick fires normally, exactly once. Only a single
        // tick's worth of delay, never a double-fire and never a stuck row.
        let trig = RecordingTrigger::new();
        let (sched, events, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        assert_eq!(created.next_run_at.as_deref(), Some("2026-07-24T10:01:00Z"));

        // What a tick would have read a moment before the concurrent PATCH.
        let stale = store
            .list_schedules_for_tick()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.schedule.id == created.id)
            .unwrap();
        assert_eq!(stale.revision, 0);

        // Concurrent PATCH: rename only (spec/enabled unchanged) → revision
        // bumps (upsert_schedule always bumps it on a USER row), next_run_at
        // stays put (`recompute` only trips on spec/enabled).
        sched
            .update(
                &created.id,
                ScheduleUpdate {
                    name: Some("renamed".to_owned()),
                    ..Default::default()
                },
                utc("2026-07-24T10:00:30Z"),
            )
            .await
            .unwrap();
        let after_patch = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert_eq!(
            after_patch.next_run_at, stale.schedule.next_run_at,
            "a rename-only PATCH must not move next_run_at"
        );

        // Drive the STALE record straight into the private fire path, as if
        // this tick's read had raced the PATCH's commit.
        let due = next_fire::parse_iso(stale.schedule.next_run_at.as_deref().unwrap()).unwrap();
        let raw_next = stale.schedule.next_run_at.clone().unwrap();
        let now1 = utc("2026-07-24T10:01:05Z");
        let fired = sched
            .fire_agent_run(stale.schedule, stale.revision, &raw_next, due, now1)
            .await
            .unwrap();
        assert!(
            !fired,
            "a stale revision must lose the CAS and fire nothing"
        );
        assert_eq!(trig.count(), 0, "trigger must not be called on a lost CAS");
        assert!(
            events.lock().unwrap().is_empty(),
            "no schedule.fired for a lost CAS"
        );

        // The next REAL tick (a fresh read) fires normally — exactly once,
        // only a tick later than it "should" have.
        assert_eq!(sched.tick(now1).await.unwrap(), 1);
        assert_eq!(trig.count(), 1);
        assert_eq!(
            events.lock().unwrap().clone(),
            vec!["schedule.fired".to_owned()]
        );
    }

    #[tokio::test]
    async fn a_cas_loss_between_pre_advance_and_the_disable_write_suppresses_the_event() {
        // Review M1: the pre-advance can land (so the trigger DOES run and
        // DOES fail — this is the 5th, disabling failure) while the SEPARATE
        // post-trigger counter/disable write loses its own CAS (a concurrent
        // PATCH landed in the narrow window between the two writes). That
        // must not announce `schedule.disabled` for a disable that never
        // actually landed.
        let trig = RecordingTrigger::always_failing();
        let (sched, events, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let created = sched
            .create(every_create(60), utc("2026-07-24T10:00:00Z"))
            .await
            .unwrap();
        // Drive 4 failing ticks normally: consecutive_failures -> 4, still enabled.
        let mut t = utc("2026-07-24T10:01:05Z");
        for _ in 0..4 {
            sched.tick(t).await.unwrap();
            t += chrono::Duration::seconds(60);
        }
        let before = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert_eq!(before.consecutive_failures, 4);
        assert!(before.enabled);

        // The 5th tick's read.
        let stale = store
            .list_schedules_for_tick()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.schedule.id == created.id)
            .unwrap();
        let raw_next = stale.schedule.next_run_at.clone().unwrap();
        let due = next_fire::parse_iso(&raw_next).unwrap();

        // Between THIS tick's pre-advance and its (disabling) counter write,
        // a concurrent PATCH (rename only) bumps revision without moving
        // next_run_at — invisible to the pre-advance CAS (which already
        // landed by then) but not to the second one.
        let id = created.id.clone();
        let fired = sched
            .fire_agent_run_inner(
                stale.schedule,
                stale.revision,
                &raw_next,
                due,
                t,
                || async {
                    sched
                        .update(
                            &id,
                            ScheduleUpdate {
                                name: Some("renamed".to_owned()),
                                ..Default::default()
                            },
                            t,
                        )
                        .await
                        .unwrap();
                },
            )
            .await
            .unwrap();
        assert!(
            fired,
            "the trigger DID run (pre-advance landed) — only the disable write lost"
        );

        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert!(
            after.enabled,
            "a lost disable-write CAS must leave the schedule enabled — it was never disabled"
        );
        assert_eq!(
            after.consecutive_failures, 4,
            "the lost write's failures=5 never landed either"
        );
        assert!(
            !events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "schedule.disabled"),
            "a lost CAS must not emit schedule.disabled"
        );

        // positive control: a subsequent tick with no race really does
        // disable and emit, once its own (unraced) 5th failure lands.
        let after_pre_advance = store.get_schedule(&created.id).await.unwrap().unwrap();
        let due2 = next_fire::parse_iso(after_pre_advance.next_run_at.as_deref().unwrap()).unwrap();
        assert_eq!(
            sched
                .tick(due2 + chrono::Duration::seconds(5))
                .await
                .unwrap(),
            1
        );
        let final_state = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert!(!final_state.enabled);
        assert_eq!(final_state.consecutive_failures, 5);
        assert!(
            events
                .lock()
                .unwrap()
                .iter()
                .any(|e| e == "schedule.disabled"),
            "an UNraced 5th failure must still disable and announce it"
        );
    }

    #[tokio::test]
    async fn tick_matches_the_raw_stored_next_run_at_not_a_recanonicalized_one() {
        // Review M2: a legacy (or otherwise non-canonically-spelled) but
        // valid RFC-3339 `next_run_at` — milliseconds included — must still
        // fire. A CAS that compared against `fmt_iso(due)` (re-canonicalized:
        // no milliseconds) instead of the RAW stored string would spuriously
        // lose every time, because the DB's actual column never matches that
        // re-derived spelling.
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let schedule = Schedule {
            id: "sch_noncanon".to_owned(),
            name: "t".to_owned(),
            enabled: true,
            spec: ScheduleSpec::Every { secs: 60 },
            action: Some(ScheduleAction::AgentRun {
                prompt: "x".to_owned(),
                session_id: None,
                model_override: None,
            }),
            delivery: vec![],
            last_run_at: None,
            // Non-canonical: `fmt_iso` would spell this "2026-07-24T10:01:00Z"
            // (no milliseconds) — a different string.
            next_run_at: Some("2026-07-24T10:01:00.000Z".to_owned()),
            consecutive_failures: 0,
            owner: None,
            user_suspended: false,
            system_disabled_reason: None,
            effective_enabled: true,
            disabled_by: None,
        };
        store.upsert_schedule(&schedule).await.unwrap();

        assert_eq!(sched.tick(utc("2026-07-24T10:01:05Z")).await.unwrap(), 1);
        assert_eq!(trig.count(), 1);
    }

    // ── ME4-1.2.2b3: module rows (design §3.2/§4.2/§4.7, v2 M5) ─────────────

    #[tokio::test]
    async fn module_row_tick_records_a_pending_delivery_and_counts_no_failure() {
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let desired = every_module(60);
        let now0 = utc("2026-07-24T10:00:00Z");
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        store
            .upsert_module_schedule(
                "sch_mod1",
                "mod-a",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();

        let now1 = utc("2026-07-24T10:01:05Z");
        // the slot fires (the row's next_run_at advances)...
        assert_eq!(sched.tick(now1).await.unwrap(), 1);
        // ...but tick never calls trigger() for a module target — that's the
        // delivery pump's job (§3.2, ME4-1.3.1). What the daemon's real
        // `KernelTrigger` answers for this exact fire is asserted at the
        // top-level (agent24d) cut, against the REAL trigger — asserting it
        // here against `RecordingTrigger`'s own hardcoded stub would only be
        // checking the test double agrees with itself.
        assert_eq!(trig.count(), 0);

        let due = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].schedule_id, "sch_mod1");
        assert_eq!(due[0].owner_module, "mod-a");
        assert_eq!(due[0].module_key, "k");
        assert_eq!(due[0].status, "pending");
        assert_eq!(due[0].fire_trigger, "tick");
        let expected_fire_id =
            FireId::derive(FireTrigger::Tick, "sch_mod1", utc("2026-07-24T10:01:00Z"));
        assert_eq!(due[0].fire_id, expected_fire_id.as_str());

        // no failure was counted for a mere Deferred (§4.1/T3/T4) — the tick
        // path doesn't even touch this column for a module row.
        let after = store.get_schedule("sch_mod1").await.unwrap().unwrap();
        assert_eq!(after.consecutive_failures, 0);
    }

    #[tokio::test]
    async fn module_tick_matches_the_raw_stored_next_run_at_not_a_recanonicalized_one() {
        // Review M2, module-row half (`fire_agent_run`'s counterpart test
        // covers the AgentRun half): a module row whose `next_run_at` is a
        // valid-but-non-canonical RFC-3339 spelling (milliseconds) must still
        // fire — `fire_module`'s pre-advance CAS must pin the RAW string, not
        // `fmt_iso(due)`. There is no public API that writes a module row
        // with an arbitrary `next_run_at` (`upsert_module_schedule` always
        // canonicalizes it), so this reaches for the store's raw-SQL test
        // escape hatch (`test_hooks::pool`) to mutate it after a normal
        // create.
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let desired = every_module(60);
        let now0 = utc("2026-07-24T10:00:00Z");
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        store
            .upsert_module_schedule(
                "sch_mod5",
                "mod-a",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE schedules SET next_run_at = ? WHERE id = ?")
            .bind("2026-07-24T10:01:00.000Z") // non-canonical: has milliseconds
            .bind("sch_mod5")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        let now1 = utc("2026-07-24T10:01:05Z");
        assert_eq!(sched.tick(now1).await.unwrap(), 1);
        let due = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(
            due.len(),
            1,
            "the CAS must match against the raw stored string"
        );
    }

    #[tokio::test]
    async fn uninstalled_owner_only_advances_next_run_at_no_delivery_row() {
        // v2 M5: an owner absent from this run's catalogue still has its slot
        // advanced (skip-missed keeps working), but produces no new delivery
        // row — an uninstalled module's schedules must stop growing the table.
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let desired = every_module(60);
        let now0 = utc("2026-07-24T10:00:00Z");
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        store
            .upsert_module_schedule(
                "sch_mod2",
                "mod-gone",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();

        // this daemon's catalogue this run does not include "mod-gone".
        sched
            .installed_owners()
            .set(HashSet::from(["mod-other".to_owned()]));

        let now1 = utc("2026-07-24T10:01:05Z");
        assert_eq!(
            sched.tick(now1).await.unwrap(),
            1,
            "the slot still advances"
        );

        let due = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert!(
            due.is_empty(),
            "an uninstalled owner must not get a delivery row"
        );

        let after = store.get_schedule("sch_mod2").await.unwrap().unwrap();
        assert_eq!(after.next_run_at.as_deref(), Some("2026-07-24T10:02:05Z"));

        // positive control: an installed owner in the very same tick still
        // gets recorded.
        store
            .upsert_module_schedule(
                "sch_mod2b",
                "mod-other",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();
        assert_eq!(sched.tick(now1).await.unwrap(), 1);
        let due2 = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(due2.len(), 1);
        assert_eq!(due2[0].schedule_id, "sch_mod2b");
    }

    #[tokio::test]
    async fn tick_cas_loses_to_a_concurrent_spec_change_and_writes_no_delivery_row() {
        // Design §2.3/§4.2: the pre-advance and the delivery record are ONE
        // CAS'd transaction. A concurrent spec change (a module re-upsert
        // landing between what a tick read and what it would write) must make
        // the whole thing a no-op: no advance, no delivery row, no trigger
        // call. Mutation check: dropping the CAS's revision condition would
        // turn this green into a fire.
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let desired = every_module(60);
        let now0 = utc("2026-07-24T10:00:00Z");
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        store
            .upsert_module_schedule(
                "sch_mod3",
                "mod-a",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();

        // What a tick would have read.
        let records = store.list_schedules_for_tick().await.unwrap();
        let stale = records
            .into_iter()
            .find(|r| r.schedule.id == "sch_mod3")
            .unwrap();
        assert_eq!(stale.revision, 1, "a fresh module row starts at revision 1");
        assert_eq!(
            stale.schedule.next_run_at.as_deref(),
            Some("2026-07-24T10:01:00Z")
        );

        // Concurrent spec change: every 60s -> every 90s, landing "between"
        // the read above and the write below.
        let changed = ModuleScheduleDesired {
            spec: ScheduleSpec::Every { secs: 90 },
            enabled: true,
            label: "k".to_owned(),
        };
        let next_changed = next_fire(&changed.spec, now0).unwrap().map(fmt_iso);
        let (outcome, _state) = store
            .upsert_module_schedule(
                "sch_mod3",
                "mod-a",
                "k",
                &changed,
                next_changed.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Updated);

        // Drive the STALE (schedule, revision) straight into the tick's
        // private fire path, as if this tick had read it a moment before the
        // concurrent upsert committed.
        let due = next_fire::parse_iso(stale.schedule.next_run_at.as_deref().unwrap()).unwrap();
        let raw_next = stale.schedule.next_run_at.clone().unwrap();
        let now1 = utc("2026-07-24T10:01:05Z");
        let fired = sched
            .fire_module(stale.schedule, stale.revision, &raw_next, due, now1)
            .await
            .unwrap();
        assert!(
            !fired,
            "a stale revision must lose the CAS and fire nothing"
        );

        let deliveries = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert!(
            deliveries.is_empty(),
            "a CAS loss must not write a delivery row"
        );
        assert_eq!(trig.count(), 0);

        // positive control: the CURRENT (post-change) record fires cleanly.
        let current = store
            .list_schedules_for_tick()
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.schedule.id == "sch_mod3")
            .unwrap();
        let due2 = next_fire::parse_iso(current.schedule.next_run_at.as_deref().unwrap()).unwrap();
        assert_eq!(due2, utc("2026-07-24T10:01:30Z"));
        let raw_next2 = current.schedule.next_run_at.clone().unwrap();
        let fired2 = sched
            .fire_module(current.schedule, current.revision, &raw_next2, due2, now1)
            .await
            .unwrap();
        assert!(fired2, "the current, un-raced record must still fire");
        let deliveries2 = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(deliveries2.len(), 1);
    }

    #[tokio::test]
    async fn tick_cas_loses_to_a_revision_only_change_even_when_next_run_at_is_unchanged() {
        // The previous test's concurrent change also moved next_run_at, so it
        // cannot by itself prove the CAS checks `revision` (as opposed to
        // just `next_run_at`) — exactly the gap design §2.4/M9 calls out
        // ("revision 单独不够" cuts both ways: neither column alone is
        // enough). Isolate it: a label-only module upsert bumps revision
        // (§2.2's table) WITHOUT recomputing next_run_at (§6.2: `recompute`
        // is false when only `label` changed) — so this concurrent change is
        // invisible to a CAS that pins `next_run_at` but not `revision`.
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let desired = every_module(60);
        let now0 = utc("2026-07-24T10:00:00Z");
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        store
            .upsert_module_schedule(
                "sch_mod4",
                "mod-a",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();

        let records = store.list_schedules_for_tick().await.unwrap();
        let stale = records
            .into_iter()
            .find(|r| r.schedule.id == "sch_mod4")
            .unwrap();
        assert_eq!(stale.revision, 1);

        // Concurrent, label-only change: same spec, same next_run_at, bumped
        // revision only.
        let relabeled = ModuleScheduleDesired {
            spec: desired.spec,
            enabled: true,
            label: "k2".to_owned(),
        };
        let (outcome, state) = store
            .upsert_module_schedule(
                "sch_mod4",
                "mod-a",
                "k",
                &relabeled,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Updated);
        assert_eq!(
            state.next_run_at.as_deref(),
            stale.schedule.next_run_at.as_deref(),
            "a label-only change must not move next_run_at"
        );

        let due = next_fire::parse_iso(stale.schedule.next_run_at.as_deref().unwrap()).unwrap();
        let raw_next = stale.schedule.next_run_at.clone().unwrap();
        let now1 = utc("2026-07-24T10:01:05Z");
        let fired = sched
            .fire_module(stale.schedule, stale.revision, &raw_next, due, now1)
            .await
            .unwrap();
        assert!(
            !fired,
            "a stale revision must lose the CAS even though next_run_at didn't move"
        );
        let deliveries = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert!(deliveries.is_empty());
        assert_eq!(trig.count(), 0);
    }

    #[tokio::test]
    async fn module_row_run_now_records_a_fire_without_disturbing_the_tick_slot() {
        // Review H1 (design §4.7): run_now on a module row answers
        // `Fire{fire_id}`, leaves next_run_at alone, records `fire_trigger =
        // "run_now"`, and does not disturb the tick source's own outstanding
        // fire (v2, H3). Same-second double calls are idempotent (same id,
        // one row); the run_now fire_id differs from the same-slot tick one
        // (trigger is in `FireId`'s domain, design v2 L3).
        let trig = RecordingTrigger::new();
        let (sched, _ev, store) = scheduler_with(Arc::clone(&trig) as Arc<dyn RunTrigger>).await;
        let desired = every_module(60);
        let now0 = utc("2026-07-24T10:00:00Z");
        let next = next_fire(&desired.spec, now0).unwrap().map(fmt_iso);
        store
            .upsert_module_schedule(
                "sch_rn1",
                "mod-a",
                "k",
                &desired,
                next.as_deref(),
                &fmt_iso(now0),
                256,
            )
            .await
            .unwrap();

        // The tick fires first, leaving an outstanding tick-sourced delivery.
        let now1 = utc("2026-07-24T10:01:05Z");
        assert_eq!(sched.tick(now1).await.unwrap(), 1);
        let tick_due = store
            .due_deliveries(&fmt_iso(now1), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(tick_due.len(), 1);
        let tick_fire_id = tick_due[0].fire_id.clone();

        let before = store.get_schedule("sch_rn1").await.unwrap().unwrap();

        let now2 = utc("2026-07-24T10:01:10Z");
        let outcome = sched.run_now("sch_rn1", now2).await.unwrap();
        let RunNowOutcome::Fire { fire_id } = outcome else {
            panic!("expected a module row to answer Fire");
        };
        assert_ne!(
            fire_id.as_str(),
            tick_fire_id,
            "run_now must not collide with the same-slot tick fire"
        );
        assert_eq!(
            trig.count(),
            0,
            "run_now never calls trigger() synchronously either"
        );

        let after = store.get_schedule("sch_rn1").await.unwrap().unwrap();
        assert_eq!(
            after.next_run_at, before.next_run_at,
            "run_now must not touch next_run_at"
        );

        let due_after = store
            .due_deliveries(&fmt_iso(now2), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(
            due_after.len(),
            2,
            "both the tick fire and the run_now fire are outstanding"
        );
        let run_now_row = due_after
            .iter()
            .find(|d| d.fire_id == fire_id.as_str())
            .unwrap();
        assert_eq!(run_now_row.fire_trigger, "run_now");
        let still_tick_row = due_after
            .iter()
            .find(|d| d.fire_id == tick_fire_id)
            .unwrap();
        assert_eq!(
            still_tick_row.status, "pending",
            "the tick's outstanding row is unaffected by run_now"
        );

        // same-second double call → same id, one row (idempotent).
        let outcome2 = sched.run_now("sch_rn1", now2).await.unwrap();
        let RunNowOutcome::Fire { fire_id: fire_id2 } = outcome2 else {
            panic!("expected a module row to answer Fire");
        };
        assert_eq!(fire_id2.as_str(), fire_id.as_str());
        let due_after2 = store
            .due_deliveries(&fmt_iso(now2), "[]", 10, 10)
            .await
            .unwrap();
        assert_eq!(
            due_after2.len(),
            2,
            "a same-second repeat run_now must not add a new row"
        );
    }
}
