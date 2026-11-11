//! Inbound text expiry that reaches the copies (X11-2d, INBOUND-ADMISSION-PLAN
//! §3.2 and §7-3).
//!
//! Scope, and what each copy gets. Nothing here deletes a row of `runs`,
//! `run_messages`, `tool_calls`, `approvals` or `sessions`: every other table
//! points at them (foreign keys, `run_message_origins` digests, the hash-chained
//! audit log), and a resumed or audited run must still find its rows. The text
//! inside is replaced by [`INBOUND_SCRUB_PLACEHOLDER`] instead, so shapes and
//! JSON stay valid.
//!
//! | copy                                  | handling                         |
//! |---------------------------------------|----------------------------------|
//! | `inbound_events.text` / `meta`        | NULL, stamp `text_scrubbed_at`   |
//! | `runs.input` (`$.prompt`)             | placeholder, other fields kept   |
//! | `runs.output` (`$.text`)              | placeholder when not NULL        |
//! | `runs.error`                          | `{code kept, message placeholder}` |
//! | `run_messages.content` / `tool_calls` | placeholder / `'[]'`             |
//! | `tool_calls.input` / `output_summary` | `'{}'` / placeholder             |
//! | `approvals.summary` / `payload`       | placeholder / `'{}'`             |
//! | `sessions.title`                      | placeholder, exclusive sessions  |
//!
//! Which runs. Only runs linked through `inbound_runs` to an expired event.
//! A run that no `inbound_runs` row names is never touched, whatever its
//! content. An event with a run that is still live (`queued`, `running`,
//! `awaiting_approval`) is skipped and stays unscrubbed, so recovery still has
//! the prompt; the next pass picks it up once the run is terminal.
//!
//! Session memory lives in `memory.db`, another file, so this transaction
//! cannot cover it. [`Store::inbound_scrub_sessions`] names the sessions whose
//! memory the caller must purge BEFORE calling [`Store::scrub_expired_inbound`]:
//! if the process dies in between, the events are still unstamped and the next
//! pass repeats the (idempotent) purge. Inbound runs get one new session per
//! message (§7-1), so a session is purged whole, but only when every run in it
//! is an inbound run of an expired event; a session shared with any other run
//! is never listed.
//!
//! The stamp must not outrun the purge. A session is listed only while one of
//! its events is unstamped, so an event stamped before its session's memory
//! was purged leaves that text in `memory.db` for good. Two ways it could
//! happen, both closed here: the session was not listed because a run in it
//! was still live and ended before the stamp; or no memory partition was
//! available. [`Store::scrub_expired_inbound`] therefore takes the sessions
//! whose memory this pass handled and, inside its own transaction, defers any
//! event with a run in a session that is exclusive (purgeable) right now but
//! not in that set. Sessions that are shared, or whose memory is not a
//! concern (`None`, ephemeral daemons), do not hold an event back. A deferred
//! event is picked up by a later pass; when the cause is a missing memory
//! partition that is the next daemon start with memory initialised, not the
//! next 6-hour tick.

use sqlx::{Row, Sqlite, Transaction};

use crate::inbound::require_inbound_timestamp;
use crate::{Result, Store};

/// What replaces scrubbed text inside rows that must keep existing.
pub const INBOUND_SCRUB_PLACEHOLDER: &str = "[expired inbound text]";

/// Statuses after which nothing reads a run's text again for recovery.
const TERMINAL: &str = "('completed', 'failed', 'cancelled')";

/// Counts from one [`Store::scrub_expired_inbound`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InboundScrubReport {
    /// `inbound_events` rows whose text and meta were cleared.
    pub events: u64,
    pub runs: u64,
    pub run_messages: u64,
    pub tool_calls: u64,
    pub approvals: u64,
    pub sessions: u64,
    /// Expired, otherwise-eligible events left unstamped because a run of
    /// theirs sits in an exclusive session whose memory this pass did not
    /// handle.
    pub deferred: u64,
}

