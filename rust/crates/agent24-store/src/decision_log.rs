//! D0-2 decision log, storage side. See `migrations/0014_decision_log.sql`
//! for the schema and its own reasoning, and `docs/agent/PLAN-DECIDE.md`
//! §2.1-§2.3 for the product requirements this implements.
//!
//! Deliberately decoupled from `agent24-decide`'s types: this crate (L2)
//! must not depend on `agent24-decide` (L3) — see
//! `docs/ARCHITECTURE-LAYERS.md` §1.1 ("依赖方向一律向下，没有向上依赖") and
//! `docs/decision.md` ADR-034. Every JSON-shaped column here is passed and
//! returned as an already-serialized string; this module never parses or
//! constructs `agent24-decide::log::LogEntry`/`OutcomeEntry` — the
//! `agent24d`-side implementation of `agent24_decide::log::DecisionLog` is
//! responsible for that translation, in both directions.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store};

/// One decision, as handed to [`Store::insert_decision_log`]. Every `*_json`
/// field is an already-serialized JSON string (object or array per the
/// migration's column comments) — this module does not validate their shape
/// beyond "is valid JSON" (enforced implicitly: SQLite stores it as TEXT
/// either way, and a malformed value would simply fail to round-trip through
/// a consumer that parses it — `export_decision_log`'s own tests cover the
/// happy path only, by design, since the writer is the one place a shape
/// contract belongs).
#[derive(Debug, Clone, PartialEq)]
pub struct NewDecisionLogEntry {
    pub decision_id: String,
    pub ts: String,
    pub schema_version: i64,
    pub point: String,
    pub input: Option<String>,
    pub context_json: Option<String>,
    pub question_json: String,
    pub layers_json: String,
    /// One of `execute` / `abstain` / `ask` / `escalate` — the migration's
    /// CHECK constraint is the actual enforcement; this type does not
    /// re-validate (a closed enum would require this crate to depend on
    /// `agent24-decide`'s `FinalAction`, which it must not — see module docs).
    pub final_action: String,
    pub hw_tier: Option<String>,
}

/// One stored decision row, as read back by [`Store::export_decision_log`].
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionLogRow {
    pub decision_id: String,
    pub ts: String,
    pub schema_version: i64,
    pub point: String,
    /// `None` if never recorded OR scrubbed by retention — the row itself
    /// does not distinguish those two cases; `scrubbed_at` does.
    pub input: Option<String>,
    pub context_json: Option<String>,
    pub question_json: String,
    pub layers_json: String,
    pub final_action: String,
    pub hw_tier: Option<String>,
    pub scrubbed_at: Option<String>,
}

/// One outcome, as handed to [`Store::append_decision_outcome`]. Inserting
/// one for a `decision_id` that was never recorded (or was already deleted)
/// fails with a foreign-key `StoreError::Sqlx` — the table's own
/// `REFERENCES decision_log(decision_id)` is the enforcement, not a
/// pre-check here, so there is no window between "checked it exists" and
/// "inserted" for a concurrent delete to land in.
#[derive(Debug, Clone, PartialEq)]
pub struct NewDecisionOutcome {
    pub decision_id: String,
    pub ts: String,
    /// One of the six signals in the migration's CHECK constraint.
    pub signal: String,
    pub label_json: String,
    /// One of `high` / `medium` / `low`.
    pub quality: String,
}

/// One stored outcome row, nested under its decision in
/// [`DecisionLogExportRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionOutcomeRow {
    pub ts: String,
    pub signal: String,
    pub label_json: String,
    pub quality: String,
}

/// One decision plus every outcome recorded against it, in append order —
/// exactly the unit `decide export --jsonl` prints as one line.
#[derive(Debug, Clone, PartialEq)]
pub struct DecisionLogExportRow {
    pub log: DecisionLogRow,
    pub outcomes: Vec<DecisionOutcomeRow>,
}

fn log_row_from(r: &SqliteRow) -> DecisionLogRow {
    DecisionLogRow {
        decision_id: r.get("decision_id"),
        ts: r.get("ts"),
        schema_version: r.get("schema_version"),
        point: r.get("point"),
        input: r.get("input"),
        context_json: r.get("context"),
        question_json: r.get("question"),
        layers_json: r.get("layers"),
        final_action: r.get("final_action"),
        hw_tier: r.get("hw_tier"),
        scrubbed_at: r.get("scrubbed_at"),
    }
}

