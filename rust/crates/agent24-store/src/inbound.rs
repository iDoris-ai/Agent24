//! Inbound event records (X11-2a, INBOUND-ADMISSION-PLAN §3).
//!
//! The store writes the row it is given. It does not decide admission.
//! [`Store::insert_inbound_event`] reports three outcomes: the row was
//! inserted, the same id was already stored (the existing row is left as it
//! was), or the in-flight unique index rejected another explicit row for
//! that sender. The caller turns the third outcome into `ignore` /
//! `rate_limited` and inserts that row under the original id.

use agent24_protocol::{Run, Session};
use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::repo::insert_run_tx;
use crate::{Result, Store, StoreError};

/// SQLite extended result code `SQLITE_CONSTRAINT_UNIQUE`.
///
/// `ErrorKind::UniqueViolation` also covers a primary-key clash (1555). The
/// in-flight index is a secondary UNIQUE, so the code has to be this one.
const SQLITE_CONSTRAINT_UNIQUE: &str = "2067";

/// UTC time stored on inbound time columns: `YYYY-MM-DDTHH:MM:SSZ`.
///
/// Second precision, `Z`, no fractional seconds. Subseconds on `dt` are
/// truncated, matching the scheduler's previous `%Y-%m-%dT%H:%M:%SZ`
/// spelling. String order is chronological only for this spelling, which is
/// what `scrub_expired_inbound_text` compares. Insert and scrub
/// reject any other spelling.
pub fn format_inbound_timestamp(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

pub(crate) fn require_inbound_timestamp(field: &str, value: &str) -> Result<()> {
    let parsed = DateTime::parse_from_rfc3339(value).ok();
    let canonical = parsed.map(|dt| format_inbound_timestamp(dt.with_timezone(&Utc)));
    if canonical.as_deref() != Some(value) {
        return Err(StoreError::Conflict(format!(
            "inbound {field} must be YYYY-MM-DDTHH:MM:SSZ, got {value}"
        )));
    }
    Ok(())
}

/// What [`Store::insert_inbound_event`] did. The statement never overwrites.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundInsert {
    /// A new row was stored.
    Inserted,
    /// `id` was already present. The existing row was not changed.
    Duplicate,
    /// Another explicit row for this `(source, actor)` is still `starting`
    /// or `started`. Nothing was stored. The caller rewrites the same id as
    /// `ignore` / `rate_limited` and inserts that.
    Inflight,
}

/// One inbound row, as handed to [`Store::insert_inbound_event`].
///
/// `at` and `received_at` are [`format_inbound_timestamp`] values. Insert
/// rejects any other spelling. `text_scrubbed_at` is not a caller field:
/// only the scrub paths set it: [`Store::scrub_expired_inbound`] in production
/// and `scrub_expired_inbound_text` in tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewInboundEvent {
    pub id: String,
    pub source: String,
    pub category: String,
    pub recipient_identity: Option<String>,
    pub thread_key: Option<String>,
    pub actor: Option<String>,
    pub human: bool,
    pub addressed: String,
    pub at: String,
    pub received_at: String,
    pub text: Option<String>,
    pub meta: Option<String>,
    pub disposition: String,
    pub disposition_reason: Option<String>,
    pub policy_version: Option<String>,
    pub run_state: Option<String>,
}

/// A stored inbound row, including the scrub stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEvent {
    pub id: String,
    pub source: String,
    pub category: String,
    pub recipient_identity: Option<String>,
    pub thread_key: Option<String>,
    pub actor: Option<String>,
    pub human: bool,
    pub addressed: String,
    pub at: String,
    pub received_at: String,
    pub text: Option<String>,
    pub meta: Option<String>,
    pub text_scrubbed_at: Option<String>,
    pub disposition: String,
    pub disposition_reason: Option<String>,
    pub policy_version: Option<String>,
    pub run_state: Option<String>,
}

/// What [`Store::insert_session_run_for_inbound`] did.
///
/// `Linked` committed the session, the run, and `inbound_runs` together.
/// `AlreadyLinked` rolled that transaction back: `event_id` already had a run.
/// `NotStarting` rolled it back with no link: the event is missing or its
/// `run_state` is not `starting`, so this call did not start another run.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundRunLink {
    Linked,
    AlreadyLinked,
    NotStarting,
}

/// `starting` rows split by whether `inbound_runs` already names them.
///
/// A linked row is reconciled to `started`. An unlinked row is reconciled
/// to `interrupted` and is not started again. This query does not write.
/// `started` rows are not listed: 2c converges a `started` row at startup
/// from the run's terminal state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundReconcile {
    pub linked: Vec<String>,
    pub unlinked: Vec<String>,
}

const INSERT_INBOUND_EVENT: &str = "\
INSERT INTO inbound_events (
    id, source, category, recipient_identity, thread_key, actor, human,
    addressed, at, received_at, text, meta, disposition, disposition_reason,
    policy_version, run_state
) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
ON CONFLICT(id) DO NOTHING";

impl Store {
    /// Insert one inbound row.
    ///
    /// `ON CONFLICT(id) DO NOTHING` leaves an existing id untouched.
    /// `INSERT OR IGNORE` / `INSERT OR REPLACE` are not used: ignore would
    /// hide the in-flight clash, and replace would overwrite `disposition`.
    /// A clash on `idx_inbound_events_inflight` is [`InboundInsert::Inflight`]
    /// and stores nothing.
    ///
    /// # Errors
    /// Storage, a timestamp that is not [`format_inbound_timestamp`], or a
    /// constraint other than that in-flight index.
    pub async fn insert_inbound_event(&self, event: &NewInboundEvent) -> Result<InboundInsert> {
        require_inbound_timestamp("at", &event.at)?;
        require_inbound_timestamp("received_at", &event.received_at)?;
        let inserted = sqlx::query(INSERT_INBOUND_EVENT)
            .bind(&event.id)
            .bind(&event.source)
            .bind(&event.category)
            .bind(&event.recipient_identity)
            .bind(&event.thread_key)
            .bind(&event.actor)
            .bind(i64::from(event.human))
            .bind(&event.addressed)
            .bind(&event.at)
            .bind(&event.received_at)
            .bind(&event.text)
            .bind(&event.meta)
            .bind(&event.disposition)
            .bind(&event.disposition_reason)
            .bind(&event.policy_version)
            .bind(&event.run_state)
            .execute(self.pool())
            .await;
        match inserted {
            Ok(done) => {
                if done.rows_affected() == 0 {
                    Ok(InboundInsert::Duplicate)
                } else {
                    Ok(InboundInsert::Inserted)
                }
            }
            Err(sqlx::Error::Database(err)) if is_inflight_conflict(err.as_ref()) => {
                Ok(InboundInsert::Inflight)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// The row for `id`, if it was stored.
    ///
    /// # Errors
    /// Storage.
    pub async fn get_inbound_event(&self, id: &str) -> Result<Option<InboundEvent>> {
        let row = sqlx::query(
            "SELECT id, source, category, recipient_identity, thread_key, actor, human, \
                    addressed, at, received_at, text, meta, text_scrubbed_at, disposition, \
                    disposition_reason, policy_version, run_state \
             FROM inbound_events WHERE id = ?",
        )
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.map(event_from_row))
    }

    /// Move `starting` to `started`. Returns whether this call changed the row.
    ///
    /// A missing id, or a row that is not `starting`, returns `false`.
    ///
    /// # Errors
    /// Storage.
    pub async fn mark_inbound_started(&self, id: &str) -> Result<bool> {
        self.mark_inbound_run_state(id, "started").await
    }

    /// Move `run_state` along one legal edge. Returns whether this call
    /// changed the row.
    ///
    /// The `WHERE` allows only `starting` → `started`, `starting` →
    /// `interrupted`, `starting` → `start_failed`, and `started` → `done`.
    /// `start_failed` also requires that `inbound_runs` has no row for this
    /// event, so a start error cannot overwrite an event that already has a
    /// run. A missing id, the wrong current state, or any other target returns
    /// `false` and writes nothing.
    ///
    /// # Errors
    /// Storage.
    pub async fn mark_inbound_run_state(&self, id: &str, state: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE inbound_events SET run_state = ? \
             WHERE id = ? AND ( \
                 (run_state = 'starting' AND ? IN ('started', 'interrupted')) \
                 OR (run_state = 'starting' AND ? = 'start_failed' \
                     AND NOT EXISTS (SELECT 1 FROM inbound_runs WHERE event_id = ?)) \
                 OR (run_state = 'started' AND ? = 'done') \
             )",
        )
        .bind(state)
        .bind(id)
        .bind(state)
        .bind(state)
        .bind(id)
        .bind(state)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Link a run to the inbound event that started it.
    ///
    /// `run_id` and `event_id` are both unique. The run row must already exist.
    ///
    /// # Errors
    /// Storage, including a foreign-key or uniqueness failure.
    pub async fn insert_inbound_run(&self, run_id: &str, event_id: &str) -> Result<()> {
        sqlx::query("INSERT INTO inbound_runs (run_id, event_id) VALUES (?, ?)")
            .bind(run_id)
            .bind(event_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Starting rows that have a run link, and starting rows that do not.
    ///
    /// `started` rows are not part of this result. 2c converges a `started`
    /// row at startup from the run's terminal state.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_reconcile(&self) -> Result<InboundReconcile> {
        let linked = sqlx::query_scalar(
            "SELECT id FROM inbound_events \
             WHERE run_state = 'starting' \
               AND EXISTS (SELECT 1 FROM inbound_runs r WHERE r.event_id = inbound_events.id) \
             ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        let unlinked = sqlx::query_scalar(
            "SELECT id FROM inbound_events \
             WHERE run_state = 'starting' \
               AND NOT EXISTS (SELECT 1 FROM inbound_runs r WHERE r.event_id = inbound_events.id) \
             ORDER BY id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(InboundReconcile { linked, unlinked })
    }

    /// Null `text` and `meta` on rows received before `cutoff` that have not
    /// been scrubbed, and stamp `text_scrubbed_at = now`.
    ///
    /// Test-only. It stamps rows without looking at whether their run has
    /// ended and leaves every copy (runs, messages, memory) in place, so a
    /// caller outside this crate would hide expired text from the real task.
    /// Production code uses [`Store::scrub_expired_inbound`].
    ///
    /// Idempotent: a row that already has `text_scrubbed_at` is skipped, so a
    /// second call returns 0 and does not rewrite the stamp. Sender, time,
    /// category, and disposition stay. This does not touch runs or memory.
    ///
    /// `cutoff` and `now` are caller-supplied [`format_inbound_timestamp`]
    /// values. Comparison is the stored text order, which is chronological
    /// for that spelling. A row received at exactly `cutoff` is kept. The
    /// comparison column is `received_at`, not `at`.
    ///
    /// # Errors
    /// Storage, or a `cutoff` / `now` that is not [`format_inbound_timestamp`].
    #[cfg(test)]
    pub(crate) async fn scrub_expired_inbound_text(&self, cutoff: &str, now: &str) -> Result<u64> {
        require_inbound_timestamp("cutoff", cutoff)?;
        require_inbound_timestamp("now", now)?;
        let affected = sqlx::query(
            "UPDATE inbound_events \
             SET text = NULL, meta = NULL, text_scrubbed_at = ? \
             WHERE received_at < ? AND text_scrubbed_at IS NULL",
        )
        .bind(now)
        .bind(cutoff)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(affected)
    }

    /// How many rows for `source` + `actor` have `received_at >= since`.
    ///
    /// Counts every disposition except `filtered`, and every category except
    /// `protocol`, including `ignore` / `rate_limited`. The row being admitted
    /// is not in the table yet. `since` is a [`format_inbound_timestamp`] value.
    ///
    /// # Errors
    /// Storage, or a `since` that is not [`format_inbound_timestamp`].
    pub async fn count_inbound_window(
        &self,
        source: &str,
        actor: &str,
        since: &str,
    ) -> Result<i64> {
        require_inbound_timestamp("since", since)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inbound_events \
             WHERE source = ? AND actor = ? AND received_at >= ? \
               AND category <> 'protocol' AND disposition <> 'filtered'",
        )
        .bind(source)
        .bind(actor)
        .bind(since)
        .fetch_one(self.pool())
        .await?;
        Ok(count)
    }

    /// Whether any `inbound_allowlist` row has this `source`.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_source_allowlisted(&self, source: &str) -> Result<bool> {
        let hit: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM inbound_allowlist WHERE source = ? LIMIT 1")
                .bind(source)
                .fetch_optional(self.pool())
                .await?;
        Ok(hit.is_some())
    }

    /// Whether `(source, actor)` is on `inbound_allowlist`.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_actor_allowlisted(&self, source: &str, actor: &str) -> Result<bool> {
        let hit: Option<i64> =
            sqlx::query_scalar("SELECT 1 FROM inbound_allowlist WHERE source = ? AND actor = ?")
                .bind(source)
                .bind(actor)
                .fetch_optional(self.pool())
                .await?;
        Ok(hit.is_some())
    }

    /// Stored `accept_mode`, or `None` when the policy row has not been written.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_accept_mode(&self) -> Result<Option<String>> {
        let mode: Option<String> =
            sqlx::query_scalar("SELECT accept_mode FROM inbound_policy WHERE id = 1")
                .fetch_optional(self.pool())
                .await?;
        Ok(mode)
    }