impl InboundScrubReport {
    /// Whether the call changed nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Events a pass may look at: older than `?1`, unstamped, not `starting`
/// (`run_state = 'starting'` means 2c may yet link a run), every linked run
/// terminal.
const BASE_EVENT_CONDITIONS: &str = "e.received_at < ?1 AND e.text_scrubbed_at IS NULL \
     AND (e.run_state IS NULL OR e.run_state <> 'starting') \
     AND NOT EXISTS ( \
         SELECT 1 FROM inbound_runs ir JOIN runs r ON r.id = ir.run_id \
         WHERE ir.event_id = e.id AND r.status NOT IN ('completed', 'failed', 'cancelled'))";

/// True when the event has a run in a session that is exclusive now (`?1`) and
/// whose memory is not in the handled set `?3` (a JSON array of session ids).
fn memory_unhandled_sql() -> String {
    format!(
        "EXISTS (SELECT 1 FROM inbound_runs ir JOIN runs r ON r.id = ir.run_id \
                 WHERE ir.event_id = e.id AND r.session_id IS NOT NULL \
                   AND r.session_id NOT IN (SELECT value FROM json_each(?3)) \
                   AND r.session_id IN ({}))",
        exclusive_sessions_sql(false)
    )
}

fn eligible_events_sql(constrain_memory: bool) -> String {
    let defer = if constrain_memory {
        format!("AND NOT {} ", memory_unhandled_sql())
    } else {
        String::new()
    };
    format!(
        "SELECT e.id FROM inbound_events e WHERE {BASE_EVENT_CONDITIONS} {defer}\
         ORDER BY e.received_at, e.id LIMIT ?2"
    )
}

/// Sessions in which every run is an inbound run of an event older than `?1`,
/// all terminal, and (when `pending_only`) at least one such event is still
/// unscrubbed.
fn exclusive_sessions_sql(pending_only: bool) -> String {
    let pending = if pending_only {
        "AND EXISTS (SELECT 1 FROM runs r JOIN inbound_runs ir ON ir.run_id = r.id \
                     JOIN inbound_events e ON e.id = ir.event_id \
                     WHERE r.session_id = s.id AND e.text_scrubbed_at IS NULL)"
    } else {
        ""
    };
    format!(
        "SELECT s.id FROM sessions s \
         WHERE EXISTS (SELECT 1 FROM runs r WHERE r.session_id = s.id) \
           AND NOT EXISTS ( \
               SELECT 1 FROM runs r \
               LEFT JOIN inbound_runs ir ON ir.run_id = r.id \
               LEFT JOIN inbound_events e ON e.id = ir.event_id \
               WHERE r.session_id = s.id \
                 AND (e.id IS NULL OR e.received_at >= ?1 OR r.status NOT IN {TERMINAL})) \
           {pending}"
    )
}

async fn exec(tx: &mut Transaction<'_, Sqlite>, sql: &str, ids_json: &str) -> Result<u64> {
    Ok(sqlx::query(sql)
        .bind(INBOUND_SCRUB_PLACEHOLDER)
        .bind(ids_json)
        .execute(&mut **tx)
        .await?
        .rows_affected())
}

impl Store {
    /// Sessions whose memory the caller must purge for the next scrub pass.
    ///
    /// See the module docs for the exclusivity rule. Read only; repeated calls
    /// after the matching scrub return nothing.
    ///
    /// # Errors
    /// Storage, or a `cutoff` that is not `format_inbound_timestamp` output.
    pub async fn inbound_scrub_sessions(&self, cutoff: &str) -> Result<Vec<String>> {
        require_inbound_timestamp("cutoff", cutoff)?;
        let sql = format!("{} ORDER BY s.id", exclusive_sessions_sql(true));
        Ok(sqlx::query_scalar(&sql)
            .bind(cutoff)
            .fetch_all(self.pool())
            .await?)
    }

