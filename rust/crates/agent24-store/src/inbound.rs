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
/// what [`Store::scrub_expired_inbound_text`] compares. Insert and scrub
/// reject any other spelling.
pub fn format_inbound_timestamp(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn require_inbound_timestamp(field: &str, value: &str) -> Result<()> {
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
/// only [`Store::scrub_expired_inbound_text`] sets it.
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
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboundRunLink {
    Linked,
    AlreadyLinked,
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
    pub async fn scrub_expired_inbound_text(&self, cutoff: &str, now: &str) -> Result<u64> {
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

    /// `inbound_allowlist` rows as `(source, actor)`.
    ///
    /// # Errors
    /// Storage.
    pub async fn list_inbound_allowlist(&self) -> Result<Vec<(String, String)>> {
        let rows =
            sqlx::query("SELECT source, actor FROM inbound_allowlist ORDER BY source, actor")
                .fetch_all(self.pool())
                .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get("source"), row.get("actor")))
            .collect())
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

    /// Insert `session`, `run`, and `inbound_runs` in one `BEGIN IMMEDIATE`.
    ///
    /// [`crate::Store::insert_run`] commits on its own, so it cannot share a
    /// transaction with the link. A clash on `inbound_runs.event_id` rolls the
    /// session and the run back and returns [`InboundRunLink::AlreadyLinked`].
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
}

fn is_inbound_run_event_conflict(err: &dyn sqlx::error::DatabaseError) -> bool {
    if err.kind() != sqlx::error::ErrorKind::UniqueViolation
        || err.code().as_deref() != Some(SQLITE_CONSTRAINT_UNIQUE)
    {
        return false;
    }
    err.message().contains("inbound_runs.event_id")
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
        assert_eq!(store.latest_applied_migration().await.unwrap(), 34);
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
        for event in rows
            .iter()
            .chain([&protocol, &filtered, &other_actor, &old])
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
}