    /// `activated_at` for `source`, if that source has a watermark row.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_watermark_activated_at(&self, source: &str) -> Result<Option<String>> {
        let at: Option<String> = sqlx::query_scalar(
            "SELECT activated_at FROM inbound_source_watermark WHERE source = ?",
        )
        .bind(source)
        .fetch_optional(self.pool())
        .await?;
        Ok(at)
    }

    /// Insert the source watermark if that source has none yet.
    ///
    /// A row that is already there is left as it was (`ON CONFLICT DO
    /// NOTHING`), so a restart does not move the activation time.
    /// `activated_at` is [`format_inbound_timestamp`]. `max_age_secs` is
    /// stored for the row; admission reads the caller's own constant.
    ///
    /// # Errors
    /// Storage, or an `activated_at` that is not [`format_inbound_timestamp`].
    pub async fn ensure_inbound_watermark(
        &self,
        source: &str,
        activated_at: &str,
        max_age_secs: i64,
    ) -> Result<()> {
        require_inbound_timestamp("activated_at", activated_at)?;
        sqlx::query(
            "INSERT INTO inbound_source_watermark (source, activated_at, max_age_secs) \
             VALUES (?, ?, ?) ON CONFLICT(source) DO NOTHING",
        )
        .bind(source)
        .bind(activated_at)
        .bind(max_age_secs)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Group-history cursor for `(source, recipient_identity, thread_key)`.
    ///
    /// `recipient_identity` is the local npub the cursor belongs to. Two
    /// identities in one group do not share a row.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_cursor(
        &self,
        source: &str,
        recipient_identity: &str,
        thread_key: &str,
    ) -> Result<Option<String>> {
        let cursor: Option<String> = sqlx::query_scalar(
            "SELECT cursor FROM inbound_cursor \
             WHERE source = ? AND recipient_identity = ? AND thread_key = ?",
        )
        .bind(source)
        .bind(recipient_identity)
        .bind(thread_key)
        .fetch_optional(self.pool())
        .await?;
        Ok(cursor)
    }

    /// Insert or replace the group-history cursor for one local identity.
    ///
    /// # Errors
    /// Storage.
    pub async fn set_inbound_cursor(
        &self,
        source: &str,
        recipient_identity: &str,
        thread_key: &str,
        cursor: &str,
    ) -> Result<()> {
        sqlx::query(
            "INSERT INTO inbound_cursor (source, recipient_identity, thread_key, cursor) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(source, recipient_identity, thread_key) \
             DO UPDATE SET cursor = excluded.cursor",
        )
        .bind(source)
        .bind(recipient_identity)
        .bind(thread_key)
        .bind(cursor)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Drop the group-history cursor for one local identity.
    ///
    /// The source watermark is a different row and is left in place. A
    /// missing row is success.
    ///
    /// # Errors
    /// Storage.
    pub async fn clear_inbound_cursor(
        &self,
        source: &str,
        recipient_identity: &str,
        thread_key: &str,
    ) -> Result<()> {
        sqlx::query(
            "DELETE FROM inbound_cursor \
             WHERE source = ? AND recipient_identity = ? AND thread_key = ?",
        )
        .bind(source)
        .bind(recipient_identity)
        .bind(thread_key)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Insert `session`, `run`, and `inbound_runs` in one `BEGIN IMMEDIATE`.
    ///
    /// [`crate::Store::insert_run`] commits on its own, so it cannot share a
    /// transaction with the link. The same transaction reads `run_state`.
    /// Only `starting` may link. Anything else rolls back: an existing link is
    /// [`InboundRunLink::AlreadyLinked`], and no link is
    /// [`InboundRunLink::NotStarting`] (an `interrupted` or `start_failed`
    /// row must not gain a run that sits outside the in-flight index). A clash
    /// on `inbound_runs.event_id` is also [`InboundRunLink::AlreadyLinked`].
    ///
    /// # Errors
    /// Storage, a workspace-bound run (this path does not admit one), a session
    /// id that does not match the run, or a constraint other than that
    /// `event_id` unique index.
    pub async fn insert_session_run_for_inbound(
        &self,
        session: &Session,
        run: &Run,
        event_id: &str,
    ) -> Result<InboundRunLink> {
        if run.session_id.as_deref() != Some(session.id.as_str()) {
            return Err(StoreError::Conflict(
                "inbound run session does not match".to_owned(),
            ));
        }
        if run.workspace_id.is_some() || session.workspace_id.is_some() {
            return Err(StoreError::Conflict(
                "inbound run is not workspace-bound".to_owned(),
            ));
        }
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let existing = sqlx::query(
            "SELECT e.run_state AS run_state, \
                    (SELECT ir.run_id FROM inbound_runs ir WHERE ir.event_id = e.id) AS run_id \
             FROM inbound_events e WHERE e.id = ?",
        )
        .bind(event_id)
        .fetch_optional(&mut *tx)
        .await?;
        let run_state = existing
            .as_ref()
            .and_then(|row| row.get::<Option<String>, _>("run_state"));
        let linked_run = existing
            .as_ref()
            .and_then(|row| row.get::<Option<String>, _>("run_id"));
        // Same write lock as the inserts below. A row that has left `starting`
        // (or was never stored) cannot gain a session and a run.
        if run_state.as_deref() != Some("starting") {
            tx.rollback().await?;
            return if linked_run.is_some() {
                Ok(InboundRunLink::AlreadyLinked)
            } else {
                Ok(InboundRunLink::NotStarting)
            };
        }
        sqlx::query(
            "INSERT INTO sessions (id, title, channel, workspace_id, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(&session.id)
        .bind(&session.title)
        .bind(&session.channel)
        .bind(session.workspace_id.as_ref().map(|id| id.as_str()))
        .bind(&session.created_at)
        .bind(&session.updated_at)
        .execute(&mut *tx)
        .await?;
        insert_run_tx(&mut tx, run).await?;
        let linked = sqlx::query("INSERT INTO inbound_runs (run_id, event_id) VALUES (?, ?)")
            .bind(&run.id)
            .bind(event_id)
            .execute(&mut *tx)
            .await;
        match linked {
            Ok(_) => {
                tx.commit().await?;
                Ok(InboundRunLink::Linked)
            }
            Err(sqlx::Error::Database(err)) if is_inbound_run_event_conflict(err.as_ref()) => {
                tx.rollback().await?;
                Ok(InboundRunLink::AlreadyLinked)
            }
            Err(err) => Err(err.into()),
        }
    }

    /// `runs.status` for the run linked to `event_id`.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_linked_run_status(&self, event_id: &str) -> Result<Option<String>> {
        let status: Option<String> = sqlx::query_scalar(
            "SELECT r.status FROM inbound_runs ir \
             JOIN runs r ON r.id = ir.run_id \
             WHERE ir.event_id = ?",
        )
        .bind(event_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(status)
    }

    /// `started` events whose run row is gone.
    ///
    /// Deleting `runs` cascades `inbound_runs`, so the event stays `started`
    /// with nothing left to resume and still occupies the in-flight index.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_started_unlinked_event_ids(&self) -> Result<Vec<String>> {
        let ids = sqlx::query_scalar(
            "SELECT id FROM inbound_events e \
             WHERE e.run_state = 'started' \
               AND NOT EXISTS (SELECT 1 FROM inbound_runs r WHERE r.event_id = e.id) \
             ORDER BY e.id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(ids)
    }

    /// `started` events whose run has already ended.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_started_terminal_event_ids(&self) -> Result<Vec<String>> {
        let ids = sqlx::query_scalar(
            "SELECT e.id FROM inbound_events e \
             JOIN inbound_runs ir ON ir.event_id = e.id \
             JOIN runs r ON r.id = ir.run_id \
             WHERE e.run_state = 'started' \
               AND r.status IN ('completed', 'failed', 'cancelled') \
             ORDER BY e.id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(ids)
    }

    /// The run id linked to `event_id`, if any.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_run_id_for_event(&self, event_id: &str) -> Result<Option<String>> {
        let id: Option<String> =
            sqlx::query_scalar("SELECT run_id FROM inbound_runs WHERE event_id = ?")
                .bind(event_id)
                .fetch_optional(self.pool())
                .await?;
        Ok(id)
    }

    /// The inbound event linked to `run_id`, if any.
    ///
    /// # Errors
    /// Storage.
    pub async fn inbound_event_id_for_run(&self, run_id: &str) -> Result<Option<String>> {
        let id: Option<String> =
            sqlx::query_scalar("SELECT event_id FROM inbound_runs WHERE run_id = ?")
                .bind(run_id)
                .fetch_optional(self.pool())
                .await?;
        Ok(id)
    }

    /// Whether any run in `session_id` is linked from an inbound event.
    ///
    /// A session with no runs, or only unlinked runs, is `false`.
    ///
    /// # Errors
    /// Storage.
    pub async fn session_has_inbound_run(&self, session_id: &str) -> Result<bool> {
        let found: Option<i64> = sqlx::query_scalar(
            "SELECT 1 FROM runs \
             WHERE session_id = ? \
               AND EXISTS (SELECT 1 FROM inbound_runs ir WHERE ir.run_id = runs.id) \
             LIMIT 1",
        )
        .bind(session_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(found.is_some())
    }

    /// Move the event linked to `run_id` to `done` from `starting` or `started`.
    ///
    /// A run can reach a terminal status before `mark_inbound_started` lands.
    /// Leaving the row at `starting` would keep the in-flight index. Returns
    /// whether this call changed a row. No link, or any other state, is `false`.
    ///
    /// # Errors
    /// Storage.
    pub async fn mark_inbound_done_for_run(&self, run_id: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE inbound_events SET run_state = 'done' \
             WHERE run_state IN ('starting', 'started') \
               AND id = (SELECT event_id FROM inbound_runs WHERE run_id = ?)",
        )
        .bind(run_id)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Move `event_id` to `done` from `starting` or `started`.
    ///
    /// Same edge as [`Self::mark_inbound_done_for_run`], for startup reconcile
    /// when the run is already terminal.
    ///
    /// # Errors
    /// Storage.
    pub async fn mark_inbound_done_by_event(&self, event_id: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE inbound_events SET run_state = 'done' \
             WHERE id = ? AND run_state IN ('starting', 'started')",
        )
        .bind(event_id)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(affected > 0)
    }

    /// Every allowlist row, ordered by `(source, actor)`.
    ///
    /// # Errors
    /// Storage.
    pub async fn list_inbound_allowlist(&self) -> Result<Vec<InboundAllowEntry>> {
        let rows = sqlx::query(
            "SELECT source, actor, added_at, note FROM inbound_allowlist \
             ORDER BY source, actor",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|row| InboundAllowEntry {
                source: row.get("source"),
                actor: row.get("actor"),
                added_at: row.get("added_at"),
                note: row.get("note"),
            })
            .collect())
    }

    /// The allowlist and the effective accept mode, read in one transaction.
    ///
    /// No policy row means `accept_mode` is `observe` and `updated_at` is
    /// `None`. A caller that hashes `policy_version` uses this so the names
    /// and the mode cannot come from two different commits.
    ///
    /// # Errors
    /// Storage.
    pub async fn read_inbound_admission(&self) -> Result<InboundAdmissionView> {
        let mut tx = self.pool().begin().await?;
        let view = admission_view(&mut tx).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Replace the allowlist in one transaction and return that transaction's
    /// view (the rows just written, plus the mode already stored).
    ///
    /// Last writer wins. The caller does not pass `expected_policy_version`,
    /// and this round does not do optimistic concurrency. An empty slice
    /// clears the table. `added_at` is the caller's
    /// [`format_inbound_timestamp`] for a `(source, actor)` that was not
    /// already stored. A pair that survives keeps the `added_at` it had;
    /// the note is the one in this write.
    ///
    /// # Errors
    /// Storage, or an `added_at` that is not [`format_inbound_timestamp`].
    pub async fn replace_inbound_allowlist(
        &self,
        entries: &[InboundAllowWrite],
        added_at: &str,
    ) -> Result<InboundAdmissionView> {
        require_inbound_timestamp("added_at", added_at)?;
        let mut tx = self.pool().begin().await?;
        let existing = sqlx::query("SELECT source, actor, added_at FROM inbound_allowlist")
            .fetch_all(&mut *tx)
            .await?;
        let mut kept_at = std::collections::HashMap::<(String, String), String>::new();
        for row in existing {
            kept_at.insert((row.get("source"), row.get("actor")), row.get("added_at"));
        }
        sqlx::query("DELETE FROM inbound_allowlist")
            .execute(&mut *tx)
            .await?;
        for entry in entries {
            let stamp = kept_at
                .get(&(entry.source.clone(), entry.actor.clone()))
                .map_or(added_at, String::as_str);
            sqlx::query(
                "INSERT INTO inbound_allowlist (source, actor, added_at, note) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&entry.source)
            .bind(&entry.actor)
            .bind(stamp)
            .bind(&entry.note)
            .execute(&mut *tx)
            .await?;
        }
        let view = admission_view(&mut tx).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// Insert one allowlist row. A `(source, actor)` that is already stored
    /// is left as it was, including `added_at` and `note`.
    ///
    /// Returns whether this call inserted the row.
    ///
    /// # Errors
    /// Storage, or an `added_at` that is not [`format_inbound_timestamp`].
    pub async fn insert_inbound_allowlist_if_absent(
        &self,
        source: &str,
        actor: &str,
        added_at: &str,
        note: Option<&str>,
    ) -> Result<bool> {
        require_inbound_timestamp("added_at", added_at)?;
        let inserted = sqlx::query(
            "INSERT INTO inbound_allowlist (source, actor, added_at, note) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(source, actor) DO NOTHING",
        )
        .bind(source)
        .bind(actor)
        .bind(added_at)
        .bind(note)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// Sync the development wechat seed in one transaction.
    ///
    /// Deletes `wechat` rows whose note is `dev-seed` and whose actor is not
    /// in `actors`. An empty slice deletes every such row. Rows with any
    /// other note, and rows for any other source, stay, including `added_at`.
    /// Each actor that is not already stored is inserted with note
    /// `dev-seed`. A pair that is already stored keeps the note and
    /// `added_at` it had.
    ///
    /// Returns how many rows this call inserted.
    ///
    /// # Errors
    /// Storage, or an `added_at` that is not [`format_inbound_timestamp`].
    pub async fn sync_wechat_dev_seed_allowlist(
        &self,
        actors: &[String],
        added_at: &str,
    ) -> Result<usize> {
        require_inbound_timestamp("added_at", added_at)?;
        let mut tx = self.pool().begin().await?;
        let mut sql = String::from(
            "DELETE FROM inbound_allowlist WHERE source = 'wechat' AND note = 'dev-seed'",
        );
        if !actors.is_empty() {
            let placeholders = actors.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
            sql.push_str(" AND actor NOT IN (");
            sql.push_str(&placeholders);
            sql.push(')');
        }
        let mut delete = sqlx::query(&sql);
        for actor in actors {
            delete = delete.bind(actor);
        }
        delete.execute(&mut *tx).await?;
        let mut inserted = 0usize;
        for actor in actors {
            let rows = sqlx::query(
                "INSERT INTO inbound_allowlist (source, actor, added_at, note) \
                 VALUES ('wechat', ?, ?, 'dev-seed') \
                 ON CONFLICT(source, actor) DO NOTHING",
            )
            .bind(actor)
            .bind(added_at)
            .execute(&mut *tx)
            .await?
            .rows_affected();
            if rows > 0 {
                inserted += 1;
            }
        }
        tx.commit().await?;
        Ok(inserted)
    }

    /// Insert the source watermark when that source has none.
    ///
    /// A second call does not move `activated_at`. Admission compares events
    /// to this stamp (H5).
    ///
    /// # Errors
    /// Storage, a non-positive `max_age_secs`, or an `activated_at` that is
    /// not [`format_inbound_timestamp`].
    pub async fn ensure_inbound_source_watermark(
        &self,
        source: &str,
        activated_at: &str,
        max_age_secs: i64,
    ) -> Result<bool> {
        require_inbound_timestamp("activated_at", activated_at)?;
        if max_age_secs <= 0 {
            return Err(StoreError::Conflict(
                "inbound watermark max_age_secs must be positive".to_owned(),
            ));
        }
        let inserted = sqlx::query(
            "INSERT INTO inbound_source_watermark (source, activated_at, max_age_secs) \
             VALUES (?, ?, ?) \
             ON CONFLICT(source) DO NOTHING",
        )
        .bind(source)
        .bind(activated_at)
        .bind(max_age_secs)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(inserted > 0)
    }

    /// The single policy row, if a caller has written one.
    ///
    /// No row means the table default `observe` has never been stored.
    /// Callers that need a mode anyway treat that as `observe`.
    ///
    /// # Errors
    /// Storage.
    pub async fn get_inbound_policy(&self) -> Result<Option<InboundPolicy>> {
        let row = sqlx::query("SELECT accept_mode, updated_at FROM inbound_policy WHERE id = 1")
            .fetch_optional(self.pool())
            .await?;
        Ok(row.map(|row| InboundPolicy {
            accept_mode: row.get("accept_mode"),
            updated_at: row.get("updated_at"),
        }))
    }

    /// Insert or replace the single policy row and return that transaction's
    /// view (the mode just written, plus the allowlist already stored).
    ///
    /// `accept_mode` must be `off`, `observe`, or `auto`. `updated_at` is a
    /// [`format_inbound_timestamp`]. This does not compute `policy_version`.
    ///
    /// # Errors
    /// Storage, a mode outside the check, or a timestamp that is not
    /// [`format_inbound_timestamp`].
    pub async fn set_inbound_policy(
        &self,
        accept_mode: &str,
        updated_at: &str,
    ) -> Result<InboundAdmissionView> {
        if !matches!(accept_mode, "off" | "observe" | "auto") {
            return Err(StoreError::Conflict(
                "inbound accept_mode must be off, observe, or auto".to_owned(),
            ));
        }
        require_inbound_timestamp("updated_at", updated_at)?;
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO inbound_policy (id, accept_mode, updated_at) VALUES (1, ?, ?) \
             ON CONFLICT(id) DO UPDATE SET \
                 accept_mode = excluded.accept_mode, \
                 updated_at = excluded.updated_at",
        )
        .bind(accept_mode)
        .bind(updated_at)
        .execute(&mut *tx)
        .await?;
        let view = admission_view(&mut tx).await?;
        tx.commit().await?;
        Ok(view)
    }

    /// One page of inbound rows, newest `at` first, then `id`.
    ///
    /// `text_preview` is `substr(text, 1, 50)` (SQLite characters, not
    /// bytes). The full `text` and `meta` columns are not selected. `limit`
    /// is 1..=101 so a caller can ask for one extra row and learn whether
    /// another page exists. `before` is exclusive: rows strictly older than
    /// that `(at, id)`.
    ///
    /// # Errors
    /// Storage, or a `limit` outside 1..=101.
    pub async fn list_inbound_event_summaries(
        &self,
        limit: i64,
        before: Option<(&str, &str)>,
    ) -> Result<Vec<InboundEventSummary>> {
        if !(1..=101).contains(&limit) {
            return Err(StoreError::Conflict(
                "inbound event page limit must be 1..=101".to_owned(),
            ));
        }
        let (before_at, before_id) = before.unzip();
        let rows = sqlx::query(
            "SELECT id, source, category, recipient_identity, thread_key, actor, human, \
                    addressed, at, received_at, \
                    CASE WHEN text IS NULL THEN NULL ELSE substr(text, 1, 50) END AS text_preview, \
                    text_scrubbed_at, disposition, disposition_reason, policy_version, run_state \
             FROM inbound_events \
             WHERE (?1 IS NULL AND ?2 IS NULL) \
                OR at < ?1 \
                OR (at = ?1 AND id < ?2) \
             ORDER BY at DESC, id DESC \
             LIMIT ?3",
        )
        .bind(before_at)
        .bind(before_id)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.into_iter().map(summary_from_row).collect())
    }
}

fn is_inbound_run_event_conflict(err: &dyn sqlx::error::DatabaseError) -> bool {
    if err.kind() != sqlx::error::ErrorKind::UniqueViolation
        || err.code().as_deref() != Some(SQLITE_CONSTRAINT_UNIQUE)
    {
        return false;
    }
    err.message().contains("inbound_runs.event_id")
}

/// One allowlist row as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundAllowEntry {
    pub source: String,
    pub actor: String,
    pub added_at: String,
    pub note: Option<String>,
}

/// A row the allowlist replace writes. A new `(source, actor)` is stamped
/// with the `added_at` passed to [`Store::replace_inbound_allowlist`]. A pair
/// that is already stored keeps its old stamp.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundAllowWrite {
    pub source: String,
    pub actor: String,
    pub note: Option<String>,
}

