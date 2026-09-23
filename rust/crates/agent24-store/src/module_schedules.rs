//! ME4-1.2.1a — module-owned schedules: ownership/identity columns and the
//! idempotent upsert. See `docs/design/ME4-S1-scheduler-callback.md`:
//! - §2 (D1) — migration `0007_module_schedules.sql` (full table, this cut),
//!   the revision rule.
//! - §6.1/§6.2 (D5) — the upsert SQL and outcome judgement, the `list`/upsert
//!   read-model (`ModuleScheduleState`, `last_fire` per source).
//!
//! Stacked on top: ME4-1.2.1b (`feat/me4-1.2.1b-deliveries`) adds the
//! `schedule_deliveries` writers (tick pre-advance + fire recording, the
//! pump's due query, the outcome CAS) — `MODULE_STATE_SELECT` below already
//! reads that table (for `last_fire`) even though nothing in this cut writes
//! it yet. ME4-1.2.1c (`feat/me4-1.2.1c-rest-guards`) adds suspend/resume and
//! the REST PATCH CAS. ME4-1.2.1d (`feat/me4-1.2.1-schedule-store`) adds the
//! tick loop's read-model.
//!
//! Scope note (task ME4-1.2.1 is store-only): the pure delivery state
//! machine (`apply_outcome`/`FireOutcome`/`Applied`) and the trigger
//! interface (`FireId`/`RunTrigger`) belong to `agent24-scheduler`
//! (ME4-1.2.2b/1.3.1) and are NOT introduced here or in any later cut of
//! this task — this crate has no dependency on `agent24-scheduler`.

use agent24_protocol::ScheduleSpec;
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
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
}
