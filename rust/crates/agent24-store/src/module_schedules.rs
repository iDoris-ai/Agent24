//! ME4-1.2.1a/b/c/d — module-owned schedules, their deliveries, the REST
//! guardrails on top of both, and the tick loop's read-model. See
//! `docs/design/ME4-S1-scheduler-callback.md`:
//! - §2 (D1) — migration `0007_module_schedules.sql` (full table, landed in
//!   1.2.1a), the revision rule, the tick's CAS pre-advance (1.2.1b), the
//!   REST PATCH CAS (1.2.1c, this cut).
//! - §4 (D3) — the `schedule_deliveries` state machine's storage side
//!   (1.2.1b): recording a fire (with supersede + prune, same transaction as
//!   the pre-advance), applying one attempt's outcome (CAS'd), the pump's due
//!   query, the expiry sweep.
//! - §6.1/§6.2 (D5) — the upsert SQL and outcome judgement, the `list`/upsert
//!   read-model (1.2.1a).
//! - §8.2 (D7) — suspend/resume (idempotent, with a revision CAS on resume —
//!   review, M-2) and the PATCH write-back CAS, for both user and AgentRun
//!   rows (1.2.1c).
//! - `TickScheduleRow`/`list_schedules_for_tick` (1.2.1d, this cut) — the
//!   tick loop's read-model; see its own doc comment for the convergence
//!   note (ME4-1.2.2a should let this collapse into the design's
//!   `ScheduleRecord`).
//!
//! ME4-1.2.1 (this whole stack) is now complete: a/b/c/d together are the
//! store layer the design's §13 lists under ME4-1.2.1. ME4-1.2.2a/b/c (the
//! protocol view, the trigger interface + tick, the REST routes) build on
//! top of it from here, in a separate task.
//!
//! Scope note (task ME4-1.2.1 is store-only): the pure delivery state
//! machine (`apply_outcome`/`FireOutcome`/`Applied`) and the trigger
//! interface (`FireId`/`RunTrigger`) belong to `agent24-scheduler`
//! (ME4-1.2.2b/1.3.1) and are NOT introduced here — this crate has no
//! dependency on `agent24-scheduler`. Every function below therefore takes
//! already-decided, primitive values (status strings, attempt counts,
//! pre-formatted ISO-8601 timestamps, `fire_id` as `&str`) rather than those
//! crate's types. `FireTrigger` below is a minimal store-local stand-in for
//! the `tick`/`run_now` domain tag (needed to bind the
//! `schedule_deliveries.fire_trigger` CHECK column correctly) — review:
//! `agent24-scheduler` should `pub use agent24_store::FireTrigger` for its
//! own use rather than defining a second, same-named type (see
//! `TickScheduleRow`'s doc comment).

use agent24_protocol::{Schedule, ScheduleSpec};
use serde::Serialize;
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store, StoreError};

/// The JSON kept in the (`NOT NULL`) `action` column of a module row. NOT a
/// `ScheduleAction`: an older binary's lenient tick list
/// (`list_schedules_lenient`) fails to deserialize it and skips the row
/// instead of firing it — the row's meaning comes from `owner_module`, never
/// from this column (§2.1).
pub const MODULE_ACTION_SENTINEL: &str = r#"{"type":"module_delivery"}"#;

/// `schedule_deliveries.fire_trigger` (§2.1: named `fire_trigger`, not
/// `trigger` — `TRIGGER` is a SQLite keyword).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireTrigger {
    Tick,
    RunNow,
}

impl FireTrigger {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Tick => "tick",
            Self::RunNow => "run_now",
        }
    }
}

// ── §6.1 read-model types ────────────────────────────────────────────────────

/// A module's desired state for one key, as carried by `_a24/scheduler/upsert`
/// (validated by the caller — ME4-1.4.1).
pub struct ModuleScheduleDesired {
    pub spec: ScheduleSpec,
    pub enabled: bool,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpsertOutcome {
    Created,
    Updated,
    Unchanged,
}

/// The schedule's most recent fire from one trigger source, whatever its
/// state — so a one-shot `At` that expired undelivered is distinguishable
/// from one that was delivered (design §4.1, v2 H3).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastFire {
    pub fire_id: String,
    pub scheduled_for: String,
    /// `pending` | `deferred` | `delivered` | `failed` | `expired`
    pub status: String,
    pub last_error: Option<String>,
}

/// v3 (M-A): the latest fire per source, so a `run_now` never hides what
/// happened to the schedule's last tick slot (and the reverse).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LastFires {
    pub tick: Option<LastFire>,
    pub run_now: Option<LastFire>,
}

/// One module row as the module itself sees it via `list`/upsert's echo: its
/// desired state (`spec`/`enabled`/`label`) plus the two kernel-side reasons
/// it may not fire and when it next will. Never carries the kernel's
/// `schedule_id` (§6.1).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ModuleScheduleState {
    pub key: String,
    pub spec: ScheduleSpec,
    pub enabled: bool,
    pub label: String,
    pub user_suspended: bool,
    pub system_disabled_reason: Option<String>,
    pub next_run_at: Option<String>,
    pub last_fire: LastFires,
}

/// `schedules` plus the schedule's most recent fire PER SOURCE (v3, M-A):
/// `t_*` = latest tick fire, `r_*` = latest run_now fire.
const MODULE_STATE_SELECT: &str = "\
SELECT s.*, \
       t.fire_id AS t_id, t.scheduled_for AS t_for, t.status AS t_status, t.last_error AS t_err, \
       r.fire_id AS r_id, r.scheduled_for AS r_for, r.status AS r_status, r.last_error AS r_err \
FROM schedules s \
LEFT JOIN schedule_deliveries t ON t.fire_id = ( \
    SELECT fire_id FROM schedule_deliveries WHERE schedule_id = s.id AND fire_trigger = 'tick' \
    ORDER BY created_at DESC, rowid DESC LIMIT 1) \
LEFT JOIN schedule_deliveries r ON r.fire_id = ( \
    SELECT fire_id FROM schedule_deliveries WHERE schedule_id = s.id AND fire_trigger = 'run_now' \
    ORDER BY created_at DESC, rowid DESC LIMIT 1) \
WHERE s.owner_module = ?";

fn last_fire_of(r: &SqliteRow, p: &str) -> Option<LastFire> {
    r.get::<Option<String>, _>(format!("{p}_id").as_str())
        .map(|fire_id| LastFire {
            fire_id,
            scheduled_for: r.get(format!("{p}_for").as_str()),
            status: r.get(format!("{p}_status").as_str()),
            last_error: r.get(format!("{p}_err").as_str()),
        })
}

fn state_from_row(r: &SqliteRow) -> Result<ModuleScheduleState> {
    let last_fire = LastFires {
        tick: last_fire_of(r, "t"),
        run_now: last_fire_of(r, "r"),
    };
    Ok(ModuleScheduleState {
        key: r.get("module_key"),
        spec: serde_json::from_str(&r.get::<String, _>("spec"))?,
        enabled: r.get("enabled"),
        label: r.get("name"),
        user_suspended: r.get("user_suspended"),
        system_disabled_reason: r.get("system_disabled_reason"),
        next_run_at: r.get("next_run_at"),
        last_fire,
    })
}

/// Retire every outstanding fire of one schedule (T9, §4.3): used by a module
/// upsert that changes `spec` or turns `enabled` off (this cut), and by a
/// user suspend (ME4-1.2.1c's `set_user_suspended`).
const EXPIRE_OUTSTANDING_SQL: &str = "\
UPDATE schedule_deliveries \
SET status = 'expired', next_attempt_at = NULL, last_error = ?1, updated_at = ?2 \
WHERE schedule_id = ?3 AND status IN ('pending', 'deferred')";

impl Store {
    // ── §6.2 upsert / delete / list ─────────────────────────────────────────

    /// S1-4: read–compare–write under `BEGIN IMMEDIATE` (the write lock is
    /// taken at BEGIN, so two concurrent upserts of one key serialise: one
    /// `Created`, the other `Updated`/`Unchanged`). `next_if_recomputed` is
    /// `next_fire(spec, now)`, computed by the caller before the transaction
    /// (a pure function); the transaction decides whether it applies.
    ///
    /// # Errors
    /// [`StoreError::QuotaExceeded`] (a brand-new key past `quota` rows for
    /// this owner — nothing written), or storage/serialization.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_module_schedule(
        &self,
        new_id: &str,
        owner: &str,
        key: &str,
        desired: &ModuleScheduleDesired,
        next_if_recomputed: Option<&str>,
        now: &str,
        quota: u32,
    ) -> Result<(UpsertOutcome, ModuleScheduleState)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let existing = sqlx::query(
            "SELECT id, name, enabled, spec, user_suspended, system_disabled_reason, \
                    next_run_at, revision, module_key \
             FROM schedules WHERE owner_module = ? AND module_key = ?",
        )
        .bind(owner)
        .bind(key)
        .fetch_optional(&mut *tx)
        .await?;
        let spec_json = serde_json::to_string(&desired.spec)?;
        let outcome = match existing {
            None => {
                let n: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM schedules WHERE owner_module = ?")
                        .bind(owner)
                        .fetch_one(&mut *tx)
                        .await?;
                if n >= i64::from(quota) {
                    return Err(StoreError::QuotaExceeded); // tx dropped → rollback
                }
                let next = if desired.enabled {
                    next_if_recomputed
                } else {
                    None
                };
                sqlx::query(
                    "INSERT INTO schedules (id, name, enabled, spec, action, delivery, last_run_at, \
                         next_run_at, consecutive_failures, owner_module, module_key, revision, \
                         user_suspended, system_disabled_reason) \
                     VALUES (?, ?, ?, ?, ?, '[]', NULL, ?, 0, ?, ?, 1, 0, NULL)",
                )
                .bind(new_id)
                .bind(&desired.label)
                .bind(desired.enabled)
                .bind(&spec_json)
                .bind(MODULE_ACTION_SENTINEL)
                .bind(next)
                .bind(owner)
                .bind(key)
                .execute(&mut *tx)
                .await?;
                UpsertOutcome::Created
            }
            Some(row) => {
                let id: String = row.get("id");
                let revision: i64 = row.get("revision");
                let stored_spec: ScheduleSpec =
                    serde_json::from_str(&row.get::<String, _>("spec"))?;
                let stored_enabled: bool = row.get("enabled");
                let stored_label: String = row.get("name");
                let suspended: bool = row.get("user_suspended");
                let sys: Option<String> = row.get("system_disabled_reason");
                let stored_next: Option<String> = row.get("next_run_at");
                let spec_changed = stored_spec != desired.spec;
                let enabled_changed = stored_enabled != desired.enabled;
                if !spec_changed
                    && !enabled_changed
                    && stored_label == desired.label
                    && sys.is_none()
                {
                    UpsertOutcome::Unchanged // no write, no revision bump
                } else {
                    let recompute = spec_changed || enabled_changed || sys.is_some();
                    let next = if !recompute {
                        stored_next
                    } else if desired.enabled && !suspended {
                        next_if_recomputed.map(str::to_owned)
                    } else {
                        None
                    };
                    let updated = sqlx::query(
                        "UPDATE schedules SET name = ?, enabled = ?, spec = ?, next_run_at = ?, \
                             consecutive_failures = CASE WHEN ? THEN 0 ELSE consecutive_failures END, \
                             system_disabled_reason = NULL, revision = revision + 1 \
                         WHERE id = ? AND revision = ?",
                    )
                    .bind(&desired.label)
                    .bind(desired.enabled)
                    .bind(&spec_json)
                    .bind(next)
                    .bind(recompute)
                    .bind(&id)
                    .bind(revision)
                    .execute(&mut *tx)
                    .await?;
                    // Structurally cannot miss: `id`/`revision` were just read
                    // inside THIS `BEGIN IMMEDIATE` transaction, which holds
                    // the write lock for its whole duration — nothing else
                    // could have changed the row between the SELECT above and
                    // this UPDATE. A 0 here would mean that invariant broke.
                    debug_assert_eq!(
                        updated.rows_affected(),
                        1,
                        "upsert_module_schedule's own read-then-write raced itself"
                    );
                    if spec_changed || (enabled_changed && !desired.enabled) {
                        sqlx::query(EXPIRE_OUTSTANDING_SQL)
                            .bind("superseded_by_upsert")
                            .bind(now)
                            .bind(&id)
                            .execute(&mut *tx)
                            .await?;
                    }
                    UpsertOutcome::Updated
                }
            }
        };
        let row = sqlx::query(&format!("{MODULE_STATE_SELECT} AND s.module_key = ?"))
            .bind(owner)
            .bind(key)
            .fetch_one(&mut *tx)
            .await?;
        let state = state_from_row(&row)?;
        tx.commit().await?;
        Ok((outcome, state))
    }

    /// `delete{key}`: scoped by owner in the `WHERE` clause — another
    /// module's key is simply absent, not an error. Deliveries go with it
    /// (`ON DELETE CASCADE`).
    ///
    /// # Errors
    /// Storage.
    pub async fn delete_module_schedule(&self, owner: &str, key: &str) -> Result<bool> {
        let r = sqlx::query("DELETE FROM schedules WHERE owner_module = ? AND module_key = ?")
            .bind(owner)
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// # Errors
    /// Storage/serialization.
    pub async fn list_module_schedules(&self, owner: &str) -> Result<Vec<ModuleScheduleState>> {
        let rows = sqlx::query(&format!("{MODULE_STATE_SELECT} ORDER BY s.module_key"))
            .bind(owner)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(state_from_row).collect()
    }

    /// # Errors
    /// Storage.
    pub async fn count_module_schedules(&self, owner: &str) -> Result<u32> {
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedules WHERE owner_module = ?")
            .bind(owner)
            .fetch_one(self.pool())
            .await?;
        Ok(u32::try_from(n).unwrap_or(u32::MAX))
    }
}