/// The single `inbound_policy` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundPolicy {
    pub accept_mode: String,
    pub updated_at: String,
}

/// Allowlist plus the effective accept mode, from one transaction.
///
/// `accept_mode` is `observe` when no policy row has been written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundAdmissionView {
    pub entries: Vec<InboundAllowEntry>,
    pub accept_mode: String,
    pub updated_at: Option<String>,
}

async fn admission_view(tx: &mut sqlx::SqliteConnection) -> Result<InboundAdmissionView> {
    let policy = sqlx::query("SELECT accept_mode, updated_at FROM inbound_policy WHERE id = 1")
        .fetch_optional(&mut *tx)
        .await?;
    let rows = sqlx::query(
        "SELECT source, actor, added_at, note FROM inbound_allowlist ORDER BY source, actor",
    )
    .fetch_all(&mut *tx)
    .await?;
    let (accept_mode, updated_at) = match policy {
        Some(row) => (
            row.get::<String, _>("accept_mode"),
            Some(row.get::<String, _>("updated_at")),
        ),
        None => ("observe".to_owned(), None),
    };
    Ok(InboundAdmissionView {
        entries: rows
            .into_iter()
            .map(|row| InboundAllowEntry {
                source: row.get("source"),
                actor: row.get("actor"),
                added_at: row.get("added_at"),
                note: row.get("note"),
            })
            .collect(),
        accept_mode,
        updated_at,
    })
}