    /// Whether `session` is still one that [`Self::inbound_scrub_sessions`]
    /// would list. The caller asks right before purging its memory, which
    /// narrows (it cannot close: the memory is another database) the window
    /// in which a run joined the session after the list was read.
    ///
    /// # Errors
    /// Storage, or a `cutoff` that is not `format_inbound_timestamp` output.
    pub async fn inbound_scrub_session_pending(&self, cutoff: &str, session: &str) -> Result<bool> {
        require_inbound_timestamp("cutoff", cutoff)?;
        let sql = format!("{} AND s.id = ?2", exclusive_sessions_sql(true));
        Ok(sqlx::query_scalar::<_, String>(&sql)
            .bind(cutoff)
            .bind(session)
            .fetch_optional(self.pool())
            .await?
            .is_some())
    }

    /// Clear up to `batch` expired inbound events and their copies in one
    /// transaction. Rows received before `cutoff` qualify; exactly at `cutoff`
    /// is kept.
    ///
    /// `memory_handled` is `Some(sessions)` when session memory exists: the
    /// sessions whose memory this pass has purged (or found nothing to purge
    /// in). An event with a run in a session that is exclusive right now and
    /// not in that set is left unstamped and counted in `deferred` (see the
    /// module docs). `None` skips that check: there is no memory to leave text
    /// in. A second call over the same data returns an empty report.
    ///
    /// # Errors
    /// Storage, or a `cutoff` / `now` that is not `format_inbound_timestamp`
    /// output. Any failure rolls the whole batch back.
    pub async fn scrub_expired_inbound(
        &self,
        cutoff: &str,
        now: &str,
        batch: u32,
        memory_handled: Option<&[String]>,
    ) -> Result<InboundScrubReport> {
        require_inbound_timestamp("cutoff", cutoff)?;
        require_inbound_timestamp("now", now)?;
        let handled_json = memory_handled
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| crate::StoreError::Conflict(e.to_string()))?;
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let eligible_sql = eligible_events_sql(handled_json.is_some());
        let mut eligible = sqlx::query(&eligible_sql)
            .bind(cutoff)
            .bind(i64::from(batch));
        if let Some(json) = &handled_json {
            eligible = eligible.bind(json);
        }
        let ids: Vec<String> = eligible
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(|row| row.get("id"))
            .collect();
        let deferred = if let Some(json) = &handled_json {
            sqlx::query_scalar(&format!(
                "SELECT COUNT(*) FROM inbound_events e WHERE {BASE_EVENT_CONDITIONS} AND {}",
                memory_unhandled_sql()
            ))
            .bind(cutoff)
            .bind(0_i64)
            .bind(json)
            .fetch_one(&mut *tx)
            .await
            .map(|n: i64| n as u64)?
        } else {
            0
        };
        if ids.is_empty() {
            return Ok(InboundScrubReport {
                deferred,
                ..InboundScrubReport::default()
            });
        }
        let ids_json =
            serde_json::to_string(&ids).map_err(|e| crate::StoreError::Conflict(e.to_string()))?;
        let runs_of = "SELECT ir.run_id FROM inbound_runs ir \
                       WHERE ir.event_id IN (SELECT value FROM json_each(?2))";
        let mut report = InboundScrubReport {
            deferred,
            ..InboundScrubReport::default()
        };

        // Sessions first: the exclusivity test reads event stamps and run
        // links, which the statements below keep intact, but it must run
        // against the unscrubbed state of this batch.
        let sessions_sql = format!(
            "UPDATE sessions SET title = ?1 WHERE title <> ?1 AND id IN ({}) \
             AND id IN (SELECT r.session_id FROM runs r WHERE r.id IN ({runs_of}))",
            exclusive_sessions_sql(false).replace("?1", "?3")
        );
        report.sessions = sqlx::query(&sessions_sql)
            .bind(INBOUND_SCRUB_PLACEHOLDER)
            .bind(&ids_json)
            .bind(cutoff)
            .execute(&mut *tx)
            .await?
            .rows_affected();