// ── §2.3/§4.2 tick pre-advance + fire recording (ME4-1.2.1b) ────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Advance {
    /// Pre-advance landed (and the fire, if any, was recorded in the same
    /// transaction).
    Advanced,
    /// The CAS lost: the row is gone, its revision moved, it is no longer
    /// eligible (disabled/suspended/system-disabled), or this `next_run_at`
    /// slot was already advanced past. Nothing was written; the caller skips
    /// this row for this tick.
    Lost,
}

/// A module fire to record in the same transaction as the tick's pre-advance
/// (`advance_and_record_fire`) or a `run_now` (`record_run_now_fire`).
///
/// `owner_module`/`module_key` are advisory for `advance_and_record_fire`
/// (review, L-1): it re-reads both from the `schedules` row itself, inside
/// the same transaction, rather than trusting these fields — a caller bug
/// (stale/mismatched owner or key) must not corrupt `schedule_deliveries`.
/// `record_run_now_fire` builds its own `NewFire` from a row it already read,
/// so for that path these fields are simply correct by construction.
pub struct NewFire<'a> {
    pub fire_id: &'a str,
    pub owner_module: &'a str,
    pub module_key: &'a str,
    pub scheduled_for: &'a str,
    pub fired_at: &'a str,
    pub trigger: FireTrigger,
    pub expires_at: &'a str,
}

/// Terminal rows kept per (schedule, source) — v3 (L-B): per SOURCE, not per
/// schedule. Five delivered `run_now`s after the schedule's last tick fire
/// must not prune the tick fire's `last_fire` away. (Review, L-4: named
/// `..._PER_SOURCE`, not `..._PER_SCHEDULE` — the cap is per (schedule,
/// source), and the old name read as "per schedule" on its own.)
pub const KEEP_TERMINAL_PER_SOURCE: i64 = 4;

const PRUNE_TERMINAL_SQL: &str = "\
DELETE FROM schedule_deliveries \
WHERE schedule_id = ?1 AND fire_trigger = ?2 AND status IN ('delivered', 'failed', 'expired') \
  AND rowid NOT IN ( \
      SELECT rowid FROM schedule_deliveries \
      WHERE schedule_id = ?1 AND fire_trigger = ?2 \
        AND status IN ('delivered', 'failed', 'expired') \
      ORDER BY updated_at DESC, rowid DESC LIMIT ?3)";

/// Supersede older outstanding fires of this schedule **with the same
/// trigger** (a `run_now` never retires a tick fire, nor the reverse, v2 H3):
/// a row never sent (`attempts = 0`) is deleted outright — it left no trace
/// worth keeping (v3, L-A: `attempts = 0` does not prove it was never sent,
/// only that no attempt result was ever recorded; deleting it only costs
/// observability, never a delivery guarantee); one sent at least once becomes
/// `expired('superseded')`. Then insert this fire (idempotent on `fire_id`)
/// and prune this source's terminal rows to `KEEP_TERMINAL_PER_SOURCE`.
async fn record_fire_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    schedule_id: &str,
    f: &NewFire<'_>,
    now: &str,
) -> Result<()> {
    sqlx::query(
        "DELETE FROM schedule_deliveries \
         WHERE schedule_id = ? AND fire_trigger = ? AND status IN ('pending', 'deferred') \
           AND attempts = 0 AND fire_id <> ?",
    )
    .bind(schedule_id)
    .bind(f.trigger.as_str())
    .bind(f.fire_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "UPDATE schedule_deliveries \
         SET status = 'expired', next_attempt_at = NULL, last_error = 'superseded', updated_at = ? \
         WHERE schedule_id = ? AND fire_trigger = ? AND status IN ('pending', 'deferred') \
           AND fire_id <> ?",
    )
    .bind(now)
    .bind(schedule_id)
    .bind(f.trigger.as_str())
    .bind(f.fire_id)
    .execute(&mut **tx)
    .await?;
    sqlx::query(
        "INSERT INTO schedule_deliveries (fire_id, schedule_id, owner_module, module_key, \
             scheduled_for, fired_at, fire_trigger, status, attempts, next_attempt_at, \
             expires_at, last_error, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?, NULL, ?, ?) \
         ON CONFLICT (fire_id) DO NOTHING",
    )
    .bind(f.fire_id)
    .bind(schedule_id)
    .bind(f.owner_module)
    .bind(f.module_key)
    .bind(f.scheduled_for)
    .bind(f.fired_at)
    .bind(f.trigger.as_str())
    .bind(now)
    .bind(f.expires_at)
    .bind(now)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    sqlx::query(PRUNE_TERMINAL_SQL)
        .bind(schedule_id)
        .bind(f.trigger.as_str())
        .bind(KEEP_TERMINAL_PER_SOURCE)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

// ── §5.4 pump read model (ME4-1.2.1b) ───────────────────────────────────────

/// One row the delivery pump picked up to attempt.
#[derive(Debug, Clone, PartialEq)]
pub struct DueDelivery {
    pub fire_id: String,
    pub schedule_id: String,
    pub owner_module: String,
    pub module_key: String,
    pub scheduled_for: String,
    pub fired_at: String,
    /// `"tick"` | `"run_now"`.
    pub fire_trigger: String,
    /// `"pending"` | `"deferred"`.
    pub status: String,
    pub attempts: i64,
}

/// The pump's due set (§5.4): non-terminal, due, not expired, owner not in
/// the pump's skip cache (`?2` = JSON array of owner names), at most
/// `?3` rows per owner (so one owner's backlog cannot fill the window),
/// oldest slot first, capped overall at `?4`.
const DUE_DELIVERIES_SQL: &str = "\
SELECT fire_id, schedule_id, owner_module, module_key, scheduled_for, fired_at, \
       fire_trigger, status, attempts \
FROM ( \
    SELECT *, ROW_NUMBER() OVER ( \
               PARTITION BY owner_module ORDER BY scheduled_for, fire_id) AS rn \
    FROM schedule_deliveries \
    WHERE status IN ('pending', 'deferred') AND next_attempt_at <= ?1 AND expires_at > ?1 \
      AND owner_module NOT IN (SELECT value FROM json_each(?2)) \
) \
WHERE rn <= ?3 \
ORDER BY scheduled_for, fire_id \
LIMIT ?4";

fn due_delivery_from(r: &SqliteRow) -> DueDelivery {
    DueDelivery {
        fire_id: r.get("fire_id"),
        schedule_id: r.get("schedule_id"),
        owner_module: r.get("owner_module"),
        module_key: r.get("module_key"),
        scheduled_for: r.get("scheduled_for"),
        fired_at: r.get("fired_at"),
        fire_trigger: r.get("fire_trigger"),
        status: r.get("status"),
        attempts: r.get("attempts"),
    }
}

/// A non-terminal row older than 24h from `fired_at` (§4.5's sweep, `EXPIRE_SQL`).
const EXPIRE_SQL: &str = "\
UPDATE schedule_deliveries \
SET status = 'expired', next_attempt_at = NULL, last_error = 'ttl', updated_at = ?1 \
WHERE status IN ('pending', 'deferred') AND expires_at <= ?1";

/// What one delivery attempt's already-decided outcome writes (the pure
/// judgement — "3 failures at 5s/15s back off, then `failed`" — is
/// `agent24-scheduler`'s `apply_outcome`, ME4-1.3.1; this is only the CAS'd
/// write of its result).
pub struct DeliveryOutcomeWrite<'a> {
    /// `"delivered"` | `"pending"` | `"deferred"` | `"failed"`.
    pub status: &'a str,
    pub attempts: i64,
    /// `Some` iff `status` is non-terminal (mirrors the table's CHECK).
    pub next_attempt_at: Option<&'a str>,
    pub last_error: Option<&'a str>,
    /// `consecutive_failures = 0` on the schedule.
    pub reset_schedule_failures: bool,
    /// `consecutive_failures += 1` on the schedule (and maybe system-disable
    /// once it reaches `disable_at`).
    pub count_schedule_failure: bool,
}

impl Store {
    /// S1-4 + S1-8: the tick's runtime write, revision-CAS'd (§2.3), and —
    /// when the row turns out to be module-owned and `fire` is `Some` — the
    /// delivery row, in ONE `BEGIN IMMEDIATE` transaction (§4.2). Used for
    /// BOTH row kinds: an AgentRun row's pre-advance passes `fire: None` (it
    /// has no delivery row); a module row's tick passes `fire: Some(..)`
    /// when its owner is installed this run, `None` otherwise (v2, M5 — the
    /// caller decides installedness, not this function).
    ///
    /// Review, L-1: `owner_module`/`module_key` for the delivery row are
    /// read fresh from `schedules` inside this transaction, not taken from
    /// `fire`'s fields — so a caller bug (wrong owner/key, or passing
    /// `Some(fire)` for what the row turns out to be, a USER row) cannot
    /// corrupt `schedule_deliveries`; a fire is recorded if and only if the
    /// row is module-owned.
    ///
    /// # Errors
    /// Storage/serialization.
    #[allow(clippy::too_many_arguments)]
    pub async fn advance_and_record_fire(
        &self,
        schedule_id: &str,
        seen_revision: i64,
        due: &str,
        advanced_next: Option<&str>,
        now: &str,
        fire: Option<NewFire<'_>>,
    ) -> Result<Advance> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let r = sqlx::query(
            "UPDATE schedules SET last_run_at = ?, next_run_at = ? \
             WHERE id = ? AND revision = ? AND next_run_at = ? \
               AND enabled = 1 AND user_suspended = 0 AND system_disabled_reason IS NULL",
        )
        .bind(now)
        .bind(advanced_next)
        .bind(schedule_id)
        .bind(seen_revision)
        .bind(due)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 {
            return Ok(Advance::Lost);
        }
        let owner_row = sqlx::query("SELECT owner_module, module_key FROM schedules WHERE id = ?")
            .bind(schedule_id)
            .fetch_one(&mut *tx)
            .await?;
        let owner_module: Option<String> = owner_row.get("owner_module");
        match (owner_module, fire) {
            (Some(owner), Some(f)) => {
                debug_assert_eq!(
                    f.trigger,
                    FireTrigger::Tick,
                    "advance_and_record_fire is the TICK pre-advance path; \
                     run_now goes through record_run_now_fire"
                );
                let key: String = owner_row.get("module_key");
                let corrected = NewFire {
                    fire_id: f.fire_id,
                    owner_module: &owner,
                    module_key: &key,
                    scheduled_for: f.scheduled_for,
                    fired_at: f.fired_at,
                    trigger: f.trigger,
                    expires_at: f.expires_at,
                };
                record_fire_in(&mut tx, schedule_id, &corrected, now).await?;
            }
            (None, _fire) => {
                // A USER (AgentRun) row has no delivery table: never record a
                // fire for it, even if the caller mistakenly passed one — a
                // fail-safe skip, not a panic (`advance_and_record_fire_never_
                // writes_a_delivery_row_for_a_user_schedule`).
            }
            (Some(_), None) => {} // module row, no fire recorded this pre-advance
        }
        tx.commit().await?;
        Ok(Advance::Advanced)
    }

    /// `run_now` on a module row (§4.7): record a fire with
    /// `scheduled_for = fired_at = now`, without touching `next_run_at` or
    /// retiring the tick source's outstanding fire (v2, H3). `None` if the id
    /// is not a module row (or gone) — the caller then takes the AgentRun /
    /// not-found path. `fire_id` is supplied by the caller (`FireId::derive`,
    /// ME4-1.2.2b) rather than computed here, keeping this crate free of that
    /// hashing logic.
    ///
    /// # Errors
    /// Storage/serialization.
    pub async fn record_run_now_fire(
        &self,
        schedule_id: &str,
        fire_id: &str,
        now: &str,
        expires_at: &str,
    ) -> Result<bool> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let Some(row) = sqlx::query(
            "SELECT owner_module, module_key FROM schedules WHERE id = ? AND owner_module IS NOT NULL",
        )
        .bind(schedule_id)
        .fetch_optional(&mut *tx)
        .await?
        else {
            return Ok(false);
        };
        let owner: String = row.get("owner_module");
        let key: String = row.get("module_key");
        let f = NewFire {
            fire_id,
            owner_module: &owner,
            module_key: &key,
            scheduled_for: now,
            fired_at: now,
            trigger: FireTrigger::RunNow,
            expires_at,
        };
        record_fire_in(&mut tx, schedule_id, &f, now).await?;
        tx.commit().await?;
        Ok(true)
    }

    // ── §4.3 delivery outcome / §5.4 pump / §4.5 sweep ──────────────────────

    /// Apply one attempt's already-decided [`DeliveryOutcomeWrite`] with a
    /// CAS on `(fire_id, status IN (pending, deferred), attempts)`, plus the
    /// schedule-side counters and system-disable, in one transaction. Returns
    /// `(applied, newly_system_disabled)` — `applied = false` means the CAS
    /// lost to expiry/supersede/delete (the row moved since the attempt was
    /// read; the result is discarded, not retried against a different row).
    ///
    /// # Errors
    /// Storage/serialization.
    pub async fn apply_delivery_outcome(
        &self,
        fire_id: &str,
        seen_attempts: i64,
        write: &DeliveryOutcomeWrite<'_>,
        now: &str,
        disable_at: i64,
    ) -> Result<(bool, bool)> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let r = sqlx::query(
            "UPDATE schedule_deliveries \
             SET status = ?, attempts = ?, next_attempt_at = ?, last_error = ?, updated_at = ? \
             WHERE fire_id = ? AND status IN ('pending', 'deferred') AND attempts = ?",
        )
        .bind(write.status)
        .bind(write.attempts)
        .bind(write.next_attempt_at)
        .bind(write.last_error)
        .bind(now)
        .bind(fire_id)
        .bind(seen_attempts)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 {
            return Ok((false, false)); // lost to expiry / supersede / delete
        }
        let schedule_id: String =
            sqlx::query_scalar("SELECT schedule_id FROM schedule_deliveries WHERE fire_id = ?")
                .bind(fire_id)
                .fetch_one(&mut *tx)
                .await?;
        let mut disabled = false;
        if write.reset_schedule_failures {
            sqlx::query(
                "UPDATE schedules SET consecutive_failures = 0 WHERE id = ? AND consecutive_failures <> 0",
            )
            .bind(&schedule_id)
            .execute(&mut *tx)
            .await?;
        }
        if write.count_schedule_failure {
            sqlx::query(
                "UPDATE schedules SET consecutive_failures = consecutive_failures + 1 WHERE id = ?",
            )
            .bind(&schedule_id)
            .execute(&mut *tx)
            .await?;
            let r = sqlx::query(
                "UPDATE schedules \
                 SET system_disabled_reason = 'consecutive_failures', next_run_at = NULL, \
                     revision = revision + 1 \
                 WHERE id = ? AND consecutive_failures >= ? AND system_disabled_reason IS NULL",
            )
            .bind(&schedule_id)
            .bind(disable_at)
            .execute(&mut *tx)
            .await?;
            disabled = r.rows_affected() > 0;
        }
        tx.commit().await?;
        Ok((true, disabled))
    }

    /// The pump's due set (§5.4). `skip_owners_json` is a JSON array of owner
    /// names the pump has recently seen `Deferred` from (its `DEFER_RECHECK`
    /// cache — a caller concern, ME4-1.3.1); `per_owner`/`limit` bound how
    /// many rows come back.
    ///
    /// # Errors
    /// Storage.
    pub async fn due_deliveries(
        &self,
        now: &str,
        skip_owners_json: &str,
        per_owner: i64,
        limit: i64,
    ) -> Result<Vec<DueDelivery>> {
        let rows = sqlx::query(DUE_DELIVERIES_SQL)
            .bind(now)
            .bind(skip_owners_json)
            .bind(per_owner)
            .bind(limit)
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(due_delivery_from).collect())
    }

    /// The 24h TTL sweep (§4.5): every non-terminal row past its
    /// `expires_at` becomes `expired('ttl')`. Returns how many rows this call
    /// flipped.
    ///
    /// # Errors
    /// Storage.
    pub async fn expire_deliveries(&self, now: &str) -> Result<u64> {
        let r = sqlx::query(EXPIRE_SQL)
            .bind(now)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected())
    }
}