/// A list-page row. `text_preview` is at most 50 characters. There is no
/// full text and no `meta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundEventSummary {
    pub id: String,
    pub source: String,
    pub category: String,
    pub recipient_identity: Option<String>,
    pub thread_key: Option<String>,
    pub actor: Option<String>,
    pub human: bool,
    pub addressed: String,
    pub at: String,
    pub received_at: String,
    pub text_preview: Option<String>,
    pub text_scrubbed_at: Option<String>,
    pub disposition: String,
    pub disposition_reason: Option<String>,
    pub policy_version: Option<String>,
    pub run_state: Option<String>,
}

fn summary_from_row(row: SqliteRow) -> InboundEventSummary {
    InboundEventSummary {
        id: row.get("id"),
        source: row.get("source"),
        category: row.get("category"),
        recipient_identity: row.get("recipient_identity"),
        thread_key: row.get("thread_key"),
        actor: row.get("actor"),
        human: row.get::<i64, _>("human") != 0,
        addressed: row.get("addressed"),
        at: row.get("at"),
        received_at: row.get("received_at"),
        text_preview: row.get("text_preview"),
        text_scrubbed_at: row.get("text_scrubbed_at"),
        disposition: row.get("disposition"),
        disposition_reason: row.get("disposition_reason"),
        policy_version: row.get("policy_version"),
        run_state: row.get("run_state"),
    }
}

fn is_inflight_conflict(err: &dyn sqlx::error::DatabaseError) -> bool {
    // Confirm the extended UNIQUE code before reading the message. SQLite
    // names the columns (`inbound_events.source, inbound_events.actor`), not
    // `idx_inbound_events_inflight`, so an index-name match never hits.
    if err.kind() != sqlx::error::ErrorKind::UniqueViolation
        || err.code().as_deref() != Some(SQLITE_CONSTRAINT_UNIQUE)
    {
        return false;
    }
    let message = err.message();
    message.contains("inbound_events.source") && message.contains("inbound_events.actor")
}

