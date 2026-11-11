//! Inbound event records (X11-2a, INBOUND-ADMISSION-PLAN §3).
//!
//! The store writes the row it is given. It does not decide admission.
//! [`Store::insert_inbound_event`] reports three outcomes: the row was
//! inserted, the same id was already stored (the existing row is left as it
//! was), or the in-flight unique index rejected another explicit row for
//! that sender. The caller turns the third outcome into `ignore` /
//! `rate_limited` and inserts that row under the original id.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::sqlite::SqliteRow;

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
    /// A missing id, the wrong current state, or any other target returns
    /// `false` and writes nothing.
    ///
    /// # Errors
    /// Storage.
    pub async fn mark_inbound_run_state(&self, id: &str, state: &str) -> Result<bool> {
        let affected = sqlx::query(
            "UPDATE inbound_events SET run_state = ? \
             WHERE id = ? AND ( \
                 (run_state = 'starting' AND ? IN ('started', 'interrupted', 'start_failed')) \
                 OR (run_state = 'started' AND ? = 'done') \
             )",
        )
        .bind(state)
        .bind(id)
        .bind(state)
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
    /// An empty slice clears the table. `added_at` is the caller's
    /// [`format_inbound_timestamp`], the same stamp on every inserted row.
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
        sqlx::query("DELETE FROM inbound_allowlist")
            .execute(&mut *tx)
            .await?;
        for entry in entries {
            sqlx::query(
                "INSERT INTO inbound_allowlist (source, actor, added_at, note) \
                 VALUES (?, ?, ?, ?)",
            )
            .bind(&entry.source)
            .bind(&entry.actor)
            .bind(added_at)
            .bind(&entry.note)
            .execute(&mut *tx)
            .await?;
        }
        let view = admission_view(&mut tx).await?;
        tx.commit().await?;
        Ok(view)
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

/// One allowlist row as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboundAllowEntry {
    pub source: String,
    pub actor: String,
    pub added_at: String,
    pub note: Option<String>,
}

/// A row the allowlist replace writes. `added_at` is supplied once for the batch.
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
        let preview = page[0].text_preview.as_deref().unwrap();
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
}