// ── §8.2 REST guardrails on the `schedules` table (ME4-1.2.1c) ─────────────

/// `Scheduler::update` (REST PATCH, user rows) write-back, CAS'd on what it
/// read (v2, M9). `revision` alone is not enough: the tick's pre-advance does
/// NOT bump it, so the CAS also pins `next_run_at`/`last_run_at` — otherwise a
/// PATCH that read `next_run_at = X` before a tick advanced it to `Y` writes
/// `X` back and the slot fires twice.
const UPDATE_USER_SCHEDULE_CAS_SQL: &str = "\
UPDATE schedules \
SET name = ?1, enabled = ?2, spec = ?3, action = ?4, delivery = ?5, \
    next_run_at = ?6, consecutive_failures = ?7, revision = revision + 1 \
WHERE id = ?8 AND owner_module IS NULL AND revision = ?9 \
  AND next_run_at IS ?10 AND last_run_at IS ?11";

/// The AgentRun path's post-fire runtime write (§2.3's closing rule:
/// "AgentRun 的失败计数 / 失败禁用写也改成带 `AND revision = ?` 的版本").
///
/// Review, M-1 (three problems in the pre-review version, all fixed here):
/// 1. It used to blind-write `next_run_at`/`last_run_at` even though it only
///    CAS'd on `revision` — a tick's pre-advance does NOT bump revision
///    (§2.2's table), so this write could land AFTER a newer pre-advance and
///    clobber it back to a stale value, re-arming the same slot (exactly the
///    M9 pattern §2.4 already closed for the REST PATCH path). Fixed: the
///    CAS now also pins `next_run_at IS ?`/`last_run_at IS ?` (what the
///    caller read), like `UPDATE_USER_SCHEDULE_CAS_SQL`; `next_run_at` is
///    only ever WRITTEN (nulled) on `disable`, `last_run_at` is never
///    written by this statement at all — pre-advance owns both columns.
/// 2. A failure that crosses the disable threshold changes whether the row
///    can fire but did not bump `revision`, violating §2.2 ("改变是否触发的
///    写 revision +1"). Fixed: `revision = revision + CASE WHEN ?disable
///    THEN 1 ELSE 0 END`.
/// 3. No `owner_module IS NULL` guard — a caller bug could flip a MODULE
///    row's `enabled`. Fixed: added, matching `upsert_schedule`'s guard.
const UPDATE_SCHEDULE_RUNTIME_CAS_SQL: &str = "\
UPDATE schedules \
SET consecutive_failures = ?1, \
    enabled = ?2, \
    next_run_at = CASE WHEN ?3 THEN NULL ELSE next_run_at END, \
    revision = revision + CASE WHEN ?3 THEN 1 ELSE 0 END \
WHERE id = ?4 AND owner_module IS NULL AND revision = ?5 \
  AND next_run_at IS ?6 AND last_run_at IS ?7";

/// REST suspend/resume, module rows only (§8.2). Idempotent (v3, L-C): a
/// suspend of an already-suspended row, or a resume of a row that is neither
/// suspended nor system-disabled, matches 0 rows — no revision bump, no
/// recomputed `next_run_at` (a repeated resume must not push the next slot
/// out). `?2` is `next_fire(spec, now)`, computed by the caller. Resume is an
/// explicit user act, so it ALSO clears a kernel `system_disabled_reason`
/// (v2, M2) — otherwise a user could never un-disable a row whose module does
/// not re-upsert.
///
/// Review, M-2: resume's `?2` (`next_run_at`) is computed by the caller from
/// a `spec` it read earlier — if a module upsert changes `spec` between that
/// read and this write, `?2` is stale for the NEW spec. `AND revision = ?4`
/// (gated to the resume arm only — suspending needs no such check, it does
/// not compute anything from `spec`) catches that race: the upsert bumps
/// revision, so a stale resume loses the CAS and `set_user_suspended`
/// classifies the miss as [`SuspendOutcome::Conflict`] rather than a
/// (wrong) no-op.
const SET_USER_SUSPENDED_SQL: &str = "\
UPDATE schedules \
SET user_suspended = ?1, \
    system_disabled_reason = CASE WHEN ?1 = 0 THEN NULL ELSE system_disabled_reason END, \
    next_run_at = CASE WHEN ?1 = 0 AND enabled = 1 THEN ?2 ELSE NULL END, \
    consecutive_failures = CASE WHEN ?1 = 0 THEN 0 ELSE consecutive_failures END, \
    revision = revision + 1 \
WHERE id = ?3 AND owner_module IS NOT NULL \
  AND ((?1 = 1 AND user_suspended = 0) \
       OR (?1 = 0 AND (user_suspended = 1 OR system_disabled_reason IS NOT NULL) AND revision = ?4))";

/// [`Store::set_user_suspended`]'s result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuspendOutcome {
    /// The row's state actually changed.
    Changed,
    /// Already in the requested state — a no-op, not an error (suspend of an
    /// already-suspended row; resume of a row that was neither suspended nor
    /// system-disabled, including one whose module itself set `enabled =
    /// false`, §8.2's `disabled_by = "module"` case).
    NoOp,
    /// Resume only (review, M-2): the row WAS suspended/system-disabled as
    /// expected, but its `revision` had moved since the caller read the
    /// `spec` it computed `resume_next_run_at` from. The caller must re-read
    /// the row and retry with a freshly computed `next_run_at`/`revision`.
    Conflict,
}

impl Store {
    /// REST PATCH write-back on a USER row (v2, M9) — CAS'd on the revision
    /// AND the `next_run_at`/`last_run_at` the caller read (§2.4); a tick's
    /// pre-advance does not bump revision, so revision alone would not catch
    /// "PATCH read → tick advanced → PATCH writes the stale value back".
    /// Returns whether the write landed; on `false` the caller re-reads and
    /// re-applies its `ScheduleUpdate` (a pure function of the row), up to 3
    /// times, before surfacing `409 schedule_conflict`.
    ///
    /// # Errors
    /// Storage/serialization.
    pub async fn update_user_schedule_cas(
        &self,
        schedule: &Schedule,
        seen_revision: i64,
        seen_next_run_at: Option<&str>,
        seen_last_run_at: Option<&str>,
    ) -> Result<bool> {
        let r = sqlx::query(UPDATE_USER_SCHEDULE_CAS_SQL)
            .bind(&schedule.name)
            .bind(schedule.enabled)
            .bind(serde_json::to_string(&schedule.spec)?)
            .bind(serde_json::to_string(&schedule.action)?)
            .bind(serde_json::to_string(&schedule.delivery)?)
            .bind(&schedule.next_run_at)
            .bind(schedule.consecutive_failures)
            .bind(&schedule.id)
            .bind(seen_revision)
            .bind(seen_next_run_at)
            .bind(seen_last_run_at)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// The AgentRun path's post-fire runtime write — see
    /// [`UPDATE_SCHEDULE_RUNTIME_CAS_SQL`]'s doc comment for the three bugs
    /// this closes (review, M-1). `disable` is a SEPARATE flag from `enabled`
    /// (rather than inferring "disabling" from `enabled == false`) so the
    /// SQL's one conditional expression can decide the revision bump/`NULL`
    /// without also having to reconstruct "did this write just now turn it
    /// off" from `enabled` alone.
    ///
    /// # Errors
    /// Storage.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_schedule_runtime_cas(
        &self,
        id: &str,
        seen_revision: i64,
        seen_next_run_at: Option<&str>,
        seen_last_run_at: Option<&str>,
        consecutive_failures: i64,
        enabled: bool,
        disable: bool,
    ) -> Result<bool> {
        let r = sqlx::query(UPDATE_SCHEDULE_RUNTIME_CAS_SQL)
            .bind(consecutive_failures)
            .bind(enabled)
            .bind(disable)
            .bind(id)
            .bind(seen_revision)
            .bind(seen_next_run_at)
            .bind(seen_last_run_at)
            .execute(self.pool())
            .await?;
        Ok(r.rows_affected() > 0)
    }

    /// REST suspend/resume, module rows only (§8.2). Idempotent (v3, L-C).
    /// When this call SETS `suspended = true` and it changed a row, the same
    /// transaction retires the schedule's outstanding deliveries (T9,
    /// `EXPIRE_OUTSTANDING_SQL` with reason `"suspended"`) — a fire for a slot
    /// the user just paused must not be delivered later. `resume_next_run_at`
    /// is `next_fire(spec, now)`, computed by the caller; ignored when
    /// `suspended` is `true`. `seen_revision` (review, M-2) is the revision
    /// the caller read `resume_next_run_at`'s `spec` from; only checked when
    /// resuming (see [`SET_USER_SUSPENDED_SQL`]'s doc comment) — a suspend
    /// call may pass any value (conventionally the row's last-known revision,
    /// but it is not load-bearing for that direction).
    ///
    /// # Errors
    /// Storage.
    pub async fn set_user_suspended(
        &self,
        id: &str,
        suspended: bool,
        resume_next_run_at: Option<&str>,
        seen_revision: i64,
        now: &str,
    ) -> Result<SuspendOutcome> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let r = sqlx::query(SET_USER_SUSPENDED_SQL)
            .bind(suspended)
            .bind(resume_next_run_at)
            .bind(id)
            .bind(seen_revision)
            .execute(&mut *tx)
            .await?;
        let outcome = if r.rows_affected() > 0 {
            SuspendOutcome::Changed
        } else {
            let row = sqlx::query(
                "SELECT user_suspended, system_disabled_reason, revision FROM schedules \
                 WHERE id = ? AND owner_module IS NOT NULL",
            )
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
            match row {
                None => SuspendOutcome::NoOp, // gone, or not a module row: nothing this call can do
                Some(row) => {
                    let cur_suspended: bool = row.get("user_suspended");
                    let cur_sys: Option<String> = row.get("system_disabled_reason");
                    let cur_rev: i64 = row.get("revision");
                    let already_target_state = if suspended {
                        cur_suspended
                    } else {
                        !cur_suspended && cur_sys.is_none()
                    };
                    if already_target_state {
                        SuspendOutcome::NoOp
                    } else if !suspended && cur_rev != seen_revision {
                        SuspendOutcome::Conflict
                    } else {
                        // Not already at the target state, revision matches
                        // (or this is a suspend, which never checks
                        // revision) — the CAS should have landed above. This
                        // arm is unreachable in practice; NoOp is the safe
                        // default if it ever is.
                        SuspendOutcome::NoOp
                    }
                }
            }
        };
        if matches!(outcome, SuspendOutcome::Changed) && suspended {
            sqlx::query(EXPIRE_OUTSTANDING_SQL)
                .bind("suspended")
                .bind(now)
                .bind(id)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(outcome)
    }
}