        report.runs = exec(
            &mut tx,
            &format!(
                "UPDATE runs SET input = json_set(input, '$.prompt', ?1), \
                 output = CASE WHEN output IS NULL THEN NULL \
                               WHEN json_valid(output) THEN json_set(output, '$.text', ?1) \
                               ELSE ?1 END, \
                 error = CASE WHEN error IS NULL THEN NULL \
                              WHEN json_valid(error) THEN json_object( \
                                  'code', json_extract(error, '$.code'), 'message', ?1) \
                              ELSE ?1 END \
                 WHERE id IN ({runs_of})"
            ),
            &ids_json,
        )
        .await?;
        report.run_messages = exec(
            &mut tx,
            &format!(
                "UPDATE run_messages SET \
                 content = CASE WHEN content IS NULL THEN NULL ELSE ?1 END, \
                 tool_calls = '[]' \
                 WHERE run_id IN ({runs_of})"
            ),
            &ids_json,
        )
        .await?;
        report.tool_calls = exec(
            &mut tx,
            &format!(
                "UPDATE tool_calls SET input = '{{}}', \
                 output_summary = CASE WHEN output_summary IS NULL THEN NULL ELSE ?1 END \
                 WHERE run_id IN ({runs_of})"
            ),
            &ids_json,
        )
        .await?;
        report.approvals = exec(
            &mut tx,
            &format!(
                "UPDATE approvals SET summary = ?1, payload = '{{}}' \
                 WHERE run_id IN ({runs_of})"
            ),
            &ids_json,
        )
        .await?;

        report.events = sqlx::query(
            "UPDATE inbound_events SET text = NULL, meta = NULL, text_scrubbed_at = ?1 \
             WHERE id IN (SELECT value FROM json_each(?2))",
        )
        .bind(now)
        .bind(&ids_json)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        tx.commit().await?;
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::NewInboundEvent;

    const CUTOFF: &str = "2026-05-15T12:00:00Z";
    const NOW: &str = "2026-11-11T12:00:00Z";

    async fn event_with_run(store: &Store, tag: &str, received_at: &str, status: &str) {
        let pool = crate::test_hooks::pool(store);
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES (?, ?, '{\"prompt\":\"p\"}', '{}', '2026-01-01T00:00:00Z')",
        )
        .bind(format!("run-{tag}"))
        .bind(status)
        .execute(pool)
        .await
        .unwrap();
        let _ = store
            .insert_inbound_event(&NewInboundEvent {
                id: format!("ev-{tag}"),
                source: "hyphae".into(),
                category: "direct".into(),
                recipient_identity: None,
                thread_key: None,
                actor: Some(format!("a-{tag}")),
                human: true,
                addressed: "direct".into(),
                at: received_at.into(),
                received_at: received_at.into(),
                text: Some("t".into()),
                meta: Some("m".into()),
                disposition: "explicit".into(),
                disposition_reason: None,
                policy_version: None,
                run_state: Some("done".into()),
            })
            .await
            .unwrap();
        store
            .insert_inbound_run(&format!("run-{tag}"), &format!("ev-{tag}"))
            .await
            .unwrap();
    }