fn event_from_row(row: SqliteRow) -> InboundEvent {
    InboundEvent {
        id: row.get("id"),
        source: row.get("source"),
        category: row.get("category"),
        recipient_identity: row.get("recipient_identity"),
        thread_key: row.get("thread_key"),
        actor: row.get("actor"),
        human: row.get::<i64, _>("human") != 0,
        addressed: row.get("addressed"),
        at: row.get("at"),
        received_at: row.get("received_at"),
        text: row.get("text"),
        meta: row.get("meta"),
        text_scrubbed_at: row.get("text_scrubbed_at"),
        disposition: row.get("disposition"),
        disposition_reason: row.get("disposition_reason"),
        policy_version: row.get("policy_version"),
        run_state: row.get("run_state"),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
    use std::str::FromStr;

    use super::*;
    use crate::StoreError;

    fn sample(
        id: &str,
        actor: &str,
        disposition: &str,
        run_state: Option<&str>,
    ) -> NewInboundEvent {
        NewInboundEvent {
            id: id.to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: Some("me".to_owned()),
            thread_key: None,
            actor: Some(actor.to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: "2026-11-11T00:00:00Z".to_owned(),
            received_at: "2026-11-11T00:00:01Z".to_owned(),
            text: Some(format!("text-{id}")),
            meta: Some(format!("meta-{id}")),
            disposition: disposition.to_owned(),
            disposition_reason: Some("first".to_owned()),
            policy_version: Some("p1".to_owned()),
            run_state: run_state.map(str::to_owned),
        }
    }

    async fn wal_store(conns: u32) -> (Store, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            .busy_timeout(Duration::from_secs(5))
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(conns)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        (crate::test_hooks::from_pool(pool), dir)
    }

    async fn seed_run(store: &Store, id: &str) {
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES (?, 'completed', '{}', '{}', '2026-11-11T00:00:00Z')",
        )
        .bind(id)
        .execute(crate::test_hooks::pool(store))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn inserting_the_same_inbound_id_twice_does_not_overwrite() {
        // Acceptance ①. Mutation: INSERT OR REPLACE stores the second
        // disposition and this test goes red. ON CONFLICT(id) DO NOTHING
        // (not OR IGNORE) is what leaves the in-flight clash visible.
        let store = Store::open_memory().await.unwrap();
        let mut first = sample("hyphae:m1", "npub1", "explicit", Some("starting"));
        first.disposition_reason = Some("allow".to_owned());
        assert_eq!(
            store.insert_inbound_event(&first).await.unwrap(),
            InboundInsert::Inserted
        );

        let mut second = sample("hyphae:m1", "npub1", "ignore", None);
        second.disposition_reason = Some("not_allowlisted".to_owned());
        second.text = Some("replaced".to_owned());
        assert_eq!(
            store.insert_inbound_event(&second).await.unwrap(),
            InboundInsert::Duplicate
        );

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(count, 1);
        let stored = store.get_inbound_event("hyphae:m1").await.unwrap().unwrap();
        assert_eq!(stored.disposition, "explicit");
        assert_eq!(stored.disposition_reason.as_deref(), Some("allow"));
        assert_eq!(stored.text.as_deref(), Some("text-hyphae:m1"));
        assert_eq!(stored.run_state.as_deref(), Some("starting"));
    }

    #[tokio::test]
    async fn insert_reports_inserted_duplicate_and_inflight() {
        let store = Store::open_memory().await.unwrap();
        let first = sample("hyphae:a", "npub1", "explicit", Some("starting"));
        assert_eq!(
            store.insert_inbound_event(&first).await.unwrap(),
            InboundInsert::Inserted
        );
        assert_eq!(
            store.insert_inbound_event(&first).await.unwrap(),
            InboundInsert::Duplicate
        );

        let other = sample("hyphae:b", "npub1", "explicit", Some("starting"));
        assert_eq!(
            store.insert_inbound_event(&other).await.unwrap(),
            InboundInsert::Inflight
        );
        assert!(store.get_inbound_event("hyphae:b").await.unwrap().is_none());

        // A different sender is not the in-flight row.
        let elsewhere = sample("hyphae:c", "npub2", "explicit", Some("starting"));
        assert_eq!(
            store.insert_inbound_event(&elsewhere).await.unwrap(),
            InboundInsert::Inserted
        );
    }

    #[tokio::test]
    async fn inflight_rewrite_keeps_the_original_id_and_a_replay_is_a_duplicate() {
        let store = Store::open_memory().await.unwrap();
        let occupying = sample("hyphae:one", "npub1", "explicit", Some("starting"));
        assert_eq!(
            store.insert_inbound_event(&occupying).await.unwrap(),
            InboundInsert::Inserted
        );

        let original_id = "hyphae:two";
        let challenger = sample(original_id, "npub1", "explicit", Some("starting"));
        assert_eq!(
            store.insert_inbound_event(&challenger).await.unwrap(),
            InboundInsert::Inflight
        );

        let mut limited = sample(original_id, "npub1", "ignore", None);
        limited.disposition_reason = Some("rate_limited".to_owned());
        limited.text = Some("still-the-message".to_owned());
        assert_eq!(
            store.insert_inbound_event(&limited).await.unwrap(),
            InboundInsert::Inserted
        );

        let stored = store.get_inbound_event(original_id).await.unwrap().unwrap();
        assert_eq!(stored.id, original_id);
        assert_eq!(stored.disposition, "ignore");
        assert_eq!(stored.disposition_reason.as_deref(), Some("rate_limited"));
        assert_eq!(stored.text.as_deref(), Some("still-the-message"));
        assert!(stored.run_state.is_none());

        let mut replay = limited.clone();
        replay.disposition = "explicit".to_owned();
        replay.run_state = Some("starting".to_owned());
        replay.text = Some("should-not-land".to_owned());
        assert_eq!(
            store.insert_inbound_event(&replay).await.unwrap(),
            InboundInsert::Duplicate
        );
        let again = store.get_inbound_event(original_id).await.unwrap().unwrap();
        assert_eq!(again.disposition, "ignore");
        assert_eq!(again.text.as_deref(), Some("still-the-message"));
    }

    #[tokio::test]
    async fn concurrent_explicit_inserts_for_one_actor_admit_only_one() {
        // Mutation: drop idx_inbound_events_inflight and both inserts return
        // Inserted, so this test goes red.
        let (store, _dir) = wal_store(2).await;
        let barrier = Arc::new(tokio::sync::Barrier::new(2));
        let left_store = store.clone();
        let right_store = store.clone();
        let left_gate = Arc::clone(&barrier);
        let right_gate = barrier;
        let (left, right) = tokio::join!(
            async move {
                left_gate.wait().await;
                left_store
                    .insert_inbound_event(&sample(
                        "hyphae:left",
                        "npub1",
                        "explicit",
                        Some("starting"),
                    ))
                    .await
            },
            async move {
                right_gate.wait().await;
                right_store
                    .insert_inbound_event(&sample(
                        "hyphae:right",
                        "npub1",
                        "explicit",
                        Some("starting"),
                    ))
                    .await
            },
        );
        let mut outcomes = vec![left.unwrap(), right.unwrap()];
        outcomes.sort_by_key(|outcome| match outcome {
            InboundInsert::Inserted => 0,
            InboundInsert::Inflight => 1,
            InboundInsert::Duplicate => 2,
        });
        assert_eq!(
            outcomes,
            vec![InboundInsert::Inserted, InboundInsert::Inflight]
        );

        let explicit_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM inbound_events WHERE disposition = 'explicit'",
        )
        .fetch_one(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(explicit_count, 1);
    }

    #[tokio::test]
    async fn mark_started_and_reconcile_split_linked_from_unlinked() {
        let store = Store::open_memory().await.unwrap();
        assert_eq!(
            store
                .insert_inbound_event(&sample(
                    "hyphae:linked",
                    "npub1",
                    "explicit",
                    Some("starting"),
                ))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        assert_eq!(
            store
                .insert_inbound_event(&sample(
                    "hyphae:orphan",
                    "npub2",
                    "explicit",
                    Some("starting"),
                ))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        seed_run(&store, "run_linked").await;
        store
            .insert_inbound_run("run_linked", "hyphae:linked")
            .await
            .unwrap();

        let found = store.inbound_reconcile().await.unwrap();
        assert_eq!(found.linked, vec!["hyphae:linked".to_owned()]);
        assert_eq!(found.unlinked, vec!["hyphae:orphan".to_owned()]);

        assert!(store.mark_inbound_started("hyphae:linked").await.unwrap());
        assert!(!store.mark_inbound_started("hyphae:linked").await.unwrap());
        assert!(!store.mark_inbound_started("hyphae:missing").await.unwrap());
        let started = store
            .get_inbound_event("hyphae:linked")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(started.run_state.as_deref(), Some("started"));

        let after = store.inbound_reconcile().await.unwrap();
        assert!(after.linked.is_empty());
        assert_eq!(after.unlinked, vec!["hyphae:orphan".to_owned()]);
    }

    #[tokio::test]
    async fn scrub_clears_text_before_the_cutoff_and_a_second_pass_is_zero() {
        let store = Store::open_memory().await.unwrap();
        let mut old = sample("hyphae:old", "npub1", "ignore", None);
        old.received_at = "2026-01-01T00:00:00Z".to_owned();
        old.at = "2025-12-31T00:00:00Z".to_owned();
        old.category = "group".to_owned();
        old.disposition_reason = Some("not_addressed".to_owned());
        let mut fresh = sample("hyphae:fresh", "npub2", "explicit", Some("started"));
        fresh.received_at = "2026-11-01T00:00:00Z".to_owned();
        let mut boundary = sample("hyphae:boundary", "npub3", "ignore", None);
        boundary.received_at = "2026-06-01T00:00:00Z".to_owned();
        // `at` is before the cutoff, `received_at` is after: kept.
        // Mutation: judge the cutoff on `at` and this row is scrubbed.
        let mut at_early = sample("hyphae:at-early", "npub4", "ignore", None);
        at_early.at = "2026-01-01T00:00:00Z".to_owned();
        at_early.received_at = "2026-11-01T00:00:00Z".to_owned();
        // `at` is after the cutoff, `received_at` is before: scrubbed.
        // Mutation: judge the cutoff on `at` and this row is kept.
        let mut received_early = sample("hyphae:received-early", "npub5", "ignore", None);
        received_early.at = "2026-11-01T00:00:00Z".to_owned();
        received_early.received_at = "2026-01-02T00:00:00Z".to_owned();
        for event in [&old, &fresh, &boundary, &at_early, &received_early] {
            assert_eq!(
                store.insert_inbound_event(event).await.unwrap(),
                InboundInsert::Inserted
            );
        }

        let cutoff = "2026-06-01T00:00:00Z";
        let stamp = "2026-11-11T12:00:00Z";
        assert_eq!(
            store
                .scrub_expired_inbound_text(cutoff, stamp)
                .await
                .unwrap(),
            2
        );

        let old_row = store
            .get_inbound_event("hyphae:old")
            .await
            .unwrap()
            .unwrap();
        assert!(old_row.text.is_none());
        assert!(old_row.meta.is_none());
        assert_eq!(old_row.text_scrubbed_at.as_deref(), Some(stamp));
        assert_eq!(old_row.actor.as_deref(), Some("npub1"));
        assert_eq!(old_row.at, "2025-12-31T00:00:00Z");
        assert_eq!(old_row.category, "group");
        assert_eq!(old_row.disposition, "ignore");
        assert_eq!(old_row.disposition_reason.as_deref(), Some("not_addressed"));

        let fresh_row = store
            .get_inbound_event("hyphae:fresh")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(fresh_row.text.as_deref(), Some("text-hyphae:fresh"));
        assert_eq!(fresh_row.meta.as_deref(), Some("meta-hyphae:fresh"));
        assert!(fresh_row.text_scrubbed_at.is_none());
        assert_eq!(fresh_row.disposition, "explicit");

        let boundary_row = store
            .get_inbound_event("hyphae:boundary")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(boundary_row.text.as_deref(), Some("text-hyphae:boundary"));
        assert!(boundary_row.text_scrubbed_at.is_none());

        let at_early_row = store
            .get_inbound_event("hyphae:at-early")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(at_early_row.at, "2026-01-01T00:00:00Z");
        assert_eq!(at_early_row.received_at, "2026-11-01T00:00:00Z");
        assert_eq!(at_early_row.text.as_deref(), Some("text-hyphae:at-early"));
        assert!(at_early_row.text_scrubbed_at.is_none());

        let received_early_row = store
            .get_inbound_event("hyphae:received-early")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received_early_row.at, "2026-11-01T00:00:00Z");
        assert_eq!(received_early_row.received_at, "2026-01-02T00:00:00Z");
        assert!(received_early_row.text.is_none());
        assert!(received_early_row.meta.is_none());
        assert_eq!(received_early_row.text_scrubbed_at.as_deref(), Some(stamp));

        assert_eq!(
            store
                .scrub_expired_inbound_text(cutoff, "2099-01-02T00:00:00Z")
                .await
                .unwrap(),
            0
        );
        let restamped = store
            .get_inbound_event("hyphae:old")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(restamped.text_scrubbed_at.as_deref(), Some(stamp));
    }

    fn is_check_violation(err: &StoreError) -> bool {
        match err {
            StoreError::Sqlx(sqlx::Error::Database(db)) => {
                db.kind() == sqlx::error::ErrorKind::CheckViolation
                    && db.code().as_deref() == Some("275")
            }
            _ => false,
        }
    }

    #[tokio::test]
    async fn explicit_without_an_actor_is_a_check_error_not_inflight() {
        let store = Store::open_memory().await.unwrap();
        let mut first = sample("hyphae:anon-1", "npub1", "explicit", Some("starting"));
        first.actor = None;
        let err = store.insert_inbound_event(&first).await.unwrap_err();
        assert!(is_check_violation(&err), "{err:?}");
        assert!(
            store
                .get_inbound_event("hyphae:anon-1")
                .await
                .unwrap()
                .is_none()
        );

        let mut second = sample("hyphae:anon-2", "npub1", "explicit", Some("starting"));
        second.actor = None;
        let err = store.insert_inbound_event(&second).await.unwrap_err();
        assert!(is_check_violation(&err), "{err:?}");

        // An empty actor would also slip past the in-flight index as a
        // catch-all bucket, so the CHECK refuses it too.
        let mut empty = sample("hyphae:anon-empty", "npub1", "explicit", Some("starting"));
        empty.actor = Some(String::new());
        let err = store.insert_inbound_event(&empty).await.unwrap_err();
        assert!(is_check_violation(&err), "{err:?}");

        let mut ignored = sample("hyphae:anon-ok", "npub1", "ignore", None);
        ignored.actor = None;
        assert_eq!(
            store.insert_inbound_event(&ignored).await.unwrap(),
            InboundInsert::Inserted
        );
    }

    #[tokio::test]
    async fn a_bad_disposition_or_run_state_fails_the_check() {
        let store = Store::open_memory().await.unwrap();
        let bad_disposition = sample("hyphae:bad-disp", "npub1", "Explicit", None);
        let err = store
            .insert_inbound_event(&bad_disposition)
            .await
            .unwrap_err();
        assert!(is_check_violation(&err), "{err:?}");
        assert!(
            store
                .get_inbound_event("hyphae:bad-disp")
                .await
                .unwrap()
                .is_none()
        );

        let bad_state = sample("hyphae:bad-state", "npub2", "ignore", Some("start"));
        let err = store.insert_inbound_event(&bad_state).await.unwrap_err();
        assert!(is_check_violation(&err), "{err:?}");

        let internal = sample("scheduler:run_1", "sched-1", "internal", None);
        assert_eq!(
            store.insert_inbound_event(&internal).await.unwrap(),
            InboundInsert::Inserted
        );

        let human_err = sqlx::query(
            "INSERT INTO inbound_events ( \
                id, source, category, human, addressed, at, received_at, disposition \
             ) VALUES ( \
                'hyphae:human', 'hyphae', 'direct', 2, 'none', \
                '2026-11-11T00:00:00Z', '2026-11-11T00:00:01Z', 'ignore' \
             )",
        )
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap_err();
        let sqlx::Error::Database(db) = human_err else {
            panic!("human=2 should be a check error, got {human_err:?}");
        };
        assert_eq!(db.kind(), sqlx::error::ErrorKind::CheckViolation);
        assert_eq!(db.code().as_deref(), Some("275"));
    }

    #[tokio::test]
    async fn mark_run_state_allows_only_the_legal_edges() {
        let store = Store::open_memory().await.unwrap();
        for id in ["hyphae:done", "hyphae:stop", "hyphae:fail", "hyphae:skip"] {
            assert_eq!(
                store
                    .insert_inbound_event(&sample(id, id, "explicit", Some("starting")))
                    .await
                    .unwrap(),
                InboundInsert::Inserted
            );
        }

        assert!(
            store
                .mark_inbound_run_state("hyphae:done", "started")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_inbound_event("hyphae:done")
                .await
                .unwrap()
                .unwrap()
                .run_state
                .as_deref(),
            Some("started")
        );
        assert!(
            store
                .mark_inbound_run_state("hyphae:done", "done")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:done", "done")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:done", "interrupted")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_inbound_event("hyphae:done")
                .await
                .unwrap()
                .unwrap()
                .run_state
                .as_deref(),
            Some("done")
        );

        assert!(
            store
                .mark_inbound_run_state("hyphae:stop", "interrupted")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:stop", "started")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_inbound_event("hyphae:stop")
                .await
                .unwrap()
                .unwrap()
                .run_state
                .as_deref(),
            Some("interrupted")
        );

        assert!(
            store
                .mark_inbound_run_state("hyphae:fail", "start_failed")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:fail", "started")
                .await
                .unwrap()
        );

        assert!(
            !store
                .mark_inbound_run_state("hyphae:skip", "done")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:skip", "start")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_inbound_event("hyphae:skip")
                .await
                .unwrap()
                .unwrap()
                .run_state
                .as_deref(),
            Some("starting")
        );
        assert!(
            store
                .mark_inbound_run_state("hyphae:skip", "started")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:skip", "start_failed")
                .await
                .unwrap()
        );
        assert!(
            !store
                .mark_inbound_run_state("hyphae:missing", "started")
                .await
                .unwrap()
        );

        // A start error must not clobber an event that already has a run.
        assert_eq!(
            store
                .insert_inbound_event(&sample(
                    "hyphae:linked",
                    "npub-linked",
                    "explicit",
                    Some("starting"),
                ))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES ('run_linked', 'queued', '{}', '{}', '2026-11-11T00:00:00Z')",
        )
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        store
            .insert_inbound_run("run_linked", "hyphae:linked")
            .await
            .unwrap();
        assert!(
            !store
                .mark_inbound_run_state("hyphae:linked", "start_failed")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_inbound_event("hyphae:linked")
                .await
                .unwrap()
                .unwrap()
                .run_state
                .as_deref(),
            Some("starting")
        );
        assert!(
            store
                .mark_inbound_run_state("hyphae:linked", "started")
                .await
                .unwrap()
        );

        let after = store.inbound_reconcile().await.unwrap();
        assert!(after.linked.is_empty());
        assert!(after.unlinked.is_empty());
    }

    #[tokio::test]
    async fn inbound_timestamps_must_be_utc_seconds() {
        let stamped = "2026-11-11T00:00:01.500Z".parse::<DateTime<Utc>>().unwrap();
        assert_eq!(format_inbound_timestamp(stamped), "2026-11-11T00:00:01Z");

        let store = Store::open_memory().await.unwrap();
        let mut fractional = sample("hyphae:frac", "npub1", "ignore", None);
        fractional.received_at = "2026-11-11T00:00:01.000Z".to_owned();
        let err = store.insert_inbound_event(&fractional).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");

        let mut offset = sample("hyphae:offset", "npub1", "ignore", None);
        offset.at = "2026-11-11T00:00:00+00:00".to_owned();
        let err = store.insert_inbound_event(&offset).await.unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
        assert!(
            store
                .get_inbound_event("hyphae:offset")
                .await
                .unwrap()
                .is_none()
        );

        let kept = sample("hyphae:kept", "npub1", "ignore", None);
        assert_eq!(
            store.insert_inbound_event(&kept).await.unwrap(),
            InboundInsert::Inserted
        );
        let err = store
            .scrub_expired_inbound_text("2026-06-01T00:00:00Z", "2026-11-11T12:00:00.000Z")
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::Conflict(_)), "{err:?}");
        assert!(
            store
                .get_inbound_event("hyphae:kept")
                .await
                .unwrap()
                .unwrap()
                .text_scrubbed_at
                .is_none()
        );
    }

    #[tokio::test]
    async fn upgrading_from_0033_leaves_old_rows_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        crate::test_hooks::prepare_db_stopped_before(&path, 34)
            .await
            .unwrap();
        assert_eq!(
            crate::test_hooks::applied_schema_version(&path)
                .await
                .unwrap(),
            33
        );

        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(false)
            .journal_mode(SqliteJournalMode::Wal)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES ('old_run', 'completed', '{\"prompt\":\"keep-me\"}', '{}', '2026-01-01T00:00:00Z')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query("INSERT INTO runs_before_tool_result_tagging (run_id) VALUES ('old_run')")
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;

        let store = Store::open(&path).await.unwrap();
        // 0034 入站表 + 0035 游标；升级后旧行不变，schema 停在当前最新。
        assert_eq!(store.latest_applied_migration().await.unwrap(), 35);
        let cursor_table: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'inbound_cursor'",
        )
        .fetch_one(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(cursor_table, 1);
        let input: String = sqlx::query_scalar("SELECT input FROM runs WHERE id = 'old_run'")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(input, "{\"prompt\":\"keep-me\"}");
        let status: String = sqlx::query_scalar("SELECT status FROM runs WHERE id = 'old_run'")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(status, "completed");
        let tagged: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM runs_before_tool_result_tagging WHERE run_id = 'old_run'",
        )
        .fetch_one(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        assert_eq!(tagged, 1);
        let inbound: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(inbound, 0);
        let index_sql: String = sqlx::query_scalar(
            "SELECT sql FROM sqlite_master WHERE type = 'index' AND name = 'idx_inbound_events_inflight'",
        )
        .fetch_one(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        assert!(index_sql.contains("UNIQUE"));
        assert!(index_sql.contains("starting"));
        assert!(index_sql.contains("started"));
    }

    #[tokio::test]
    async fn window_count_includes_ignore_and_skips_protocol_and_filtered() {
        let store = Store::open_memory().await.unwrap();
        let mut rows = [
            sample("hyphae:in-ignore", "npub1", "ignore", None),
            sample("hyphae:in-limited", "npub1", "ignore", None),
            sample("hyphae:in-explicit", "npub1", "explicit", Some("done")),
        ];
        rows[1].disposition_reason = Some("rate_limited".to_owned());
        let mut protocol = sample("hyphae:proto", "npub1", "ignore", None);
        protocol.category = "protocol".to_owned();
        let filtered = sample("hyphae:filtered", "npub1", "filtered", None);
        let other_actor = sample("hyphae:other", "npub2", "ignore", None);
        let mut old = sample("hyphae:old-window", "npub1", "ignore", None);
        old.received_at = "2026-11-10T00:00:00Z".to_owned();
        // Same actor and the same `received_at` as the counted rows, other source.
        // Mutation: drop `source = ?` (and its bind) and this row is counted, so 3 becomes 4.
        let mut other_source = sample("wechat:same-actor", "npub1", "ignore", None);
        other_source.source = "wechat".to_owned();
        for event in rows
            .iter()
            .chain([&protocol, &filtered, &other_actor, &old, &other_source])
        {
            assert_eq!(
                store.insert_inbound_event(event).await.unwrap(),
                InboundInsert::Inserted
            );
        }
        assert_eq!(
            store
                .count_inbound_window("hyphae", "npub1", "2026-11-11T00:00:00Z")
                .await
                .unwrap(),
            3
        );
    }

    #[tokio::test]
    async fn linking_an_event_twice_keeps_one_run() {
        // The second call rolls its session and run back. Mutation: commit the
        // run before the link, and this count becomes 2.
        let store = Store::open_memory().await.unwrap();
        assert_eq!(
            store
                .insert_inbound_event(&sample(
                    "hyphae:link",
                    "npub1",
                    "explicit",
                    Some("starting"),
                ))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        let first = inbound_run("run_one", "sess_one");
        let first_session = inbound_session("sess_one");
        assert_eq!(
            store
                .insert_session_run_for_inbound(&first_session, &first, "hyphae:link")
                .await
                .unwrap(),
            InboundRunLink::Linked
        );
        let second = inbound_run("run_two", "sess_two");
        let second_session = inbound_session("sess_two");
        assert_eq!(
            store
                .insert_session_run_for_inbound(&second_session, &second, "hyphae:link")
                .await
                .unwrap(),
            InboundRunLink::AlreadyLinked
        );
        let runs: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(runs, 1);
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(sessions, 1);
        assert_eq!(
            store
                .inbound_run_id_for_event("hyphae:link")
                .await
                .unwrap()
                .as_deref(),
            Some("run_one")
        );
        assert!(store.session_has_inbound_run("sess_one").await.unwrap());
        assert!(!store.session_has_inbound_run("sess_two").await.unwrap());
    }

    #[tokio::test]
    async fn a_row_that_is_not_starting_does_not_link() {
        // Mutation: drop the `run_state = 'starting'` check in
        // `insert_session_run_for_inbound` and both calls return Linked.
        let store = Store::open_memory().await.unwrap();
        for (id, state) in [
            ("hyphae:cut", Some("interrupted")),
            ("hyphae:broke", Some("start_failed")),
        ] {
            assert_eq!(
                store
                    .insert_inbound_event(&sample(id, id, "explicit", state))
                    .await
                    .unwrap(),
                InboundInsert::Inserted
            );
            let outcome = store
                .insert_session_run_for_inbound(&inbound_session(id), &inbound_run(id, id), id)
                .await
                .unwrap();
            assert_eq!(outcome, InboundRunLink::NotStarting);
        }
        assert_eq!(table_count(&store, "runs").await, 0);
        assert_eq!(table_count(&store, "sessions").await, 0);
        assert_eq!(table_count(&store, "inbound_runs").await, 0);
    }

    #[tokio::test]
    async fn a_mismatched_session_is_refused_before_the_link() {
        // Mutation: drop the session-id guard and the null session_id call
        // returns Linked, while a different id fails the sessions FK instead
        // of this Conflict.
        let store = Store::open_memory().await.unwrap();
        assert_eq!(
            store
                .insert_inbound_event(&sample(
                    "hyphae:mismatch",
                    "npub1",
                    "explicit",
                    Some("starting"),
                ))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        let session = inbound_session("sess_one");
        let other = inbound_run("run_other", "sess_other");
        let err = store
            .insert_session_run_for_inbound(&session, &other, "hyphae:mismatch")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message == "inbound run session does not match"),
            "{err:?}"
        );

        let mut missing = inbound_run("run_missing", "sess_one");
        missing.session_id = None;
        let err = store
            .insert_session_run_for_inbound(&session, &missing, "hyphae:mismatch")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message == "inbound run session does not match"),
            "{err:?}"
        );
        assert_eq!(table_count(&store, "sessions").await, 0);
        assert_eq!(table_count(&store, "runs").await, 0);
        assert_eq!(table_count(&store, "inbound_runs").await, 0);
    }

    #[tokio::test]
    async fn a_workspace_binding_is_refused_before_the_link() {
        // Mutation: drop the workspace guard and both calls miss this
        // Conflict (the workspace FK, or a committed link).
        let store = Store::open_memory().await.unwrap();
        assert_eq!(
            store
                .insert_inbound_event(&sample("hyphae:ws", "npub1", "explicit", Some("starting")))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        let workspace =
            agent24_protocol::WorkspaceId::parse("ws_0123456789ABCDEFGHJKMNPQRS").unwrap();

        let mut session = inbound_session("sess_ws");
        session.workspace_id = Some(workspace.clone());
        let run = inbound_run("run_ws_session", "sess_ws");
        let err = store
            .insert_session_run_for_inbound(&session, &run, "hyphae:ws")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message == "inbound run is not workspace-bound"),
            "{err:?}"
        );

        let plain = inbound_session("sess_plain");
        let mut bound = inbound_run("run_ws_run", "sess_plain");
        bound.workspace_id = Some(workspace.clone());
        bound.input.workspace_id = Some(workspace);
        let err = store
            .insert_session_run_for_inbound(&plain, &bound, "hyphae:ws")
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict(ref message) if message == "inbound run is not workspace-bound"),
            "{err:?}"
        );
        assert_eq!(table_count(&store, "sessions").await, 0);
        assert_eq!(table_count(&store, "runs").await, 0);
        assert_eq!(table_count(&store, "inbound_runs").await, 0);
    }

    #[tokio::test]
    async fn a_session_with_only_an_unlinked_run_is_not_inbound() {
        // The session exists and has a run. That run is not in `inbound_runs`.
        // Mutation: drop the `inbound_runs` EXISTS and this becomes true.
        let store = Store::open_memory().await.unwrap();
        let session = inbound_session("sess_plain");
        store.insert_session(&session).await.unwrap();
        let run = inbound_run("run_plain", "sess_plain");
        store.insert_run(&run).await.unwrap();
        assert!(!store.session_has_inbound_run("sess_plain").await.unwrap());
    }

    #[tokio::test]
    async fn started_terminal_event_ids_cover_each_terminal_status() {
        // Each terminal status is load-bearing: drop `completed`, `failed`, or
        // `cancelled` from the IN list and that id disappears. A still-running
        // started row is not terminal. Mutation: drop `starting` from
        // `mark_inbound_done_for_run` and the last call returns false.
        let store = Store::open_memory().await.unwrap();
        for (event_id, actor, run_id, status) in [
            (
                "hyphae:term-cancelled",
                "npub-c",
                "run_cancelled",
                "cancelled",
            ),
            (
                "hyphae:term-completed",
                "npub-d",
                "run_completed",
                "completed",
            ),
            ("hyphae:term-failed", "npub-f", "run_failed", "failed"),
            ("hyphae:still-running", "npub-r", "run_running", "running"),
        ] {
            assert_eq!(
                store
                    .insert_inbound_event(&sample(event_id, actor, "explicit", Some("starting"),))
                    .await
                    .unwrap(),
                InboundInsert::Inserted
            );
            seed_run(&store, run_id).await;
            sqlx::query("UPDATE runs SET status = ? WHERE id = ?")
                .bind(status)
                .bind(run_id)
                .execute(crate::test_hooks::pool(&store))
                .await
                .unwrap();
            store.insert_inbound_run(run_id, event_id).await.unwrap();
            assert!(store.mark_inbound_started(event_id).await.unwrap());
        }
        assert_eq!(
            store.inbound_started_terminal_event_ids().await.unwrap(),
            vec![
                "hyphae:term-cancelled".to_owned(),
                "hyphae:term-completed".to_owned(),
                "hyphae:term-failed".to_owned(),
            ]
        );

        assert_eq!(
            store
                .insert_inbound_event(&sample(
                    "hyphae:from-starting",
                    "npub-s",
                    "explicit",
                    Some("starting"),
                ))
                .await
                .unwrap(),
            InboundInsert::Inserted
        );
        seed_run(&store, "run_from_starting").await;
        store
            .insert_inbound_run("run_from_starting", "hyphae:from-starting")
            .await
            .unwrap();
        assert!(
            store
                .mark_inbound_done_for_run("run_from_starting")
                .await
                .unwrap()
        );
        assert_eq!(
            store
                .get_inbound_event("hyphae:from-starting")
                .await
                .unwrap()
                .unwrap()
                .run_state
                .as_deref(),
            Some("done")
        );
    }

    async fn table_count(store: &Store, table: &str) -> i64 {
        let sql = match table {
            "sessions" => "SELECT COUNT(*) FROM sessions",
            "runs" => "SELECT COUNT(*) FROM runs",
            "inbound_runs" => "SELECT COUNT(*) FROM inbound_runs",
            other => panic!("unexpected table {other}"),
        };
        sqlx::query_scalar(sql)
            .fetch_one(crate::test_hooks::pool(store))
            .await
            .unwrap()
    }

    fn inbound_session(id: &str) -> agent24_protocol::Session {
        agent24_protocol::Session {
            id: id.to_owned(),
            title: String::new(),
            channel: "hyphae".to_owned(),
            workspace_id: None,
            created_at: "2026-11-11T00:00:00Z".to_owned(),
            updated_at: "2026-11-11T00:00:00Z".to_owned(),
        }
    }

    fn inbound_run(id: &str, session: &str) -> agent24_protocol::Run {
        agent24_protocol::Run {
            id: id.to_owned(),
            session_id: Some(session.to_owned()),
            workspace_id: None,
            status: agent24_protocol::RunStatus::Queued,
            input: agent24_protocol::RunInput {
                prompt: "hello".to_owned(),
                workspace_id: None,
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: None,
            error: None,
            usage: agent24_protocol::Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
            },
            schedule_id: None,
            created_at: "2026-11-11T00:00:00Z".to_owned(),
            started_at: None,
            ended_at: None,
        }
    }

    #[tokio::test]
    async fn allowlist_policy_and_event_page_keep_the_body_out_of_the_summary() {
        let store = Store::open_memory().await.unwrap();
        assert!(store.get_inbound_policy().await.unwrap().is_none());
        let written = store
            .set_inbound_policy("auto", "2026-11-11T01:00:00Z")
            .await
            .unwrap();
        assert_eq!(written.accept_mode, "auto");
        assert_eq!(written.updated_at.as_deref(), Some("2026-11-11T01:00:00Z"));
        assert!(written.entries.is_empty());
        let policy = store.get_inbound_policy().await.unwrap().unwrap();
        assert_eq!(policy.accept_mode, written.accept_mode);
        assert_eq!(policy.updated_at, "2026-11-11T01:00:00Z");
        // Dropping the accept_mode guard in `set_inbound_policy` turns this
        // red: SQLite's CHECK then fails as `StoreError::Sqlx`, not Conflict,
        // and the stored mode stays `auto`.
        let rejected = store
            .set_inbound_policy("sometimes", "2026-11-11T01:00:00Z")
            .await
            .unwrap_err();
        assert!(
            matches!(rejected, StoreError::Conflict(ref detail) if detail.contains("accept_mode")),
            "{rejected:?}"
        );
        assert_eq!(
            store
                .get_inbound_policy()
                .await
                .unwrap()
                .unwrap()
                .accept_mode,
            "auto"
        );

        let replaced = store
            .replace_inbound_allowlist(
                &[
                    InboundAllowWrite {
                        source: "wechat".into(),
                        actor: "uid-b".into(),
                        note: None,
                    },
                    InboundAllowWrite {
                        source: "hyphae".into(),
                        actor: "npub1".into(),
                        note: Some("desk".into()),
                    },
                ],
                "2026-11-11T01:00:01Z",
            )
            .await
            .unwrap();
        assert_eq!(replaced.accept_mode, "auto");
        assert_eq!(replaced.entries[0].source, "hyphae");
        assert_eq!(replaced.entries[1].actor, "uid-b");
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert_eq!(listed, replaced.entries);
        store
            .replace_inbound_allowlist(&[], "2026-11-11T01:00:02Z")
            .await
            .unwrap();
        assert!(store.list_inbound_allowlist().await.unwrap().is_empty());

        let empty = store.read_inbound_admission().await.unwrap();
        assert!(empty.entries.is_empty());
        assert_eq!(empty.accept_mode, "auto");
        assert_eq!(empty.updated_at.as_deref(), Some("2026-11-11T01:00:00Z"));

        let mut event = sample("hyphae:long", "npub1", "ignore", None);
        event.text = Some(format!("{}SECRET-TAIL", "你".repeat(50)));
        event.meta = Some("META-SECRET".into());
        assert_eq!(
            store.insert_inbound_event(&event).await.unwrap(),
            InboundInsert::Inserted
        );
        // Dropping the limit guard turns the `0` case red: SQLite `LIMIT 0`
        // is an empty page, `Ok`, not `Conflict`. `102` is past the 1..=101
        // window the route uses for one extra row.
        for limit in [0, 102, -1] {
            let rejected = store
                .list_inbound_event_summaries(limit, None)
                .await
                .unwrap_err();
            assert!(
                matches!(rejected, StoreError::Conflict(ref detail) if detail.contains("limit")),
                "{limit} {rejected:?}"
            );
        }
        let page = store.list_inbound_event_summaries(2, None).await.unwrap();
        assert_eq!(page.len(), 1);
        // `substr(text, 1, 50)` is what caps this. The route clips again,
        // but this call does not go through it. Changing the `50` (or
        // dropping the `substr`) turns the equality red.
        let preview = page[0].text_preview.as_deref().unwrap();
        assert_eq!(preview, "你".repeat(50));
        assert_eq!(preview.chars().count(), 50);
        assert!(!preview.contains("SECRET-TAIL"));
        let stored = store
            .get_inbound_event("hyphae:long")
            .await
            .unwrap()
            .unwrap();
        assert!(stored.text.unwrap().contains("SECRET-TAIL"));
        assert_eq!(stored.meta.as_deref(), Some("META-SECRET"));
    }

    /// Three rows share `at`. Page size 2, then the cursor of the last row.
    /// Changing `id < ?2` to `id <= ?2` repeats that cursor row on the next
    /// page and this test goes red.
    #[tokio::test]
    async fn event_page_with_tied_at_follows_the_cursor_without_repeat_or_gap() {
        let store = Store::open_memory().await.unwrap();
        let at = "2026-11-11T02:00:00Z";
        for id in ["id-a", "id-c", "id-b"] {
            let mut event = sample(id, id, "ignore", None);
            event.at = at.to_owned();
            event.text = Some(format!("body-{id}"));
            assert_eq!(
                store.insert_inbound_event(&event).await.unwrap(),
                InboundInsert::Inserted
            );
        }
        let page1 = store.list_inbound_event_summaries(2, None).await.unwrap();
        let ids1: Vec<&str> = page1.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids1, ["id-c", "id-b"]);
        let cursor = page1.last().unwrap();
        let page2 = store
            .list_inbound_event_summaries(2, Some((cursor.at.as_str(), cursor.id.as_str())))
            .await
            .unwrap();
        let ids2: Vec<&str> = page2.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids2, ["id-a"]);
        let mut seen = Vec::new();
        seen.extend(ids1);
        seen.extend(ids2);
        seen.sort_unstable();
        assert_eq!(seen, ["id-a", "id-b", "id-c"]);
        let tail = page2.last().unwrap();
        let page3 = store
            .list_inbound_event_summaries(2, Some((tail.at.as_str(), tail.id.as_str())))
            .await
            .unwrap();
        assert!(page3.is_empty());
    }

    /// A repeated `(source, actor)` in one replace fails the primary key
    /// inside the same transaction. Committing the `DELETE` before the
    /// inserts drops the old row and this test goes red.
    #[tokio::test]
    async fn a_duplicate_allowlist_entry_rolls_back_the_whole_replace() {
        let store = Store::open_memory().await.unwrap();
        let stamp = "2026-11-11T03:00:00Z";
        store
            .replace_inbound_allowlist(
                &[InboundAllowWrite {
                    source: "hyphae".into(),
                    actor: "npub-keep".into(),
                    note: Some("desk".into()),
                }],
                stamp,
            )
            .await
            .unwrap();
        let rejected = store
            .replace_inbound_allowlist(
                &[
                    InboundAllowWrite {
                        source: "wechat".into(),
                        actor: "uid-new".into(),
                        note: None,
                    },
                    InboundAllowWrite {
                        source: "wechat".into(),
                        actor: "uid-new".into(),
                        note: Some("dup".into()),
                    },
                ],
                "2026-11-11T03:00:01Z",
            )
            .await
            .unwrap_err();
        assert!(matches!(rejected, StoreError::Sqlx(_)), "{rejected:?}");
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].source, "hyphae");
        assert_eq!(listed[0].actor, "npub-keep");
        assert_eq!(listed[0].note.as_deref(), Some("desk"));
        assert_eq!(listed[0].added_at, stamp);
    }

    /// A `(source, actor)` that is written again keeps its original
    /// `added_at`. A new pair gets the stamp of this replace. Stamping
    /// every inserted row with the new time turns the kept assertion red.
    #[tokio::test]
    async fn replace_keeps_added_at_for_a_source_actor_that_survives() {
        let store = Store::open_memory().await.unwrap();
        let first = "2026-11-11T01:00:00Z";
        let second = "2026-11-11T02:00:00Z";
        store
            .replace_inbound_allowlist(
                &[
                    InboundAllowWrite {
                        source: "hyphae".into(),
                        actor: "npub-keep".into(),
                        note: Some("old".into()),
                    },
                    InboundAllowWrite {
                        source: "wechat".into(),
                        actor: "uid-drop".into(),
                        note: None,
                    },
                ],
                first,
            )
            .await
            .unwrap();
        let replaced = store
            .replace_inbound_allowlist(
                &[
                    InboundAllowWrite {
                        source: "remote".into(),
                        actor: "host-new".into(),
                        note: None,
                    },
                    InboundAllowWrite {
                        source: "hyphae".into(),
                        actor: "npub-keep".into(),
                        note: Some("new".into()),
                    },
                ],
                second,
            )
            .await
            .unwrap();
        assert_eq!(replaced.entries.len(), 2);
        let keep = replaced
            .entries
            .iter()
            .find(|entry| entry.actor == "npub-keep")
            .unwrap();
        assert_eq!(keep.added_at, first);
        assert_eq!(keep.note.as_deref(), Some("new"));
        let fresh = replaced
            .entries
            .iter()
            .find(|entry| entry.actor == "host-new")
            .unwrap();
        assert_eq!(fresh.added_at, second);
        assert_eq!(fresh.source, "remote");
        assert!(
            replaced
                .entries
                .iter()
                .all(|entry| entry.actor != "uid-drop")
        );
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert_eq!(listed, replaced.entries);
    }

    /// `substr` of NULL is NULL in SQLite, so deleting the `CASE` stays
    /// green. Changing `THEN NULL` to `THEN ''` makes the preview `Some("")`
    /// and this test goes red.
    #[tokio::test]
    async fn a_scrubbed_body_has_a_null_preview() {
        let store = Store::open_memory().await.unwrap();
        let mut scrubbed = sample("hyphae:scrub", "npub1", "ignore", None);
        scrubbed.text = Some("visible-body".into());
        scrubbed.meta = Some("visible-meta".into());
        scrubbed.received_at = "2026-11-01T00:00:00Z".into();
        assert_eq!(
            store.insert_inbound_event(&scrubbed).await.unwrap(),
            InboundInsert::Inserted
        );
        let mut live = sample("hyphae:live", "npub2", "ignore", None);
        live.text = Some("still-here".into());
        live.received_at = "2026-11-11T03:00:00Z".into();
        assert_eq!(
            store.insert_inbound_event(&live).await.unwrap(),
            InboundInsert::Inserted
        );
        let cleared = store
            .scrub_expired_inbound_text("2026-11-10T00:00:00Z", "2026-11-11T04:00:00Z")
            .await
            .unwrap();
        assert_eq!(cleared, 1);
        let page = store.list_inbound_event_summaries(10, None).await.unwrap();
        let scrubbed_row = page.iter().find(|row| row.id == "hyphae:scrub").unwrap();
        assert!(scrubbed_row.text_preview.is_none(), "{scrubbed_row:?}");
        assert_eq!(
            scrubbed_row.text_scrubbed_at.as_deref(),
            Some("2026-11-11T04:00:00Z")
        );
        let live_row = page.iter().find(|row| row.id == "hyphae:live").unwrap();
        assert_eq!(live_row.text_preview.as_deref(), Some("still-here"));
        assert!(live_row.text_scrubbed_at.is_none());
        let stored = store
            .get_inbound_event("hyphae:scrub")
            .await
            .unwrap()
            .unwrap();
        assert!(stored.text.is_none());
        assert!(stored.meta.is_none());
    }

    /// Same store: actors that leave the dev seed are deleted. A host note
    /// and a non-wechat `dev-seed` row stay. Dropping the `DELETE` in
    /// `sync_wechat_dev_seed_allowlist` leaves bob and turns this red.
    #[tokio::test]
    async fn sync_inbound_wechat_dev_seed_drops_rows_missing_from_the_list() {
        let store = Store::open_memory().await.unwrap();
        let first = "2026-11-11T01:00:00Z";
        let later = "2026-11-11T02:00:00Z";
        assert!(
            store
                .insert_inbound_allowlist_if_absent("wechat", "alice", first, Some("dev-seed"))
                .await
                .unwrap()
        );
        assert!(
            store
                .insert_inbound_allowlist_if_absent("wechat", "bob", first, Some("dev-seed"))
                .await
                .unwrap()
        );
        assert!(
            store
                .insert_inbound_allowlist_if_absent("wechat", "carol", first, Some("host"))
                .await
                .unwrap()
        );
        assert!(
            store
                .insert_inbound_allowlist_if_absent("hyphae", "npub1", first, Some("dev-seed"))
                .await
                .unwrap()
        );

        let inserted = store
            .sync_wechat_dev_seed_allowlist(&["alice".into(), "carol".into()], later)
            .await
            .unwrap();
        assert_eq!(inserted, 0);
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert!(listed.iter().all(|row| row.actor != "bob"), "{listed:?}");
        let alice = listed.iter().find(|row| row.actor == "alice").unwrap();
        assert_eq!(alice.added_at, first);
        assert_eq!(alice.note.as_deref(), Some("dev-seed"));
        let carol = listed.iter().find(|row| row.actor == "carol").unwrap();
        assert_eq!(carol.note.as_deref(), Some("host"));
        assert_eq!(carol.added_at, first);
        assert!(listed.iter().any(|row| {
            row.source == "hyphae"
                && row.actor == "npub1"
                && row.note.as_deref() == Some("dev-seed")
        }));

        let inserted = store
            .sync_wechat_dev_seed_allowlist(&["dave".into()], later)
            .await
            .unwrap();
        assert_eq!(inserted, 1);
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert!(listed.iter().any(|row| {
            row.actor == "dave" && row.note.as_deref() == Some("dev-seed") && row.added_at == later
        }));
        assert!(listed.iter().all(|row| row.actor != "alice"));
        assert!(listed.iter().any(|row| row.actor == "carol"));

        let inserted = store
            .sync_wechat_dev_seed_allowlist(&[], later)
            .await
            .unwrap();
        assert_eq!(inserted, 0);
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert!(
            listed
                .iter()
                .all(|row| row.note.as_deref() != Some("dev-seed") || row.source != "wechat")
        );
        assert!(
            listed
                .iter()
                .any(|row| row.actor == "carol" && row.note.as_deref() == Some("host"))
        );
        assert!(listed.iter().any(|row| row.source == "hyphae"));

        let rejected = store
            .sync_wechat_dev_seed_allowlist(&["alice".into()], "not-a-stamp")
            .await
            .unwrap_err();
        assert!(
            matches!(rejected, StoreError::Conflict(ref detail) if detail.contains("added_at")),
            "{rejected:?}"
        );
        let listed = store.list_inbound_allowlist().await.unwrap();
        assert!(listed.iter().all(|row| row.actor != "alice"));
    }

    /// Two local identities in one group keep their own cursors.
    /// Mutation: drop `recipient_identity` from the primary key and the
    /// second `set` overwrites the first, so the `logical-a` assert goes red.
    #[tokio::test]
    async fn two_identities_in_one_group_keep_separate_cursors() {
        let store = Store::open_memory().await.unwrap();
        store
            .set_inbound_cursor("hyphae", "npub-a", "group-1", "logical-a")
            .await
            .unwrap();
        store
            .set_inbound_cursor("hyphae", "npub-b", "group-1", "logical-b")
            .await
            .unwrap();
        assert_eq!(
            store
                .inbound_cursor("hyphae", "npub-a", "group-1")
                .await
                .unwrap()
                .as_deref(),
            Some("logical-a")
        );
        assert_eq!(
            store
                .inbound_cursor("hyphae", "npub-b", "group-1")
                .await
                .unwrap()
                .as_deref(),
            Some("logical-b")
        );
        store
            .set_inbound_cursor("hyphae", "npub-a", "group-1", "logical-a2")
            .await
            .unwrap();
        assert_eq!(
            store
                .inbound_cursor("hyphae", "npub-b", "group-1")
                .await
                .unwrap()
                .as_deref(),
            Some("logical-b"),
            "advancing one identity must not move the other"
        );
        store
            .clear_inbound_cursor("hyphae", "npub-a", "group-1")
            .await
            .unwrap();
        assert!(
            store
                .inbound_cursor("hyphae", "npub-a", "group-1")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            store
                .inbound_cursor("hyphae", "npub-b", "group-1")
                .await
                .unwrap()
                .as_deref(),
            Some("logical-b"),
            "clearing one identity must leave the other cursor"
        );
        assert!(!store.inbound_source_allowlisted("hyphae").await.unwrap());
    }
}