// ── tick read-model (ME4-1.2.1d, collapsed onto `Schedule` by ME4-1.2.2a) ───

/// A schedule row bundled with its `revision` (design §13). Before
/// ME4-1.2.2a this was a separate `TickScheduleRow` type that duplicated
/// `Schedule`'s columns, because the old `Schedule.action: ScheduleAction`
/// (non-`Option`) could not represent a module row at all — the tick needed
/// a second, parallel struct just to see them. Now that `Schedule.action` is
/// `Option<ScheduleAction>` (§8.1), every field the tick loop (ME4-1.2.2b)
/// needs is already on `Schedule`/[`Store::row_to_schedule`]; only
/// `revision` — a storage/CAS concept, not part of the wire view — has to
/// ride alongside it. Keeping this as ONE struct (rather than the old
/// parallel one) means a future field can't drift between the two.
#[derive(Debug, Clone, PartialEq)]
pub struct ScheduleRecord {
    pub schedule: Schedule,
    pub revision: i64,
}

impl Store {
    /// Every schedule row (user AND module), unfiltered, for the tick loop
    /// (ME4-1.2.2b) — see [`ScheduleRecord`]. Built on the exact same
    /// `row_to_schedule` the strict `list_schedules`/`get_schedule` paths
    /// use, so a row whose `spec`/`action` JSON does not deserialize is
    /// skipped and logged, exactly like [`Store::list_schedules_lenient`] —
    /// one corrupt row must never wedge the whole tick. The caller decides
    /// `enabled`/due/suspended/owner-installed, so a change to those rules
    /// never requires a store-layer change.
    ///
    /// # Errors
    /// Storage.
    pub async fn list_schedules_for_tick(&self) -> Result<Vec<ScheduleRecord>> {
        let rows = sqlx::query("SELECT * FROM schedules ORDER BY name ASC")
            .fetch_all(self.pool())
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            match Store::row_to_schedule(row) {
                Ok(schedule) => out.push(ScheduleRecord {
                    schedule,
                    revision: row.get("revision"),
                }),
                Err(err) => {
                    let id: String = row.get("id");
                    tracing::error!("skipping unreadable schedule {id} in tick read-model: {err}");
                }
            }
        }
        Ok(out)
    }

    /// Single-row counterpart to [`list_schedules_for_tick`] (ME4-1.2.2c):
    /// the REST PATCH CAS retry loop (§2.4) and `suspend`/`resume` (§8.2)
    /// need one row's `revision` without pulling every schedule. Same
    /// `row_to_schedule` mapping, same "corrupt row" error surfacing as
    /// [`Store::get_schedule`] — the REST layer already treats a corrupt row
    /// as a 500, unlike the tick's lenient skip.
    ///
    /// # Errors
    /// Storage/serialization.
    pub async fn get_schedule_record(&self, id: &str) -> Result<Option<ScheduleRecord>> {
        let row = sqlx::query("SELECT * FROM schedules WHERE id = ?")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        row.as_ref()
            .map(|row| -> Result<ScheduleRecord> {
                Ok(ScheduleRecord {
                    schedule: Store::row_to_schedule(row)?,
                    revision: row.get("revision"),
                })
            })
            .transpose()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_protocol::ScheduleAction;
    use sqlx::SqlitePool;
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::str::FromStr;
    use tempfile::TempDir;

    fn every(secs: u32) -> ModuleScheduleDesired {
        ModuleScheduleDesired {
            spec: ScheduleSpec::Every { secs },
            enabled: true,
            label: "k".into(),
        }
    }

    fn pool(store: &Store) -> &SqlitePool {
        crate::test_hooks::pool(store)
    }

    /// A fresh `TempDir` (auto-cleaned on drop — the caller must keep it
    /// bound, e.g. `let (_dir, path) = temp_db();`, for as long as the
    /// database file needs to exist) and the `s.db` path inside it.
    fn temp_db() -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        (dir, path)
    }

    /// Same shape as `agent24-memory`'s `pool_migrated_up_to`
    /// (`agent24-memory/src/lib.rs:1097`): through the REAL migrator, with
    /// migrations at or past `stop_before` removed — not by hand-executing
    /// SQL, which would leave `_sqlx_migrations` empty. WAL + `foreign_keys`,
    /// mirroring `Store::open`'s production pool options (design §11, C1.2:
    /// "WAL 文件库，`max_connections(2)`").
    async fn pool_migrated_up_to(
        path: &std::path::Path,
        stop_before: i64,
        conns: u32,
    ) -> SqlitePool {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(conns)
            .connect_with(options)
            .await
            .unwrap();
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.migrations = migrator
            .migrations
            .iter()
            .filter(|m| m.version < stop_before)
            .cloned()
            .collect::<Vec<_>>()
            .into();
        migrator.run(&pool).await.unwrap();
        pool
    }

    /// 0001's `schedules` row shape, seeded post-migration as a fixture user
    /// (AgentRun) row several tests need alongside their module rows: PATCH
    /// CAS (1.2.1c), `upsert_schedule`'s module-row guard (1.2.1c), and
    /// `record_run_now_fire` (1.2.1b) returning `false` for a non-module id.
    const SEED_SCH_OLD: &str = "INSERT INTO schedules \
        (id, name, enabled, spec, action, delivery, last_run_at, next_run_at, consecutive_failures) \
        VALUES ('sch_old', 'n', 1, '{\"type\":\"every\",\"secs\":60}', \
        '{\"type\":\"agent_run\",\"prompt\":\"p\",\"session_id\":null,\"model_override\":null}', \
        '[]', NULL, '2026-09-23T09:00:00Z', 2)";

    /// A fully-migrated (through 0007) WAL file database with `conns`
    /// connections and the `sch_old` fixture row. Returns the `TempDir`
    /// alongside the `Store` — the caller must keep it bound (`let (store,
    /// _dir) = fresh(1).await;`) for the file to survive the test.
    async fn fresh(conns: u32) -> (Store, TempDir) {
        let (dir, path) = temp_db();
        let pool = pool_migrated_up_to(&path, 1000, conns).await;
        sqlx::query(SEED_SCH_OLD).execute(&pool).await.unwrap();
        (crate::test_hooks::from_pool(pool), dir)
    }

    async fn revision_of(store: &Store, id: &str) -> i64 {
        sqlx::query_scalar("SELECT revision FROM schedules WHERE id = ?")
            .bind(id)
            .fetch_one(pool(store))
            .await
            .unwrap()
    }

    // ── C1.5 — migration keeps old rows, and the new CHECKs hold ───────────

    #[tokio::test]
    async fn migration_keeps_old_rows_and_the_index_is_module_only() {
        let (_dir, path) = temp_db();
        let pool = pool_migrated_up_to(&path, 7, 1).await; // through 0006 only
        sqlx::query(SEED_SCH_OLD).execute(&pool).await.unwrap();
        // Resume the SAME migrator, untruncated: `_sqlx_migrations` already
        // has 1..6 recorded, so only 0007 applies now — through the REAL
        // migrator, not by hand-executing the file's SQL (review fix).
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();

        let r = sqlx::query("SELECT * FROM schedules WHERE id = 'sch_old'")
            .fetch_one(&pool)
            .await
            .unwrap();
        // every pre-0007 column, byte-for-byte
        assert_eq!(r.get::<String, _>("id"), "sch_old");
        assert_eq!(r.get::<String, _>("name"), "n");
        assert!(r.get::<bool, _>("enabled"));
        assert_eq!(
            r.get::<String, _>("spec"),
            "{\"type\":\"every\",\"secs\":60}"
        );
        assert_eq!(
            r.get::<String, _>("action"),
            "{\"type\":\"agent_run\",\"prompt\":\"p\",\"session_id\":null,\"model_override\":null}"
        );
        assert_eq!(r.get::<String, _>("delivery"), "[]");
        assert_eq!(r.get::<Option<String>, _>("last_run_at"), None);
        assert_eq!(
            r.get::<Option<String>, _>("next_run_at").as_deref(),
            Some("2026-09-23T09:00:00Z")
        );
        assert_eq!(r.get::<i64, _>("consecutive_failures"), 2);
        // the new columns, all defaulted
        assert_eq!(r.get::<Option<String>, _>("owner_module"), None);
        assert_eq!(r.get::<Option<String>, _>("module_key"), None);
        assert_eq!(r.get::<i64, _>("revision"), 0);
        assert!(!r.get::<bool, _>("user_suspended"));
        assert_eq!(r.get::<Option<String>, _>("system_disabled_reason"), None);

        let store = crate::test_hooks::from_pool(pool.clone());
        store
            .upsert_module_schedule(
                "sch_a",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        // the unique index is scoped to module rows: a second module row for
        // the same (owner, key) is refused. (A dedicated, index-only judgement
        // — bypassing upsert entirely — is `module_key_unique_index_rejects_...`
        // below; this is the positive-path exercise alongside a real upsert.)
        let dup = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery, owner_module, module_key) \
             VALUES ('sch_b', 'n', 1, '{}', '{}', '[]', 'm', 'k')",
        )
        .execute(&pool)
        .await;
        assert!(dup.is_err());
    }

    // ── unique index depth-defense: its own judgement (not proven only via
    // C1.2's concurrency test, which BEGIN IMMEDIATE already serialises on
    // its own) ────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn module_key_unique_index_rejects_a_second_row_for_the_same_owner_key() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                None,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        // bypass upsert entirely: a raw second module row for the SAME
        // (owner_module, module_key) must be rejected by the index alone.
        let dup = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery, owner_module, module_key) \
             VALUES ('sch_dup', 'n', 1, '{\"type\":\"every\",\"secs\":60}', ?, '[]', 'm', 'k')",
        )
        .bind(MODULE_ACTION_SENTINEL)
        .execute(pool(&store))
        .await;
        assert!(
            dup.is_err(),
            "the unique index must reject a second module row for the same (owner, key)"
        );

        // positive control: a different key for the same owner is fine
        let ok_key = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery, owner_module, module_key) \
             VALUES ('sch_ok1', 'n', 1, '{\"type\":\"every\",\"secs\":60}', ?, '[]', 'm', 'k2')",
        )
        .bind(MODULE_ACTION_SENTINEL)
        .execute(pool(&store))
        .await;
        assert!(ok_key.is_ok());
        // positive control: the same key for a DIFFERENT owner is fine
        let ok_owner = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery, owner_module, module_key) \
             VALUES ('sch_ok2', 'n', 1, '{\"type\":\"every\",\"secs\":60}', ?, '[]', 'other', 'k')",
        )
        .bind(MODULE_ACTION_SENTINEL)
        .execute(pool(&store))
        .await;
        assert!(ok_owner.is_ok());
        // positive control: two USER rows (owner_module IS NULL) never
        // collide on the partial index — it is `WHERE owner_module IS NOT NULL`
        let user_action =
            "{\"type\":\"agent_run\",\"prompt\":\"p\",\"session_id\":null,\"model_override\":null}";
        let user1 = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery) \
             VALUES ('sch_u1', 'n', 1, '{\"type\":\"every\",\"secs\":60}', ?, '[]')",
        )
        .bind(user_action)
        .execute(pool(&store))
        .await;
        assert!(user1.is_ok());
        let user2 = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery) \
             VALUES ('sch_u2', 'n', 1, '{\"type\":\"every\",\"secs\":60}', ?, '[]')",
        )
        .bind(user_action)
        .execute(pool(&store))
        .await;
        assert!(
            user2.is_ok(),
            "user rows (owner_module NULL) are excluded from the partial unique index"
        );
    }

    // ── C1.4 — CHECK constraints ─────────────────────────────────────────────

    #[tokio::test]
    async fn check_constraints_reject_half_null_owner_and_user_row_module_flags() {
        let (store, _dir) = fresh(1).await;
        let p = pool(&store);
        // half-null owner/key
        let half = sqlx::query(
            "INSERT INTO schedules (id, name, enabled, spec, action, delivery, owner_module) \
             VALUES ('x', 'n', 1, '{}', '{}', '[]', 'm')",
        )
        .execute(p)
        .await;
        assert!(half.is_err());
        // a user row cannot carry user_suspended = 1
        assert!(
            sqlx::query("UPDATE schedules SET user_suspended = 1 WHERE id = 'sch_old'")
                .execute(p)
                .await
                .is_err()
        );
        // a user row cannot carry a system_disabled_reason
        assert!(
            sqlx::query("UPDATE schedules SET system_disabled_reason = 'x' WHERE id = 'sch_old'")
                .execute(p)
                .await
                .is_err()
        );
        // positive control: a fully-formed module row inserts fine
        store
            .upsert_module_schedule(
                "sch_m",
                "m",
                "k",
                &every(60),
                None,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
    }

    // ── C1.1 — upsert outcomes, one row per (owner, key) ────────────────────

    #[tokio::test]
    async fn upsert_outcomes_and_one_row_per_key() {
        let (store, _dir) = fresh(1).await;
        let n = Some("2026-09-23T09:01:00Z");
        let (o1, _) = store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                n,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let (o2, _) = store
            .upsert_module_schedule(
                "sch_2",
                "m",
                "k",
                &every(60),
                n,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let (o3, s3) = store
            .upsert_module_schedule(
                "sch_3",
                "m",
                "k",
                &every(120),
                n,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        assert_eq!(
            (o1, o2, o3),
            (
                UpsertOutcome::Created,
                UpsertOutcome::Unchanged,
                UpsertOutcome::Updated
            )
        );
        assert_eq!(s3.spec, ScheduleSpec::Every { secs: 120 });
        assert_eq!(store.list_module_schedules("m").await.unwrap().len(), 1);
        // positive control: another key is another row
        store
            .upsert_module_schedule(
                "sch_4",
                "m",
                "k2",
                &every(60),
                n,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        assert_eq!(store.list_module_schedules("m").await.unwrap().len(), 2);
        assert_eq!(store.count_module_schedules("m").await.unwrap(), 2);
        // another owner cannot see or delete this owner's key
        assert!(!store.delete_module_schedule("other", "k").await.unwrap());
        assert_eq!(store.list_module_schedules("m").await.unwrap().len(), 2);
    }

    // ── C1.2 — concurrent upsert of the same key makes one row ──────────────

    #[tokio::test]
    async fn concurrent_upserts_on_two_connections_make_one_row() {
        let (store, _dir) = fresh(2).await; // WAL file, 2 connections (design §11 C1.2)
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let run = |id: &'static str| {
            let (store, barrier) = (store.clone(), barrier.clone());
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .upsert_module_schedule(
                        id,
                        "m",
                        "k",
                        &every(60),
                        Some("2026-09-23T09:01:00Z"),
                        "2026-09-23T09:00:00Z",
                        256,
                    )
                    .await
                    .unwrap()
                    .0
            })
        };
        let (a, c) = (run("sch_a"), run("sch_c"));
        let mut outs = vec![a.await.unwrap(), c.await.unwrap()];
        outs.sort_by_key(|o| format!("{o:?}"));
        assert_eq!(outs, vec![UpsertOutcome::Created, UpsertOutcome::Unchanged]);
        assert_eq!(store.list_module_schedules("m").await.unwrap().len(), 1);
    }

    // ── C1.6 — quota ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn quota_is_counted_inside_the_transaction() {
        let (store, _dir) = fresh(1).await;
        for i in 0..3 {
            store
                .upsert_module_schedule(
                    &format!("sch_{i}"),
                    "m",
                    &format!("k{i}"),
                    &every(60),
                    None,
                    "2026-09-23T09:00:00Z",
                    3,
                )
                .await
                .unwrap();
        }
        assert!(matches!(
            store
                .upsert_module_schedule(
                    "sch_x",
                    "m",
                    "k9",
                    &every(60),
                    None,
                    "2026-09-23T09:00:00Z",
                    3
                )
                .await,
            Err(StoreError::QuotaExceeded)
        ));
        assert_eq!(
            store.count_module_schedules("m").await.unwrap(),
            3,
            "the rejected insert wrote nothing"
        );
        // an existing key at quota still updates
        assert_eq!(
            store
                .upsert_module_schedule(
                    "sch_y",
                    "m",
                    "k0",
                    &every(120),
                    None,
                    "2026-09-23T09:00:00Z",
                    3
                )
                .await
                .unwrap()
                .0,
            UpsertOutcome::Updated
        );
    }

    // ── C1.3 — tick CAS loses to a newer spec ────────────────────────────────

    #[tokio::test]
    async fn tick_cas_loses_to_a_newer_spec() {
        let (store, _dir) = fresh(1).await;
        let due = "2026-09-23T09:01:00Z";
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some(due),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        // the module changes its spec between the tick's read and its write
        store
            .upsert_module_schedule(
                "x",
                "m",
                "k",
                &every(3600),
                Some("2026-09-23T10:00:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let f = NewFire {
            fire_id: "fire_tick_sch_1_stale",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        let r = store
            .advance_and_record_fire(
                "sch_1",
                rev,
                due,
                Some("2026-09-23T09:02:00Z"),
                due,
                Some(f),
            )
            .await
            .unwrap();
        assert_eq!(r, Advance::Lost);
        let next: Option<String> =
            sqlx::query_scalar("SELECT next_run_at FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            next.as_deref(),
            Some("2026-09-23T10:00:00Z"),
            "the new spec's next_run_at survives"
        );
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(n, 0, "no delivery row without the pre-advance landing");
    }

    // ── C1.3 (isolated) — the revision guard alone, independent of
    // `next_run_at` ─────────────────────────────────────────────────────────
    //
    // The test above changes spec AND next_run_at together, so a mutation
    // that drops "AND revision = ?" from the CAS still passes it (the
    // `next_run_at = due` clause alone already loses the race). A label-only
    // upsert bumps `revision` (§2.2's table: every `Updated` write bumps it)
    // while `recompute = false` keeps `next_run_at` byte-identical — the one
    // scenario that isolates the revision guard's own contribution.

    #[tokio::test]
    async fn tick_cas_loses_on_a_stale_revision_even_when_next_run_at_is_unchanged() {
        let (store, _dir) = fresh(1).await;
        let due = "2026-09-23T09:01:00Z";
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some(due),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let stale_rev = revision_of(&store, "sch_1").await;
        // a label-only upsert: same spec/enabled, so `recompute = false` and
        // `next_run_at` is carried over unchanged — but the row is still
        // `Updated` (a real desired-state change), which bumps revision.
        let mut relabelled = every(60);
        relabelled.label = "renamed".into();
        let (outcome, _) = store
            .upsert_module_schedule(
                "x",
                "m",
                "k",
                &relabelled,
                Some(due),
                "2026-09-23T09:00:30Z",
                256,
            )
            .await
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Updated);
        let fresh_rev = revision_of(&store, "sch_1").await;
        assert_eq!(
            fresh_rev,
            stale_rev + 1,
            "a label change must bump revision"
        );
        let unchanged_next: Option<String> =
            sqlx::query_scalar("SELECT next_run_at FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            unchanged_next.as_deref(),
            Some(due),
            "recompute = false must leave next_run_at byte-identical"
        );

        let f = NewFire {
            fire_id: "fire_tick_stale_revision",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        // the CAS reads the STALE revision but `due` still matches the
        // (unchanged) stored `next_run_at` — only the revision mismatch can
        // stop this from landing.
        let r = store
            .advance_and_record_fire(
                "sch_1",
                stale_rev,
                due,
                Some("2026-09-23T09:02:00Z"),
                due,
                Some(f),
            )
            .await
            .unwrap();
        assert_eq!(
            r,
            Advance::Lost,
            "a stale revision must lose even when next_run_at still matches"
        );
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(n, 0);
        // positive control: the CURRENT revision succeeds
        let f2 = NewFire {
            fire_id: "fire_tick_current_revision",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        assert_eq!(
            store
                .advance_and_record_fire(
                    "sch_1",
                    fresh_rev,
                    due,
                    Some("2026-09-23T09:02:00Z"),
                    due,
                    Some(f2)
                )
                .await
                .unwrap(),
            Advance::Advanced
        );
    }

    // ── C1.7 — the pre-advance and the delivery insert are one transaction ──

    #[tokio::test]
    async fn advance_and_record_fire_is_all_or_nothing() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        // Test hook: a trigger that aborts exactly the one INSERT this test
        // will attempt — simulating a storage failure inside
        // `record_fire_in`, after the pre-advance's UPDATE already ran in the
        // same (uncommitted) transaction (design §11, C1.7).
        sqlx::query(
            "CREATE TRIGGER inject_fail BEFORE INSERT ON schedule_deliveries \
             WHEN NEW.fire_id = 'inject-fail' BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(pool(&store))
        .await
        .unwrap();
        let due = "2026-09-23T09:01:00Z";
        let failing = NewFire {
            fire_id: "inject-fail",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        assert!(
            store
                .advance_and_record_fire(
                    "sch_1",
                    rev,
                    due,
                    Some("2026-09-23T09:02:00Z"),
                    due,
                    Some(failing)
                )
                .await
                .is_err()
        );
        let next: Option<String> =
            sqlx::query_scalar("SELECT next_run_at FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            next.as_deref(),
            Some(due),
            "the pre-advance must have rolled back with the failed insert"
        );
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(n, 0);
        // positive control: without the injected fire_id, both statements land
        let ok = NewFire {
            fire_id: "fire_ok",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        assert_eq!(
            store
                .advance_and_record_fire(
                    "sch_1",
                    rev,
                    due,
                    Some("2026-09-23T09:02:00Z"),
                    due,
                    Some(ok)
                )
                .await
                .unwrap(),
            Advance::Advanced
        );
    }

    // ── review, L-1 — the row's OWN owner/key are used, not the caller's ────

    #[tokio::test]
    async fn advance_and_record_fire_never_writes_a_delivery_row_for_a_user_schedule() {
        let (store, _dir) = fresh(1).await;
        let (rev, next): (i64, Option<String>) =
            sqlx::query_as("SELECT revision, next_run_at FROM schedules WHERE id = 'sch_old'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        let due = next.unwrap();
        // a caller bug: passing `Some(fire)` — with a bogus owner/key — for
        // what is actually `sch_old`, a USER row.
        let bogus = NewFire {
            fire_id: "fire_bogus",
            owner_module: "not-even-real",
            module_key: "k",
            scheduled_for: &due,
            fired_at: &due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:00:00Z",
        };
        let r = store
            .advance_and_record_fire(
                "sch_old",
                rev,
                &due,
                Some("2026-09-23T09:01:00Z"),
                &due,
                Some(bogus),
            )
            .await
            .unwrap();
        assert_eq!(r, Advance::Advanced, "the pre-advance itself still lands");
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(
            n, 0,
            "a user row must never get a delivery row, even if the caller passed one"
        );
    }

    // ── C1.9 — supersede (same trigger only) and cascade delete ────────────

    #[tokio::test]
    async fn advance_records_supersedes_same_trigger_only_and_cascades() {
        let (store, _dir) = fresh(1).await;
        let t1 = "2026-09-23T09:01:00Z";
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some(t1),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        let f1 = NewFire {
            fire_id: "fire_tick_1",
            owner_module: "m",
            module_key: "k",
            scheduled_for: t1,
            fired_at: t1,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        assert_eq!(
            store
                .advance_and_record_fire(
                    "sch_1",
                    rev,
                    t1,
                    Some("2026-09-23T09:02:00Z"),
                    t1,
                    Some(f1)
                )
                .await
                .unwrap(),
            Advance::Advanced
        );
        // replaying the same slot (e.g. a second tick that read stale state) is a no-op
        assert_eq!(
            store
                .advance_and_record_fire("sch_1", rev, t1, Some("x"), t1, None)
                .await
                .unwrap(),
            Advance::Lost
        );
        // mark fire_tick_1 as having been sent at least once, THEN supersede
        // it — it must become `expired('superseded')`, not be deleted.
        sqlx::query("UPDATE schedule_deliveries SET attempts = 1 WHERE fire_id = 'fire_tick_1'")
            .execute(pool(&store))
            .await
            .unwrap();
        let t2 = "2026-09-23T09:02:00Z";
        let f2 = NewFire {
            fire_id: "fire_tick_2",
            owner_module: "m",
            module_key: "k",
            scheduled_for: t2,
            fired_at: t2,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:02:00Z",
        };
        store
            .advance_and_record_fire("sch_1", rev, t2, Some("2026-09-23T09:03:00Z"), t2, Some(f2))
            .await
            .unwrap();
        let (status1, last_error1): (String, Option<String>) = sqlx::query_as(
            "SELECT status, last_error FROM schedule_deliveries WHERE fire_id = 'fire_tick_1'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(
            (status1.as_str(), last_error1.as_deref()),
            ("expired", Some("superseded")),
            "a sent (attempts > 0) superseded fire becomes expired, not deleted"
        );
        // run_now: its own fire id; does NOT retire the tick fire (v2, H3)
        let now = "2026-09-23T09:02:30Z";
        assert!(
            store
                .record_run_now_fire("sch_1", "fire_run_now_1", now, "2026-09-24T09:02:30Z")
                .await
                .unwrap()
        );
        let st: String = sqlx::query_scalar(
            "SELECT status FROM schedule_deliveries WHERE fire_id = 'fire_tick_2'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(st, "pending");
        // reverse direction: a NEW tick fire must not retire the outstanding
        // run_now fire either.
        let t3 = "2026-09-23T09:03:00Z";
        let f3 = NewFire {
            fire_id: "fire_tick_3",
            owner_module: "m",
            module_key: "k",
            scheduled_for: t3,
            fired_at: t3,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:03:00Z",
        };
        store
            .advance_and_record_fire("sch_1", rev, t3, Some("2026-09-23T09:04:00Z"), t3, Some(f3))
            .await
            .unwrap();
        let st_run_now: String = sqlx::query_scalar(
            "SELECT status FROM schedule_deliveries WHERE fire_id = 'fire_run_now_1'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(
            st_run_now, "pending",
            "a new tick fire must not retire the outstanding run_now fire"
        );
        // not a module row → false
        assert!(
            !store
                .record_run_now_fire("sch_old", "fire_x", now, now)
                .await
                .unwrap()
        );
        // delete cascades to every delivery row
        assert!(store.delete_module_schedule("m", "k").await.unwrap());
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(n, 0);
    }

    // ── C1.13 — an uninstalled owner never gets a delivery row ──────────────

    #[tokio::test]
    async fn uninstalled_owner_advances_next_run_at_without_a_delivery_row() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        let due = "2026-09-23T09:01:00Z";
        // the caller decides installedness — passing `fire: None` is what an
        // owner missing from this run's InstalledOwners set looks like (v2, M5)
        let r = store
            .advance_and_record_fire("sch_1", rev, due, Some("2026-09-23T09:02:00Z"), due, None)
            .await
            .unwrap();
        assert_eq!(r, Advance::Advanced);
        let next: Option<String> =
            sqlx::query_scalar("SELECT next_run_at FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(next.as_deref(), Some("2026-09-23T09:02:00Z"));
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(n, 0, "no delivery row for an uninstalled owner");
        // positive control: installed this time → a row is written
        let rev2 = revision_of(&store, "sch_1").await;
        let f = NewFire {
            fire_id: "fire_installed",
            owner_module: "m",
            module_key: "k",
            scheduled_for: "2026-09-23T09:02:00Z",
            fired_at: "2026-09-23T09:02:00Z",
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:02:00Z",
        };
        store
            .advance_and_record_fire(
                "sch_1",
                rev2,
                "2026-09-23T09:02:00Z",
                Some("2026-09-23T09:03:00Z"),
                "2026-09-23T09:02:00Z",
                Some(f),
            )
            .await
            .unwrap();
        let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(n, 1);
    }

    // ── system-disable at 5 failed fires, cleared by the module's next upsert ─
    //
    // Not itself one of §11's C1.N judgements, but exercises two of this
    // task's own functions end to end: `apply_delivery_outcome`'s
    // `disable_at` branch and `upsert_module_schedule`'s
    // `system_disabled_reason`-clearing branch.

    #[tokio::test]
    async fn five_failed_fires_system_disable_the_schedule_and_the_next_upsert_clears_it() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let revision_before_failures = revision_of(&store, "sch_1").await;
        let mut disabled_at = None;
        for slot in 0u32..5 {
            let ts = format!("2026-09-23T09:{:02}:00Z", slot + 1);
            let next = format!("2026-09-23T09:{:02}:00Z", slot + 2);
            let rev = revision_of(&store, "sch_1").await;
            let fid = format!("fire_tick_slot_{slot}");
            let f = NewFire {
                fire_id: &fid,
                owner_module: "m",
                module_key: "k",
                scheduled_for: &ts,
                fired_at: &ts,
                trigger: FireTrigger::Tick,
                expires_at: "2026-09-25T00:00:00Z",
            };
            assert_eq!(
                store
                    .advance_and_record_fire("sch_1", rev, &ts, Some(&next), &ts, Some(f))
                    .await
                    .unwrap(),
                Advance::Advanced
            );
            let mut attempts = 0i64;
            let mut last_status = String::new();
            for _ in 0..3 {
                let sent = attempts + 1;
                let (status, next_attempt_at, count_fail) = if sent >= 3 {
                    ("failed", None, true)
                } else {
                    ("pending", Some(ts.as_str()), false)
                };
                let write = DeliveryOutcomeWrite {
                    status,
                    attempts: sent,
                    next_attempt_at,
                    last_error: Some("500"),
                    reset_schedule_failures: false,
                    count_schedule_failure: count_fail,
                };
                let (applied, disabled) = store
                    .apply_delivery_outcome(&fid, attempts, &write, &ts, 5)
                    .await
                    .unwrap();
                assert!(applied);
                if disabled {
                    disabled_at = Some(slot);
                }
                attempts = sent;
                last_status = status.to_owned();
            }
            assert_eq!(last_status, "failed");
        }
        assert_eq!(
            disabled_at,
            Some(4),
            "the 5th failed fire (not the 15th attempt) crosses the line"
        );
        let (reason, next, revision_after_disable): (Option<String>, Option<String>, i64) =
            sqlx::query_as(
                "SELECT system_disabled_reason, next_run_at, revision FROM schedules WHERE id = 'sch_1'",
            )
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(
            (reason.as_deref(), next),
            (Some("consecutive_failures"), None)
        );
        assert_eq!(
            revision_after_disable,
            revision_before_failures + 1,
            "system-disable is a write that changes whether the row can fire — it must bump revision (§2.2)"
        );
        // the module's next upsert (even with the SAME desired state) clears
        // it — outcome is `Updated`, not `Unchanged` (§6.2)
        let (o, s) = store
            .upsert_module_schedule(
                "x",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T10:00:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        assert_eq!(o, UpsertOutcome::Updated);
        assert!(s.system_disabled_reason.is_none() && s.next_run_at.is_some());
    }

    // ── apply_delivery_outcome is a CAS on `attempts`, not just on status ────

    #[tokio::test]
    async fn apply_delivery_outcome_is_cas_on_attempts() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        let due = "2026-09-23T09:01:00Z";
        let f = NewFire {
            fire_id: "fire_x",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        store
            .advance_and_record_fire(
                "sch_1",
                rev,
                due,
                Some("2026-09-23T09:02:00Z"),
                due,
                Some(f),
            )
            .await
            .unwrap();

        let write = DeliveryOutcomeWrite {
            status: "pending",
            attempts: 1,
            next_attempt_at: Some(due),
            last_error: Some("500"),
            reset_schedule_failures: false,
            count_schedule_failure: false,
        };
        // a stale `seen_attempts` (the row is at 0; this call claims to have
        // read 1) must lose the CAS
        let (applied, disabled) = store
            .apply_delivery_outcome("fire_x", 1, &write, due, 5)
            .await
            .unwrap();
        assert!(!applied && !disabled, "a stale attempts read must lose");
        let (status, attempts): (String, i64) = sqlx::query_as(
            "SELECT status, attempts FROM schedule_deliveries WHERE fire_id = 'fire_x'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(
            (status.as_str(), attempts),
            ("pending", 0),
            "the stale write must not have landed"
        );

        // positive control: the correct seen_attempts (0) succeeds
        let (applied2, _) = store
            .apply_delivery_outcome("fire_x", 0, &write, due, 5)
            .await
            .unwrap();
        assert!(applied2);
        let (status2, attempts2): (String, i64) = sqlx::query_as(
            "SELECT status, attempts FROM schedule_deliveries WHERE fire_id = 'fire_x'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!((status2.as_str(), attempts2), ("pending", 1));
    }

    // ── C1.12 — an expired `At` is visible via `last_fire`, run_now doesn't hide it ─

    #[tokio::test]
    async fn at_expiry_is_visible_in_last_fire_and_a_later_run_now_does_not_hide_it() {
        let (store, _dir) = fresh(1).await;
        let at = ModuleScheduleDesired {
            spec: ScheduleSpec::At {
                ts: "2026-09-23T09:00:00Z".into(),
            },
            enabled: true,
            label: "a".into(),
        };
        let t = "2026-09-23T09:00:00Z";
        store
            .upsert_module_schedule("sch_at", "m", "a", &at, Some(t), t, 256)
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_at").await;
        let f = NewFire {
            fire_id: "fire_at_1",
            owner_module: "m",
            module_key: "a",
            scheduled_for: t,
            fired_at: t,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:00:00Z",
        };
        store
            .advance_and_record_fire("sch_at", rev, t, None, t, Some(f))
            .await
            .unwrap();
        let s0 = store.list_module_schedules("m").await.unwrap().remove(0);
        assert_eq!(s0.last_fire.tick.as_ref().unwrap().status, "pending");

        store
            .expire_deliveries("2026-09-24T09:00:00Z")
            .await
            .unwrap();
        let s1 = store.list_module_schedules("m").await.unwrap().remove(0);
        let lf = s1.last_fire.tick.clone().unwrap();
        assert_eq!(
            (
                lf.fire_id.as_str(),
                lf.status.as_str(),
                lf.last_error.as_deref()
            ),
            ("fire_at_1", "expired", Some("ttl"))
        );
        assert!(s1.next_run_at.is_none() && s1.last_fire.run_now.is_none());

        // v3, M-A: a run_now afterwards does not hide the expired tick fire
        let now = "2026-09-24T10:00:00Z";
        assert!(
            store
                .record_run_now_fire("sch_at", "fire_run_now_at", now, "2026-09-25T10:00:00Z")
                .await
                .unwrap()
        );
        let s2 = store.list_module_schedules("m").await.unwrap().remove(0);
        assert_eq!(s2.last_fire.tick.unwrap().status, "expired");
        assert_eq!(s2.last_fire.run_now.unwrap().status, "pending");
    }

    // ── last_fire's tie-break: (created_at DESC, rowid DESC) ────────────────

    #[tokio::test]
    async fn last_fire_tie_break_prefers_the_higher_rowid_on_equal_created_at() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        // two terminal tick rows inserted with the IDENTICAL created_at (same
        // second) — MODULE_STATE_SELECT's decider must fall through to rowid.
        for fid in ["fire_a", "fire_b"] {
            sqlx::query(
                "INSERT INTO schedule_deliveries (fire_id, schedule_id, owner_module, module_key, \
                     scheduled_for, fired_at, fire_trigger, status, attempts, next_attempt_at, \
                     expires_at, last_error, created_at, updated_at) \
                 VALUES (?, 'sch_1', 'm', 'k', '2026-09-23T09:00:00Z', '2026-09-23T09:00:00Z', \
                     'tick', 'delivered', 1, NULL, '2026-09-25T00:00:00Z', NULL, \
                     '2026-09-23T09:00:00Z', '2026-09-23T09:00:00Z')",
            )
            .bind(fid)
            .execute(pool(&store))
            .await
            .unwrap();
        }
        // fire_b was inserted SECOND, so it has the higher rowid despite the
        // tie on created_at.
        let s = store.list_module_schedules("m").await.unwrap().remove(0);
        assert_eq!(s.last_fire.tick.unwrap().fire_id, "fire_b");
    }

    // ── C1.11 — terminal rows are pruned per (schedule, source) ─────────────

    #[tokio::test]
    async fn terminal_deliveries_are_pruned_per_schedule_and_source() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_2",
                "m",
                "b",
                &every(60),
                Some("2026-09-23T09:00:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let mut prev: Option<String> = None;
        for i in 0u32..8 {
            let ts = format!("2026-09-23T09:{i:02}:00Z");
            let next = format!("2026-09-23T09:{:02}:00Z", i + 10);
            let rev = revision_of(&store, "sch_2").await;
            let due: Option<String> =
                sqlx::query_scalar("SELECT next_run_at FROM schedules WHERE id = 'sch_2'")
                    .fetch_one(pool(&store))
                    .await
                    .unwrap();
            let due = due.unwrap();
            if let Some(p) = &prev {
                sqlx::query("UPDATE schedule_deliveries SET attempts = 1 WHERE fire_id = ?")
                    .bind(p)
                    .execute(pool(&store))
                    .await
                    .unwrap();
            }
            let fid = format!("fire_tick_prune_{i}");
            let f = NewFire {
                fire_id: &fid,
                owner_module: "m",
                module_key: "b",
                scheduled_for: &ts,
                fired_at: &ts,
                trigger: FireTrigger::Tick,
                expires_at: "2026-09-25T00:00:00Z",
            };
            store
                .advance_and_record_fire("sch_2", rev, &due, Some(&next), &ts, Some(f))
                .await
                .unwrap();
            prev = Some(fid);
        }
        let (term, open): (i64, i64) = sqlx::query_as(
            "SELECT SUM(status IN ('delivered', 'failed', 'expired')), SUM(status IN ('pending', 'deferred')) \
             FROM schedule_deliveries WHERE schedule_id = 'sch_2'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!((term, open), (KEEP_TERMINAL_PER_SOURCE, 1));

        // v3, L-B: many delivered run_nows do not prune the tick source's rows
        let tick_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries WHERE schedule_id = 'sch_2' AND fire_trigger = 'tick'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        for i in 0u32..6 {
            let now = format!("2026-09-24T12:00:{i:02}Z");
            let fid = format!("fire_run_now_prune_{i}");
            store
                .record_run_now_fire("sch_2", &fid, &now, "2026-09-26T00:00:00Z")
                .await
                .unwrap();
            sqlx::query(
                "UPDATE schedule_deliveries SET status = 'delivered', next_attempt_at = NULL, attempts = 1, \
                 updated_at = '2026-09-30T00:00:00Z' WHERE fire_id = ?",
            )
            .bind(&fid)
            .execute(pool(&store))
            .await
            .unwrap();
        }
        let tick_after: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM schedule_deliveries WHERE schedule_id = 'sch_2' AND fire_trigger = 'tick'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(
            tick_before, tick_after,
            "run_now deliveries must not prune the tick source's rows"
        );
        let st = store
            .list_module_schedules("m")
            .await
            .unwrap()
            .into_iter()
            .find(|x| x.key == "b")
            .unwrap();
        assert!(st.last_fire.tick.is_some() && st.last_fire.run_now.is_some());
    }

    // ── C1.10 — the pump's due query is fair and skips cached owners ────────

    #[tokio::test]
    async fn due_query_is_fair_skips_cached_owners_and_spec_change_retires_fires() {
        let (store, _dir) = fresh(1).await;
        let t = "2026-09-23T09:00:00Z";
        for (owner, n) in [("a", 10), ("b", 2)] {
            for i in 0..n {
                let key = format!("k{i}");
                let id = format!("sch_{owner}{i}");
                store
                    .upsert_module_schedule(
                        &id,
                        owner,
                        &key,
                        &every(60),
                        Some(t),
                        "2026-09-23T09:00:00Z",
                        256,
                    )
                    .await
                    .unwrap();
                let rev = revision_of(&store, &id).await;
                let fid = format!("fire_due_{id}");
                let f = NewFire {
                    fire_id: &fid,
                    owner_module: owner,
                    module_key: &key,
                    scheduled_for: t,
                    fired_at: t,
                    trigger: FireTrigger::Tick,
                    expires_at: "2026-09-24T09:00:00Z",
                };
                store
                    .advance_and_record_fire(&id, rev, t, Some("2026-09-23T09:01:00Z"), t, Some(f))
                    .await
                    .unwrap();
            }
        }
        let owners = |rows: &[DueDelivery]| {
            rows.iter()
                .map(|r| r.owner_module.clone())
                .collect::<Vec<_>>()
        };
        let rows = store.due_deliveries(t, "[]", 4, 64).await.unwrap();
        let o = owners(&rows);
        assert_eq!(
            o.iter().filter(|x| x.as_str() == "a").count(),
            4,
            "a is capped at 4 per pass"
        );
        assert_eq!(
            o.iter().filter(|x| x.as_str() == "b").count(),
            2,
            "b is not starved by a's backlog"
        );
        let rows = store.due_deliveries(t, r#"["a"]"#, 4, 64).await.unwrap();
        assert!(
            owners(&rows).iter().all(|x| x == "b"),
            "a is in the skip cache"
        );

        // a spec change retires b/k0's outstanding fire (T9); b/k1's stays
        store
            .upsert_module_schedule(
                "x",
                "b",
                "k0",
                &every(120),
                Some("2026-09-23T09:02:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let st: Vec<(String, String)> =
            sqlx::query_as("SELECT module_key, status FROM schedule_deliveries WHERE owner_module = 'b' ORDER BY module_key")
                .fetch_all(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            st,
            vec![
                ("k0".into(), "expired".into()),
                ("k1".into(), "pending".into())
            ]
        );
    }

    // ── review gap #5 — the due query's OWN expires_at > now filter ─────────

    #[tokio::test]
    async fn due_deliveries_excludes_rows_whose_expires_at_has_passed() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        let due = "2026-09-23T09:01:00Z";
        // a fire whose `expires_at` is already in the PAST relative to the
        // due query's `now`, but whose status is still 'pending' (the 60s
        // sweep has not run yet) — due_deliveries must filter it out itself.
        let f = NewFire {
            fire_id: "fire_already_expired",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-23T09:00:30Z",
        };
        store
            .advance_and_record_fire(
                "sch_1",
                rev,
                due,
                Some("2026-09-23T09:02:00Z"),
                due,
                Some(f),
            )
            .await
            .unwrap();
        let rows = store.due_deliveries(due, "[]", 4, 64).await.unwrap();
        assert!(
            rows.is_empty(),
            "a row whose expires_at <= now must not be picked up, even before the sweep runs"
        );
    }

    // ── review gap #4 — upsert turning `enabled` off is T9 too ──────────────

    #[tokio::test]
    async fn upsert_disabling_enabled_expires_outstanding_fires() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        let due = "2026-09-23T09:01:00Z";
        let f = NewFire {
            fire_id: "fire_x",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        store
            .advance_and_record_fire(
                "sch_1",
                rev,
                due,
                Some("2026-09-23T09:02:00Z"),
                due,
                Some(f),
            )
            .await
            .unwrap();

        let mut disabled = every(60);
        disabled.enabled = false;
        store
            .upsert_module_schedule("x", "m", "k", &disabled, None, "2026-09-23T09:03:00Z", 256)
            .await
            .unwrap();
        let (status, last_error): (String, Option<String>) = sqlx::query_as(
            "SELECT status, last_error FROM schedule_deliveries WHERE fire_id = 'fire_x'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(
            (status.as_str(), last_error.as_deref()),
            ("expired", Some("superseded_by_upsert"))
        );
    }

    // ── review gap #7 — a recompute clears a partial (not-yet-disabling)
    // failure streak too ─────────────────────────────────────────────────────

    #[tokio::test]
    async fn upsert_recompute_clears_consecutive_failures() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE schedules SET consecutive_failures = 3 WHERE id = 'sch_1'")
            .execute(pool(&store))
            .await
            .unwrap();
        // a spec change forces `recompute = true`
        let (outcome, _) = store
            .upsert_module_schedule(
                "x",
                "m",
                "k",
                &every(120),
                Some("2026-09-23T09:05:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        assert_eq!(outcome, UpsertOutcome::Updated);
        let failures: i64 =
            sqlx::query_scalar("SELECT consecutive_failures FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            failures, 0,
            "a recompute must clear a partial failure streak, not just a full system-disable"
        );
    }

    // ── C1.15 — suspend/resume are idempotent, resume clears system-disable ─

    #[tokio::test]
    async fn user_suspend_resume_survive_module_upserts_and_are_idempotent() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev1 = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended("sch_1", true, None, rev1, "2026-09-23T09:00:30Z")
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        let (_, s) = store
            .upsert_module_schedule(
                "x",
                "m",
                "k",
                &every(120),
                Some("2026-09-23T09:02:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        assert!(
            s.user_suspended && s.next_run_at.is_none(),
            "a suspended row must not resume ticking via upsert"
        );
        // positive control: resume fires again
        let rev2 = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended(
                    "sch_1",
                    false,
                    Some("2026-09-23T09:05:00Z"),
                    rev2,
                    "2026-09-23T09:05:00Z"
                )
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        let s = store.list_module_schedules("m").await.unwrap().remove(0);
        assert!(!s.user_suspended && s.next_run_at.as_deref() == Some("2026-09-23T09:05:00Z"));
        // a repeated resume is a no-op: 0 rows, next_run_at not pushed out
        let rev3 = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended(
                    "sch_1",
                    false,
                    Some("2026-09-23T09:59:00Z"),
                    rev3,
                    "2026-09-23T09:05:00Z"
                )
                .await
                .unwrap(),
            SuspendOutcome::NoOp
        );
        let s = store.list_module_schedules("m").await.unwrap().remove(0);
        assert_eq!(
            s.next_run_at.as_deref(),
            Some("2026-09-23T09:05:00Z"),
            "a repeated resume must not push next_run_at out"
        );
        // a repeated suspend is likewise a no-op
        let rev4 = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended("sch_1", true, None, rev4, "2026-09-23T09:06:00Z")
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        assert_eq!(
            store
                .set_user_suspended("sch_1", true, None, rev4, "2026-09-23T09:07:00Z")
                .await
                .unwrap(),
            SuspendOutcome::NoOp
        );
        // suspend/resume on a USER row is a no-op (the guard is `owner_module IS NOT NULL`)
        assert_eq!(
            store
                .set_user_suspended("sch_old", true, None, 0, "2026-09-23T09:06:00Z")
                .await
                .unwrap(),
            SuspendOutcome::NoOp
        );

        // M2: resume also clears a kernel system-disable
        store
            .upsert_module_schedule(
                "sch_2",
                "m",
                "b",
                &every(60),
                Some("2026-09-23T09:00:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        sqlx::query("UPDATE schedules SET system_disabled_reason = 'consecutive_failures', next_run_at = NULL WHERE id = 'sch_2'")
            .execute(pool(&store))
            .await
            .unwrap();
        let rev5 = revision_of(&store, "sch_2").await;
        assert_eq!(
            store
                .set_user_suspended(
                    "sch_2",
                    false,
                    Some("2026-09-23T09:05:00Z"),
                    rev5,
                    "2026-09-23T09:05:00Z"
                )
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        let (reason, next): (Option<String>, Option<String>) = sqlx::query_as(
            "SELECT system_disabled_reason, next_run_at FROM schedules WHERE id = 'sch_2'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(
            (reason, next.as_deref()),
            (None, Some("2026-09-23T09:05:00Z"))
        );
    }

    // ── review, M-2 — resume conflicts on a concurrent spec change ──────────

    #[tokio::test]
    async fn resume_conflicts_when_spec_changed_after_the_callers_read() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev_before_suspend = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended(
                    "sch_1",
                    true,
                    None,
                    rev_before_suspend,
                    "2026-09-23T09:00:30Z"
                )
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        // the caller's "read" of `spec`/`revision` for its resume attempt
        let seen_rev = revision_of(&store, "sch_1").await;
        // a module upsert changes spec in between — bumps revision
        store
            .upsert_module_schedule(
                "x",
                "m",
                "k",
                &every(120),
                Some("2026-09-23T10:00:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        // the resume, computed from the now-STALE spec, must conflict rather
        // than silently write a next_run_at for the OLD spec
        let outcome = store
            .set_user_suspended(
                "sch_1",
                false,
                Some("2026-09-23T09:05:00Z"),
                seen_rev,
                "2026-09-23T09:05:00Z",
            )
            .await
            .unwrap();
        assert_eq!(outcome, SuspendOutcome::Conflict);
        let suspended: bool =
            sqlx::query_scalar("SELECT user_suspended FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert!(suspended, "the stale resume must not have landed");

        // positive control: no concurrent change → Changed
        let fresh_rev = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended(
                    "sch_1",
                    false,
                    Some("2026-09-23T09:06:00Z"),
                    fresh_rev,
                    "2026-09-23T09:06:00Z"
                )
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        // already resumed → NoOp
        let after_rev = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended(
                    "sch_1",
                    false,
                    Some("2026-09-23T09:07:00Z"),
                    after_rev,
                    "2026-09-23T09:07:00Z"
                )
                .await
                .unwrap(),
            SuspendOutcome::NoOp
        );
    }

    // ── suspend expires outstanding deliveries in the same transaction (T9) ──

    #[tokio::test]
    async fn suspend_expires_the_schedules_outstanding_deliveries() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rev = revision_of(&store, "sch_1").await;
        let due = "2026-09-23T09:01:00Z";
        let f = NewFire {
            fire_id: "fire_pending",
            owner_module: "m",
            module_key: "k",
            scheduled_for: due,
            fired_at: due,
            trigger: FireTrigger::Tick,
            expires_at: "2026-09-24T09:01:00Z",
        };
        store
            .advance_and_record_fire(
                "sch_1",
                rev,
                due,
                Some("2026-09-23T09:02:00Z"),
                due,
                Some(f),
            )
            .await
            .unwrap();
        let rev2 = revision_of(&store, "sch_1").await;
        assert_eq!(
            store
                .set_user_suspended("sch_1", true, None, rev2, "2026-09-23T09:01:30Z")
                .await
                .unwrap(),
            SuspendOutcome::Changed
        );
        let (status, last_error): (String, Option<String>) = sqlx::query_as(
            "SELECT status, last_error FROM schedule_deliveries WHERE fire_id = 'fire_pending'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert_eq!(
            (status.as_str(), last_error.as_deref()),
            ("expired", Some("suspended"))
        );
    }

    // ── C1.14 — REST PATCH write-back CAS ────────────────────────────────────

    #[tokio::test]
    async fn patch_write_back_cas_loses_to_a_stale_read_and_wins_after_a_reread() {
        let (store, _dir) = fresh(1).await;
        let (rev, next_read, last_read): (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT revision, next_run_at, last_run_at FROM schedules WHERE id = 'sch_old'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        // a tick advances the row between the PATCH's read and its write
        store
            .advance_and_record_fire(
                "sch_old",
                rev,
                next_read.as_deref().unwrap(),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                None,
            )
            .await
            .unwrap();

        let patched = Schedule {
            id: "sch_old".into(),
            name: "renamed".into(),
            enabled: true,
            spec: ScheduleSpec::Every { secs: 60 },
            action: Some(ScheduleAction::AgentRun {
                prompt: "p".into(),
                session_id: None,
                model_override: None,
            }),
            delivery: vec![],
            last_run_at: None,
            next_run_at: None,
            consecutive_failures: 0,
            owner: None,
            user_suspended: false,
            system_disabled_reason: None,
            effective_enabled: true,
            disabled_by: None,
        };
        let stale = store
            .update_user_schedule_cas(&patched, rev, next_read.as_deref(), last_read.as_deref())
            .await
            .unwrap();
        assert!(
            !stale,
            "a PATCH that read next_run_at before the tick advanced it must lose"
        );

        let (next2, last2): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT next_run_at, last_run_at FROM schedules WHERE id = 'sch_old'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        let fresh_write = store
            .update_user_schedule_cas(&patched, rev, next2.as_deref(), last2.as_deref())
            .await
            .unwrap();
        assert!(fresh_write, "positive control: re-read then write wins");
    }

    // ── C1.8 — the REST/self-wake upsert cannot touch a module row ──────────

    #[tokio::test]
    async fn rest_upsert_cannot_touch_a_module_row_but_still_updates_a_user_row() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                None,
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let hijack = Schedule {
            id: "sch_1".into(),
            name: "hijack".into(),
            enabled: true,
            spec: ScheduleSpec::Every { secs: 60 },
            action: Some(ScheduleAction::AgentRun {
                prompt: "x".into(),
                session_id: None,
                model_override: None,
            }),
            delivery: vec![],
            last_run_at: None,
            next_run_at: None,
            consecutive_failures: 0,
            owner: None,
            user_suspended: false,
            system_disabled_reason: None,
            effective_enabled: true,
            disabled_by: None,
        };
        assert!(
            !store.upsert_schedule(&hijack).await.unwrap(),
            "the guard must refuse a module row and report that it wrote nothing"
        );
        let (name, action): (String, String) =
            sqlx::query_as("SELECT name, action FROM schedules WHERE id = 'sch_1'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            (name.as_str(), action.as_str()),
            ("k", MODULE_ACTION_SENTINEL)
        );

        // positive control: a user row still updates, bumps revision, and reports true
        let renamed = Schedule {
            id: "sch_old".into(),
            name: "renamed".into(),
            ..hijack
        };
        assert!(store.upsert_schedule(&renamed).await.unwrap());
        let name2: String = sqlx::query_scalar("SELECT name FROM schedules WHERE id = 'sch_old'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(name2, "renamed");
        let rev: i64 = sqlx::query_scalar("SELECT revision FROM schedules WHERE id = 'sch_old'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert_eq!(rev, 1);
    }

    // ── review, M-1 — the AgentRun runtime-write CAS ─────────────────────────

    #[tokio::test]
    async fn update_schedule_runtime_cas_loses_to_an_interleaving_tick() {
        let (store, _dir) = fresh(1).await;
        let (rev, next, last): (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT revision, next_run_at, last_run_at FROM schedules WHERE id = 'sch_old'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        // a tick pre-advances the row between the caller's read and its write.
        // Crucially, the pre-advance does NOT bump revision (§2.2) — so a CAS
        // on revision alone would not catch this race.
        store
            .advance_and_record_fire(
                "sch_old",
                rev,
                next.as_deref().unwrap(),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                None,
            )
            .await
            .unwrap();
        let stale = store
            .update_schedule_runtime_cas(
                "sch_old",
                rev,
                next.as_deref(),
                last.as_deref(),
                1,
                true,
                false,
            )
            .await
            .unwrap();
        assert!(
            !stale,
            "a post-fire write that read next_run_at before the tick advanced it must lose"
        );
        let failures: i64 =
            sqlx::query_scalar("SELECT consecutive_failures FROM schedules WHERE id = 'sch_old'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert_eq!(
            failures, 2,
            "the seeded value must be unchanged — the stale write never landed"
        );

        // positive control: re-read then write wins
        let (next2, last2): (Option<String>, Option<String>) =
            sqlx::query_as("SELECT next_run_at, last_run_at FROM schedules WHERE id = 'sch_old'")
                .fetch_one(pool(&store))
                .await
                .unwrap();
        assert!(
            store
                .update_schedule_runtime_cas(
                    "sch_old",
                    rev,
                    next2.as_deref(),
                    last2.as_deref(),
                    1,
                    true,
                    false
                )
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn update_schedule_runtime_cas_disable_bumps_revision_and_nulls_next_run_at() {
        let (store, _dir) = fresh(1).await;
        let (rev, next, last): (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT revision, next_run_at, last_run_at FROM schedules WHERE id = 'sch_old'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert!(
            store
                .update_schedule_runtime_cas(
                    "sch_old",
                    rev,
                    next.as_deref(),
                    last.as_deref(),
                    5,
                    false,
                    true
                )
                .await
                .unwrap()
        );
        let (enabled, next_after, rev_after, failures): (bool, Option<String>, i64, i64) =
            sqlx::query_as(
                "SELECT enabled, next_run_at, revision, consecutive_failures FROM schedules WHERE id = 'sch_old'",
            )
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert!(!enabled);
        assert_eq!(next_after, None);
        assert_eq!(
            rev_after,
            rev + 1,
            "system-disable changes whether the row can fire — it must bump revision (§2.2)"
        );
        assert_eq!(failures, 5);
    }

    #[tokio::test]
    async fn update_schedule_runtime_cas_cannot_write_a_module_row() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let (rev, next, last): (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT revision, next_run_at, last_run_at FROM schedules WHERE id = 'sch_1'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        let ok = store
            .update_schedule_runtime_cas(
                "sch_1",
                rev,
                next.as_deref(),
                last.as_deref(),
                1,
                false,
                false,
            )
            .await
            .unwrap();
        assert!(
            !ok,
            "a module row must be unwritable via this AgentRun-only path"
        );
        let enabled: bool = sqlx::query_scalar("SELECT enabled FROM schedules WHERE id = 'sch_1'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert!(enabled, "the module row's enabled must be untouched");

        // positive control: the SAME call shape against a user row succeeds
        let (rev2, next2, last2): (i64, Option<String>, Option<String>) = sqlx::query_as(
            "SELECT revision, next_run_at, last_run_at FROM schedules WHERE id = 'sch_old'",
        )
        .fetch_one(pool(&store))
        .await
        .unwrap();
        assert!(
            store
                .update_schedule_runtime_cas(
                    "sch_old",
                    rev2,
                    next2.as_deref(),
                    last2.as_deref(),
                    1,
                    true,
                    false
                )
                .await
                .unwrap()
        );
    }

    // ── review, L-6 — the pre-existing, non-CAS runtime write is also guarded ─

    #[tokio::test]
    async fn update_schedule_runtime_cannot_write_a_module_row_either() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let module_row = Schedule {
            id: "sch_1".into(),
            name: "hijack".into(),
            enabled: false,
            spec: ScheduleSpec::Every { secs: 60 },
            action: Some(ScheduleAction::AgentRun {
                prompt: "x".into(),
                session_id: None,
                model_override: None,
            }),
            delivery: vec![],
            last_run_at: None,
            next_run_at: None,
            consecutive_failures: 9,
            owner: None,
            user_suspended: false,
            system_disabled_reason: None,
            effective_enabled: false,
            disabled_by: Some(agent24_protocol::DisabledBy::User),
        };
        assert!(
            !store.update_schedule_runtime(&module_row).await.unwrap(),
            "the pre-existing runtime write must also refuse a module row"
        );
        let enabled: bool = sqlx::query_scalar("SELECT enabled FROM schedules WHERE id = 'sch_1'")
            .fetch_one(pool(&store))
            .await
            .unwrap();
        assert!(enabled, "the module row's enabled must be untouched");

        // positive control: a user row still updates
        let user_row = Schedule {
            id: "sch_old".into(),
            enabled: false,
            ..module_row
        };
        assert!(store.update_schedule_runtime(&user_row).await.unwrap());
    }

    // ── the tick read-model tells module rows and user rows apart ───────────

    #[tokio::test]
    async fn tick_read_model_omits_action_for_module_rows_and_carries_it_for_user_rows() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();
        let rows = store.list_schedules_for_tick().await.unwrap();
        let module_row = &rows
            .iter()
            .find(|r| r.schedule.id == "sch_1")
            .unwrap()
            .schedule;
        assert!(
            module_row.action.is_none()
                && module_row.owner.as_ref().map(|o| o.module.as_str()) == Some("m")
        );
        // review, M-3: consecutive_failures/last_run_at are on the type too
        assert_eq!(module_row.consecutive_failures, 0);
        assert_eq!(module_row.last_run_at, None);

        let user_row = &rows
            .iter()
            .find(|r| r.schedule.id == "sch_old")
            .unwrap()
            .schedule;
        assert!(user_row.action.is_some() && user_row.owner.is_none());
        assert_eq!(
            user_row.consecutive_failures, 2,
            "SEED_SCH_OLD's seeded value"
        );
        assert_eq!(user_row.last_run_at, None);
    }

    // ── ME4-1.2.2a — the view fields, round-tripped through the strict paths ─

    /// C1's "fixtures round-trip" for the protocol view (design §13
    /// acceptance): a module row now decodes cleanly through the STRICT
    /// `get_schedule`/`list_schedules` (not just the lenient tick path) —
    /// the whole point of `action` becoming `Option<ScheduleAction>`. Before
    /// this task both paths returned `Err` for a module row's sentinel
    /// `action` (§14 R7).
    #[tokio::test]
    async fn module_row_roundtrips_through_the_strict_get_and_list_with_full_view_fields() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "sin90",
                "daily-digest",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();

        let got = store.get_schedule("sch_1").await.unwrap().unwrap();
        assert_eq!(got.action, None);
        assert_eq!(
            got.owner,
            Some(agent24_protocol::ScheduleOwner {
                module: "sin90".into(),
                key: "daily-digest".into(),
            })
        );
        assert!(!got.user_suspended);
        assert_eq!(got.system_disabled_reason, None);
        assert!(got.effective_enabled);
        assert_eq!(got.disabled_by, None);
        assert_eq!(got.next_run_at.as_deref(), Some("2026-09-23T09:01:00Z"));

        // list_schedules (strict) must also see it — this used to 500
        // (§14 R7's decode failure) rather than skip-and-log like the
        // lenient tick path.
        let all = store.list_schedules().await.unwrap();
        assert!(all.iter().any(|s| s.id == "sch_1" && s.action.is_none()));
        assert!(all.iter().any(|s| s.id == "sch_old" && s.action.is_some()));
    }

    /// `disabled_by`/`effective_enabled` priority (design §8.1, v3 L-D) as
    /// actually computed by `row_to_schedule` — not hand-asserted, READ BACK
    /// from the store after each real mutating call. User-suspended beats
    /// system-disabled beats the module's own `enabled=false`.
    #[tokio::test]
    async fn disabled_by_priority_is_user_then_system_then_module_enabled() {
        let (store, _dir) = fresh(1).await;
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &every(60),
                Some("2026-09-23T09:01:00Z"),
                "2026-09-23T09:00:00Z",
                256,
            )
            .await
            .unwrap();

        // 1. enabled, nothing blocking it → eligible.
        let s = store.get_schedule("sch_1").await.unwrap().unwrap();
        assert!(s.effective_enabled);
        assert_eq!(s.disabled_by, None);

        // 2. one failed delivery attempt, `disable_at = 1` → system-disable.
        // `enabled` stays `true` on the row (T6, §4.3): only
        // `system_disabled_reason` is set.
        assert!(
            store
                .record_run_now_fire(
                    "sch_1",
                    "fire_1",
                    "2026-09-23T09:02:00Z",
                    "2026-09-24T09:02:00Z"
                )
                .await
                .unwrap()
        );
        let (_applied, disabled) = store
            .apply_delivery_outcome(
                "fire_1",
                0,
                &DeliveryOutcomeWrite {
                    status: "failed",
                    attempts: 1,
                    next_attempt_at: None,
                    last_error: Some("boom"),
                    reset_schedule_failures: false,
                    count_schedule_failure: true,
                },
                "2026-09-23T09:02:01Z",
                1,
            )
            .await
            .unwrap();
        assert!(
            disabled,
            "one failure past disable_at=1 must system-disable"
        );
        let s = store.get_schedule("sch_1").await.unwrap().unwrap();
        assert!(
            s.enabled,
            "system-disable does not touch the module's own enabled flag"
        );
        assert!(!s.effective_enabled);
        assert_eq!(s.disabled_by, Some(agent24_protocol::DisabledBy::System));

        // 3. user-suspend ON TOP of the still-set system reason → "user" wins
        // over "system" (§8.1's priority: user_suspended checked first).
        let outcome = store
            .set_user_suspended("sch_1", true, None, 0, "2026-09-23T09:03:00Z")
            .await
            .unwrap();
        assert_eq!(outcome, SuspendOutcome::Changed);
        let s = store.get_schedule("sch_1").await.unwrap().unwrap();
        assert!(s.user_suspended);
        assert!(s.system_disabled_reason.is_some(), "still set underneath");
        assert!(!s.effective_enabled);
        assert_eq!(s.disabled_by, Some(agent24_protocol::DisabledBy::User));

        // 4. resume clears BOTH `user_suspended` and `system_disabled_reason`
        // (v2, M2) in one call → back to eligible.
        let rev = revision_of(&store, "sch_1").await;
        let outcome = store
            .set_user_suspended(
                "sch_1",
                false,
                Some("2026-09-23T10:00:00Z"),
                rev,
                "2026-09-23T09:04:00Z",
            )
            .await
            .unwrap();
        assert_eq!(outcome, SuspendOutcome::Changed);
        let s = store.get_schedule("sch_1").await.unwrap().unwrap();
        assert!(!s.user_suspended);
        assert_eq!(s.system_disabled_reason, None);
        assert!(s.effective_enabled);
        assert_eq!(s.disabled_by, None);

        // 5. the module itself turns `enabled` off (nothing else blocking) →
        // "module" — distinct from "user"/"system" above.
        store
            .upsert_module_schedule(
                "sch_1",
                "m",
                "k",
                &ModuleScheduleDesired {
                    spec: ScheduleSpec::Every { secs: 60 },
                    enabled: false,
                    label: "k".into(),
                },
                None,
                "2026-09-23T09:05:00Z",
                256,
            )
            .await
            .unwrap();
        let s = store.get_schedule("sch_1").await.unwrap().unwrap();
        assert!(!s.effective_enabled);
        assert_eq!(s.disabled_by, Some(agent24_protocol::DisabledBy::Module));

        // positive control: a plain enabled user row is never blocked.
        let user_row = store.get_schedule("sch_old").await.unwrap().unwrap();
        assert!(user_row.effective_enabled);
        assert_eq!(user_row.disabled_by, None);
    }
}