    async fn prompt_of(store: &Store, tag: &str) -> String {
        sqlx::query_scalar("SELECT json_extract(input, '$.prompt') FROM runs WHERE id = ?")
            .bind(format!("run-{tag}"))
            .fetch_one(crate::test_hooks::pool(store))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_row_exactly_at_the_cutoff_is_kept_and_older_is_cleared() {
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "edge", CUTOFF, "completed").await;
        event_with_run(&store, "older", "2026-05-15T11:59:59Z", "completed").await;
        let report = store
            .scrub_expired_inbound(CUTOFF, NOW, 10, None)
            .await
            .unwrap();
        assert_eq!(report.events, 1);
        assert_eq!(prompt_of(&store, "edge").await, "p");
        assert_eq!(prompt_of(&store, "older").await, INBOUND_SCRUB_PLACEHOLDER);
        assert!(
            store
                .scrub_expired_inbound(CUTOFF, NOW, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_batch_limit_leaves_the_rest_for_the_next_call() {
        let store = Store::open_memory().await.unwrap();
        for tag in ["a", "b", "c"] {
            event_with_run(&store, tag, "2026-01-01T00:00:00Z", "failed").await;
        }
        assert_eq!(
            store
                .scrub_expired_inbound(CUTOFF, NOW, 2, None)
                .await
                .unwrap()
                .events,
            2
        );
        assert_eq!(
            store
                .scrub_expired_inbound(CUTOFF, NOW, 2, None)
                .await
                .unwrap()
                .events,
            1
        );
        assert!(
            store
                .scrub_expired_inbound(CUTOFF, NOW, 2, None)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn a_run_not_linked_to_an_inbound_event_is_never_touched() {
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "linked", "2026-01-01T00:00:00Z", "completed").await;
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES ('run-plain', 'completed', '{\"prompt\":\"p\"}', '{}', '2025-01-01T00:00:00Z')",
        )
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        store
            .scrub_expired_inbound(CUTOFF, NOW, 10, None)
            .await
            .unwrap();
        assert_eq!(prompt_of(&store, "plain").await, "p");
        assert_eq!(prompt_of(&store, "linked").await, INBOUND_SCRUB_PLACEHOLDER);
    }

    #[tokio::test]
    async fn a_malformed_timestamp_is_refused_before_any_write() {
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "x", "2026-01-01T00:00:00Z", "completed").await;
        assert!(
            store
                .scrub_expired_inbound("2026-05-15", NOW, 10, None)
                .await
                .is_err()
        );
        assert!(
            store
                .scrub_expired_inbound(CUTOFF, "now", 10, None)
                .await
                .is_err()
        );
        assert!(store.inbound_scrub_sessions("yesterday").await.is_err());
        assert_eq!(prompt_of(&store, "x").await, "p");
    }

    #[tokio::test]
    async fn an_event_still_starting_is_not_scrubbed() {
        // 2c may still link a run to it; scrubbing now would stamp it before
        // that run exists.
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "s", "2026-01-01T00:00:00Z", "completed").await;
        sqlx::query("UPDATE inbound_events SET run_state = 'starting' WHERE id = 'ev-s'")
            .execute(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert!(
            store
                .scrub_expired_inbound(CUTOFF, NOW, 10, None)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(prompt_of(&store, "s").await, "p");
    }

    #[tokio::test]
    async fn deferring_session_runs_leaves_them_unstamped_and_counted() {
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "sess", "2026-01-01T00:00:00Z", "completed").await;
        event_with_run(&store, "bare", "2026-01-01T00:00:00Z", "completed").await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at) \
             VALUES ('s1', 't', 'hyphae', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE runs SET session_id = 's1' WHERE id = 'run-sess'")
            .execute(pool)
            .await
            .unwrap();
        let report = store
            .scrub_expired_inbound(CUTOFF, NOW, 10, Some(&[]))
            .await
            .unwrap();
        assert_eq!((report.events, report.deferred), (1, 1));
        assert_eq!(prompt_of(&store, "sess").await, "p");
        assert_eq!(prompt_of(&store, "bare").await, INBOUND_SCRUB_PLACEHOLDER);
        // Once memory is available the same event goes.
        let report = store
            .scrub_expired_inbound(CUTOFF, NOW, 10, None)
            .await
            .unwrap();
        assert_eq!((report.events, report.deferred), (1, 0));
        assert_eq!(prompt_of(&store, "sess").await, INBOUND_SCRUB_PLACEHOLDER);
    }

    #[tokio::test]
    async fn error_keeps_its_code_and_drops_everything_else() {
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "e", "2026-01-01T00:00:00Z", "failed").await;
        sqlx::query(
            "UPDATE runs SET error = '{\"code\":\"internal\",\"message\":\"leak\",\"hint\":\"leak2\",\"details\":{\"x\":\"leak3\"}}' \
             WHERE id = 'run-e'",
        )
        .execute(crate::test_hooks::pool(&store))
        .await
        .unwrap();
        store
            .scrub_expired_inbound(CUTOFF, NOW, 10, None)
            .await
            .unwrap();
        let error: String = sqlx::query_scalar("SELECT error FROM runs WHERE id = 'run-e'")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(
            error,
            format!("{{\"code\":\"internal\",\"message\":\"{INBOUND_SCRUB_PLACEHOLDER}\"}}")
        );
    }

    #[tokio::test]
    async fn a_run_that_ends_after_the_session_list_does_not_get_its_event_stamped_early() {
        // The race: S holds a live run, so it is not listed; the run ends;
        // the scrub then runs. Without the handled-set check the event is
        // stamped, S is never listed again, and its memory text stays.
        let store = Store::open_memory().await.unwrap();
        event_with_run(&store, "r", "2026-01-01T00:00:00Z", "running").await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query(
            "INSERT INTO sessions (id, title, channel, created_at, updated_at) \
             VALUES ('s-r', 't', 'hyphae', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
        )
        .execute(pool)
        .await
        .unwrap();
        sqlx::query("UPDATE runs SET session_id = 's-r' WHERE id = 'run-r'")
            .execute(pool)
            .await
            .unwrap();
        let listed = store.inbound_scrub_sessions(CUTOFF).await.unwrap();
        assert!(listed.is_empty(), "a live run keeps the session out");

        sqlx::query("UPDATE runs SET status = 'completed' WHERE id = 'run-r'")
            .execute(pool)
            .await
            .unwrap();
        let report = store
            .scrub_expired_inbound(CUTOFF, NOW, 10, Some(&listed))
            .await
            .unwrap();
        assert_eq!((report.events, report.deferred), (0, 1));
        assert_eq!(prompt_of(&store, "r").await, "p");

        // The next pass lists it, handles it, and only then is it stamped.
        let listed = store.inbound_scrub_sessions(CUTOFF).await.unwrap();
        assert_eq!(listed, vec!["s-r".to_owned()]);
        let report = store
            .scrub_expired_inbound(CUTOFF, NOW, 10, Some(&listed))
            .await
            .unwrap();
        assert_eq!((report.events, report.deferred), (1, 0));
    }

    #[tokio::test]
    async fn deferred_counts_only_what_the_scrub_would_otherwise_clear() {
        let store = Store::open_memory().await.unwrap();
        // A live run and a `starting` event are not "deferred": they are not
        // eligible for any reason.
        event_with_run(&store, "live", "2026-01-01T00:00:00Z", "running").await;
        event_with_run(&store, "start", "2026-01-01T00:00:00Z", "completed").await;
        let pool = crate::test_hooks::pool(&store);
        sqlx::query("UPDATE inbound_events SET run_state = 'starting' WHERE id = 'ev-start'")
            .execute(pool)
            .await
            .unwrap();
        for (run, session) in [("run-live", "s-live"), ("run-start", "s-start")] {
            sqlx::query(
                "INSERT INTO sessions (id, title, channel, created_at, updated_at) \
                 VALUES (?, 't', 'hyphae', '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z')",
            )
            .bind(session)
            .execute(pool)
            .await
            .unwrap();
            sqlx::query("UPDATE runs SET session_id = ? WHERE id = ?")
                .bind(session)
                .bind(run)
                .execute(pool)
                .await
                .unwrap();
        }
        let report = store
            .scrub_expired_inbound(CUTOFF, NOW, 10, Some(&[]))
            .await
            .unwrap();
        assert!(report.is_empty(), "{report:?}");
    }
}
