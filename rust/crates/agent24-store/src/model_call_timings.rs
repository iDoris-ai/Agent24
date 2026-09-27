//! ME4-desktop-model-ui — per-call timing ledger, storage side. See
//! `migrations/0013_model_call_timings.sql` for the schema and its own
//! reasoning (a raw ledger, not an aggregate like `module_model_usage.rs`;
//! never stores content). Written by `agent24d/src/timing_recorder.rs`, read
//! by `agent24d/src/routes.rs`'s `GET /api/v1/timings`/`.../summary`.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store};

/// One call's timing facts, as handed to [`Store::record_call_timing`].
/// `ts` is caller-supplied (this crate never reads a clock, same rule as
/// `module_model_usage.rs`'s `day`) — `timing_recorder.rs` is the one place
/// that does, via `agent24_core::util::now_iso8601`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewCallTiming {
    pub ts: String,
    pub source: String,
    pub model_id: Option<String>,
    pub tier: Option<String>,
    pub served_by: Option<String>,
    pub ok: bool,
    pub error_kind: Option<String>,
    pub step: Option<String>,
    /// AgentEar's own per-turn correlation ids (design ask, AgentEar PR #102
    /// / agent-speaker v0.26.0) — opaque identifiers, never content. `None`
    /// for every non-turn-scoped row (`_a24/model/complete`, `/api/v1/chat`).
    pub session_id: Option<String>,
    pub seq: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

/// One stored row, as read back by [`Store::query_call_timings`].
#[derive(Debug, Clone, PartialEq)]
pub struct CallTimingRow {
    pub id: i64,
    pub ts: String,
    pub source: String,
    pub model_id: Option<String>,
    pub tier: Option<String>,
    pub served_by: Option<String>,
    pub ok: bool,
    pub error_kind: Option<String>,
    pub step: Option<String>,
    pub session_id: Option<String>,
    pub seq: Option<u64>,
    pub first_token_ms: Option<u64>,
    pub total_ms: u64,
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
}

/// One (source, model_id) group's summary — `GET /api/v1/timings/summary`.
/// `model_id` is `""` for the group of rows that reported none (SQLite
/// `GROUP BY` already treats every `NULL` as one group; this just gives that
/// group a printable key alongside the real model ids).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTimingSummaryRow {
    pub source: String,
    pub model_id: String,
    pub count: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
}

fn clamp_i64(x: u64) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

fn clamp_i64_opt(x: Option<u64>) -> Option<i64> {
    x.map(clamp_i64)
}

/// A stored counter column read back as `u64`. The table's own CHECK
/// constraints guarantee every stored value is `>= 0`; a negative value here
/// should be unreachable through this crate's own writes.
fn nonneg_u64(x: i64) -> u64 {
    u64::try_from(x).unwrap_or_else(|_| {
        tracing::warn!(
            value = x,
            "model_call_timings: read a negative value — the table's CHECK constraints \
             should make this impossible; the data may be corrupted"
        );
        0
    })
}

fn opt_nonneg_u64(x: Option<i64>) -> Option<u64> {
    x.map(nonneg_u64)
}

fn row_from(r: &SqliteRow) -> CallTimingRow {
    CallTimingRow {
        id: r.get("id"),
        ts: r.get("ts"),
        source: r.get("source"),
        model_id: r.get("model_id"),
        tier: r.get("tier"),
        served_by: r.get("served_by"),
        ok: r.get::<i64, _>("ok") != 0,
        error_kind: r.get("error_kind"),
        step: r.get("step"),
        session_id: r.get("session_id"),
        seq: opt_nonneg_u64(r.get("seq")),
        first_token_ms: opt_nonneg_u64(r.get("first_token_ms")),
        total_ms: nonneg_u64(r.get("total_ms")),
        prompt_tokens: opt_nonneg_u64(r.get("prompt_tokens")),
        completion_tokens: opt_nonneg_u64(r.get("completion_tokens")),
    }
}

