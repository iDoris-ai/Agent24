//! Agent24 wall-clock scheduler (C5).
//!
//! The product soul: cron / every / at schedules that fire agent runs. Design
//! constraints (SPEC-002 §1.5, ADR-026 hard constraint #7):
//! - **pre-advance**: a due schedule's `next_run_at` is recomputed and
//!   persisted BEFORE the run is triggered, so a crash mid-fire cannot double
//!   fire (openfang's cron lesson).
//! - **skip-missed**: the new `next_run_at` is computed from *now*, so a
//!   schedule that lay due while the daemon was down fires once and jumps to
//!   its next future slot — never a replay burst (`MissedTickBehavior::Skip`).
//! - **fail-safe disable**: `MAX_CONSECUTIVE_FAILURES` trigger failures in a
//!   row disable the schedule and emit `schedule.disabled`.
//!
//! `tick(now)` is a single pass driven by an injected instant, so the whole
//! engine is testable with a mock clock and NO real sleeps.

pub mod next_fire;

use std::sync::Arc;
use std::time::Duration;

use agent24_core::record_schedule_result;
use agent24_core::util::ulid;
use agent24_protocol::{
    EventBody, Schedule, ScheduleAction, ScheduleCreate, ScheduleDisabledPayload,
    ScheduleFiredPayload, ScheduleUpdate,
};
use agent24_store::{Store, StoreError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio_util::sync::CancellationToken;

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

/// Fires a schedule's action, returning the created run id. Implemented by the
/// daemon over `RunManager` — the scheduler crate stays free of the agent loop.
#[async_trait]
pub trait RunTrigger: Send + Sync {
    async fn trigger(&self, action: &ScheduleAction, schedule_id: &str) -> Result<String, String>;
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
        })
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
            action: create.action,
            delivery: create.delivery,
            last_run_at: None,
            next_run_at,
            consecutive_failures: 0,
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
            schedule.action = action;
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

    /// Fire immediately without touching `next_run_at` (manual "run now").
    pub async fn run_now(&self, id: &str) -> Result<String, ScheduleError> {
        let schedule = self.get(id).await?;
        self.trigger
            .trigger(&schedule.action, &schedule.id)
            .await
            .map_err(ScheduleError::Invalid)
    }

    // ── the tick ─────────────────────────────────────────────────────────────

    /// Process every schedule due at `now`. Returns how many fired. Free of
    /// real time — the caller supplies `now`, so tests drive it directly.
    pub async fn tick(&self, now: DateTime<Utc>) -> Result<usize, ScheduleError> {
        let schedules = self.store.list_schedules().await?;
        let mut fired = 0;
        for schedule in schedules {
            if !schedule.enabled {
                continue;
            }
            let Some(next_run_at) = &schedule.next_run_at else {
                continue;
            };
            let due = match next_fire::parse_iso(next_run_at) {
                Ok(due) => due,
                Err(err) => {
                    tracing::error!("schedule {} has unparsable next_run_at: {err}", schedule.id);
                    continue;
                }
            };
            if due > now {
                continue;
            }
            self.fire(schedule, now).await?;
            fired += 1;
        }
        Ok(fired)
    }

    /// Pre-advance THEN trigger. The advanced `next_run_at` is persisted first
    /// so a crash between here and the trigger cannot re-fire the same slot.
    async fn fire(&self, mut schedule: Schedule, now: DateTime<Utc>) -> Result<(), ScheduleError> {
        // Skip-missed: next slot is computed from `now`, not the stale due time
        let advanced = match next_fire(&schedule.spec, now) {
            Ok(next) => next.map(fmt_iso),
            Err(err) => {
                // A spec that no longer computes (shouldn't happen post-validate)
                // disables the schedule rather than looping forever.
                tracing::error!(
                    "schedule {} next_fire failed: {err}; disabling",
                    schedule.id
                );
                schedule.enabled = false;
                schedule.next_run_at = None;
                self.store.upsert_schedule(&schedule).await?;
                self.emit_disabled(&schedule.id, "next_fire_error");
                return Ok(());
            }
        };
        schedule.last_run_at = Some(fmt_iso(now));
        schedule.next_run_at = advanced;
        self.store.upsert_schedule(&schedule).await?;

        // Trigger the run (synchronous creation; the run executes in the bg)
        match self.trigger.trigger(&schedule.action, &schedule.id).await {
            Ok(run_id) => {
                if schedule.consecutive_failures != 0 {
                    schedule.consecutive_failures = 0;
                    self.store.upsert_schedule(&schedule).await?;
                }
                self.emit.as_ref()(EventBody::ScheduleFired(ScheduleFiredPayload {
                    schedule_id: schedule.id.clone(),
                    run_id,
                }));
            }
            Err(err) => {
                tracing::warn!("schedule {} trigger failed: {err}", schedule.id);
                let health = record_schedule_result(&mut schedule.consecutive_failures, false);
                if health == agent24_core::ScheduleHealth::MustDisable {
                    schedule.enabled = false;
                    schedule.next_run_at = None;
                    self.store.upsert_schedule(&schedule).await?;
                    self.emit_disabled(&schedule.id, "consecutive_failures");
                } else {
                    self.store.upsert_schedule(&schedule).await?;
                }
            }
        }
        Ok(())
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
    use agent24_protocol::ScheduleSpec;
    use next_fire::parse_iso;
    use std::sync::Mutex;

    /// Records every trigger; optionally fails the first N calls.
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
        async fn trigger(
            &self,
            _action: &ScheduleAction,
            schedule_id: &str,
        ) -> Result<String, String> {
            let mut fail_until = self.fail_until.lock().unwrap();
            if *fail_until > 0 {
                *fail_until = fail_until.saturating_sub(1);
                return Err("trigger boom".to_owned());
            }
            let mut calls = self.calls.lock().unwrap();
            calls.push(schedule_id.to_owned());
            Ok(format!("run_{}", calls.len()))
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
        let run_id = sched.run_now(&created.id).await.unwrap();
        assert!(run_id.starts_with("run_"));
        assert_eq!(trig.count(), 1);
        // next_run_at unchanged; no schedule.fired event (that's tick-only)
        let after = store.get_schedule(&created.id).await.unwrap().unwrap();
        assert_eq!(after.next_run_at, created.next_run_at);
        assert!(events.lock().unwrap().is_empty());
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
}