fn outcome_row_from(r: &SqliteRow) -> DecisionOutcomeRow {
    DecisionOutcomeRow {
        ts: r.get("ts"),
        signal: r.get("signal"),
        label_json: r.get("label"),
        quality: r.get("quality"),
    }
}

impl Store {
    /// Inserts one decision. `decision_id` must be unique — a duplicate
    /// fails with a `StoreError::Sqlx` primary-key violation rather than
    /// silently overwriting a prior decision under the same id.
    ///
    /// # Errors
    /// Storage, including a primary-key or CHECK violation.
    pub async fn insert_decision_log(&self, e: &NewDecisionLogEntry) -> Result<()> {
        sqlx::query(
            "INSERT INTO decision_log \
                (decision_id, ts, schema_version, point, input, context, \
                 question, layers, final_action, hw_tier) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&e.decision_id)
        .bind(&e.ts)
        .bind(e.schema_version)
        .bind(&e.point)
        .bind(&e.input)
        .bind(&e.context_json)
        .bind(&e.question_json)
        .bind(&e.layers_json)
        .bind(&e.final_action)
        .bind(&e.hw_tier)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Appends one outcome. A decision can accumulate any number of these
    /// over time (§2.2) — never upserts, never replaces a prior outcome.
    ///
    /// # Errors
    /// Storage, including the foreign-key violation described on
    /// [`NewDecisionOutcome`] when `decision_id` does not exist.
    pub async fn append_decision_outcome(&self, o: &NewDecisionOutcome) -> Result<()> {
        sqlx::query(
            "INSERT INTO decision_outcome (decision_id, ts, signal, label, quality) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(&o.decision_id)
        .bind(&o.ts)
        .bind(&o.signal)
        .bind(&o.label_json)
        .bind(&o.quality)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Every decision matching the filters, each with its outcomes attached
    /// in append (`id ASC`) order. Two queries, not N+1: one for the matching
    /// `decision_log` rows, one `WHERE decision_id IN (...)` for every
    /// outcome of exactly those rows — fine at this table's expected scale
    /// (a single user's local decision history), and it keeps the method
    /// simple rather than hand-writing a `GROUP_CONCAT`/JSON aggregation in
    /// SQL that `sqlx`'s runtime-checked queries would not meaningfully
    /// validate anyway.
    ///
    /// Ordered oldest-first (`ts, decision_id`) — a stable, reproducible
    /// export order rather than SQLite's unspecified default.
    ///
    /// # Errors
    /// Storage.
    pub async fn export_decision_log(
        &self,
        point: Option<&str>,
        since: Option<&str>,
    ) -> Result<Vec<DecisionLogExportRow>> {
        let mut sql = String::from(
            "SELECT decision_id, ts, schema_version, point, input, context, \
                    question, layers, final_action, hw_tier, scrubbed_at \
             FROM decision_log WHERE 1 = 1",
        );
        if point.is_some() {
            sql.push_str(" AND point = ?");
        }
        if since.is_some() {
            sql.push_str(" AND ts >= ?");
        }
        sql.push_str(" ORDER BY ts ASC, decision_id ASC");
        let mut q = sqlx::query(&sql);
        if let Some(p) = point {
            q = q.bind(p);
        }
        if let Some(s) = since {
            q = q.bind(s);
        }
        let log_rows = q.fetch_all(self.pool()).await?;
        let logs: Vec<DecisionLogRow> = log_rows.iter().map(log_row_from).collect();

        if logs.is_empty() {
            return Ok(Vec::new());
        }

        let placeholders = vec!["?"; logs.len()].join(", ");
        let outcome_sql = format!(
            "SELECT decision_id, ts, signal, label, quality FROM decision_outcome \
             WHERE decision_id IN ({placeholders}) ORDER BY decision_id ASC, id ASC"
        );
        let mut oq = sqlx::query(&outcome_sql);
        for l in &logs {
            oq = oq.bind(&l.decision_id);
        }
        let outcome_rows = oq.fetch_all(self.pool()).await?;

        let mut by_decision: std::collections::HashMap<String, Vec<DecisionOutcomeRow>> =
            std::collections::HashMap::new();
        for r in &outcome_rows {
            let decision_id: String = r.get("decision_id");
            by_decision
                .entry(decision_id)
                .or_default()
                .push(outcome_row_from(r));
        }

        Ok(logs
            .into_iter()
            .map(|log| {
                let outcomes = by_decision.remove(&log.decision_id).unwrap_or_default();
                DecisionLogExportRow { log, outcomes }
            })
            .collect())
    }

    /// Deletes one decision by id, cascading to its outcomes (the
    /// migration's `ON DELETE CASCADE` + `Store::open`'s `foreign_keys(true)`
    /// — see `migrations/0014_decision_log.sql`). Returns whether a row was
    /// actually deleted (`false` for an unknown id, not an error).
    ///
    /// # Errors
    /// Storage.
    pub async fn delete_decision_log_by_id(&self, decision_id: &str) -> Result<bool> {
        let affected = sqlx::query("DELETE FROM decision_log WHERE decision_id = ?")
            .bind(decision_id)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(affected > 0)
    }

    /// Deletes every decision at `point`, cascading to their outcomes.
    /// Returns the number of decisions deleted.
    ///
    /// # Errors
    /// Storage.
    pub async fn delete_decision_log_by_point(&self, point: &str) -> Result<u64> {
        let affected = sqlx::query("DELETE FROM decision_log WHERE point = ?")
            .bind(point)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(affected)
    }

    /// Deletes every decision, cascading to every outcome. Returns the
    /// number of decisions deleted.
    ///
    /// # Errors
    /// Storage.
    pub async fn delete_all_decision_log(&self) -> Result<u64> {
        let affected = sqlx::query("DELETE FROM decision_log")
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(affected)
    }

    /// Retention sweep (§2.3): nulls `input`/`context` on every decision
    /// older than `cutoff_ts` that has not already been scrubbed, and stamps
    /// `scrubbed_at = recorded_at`. Idempotent — a row with `scrubbed_at` set
    /// is excluded, so running this twice with the same `cutoff_ts` only
    /// touches rows once. Never deletes a row and never touches `point` /
    /// `question` / `layers` / `final_action` / `hw_tier` (the "统计字段").
    ///
    /// `cutoff_ts`/`recorded_at` are caller-supplied (this crate never reads
    /// a clock, same convention as `model_call_timings.rs`'s `ts`) — nothing
    /// in D0 calls this yet; the default retention period is a decide-crate
    /// constant pending confirmation (see
    /// `agent24_decide::log::DEFAULT_DECISION_LOG_RETENTION_DAYS`'s doc
    /// comment).
    ///
    /// # Errors
    /// Storage.
    pub async fn scrub_expired_decision_log(
        &self,
        cutoff_ts: &str,
        recorded_at: &str,
    ) -> Result<u64> {
        let affected = sqlx::query(
            "UPDATE decision_log SET input = NULL, context = NULL, scrubbed_at = ? \
             WHERE ts < ? AND scrubbed_at IS NULL",
        )
        .bind(recorded_at)
        .bind(cutoff_ts)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(affected)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn entry(id: &str, ts: &str, point: &str) -> NewDecisionLogEntry {
        NewDecisionLogEntry {
            decision_id: id.to_owned(),
            ts: ts.to_owned(),
            schema_version: 1,
            point: point.to_owned(),
            input: Some("你记住，我对花生过敏。".to_owned()),
            context_json: Some(r#"{"turn":1}"#.to_owned()),
            question_json: r#"[{"kind":"noul","id":"is_remember","prompt":"是否要求记住？"}]"#
                .to_owned(),
            layers_json: r#"[{"backend":"rule","label":"true","p":0.99,"latency_ms":1}]"#
                .to_owned(),
            final_action: "execute".to_owned(),
            hw_tier: Some("t2".to_owned()),
        }
    }

    #[tokio::test]
    async fn write_then_export_round_trips_every_field() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();

        let exported = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(exported.len(), 1);
        let row = &exported[0].log;
        assert_eq!(row.decision_id, "d1");
        assert_eq!(row.schema_version, 1);
        assert_eq!(row.point, "retain.intent");
        assert_eq!(row.input.as_deref(), Some("你记住，我对花生过敏。"));
        assert_eq!(row.context_json.as_deref(), Some(r#"{"turn":1}"#));
        assert!(row.question_json.contains("is_remember"));
        assert!(row.layers_json.contains("\"backend\":\"rule\""));
        assert_eq!(row.final_action, "execute");
        assert_eq!(row.hw_tier.as_deref(), Some("t2"));
        assert!(row.scrubbed_at.is_none());
        assert!(exported[0].outcomes.is_empty());
    }

    #[tokio::test]
    async fn a_decision_can_accumulate_several_outcomes_in_append_order() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();

        store
            .append_decision_outcome(&NewDecisionOutcome {
                decision_id: "d1".to_owned(),
                ts: "2026-10-07T00:01:00Z".to_owned(),
                signal: "clarify_answer".to_owned(),
                label_json: r#"{"answer":true}"#.to_owned(),
                quality: "high".to_owned(),
            })
            .await
            .unwrap();
        store
            .append_decision_outcome(&NewDecisionOutcome {
                decision_id: "d1".to_owned(),
                ts: "2026-10-08T00:00:00Z".to_owned(),
                signal: "user_retract".to_owned(),
                label_json: r#"{"retracted":true}"#.to_owned(),
                quality: "high".to_owned(),
            })
            .await
            .unwrap();

        let exported = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(exported.len(), 1);
        let outcomes = &exported[0].outcomes;
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].signal, "clarify_answer");
        assert_eq!(outcomes[1].signal, "user_retract");
    }

    #[tokio::test]
    async fn inserting_a_duplicate_decision_id_fails_instead_of_overwriting() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        let second = store
            .insert_decision_log(&entry("d1", "2026-10-08T00:00:00Z", "recall.gate"))
            .await;
        assert!(
            second.is_err(),
            "a duplicate decision_id must not silently overwrite"
        );

        let exported = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(exported.len(), 1);
        assert_eq!(
            exported[0].log.point, "retain.intent",
            "the original row must survive a rejected duplicate insert"
        );
    }

    #[tokio::test]
    async fn appending_an_outcome_for_an_unknown_decision_fails_the_foreign_key() {
        let store = Store::open_memory().await.unwrap();
        let result = store
            .append_decision_outcome(&NewDecisionOutcome {
                decision_id: "does-not-exist".to_owned(),
                ts: "2026-10-07T00:00:00Z".to_owned(),
                signal: "clarify_answer".to_owned(),
                label_json: "{}".to_owned(),
                quality: "high".to_owned(),
            })
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn export_filters_by_point_and_since() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-01T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("d2", "2026-10-05T00:00:00Z", "recall.gate"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("d3", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();

        let by_point = store
            .export_decision_log(Some("retain.intent"), None)
            .await
            .unwrap();
        assert_eq!(by_point.len(), 2);
        assert!(by_point.iter().all(|r| r.log.point == "retain.intent"));

        let by_since = store
            .export_decision_log(None, Some("2026-10-05T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(by_since.len(), 2);
        assert_eq!(by_since[0].log.decision_id, "d2", "oldest-matching first");

        let both = store
            .export_decision_log(Some("retain.intent"), Some("2026-10-05T00:00:00Z"))
            .await
            .unwrap();
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].log.decision_id, "d3");
    }

    #[tokio::test]
    async fn delete_by_id_removes_the_decision_and_its_outcomes_but_nothing_else() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("d2", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .append_decision_outcome(&NewDecisionOutcome {
                decision_id: "d1".to_owned(),
                ts: "2026-10-07T00:01:00Z".to_owned(),
                signal: "clarify_answer".to_owned(),
                label_json: "{}".to_owned(),
                quality: "high".to_owned(),
            })
            .await
            .unwrap();

        let deleted = store.delete_decision_log_by_id("d1").await.unwrap();
        assert!(deleted);
        let again = store.delete_decision_log_by_id("d1").await.unwrap();
        assert!(
            !again,
            "deleting an already-gone id reports false, not an error"
        );

        let remaining = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].log.decision_id, "d2");

        let orphaned_outcomes: i64 = sqlx::query("SELECT COUNT(*) AS c FROM decision_outcome")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap()
            .get("c");
        assert_eq!(orphaned_outcomes, 0, "d1's outcome must cascade-delete too");
    }

    #[tokio::test]
    async fn delete_by_point_removes_only_that_point() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("d2", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("d3", "2026-10-07T00:00:00Z", "recall.gate"))
            .await
            .unwrap();

        let deleted = store
            .delete_decision_log_by_point("retain.intent")
            .await
            .unwrap();
        assert_eq!(deleted, 2);

        let remaining = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].log.point, "recall.gate");
    }