/// The percentile of an ALREADY-SORTED (ascending) slice, nearest-rank
/// method — deterministic and dependency-free (no interpolation), good
/// enough for a diagnostic summary. Empty input reports 0, never panics.
fn percentile_of_sorted(sorted: &[u64], p: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let idx = (((sorted.len() - 1) as f64) * p).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

impl Store {
    /// Appends one row. Never upserts, never reads before writing (unlike
    /// `module_model_usage`'s per-day aggregate) — this is a ledger, and one
    /// call is always exactly one row.
    ///
    /// # Errors
    /// Storage — including a CHECK violation (an out-of-closed-set `tier`, a
    /// negative duration, etc.), which should be unreachable through
    /// `timing_recorder.rs`'s own construction.
    pub async fn record_call_timing(&self, t: &NewCallTiming) -> Result<()> {
        sqlx::query(
            "INSERT INTO model_call_timings \
                (ts, source, model_id, tier, served_by, ok, error_kind, step, \
                 session_id, seq, first_token_ms, total_ms, prompt_tokens, completion_tokens) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&t.ts)
        .bind(&t.source)
        .bind(&t.model_id)
        .bind(&t.tier)
        .bind(&t.served_by)
        .bind(i64::from(t.ok))
        .bind(&t.error_kind)
        .bind(&t.step)
        .bind(&t.session_id)
        .bind(clamp_i64_opt(t.seq))
        .bind(clamp_i64_opt(t.first_token_ms))
        .bind(clamp_i64(t.total_ms))
        .bind(clamp_i64_opt(t.prompt_tokens))
        .bind(clamp_i64_opt(t.completion_tokens))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Raw rows, newest first, optionally filtered by exact `source` and/or
    /// `ts >= since`. `limit` is always applied (the caller — the REST route
    /// — picks the default; this method has no opinion on one).
    ///
    /// # Errors
    /// Storage.
    pub async fn query_call_timings(
        &self,
        source: Option<&str>,
        since: Option<&str>,
        limit: u32,
    ) -> Result<Vec<CallTimingRow>> {
        let mut sql = String::from(
            "SELECT id, ts, source, model_id, tier, served_by, ok, error_kind, step, \
                    session_id, seq, first_token_ms, total_ms, prompt_tokens, completion_tokens \
             FROM model_call_timings WHERE 1 = 1",
        );
        if source.is_some() {
            sql.push_str(" AND source = ?");
        }
        if since.is_some() {
            sql.push_str(" AND ts >= ?");
        }
        sql.push_str(" ORDER BY id DESC LIMIT ?");
        let mut q = sqlx::query(&sql);
        if let Some(s) = source {
            q = q.bind(s);
        }
        if let Some(s) = since {
            q = q.bind(s);
        }
        q = q.bind(i64::from(limit));
        let rows = q.fetch_all(self.pool()).await?;
        Ok(rows.iter().map(row_from).collect())
    }

    /// `count`/p50/p95/max `total_ms`, grouped by (source, model_id).
    /// Computed in Rust over the filtered rows (a nearest-rank percentile has
    /// no single-statement SQLite equivalent) — fine at this table's scale
    /// (retention caps it at 100k rows; `since` narrows it further).
    ///
    /// # Errors
    /// Storage.
    pub async fn call_timing_summary(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<CallTimingSummaryRow>> {
        let mut sql =
            String::from("SELECT source, model_id, total_ms FROM model_call_timings WHERE 1 = 1");
        if since.is_some() {
            sql.push_str(" AND ts >= ?");
        }
        let mut q = sqlx::query(&sql);
        if let Some(s) = since {
            q = q.bind(s);
        }
        let rows = q.fetch_all(self.pool()).await?;

        let mut groups: std::collections::BTreeMap<(String, String), Vec<u64>> =
            std::collections::BTreeMap::new();
        for r in &rows {
            let source: String = r.get("source");
            let model_id: Option<String> = r.get("model_id");
            let total_ms = nonneg_u64(r.get("total_ms"));
            groups
                .entry((source, model_id.unwrap_or_default()))
                .or_default()
                .push(total_ms);
        }

        let mut out = Vec::with_capacity(groups.len());
        for ((source, model_id), mut ms) in groups {
            ms.sort_unstable();
            out.push(CallTimingSummaryRow {
                source,
                model_id,
                count: ms.len() as u64,
                p50_ms: percentile_of_sorted(&ms, 0.50),
                p95_ms: percentile_of_sorted(&ms, 0.95),
                max_ms: ms.last().copied().unwrap_or(0),
            });
        }
        Ok(out)
    }

    /// Retention: deletes every row older than `cutoff_ts` (ISO-8601,
    /// fixed-width — a lexical compare is a chronological one), THEN caps
    /// whatever remains at the newest `max_rows` — "whichever bound is hit
    /// first" (design ask). Returns the total rows deleted.
    ///
    /// # Errors
    /// Storage.
    pub async fn prune_call_timings(&self, cutoff_ts: &str, max_rows: u32) -> Result<u64> {
        let by_age = sqlx::query("DELETE FROM model_call_timings WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(self.pool())
            .await?
            .rows_affected();
        let by_count = sqlx::query(
            "DELETE FROM model_call_timings WHERE id NOT IN \
                (SELECT id FROM model_call_timings ORDER BY id DESC LIMIT ?)",
        )
        .bind(i64::from(max_rows))
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(by_age + by_count)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn ok_row(ts: &str, source: &str, total_ms: u64) -> NewCallTiming {
        NewCallTiming {
            ts: ts.to_owned(),
            source: source.to_owned(),
            model_id: Some("m1".to_owned()),
            tier: Some("local".to_owned()),
            served_by: Some("omlx".to_owned()),
            ok: true,
            total_ms,
            prompt_tokens: Some(10),
            completion_tokens: Some(5),
            ..Default::default()
        }
    }

    // ── structural: no content column exists (grep-proof against a future PR
    // that adds one back) ───────────────────────────────────────────────────

    #[test]
    fn the_migration_carries_no_prompt_response_or_transcript_column() {
        let sql = include_str!("../migrations/0013_model_call_timings.sql");
        // Strip `--` comments first — this file's OWN doc comments discuss
        // "content" in prose (explaining why there is none); only the actual
        // SQL (column names) must be checked, or this test would trip on its
        // own documentation.
        let code_only: String = sql
            .lines()
            .map(|l| l.split("--").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        for banned in [
            "prompt_text",
            "content",
            "message",
            "transcript",
            "reply",
            "response_text",
        ] {
            assert!(
                !code_only.contains(banned),
                "migration must never introduce a content-carrying column named {banned:?}"
            );
        }
    }

    #[tokio::test]
    async fn write_then_read_back_round_trips_every_field() {
        let store = Store::open_memory().await.unwrap();
        store
            .record_call_timing(&NewCallTiming {
                ts: "2026-09-27T00:00:00Z".to_owned(),
                source: "module:agentear".to_owned(),
                model_id: Some("Qwen3.6-35B-A3B-MLX-8bit".to_owned()),
                tier: Some("local".to_owned()),
                served_by: Some("omlx".to_owned()),
                ok: true,
                error_kind: None,
                step: Some("asr".to_owned()),
                session_id: Some("ses_1".to_owned()),
                seq: Some(3),
                first_token_ms: Some(120),
                total_ms: 842,
                prompt_tokens: Some(12),
                completion_tokens: Some(8),
            })
            .await
            .unwrap();
        let rows = store.query_call_timings(None, None, 10).await.unwrap();
        assert_eq!(rows.len(), 1);
        let r = &rows[0];
        assert_eq!(r.source, "module:agentear");
        assert_eq!(r.model_id.as_deref(), Some("Qwen3.6-35B-A3B-MLX-8bit"));
        assert_eq!(r.tier.as_deref(), Some("local"));
        assert_eq!(r.served_by.as_deref(), Some("omlx"));
        assert!(r.ok);
        assert_eq!(r.step.as_deref(), Some("asr"));
        assert_eq!(r.session_id.as_deref(), Some("ses_1"));
        assert_eq!(r.seq, Some(3));
        assert_eq!(r.first_token_ms, Some(120));
        assert_eq!(r.total_ms, 842);
        assert_eq!(r.prompt_tokens, Some(12));
        assert_eq!(r.completion_tokens, Some(8));
    }

    #[tokio::test]
    async fn query_filters_by_source_and_since_newest_first() {
        let store = Store::open_memory().await.unwrap();
        store
            .record_call_timing(&ok_row("2026-09-25T00:00:00Z", "chat", 100))
            .await
            .unwrap();
        store
            .record_call_timing(&ok_row("2026-09-26T00:00:00Z", "module:agentear", 200))
            .await
            .unwrap();
        store
            .record_call_timing(&ok_row("2026-09-27T00:00:00Z", "chat", 300))
            .await
            .unwrap();

        let all = store.query_call_timings(None, None, 10).await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].total_ms, 300, "newest (highest id) first");

        let chat_only = store
            .query_call_timings(Some("chat"), None, 10)
            .await
            .unwrap();
        assert_eq!(chat_only.len(), 2);
        assert!(chat_only.iter().all(|r| r.source == "chat"));

        let since_26 = store
            .query_call_timings(None, Some("2026-09-26T00:00:00Z"), 10)
            .await
            .unwrap();
        assert_eq!(since_26.len(), 2);

        let limited = store.query_call_timings(None, None, 1).await.unwrap();
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].total_ms, 300);
    }

    #[tokio::test]
    async fn summary_groups_by_source_and_model_with_correct_percentiles() {
        let store = Store::open_memory().await.unwrap();
        // "chat"/"m1": totals 100, 200, 300, 400, 500 (five rows).
        for ms in [100u64, 200, 300, 400, 500] {
            store
                .record_call_timing(&ok_row("2026-09-27T00:00:00Z", "chat", ms))
                .await
                .unwrap();
        }
        // A different (source, model) group must not blend into the above.
        store
            .record_call_timing(&NewCallTiming {
                model_id: Some("m2".to_owned()),
                ..ok_row("2026-09-27T00:00:00Z", "chat", 9999)
            })
            .await
            .unwrap();

        let summary = store.call_timing_summary(None).await.unwrap();
        let chat_m1 = summary
            .iter()
            .find(|r| r.source == "chat" && r.model_id == "m1")
            .unwrap();
        assert_eq!(chat_m1.count, 5);
        assert_eq!(chat_m1.p50_ms, 300, "median of [100,200,300,400,500]");
        assert_eq!(chat_m1.max_ms, 500);
        // p95 of 5 sorted values, nearest-rank: index round(4*0.95)=4 → 500.
        assert_eq!(chat_m1.p95_ms, 500);

        let chat_m2 = summary
            .iter()
            .find(|r| r.source == "chat" && r.model_id == "m2")
            .unwrap();
        assert_eq!(chat_m2.count, 1);
        assert_eq!(chat_m2.max_ms, 9999);
    }

    #[tokio::test]
    async fn prune_deletes_rows_older_than_cutoff_then_caps_the_remainder_by_count() {
        let store = Store::open_memory().await.unwrap();
        store
            .record_call_timing(&ok_row("2026-08-01T00:00:00Z", "chat", 1))
            .await
            .unwrap(); // old
        for i in 0..5 {
            store
                .record_call_timing(&ok_row(&format!("2026-09-2{i}T00:00:00Z"), "chat", 1))
                .await
                .unwrap();
        }
        // Age-based prune: the 2026-08-01 row is older than the cutoff, the
        // rest (2026-09-2x) are not.
        let deleted = store
            .prune_call_timings("2026-09-01T00:00:00Z", 100)
            .await
            .unwrap();
        assert_eq!(deleted, 1);
        assert_eq!(
            store
                .query_call_timings(None, None, 100)
                .await
                .unwrap()
                .len(),
            5
        );

        // Count-based cap: even with every row fresh enough, more than
        // max_rows still gets trimmed down to the newest max_rows.
        let deleted2 = store
            .prune_call_timings("2000-01-01T00:00:00Z", 2)
            .await
            .unwrap();
        assert_eq!(deleted2, 3);
        let remaining = store.query_call_timings(None, None, 100).await.unwrap();
        assert_eq!(remaining.len(), 2);
        assert_eq!(
            remaining[0].ts, "2026-09-24T00:00:00Z",
            "the two newest survive"
        );
    }

    #[tokio::test]
    async fn check_constraints_reject_an_out_of_set_tier() {
        let store = Store::open_memory().await.unwrap();
        let p = crate::test_hooks::pool(&store);
        let bad = sqlx::query(
            "INSERT INTO model_call_timings (ts, source, tier, ok, total_ms) \
             VALUES ('2026-09-27T00:00:00Z', 'chat', 'orbital', 1, 1)",
        )
        .execute(p)
        .await;
        assert!(bad.is_err(), "tier must be local/remote/NULL");

        let ok = sqlx::query(
            "INSERT INTO model_call_timings (ts, source, tier, ok, total_ms) \
             VALUES ('2026-09-27T00:00:00Z', 'chat', 'local', 1, 1)",
        )
        .execute(p)
        .await;
        assert!(ok.is_ok(), "positive control: a legal tier inserts fine");
    }
}