    #[tokio::test]
    async fn delete_all_removes_everything_including_outcomes() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("d2", "2026-10-07T00:00:00Z", "recall.gate"))
            .await
            .unwrap();
        store
            .append_decision_outcome(&NewDecisionOutcome {
                decision_id: "d1".to_owned(),
                ts: "2026-10-07T00:01:00Z".to_owned(),
                signal: "clarify_answer".to_owned(),
                label_json: "{}".to_owned(),
                quality: "high".to_owned(),
            })
            .await
            .unwrap();

        let deleted = store.delete_all_decision_log().await.unwrap();
        assert_eq!(deleted, 2);
        assert!(
            store
                .export_decision_log(None, None)
                .await
                .unwrap()
                .is_empty()
        );
        let outcome_count: i64 = sqlx::query("SELECT COUNT(*) AS c FROM decision_outcome")
            .fetch_one(crate::test_hooks::pool(&store))
            .await
            .unwrap()
            .get("c");
        assert_eq!(outcome_count, 0);
    }

    #[tokio::test]
    async fn retention_scrub_removes_input_and_context_but_keeps_statistical_fields() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("old", "2026-01-01T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store
            .insert_decision_log(&entry("fresh", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();

        let scrubbed = store
            .scrub_expired_decision_log("2026-06-01T00:00:00Z", "2026-10-07T12:00:00Z")
            .await
            .unwrap();
        assert_eq!(scrubbed, 1);

        let exported = store.export_decision_log(None, None).await.unwrap();
        let old = exported
            .iter()
            .find(|r| r.log.decision_id == "old")
            .unwrap();
        assert!(old.log.input.is_none(), "raw input must be gone");
        assert!(old.log.context_json.is_none(), "raw context must be gone");
        assert_eq!(old.log.scrubbed_at.as_deref(), Some("2026-10-07T12:00:00Z"));
        // Statistical fields survive untouched.
        assert_eq!(old.log.point, "retain.intent");
        assert_eq!(old.log.final_action, "execute");
        assert!(old.log.question_json.contains("is_remember"));
        assert!(old.log.layers_json.contains("rule"));
        assert_eq!(old.log.hw_tier.as_deref(), Some("t2"));

        let fresh = exported
            .iter()
            .find(|r| r.log.decision_id == "fresh")
            .unwrap();
        assert!(fresh.log.input.is_some(), "fresh row must not be touched");
        assert!(fresh.log.scrubbed_at.is_none());
    }

    #[tokio::test]
    async fn retention_scrub_is_idempotent_and_does_not_re_stamp_already_scrubbed_rows() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("old", "2026-01-01T00:00:00Z", "retain.intent"))
            .await
            .unwrap();

        let first = store
            .scrub_expired_decision_log("2026-06-01T00:00:00Z", "2026-10-07T12:00:00Z")
            .await
            .unwrap();
        assert_eq!(first, 1);
        let second = store
            .scrub_expired_decision_log("2026-06-01T00:00:00Z", "2099-01-01T00:00:00Z")
            .await
            .unwrap();
        assert_eq!(
            second, 0,
            "an already-scrubbed row must not be touched again"
        );

        let exported = store.export_decision_log(None, None).await.unwrap();
        assert_eq!(
            exported[0].log.scrubbed_at.as_deref(),
            Some("2026-10-07T12:00:00Z"),
            "the second sweep's timestamp must not overwrite the first"
        );
    }

    #[tokio::test]
    async fn export_omits_deleted_decisions() {
        let store = Store::open_memory().await.unwrap();
        store
            .insert_decision_log(&entry("d1", "2026-10-07T00:00:00Z", "retain.intent"))
            .await
            .unwrap();
        store.delete_decision_log_by_id("d1").await.unwrap();
        assert!(
            store
                .export_decision_log(None, None)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
