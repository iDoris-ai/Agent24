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

/// One (source, model_id, step) group's summary — `GET /api/v1/timings/summary`.
/// `model_id`/`step` are `""` for the group of rows that reported none
/// (SQLite `GROUP BY` already treats every `NULL` as one group; this just
/// gives that group a printable key alongside the real values).
///
/// Review (ME4-CODEX-DEBT-10 #2): `step` joined the group key so this can no
/// longer blend unrelated quantities together — a real model-answer call
/// (`step == ""`, from `_a24/model/complete`/`/api/v1/chat`) versus
/// AgentEar's own per-turn breakdown steps (`"asr"`, `"llm"`, `"total"`, …,
/// from `agentear_timings.rs`), which previously shared a group purely by
/// having the same `(source, model_id)` — e.g. a turn's ~900ms `llm` step
/// and its ~3500ms `total` (whole turn incl. audio playback) used to land in
/// the SAME p50/p95, making "the model is slow" indistinguishable from "the
/// speaker is still talking".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallTimingSummaryRow {
    pub source: String,
    pub model_id: String,
    pub step: String,
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

    /// `count`/p50/p95/max `total_ms`, grouped by (source, model_id, step).
    /// Computed in Rust over the filtered rows (a nearest-rank percentile has
    /// no single-statement SQLite equivalent) — fine at this table's scale
    /// (retention caps it at 100k rows; `since` narrows it further).
    ///
    /// Review (ME4-CODEX-DEBT-10 #2): `step` joined the group key — see
    /// [`CallTimingSummaryRow`]'s doc comment for why grouping by
    /// `(source, model_id)` alone silently blended a real model call's
    /// latency together with AgentEar's own unrelated per-turn sub-steps
    /// that happen to share the same source/model.
    ///
    /// # Errors
    /// Storage.
    pub async fn call_timing_summary(
        &self,
        since: Option<&str>,
    ) -> Result<Vec<CallTimingSummaryRow>> {
        let mut sql = String::from(
            "SELECT source, model_id, step, total_ms FROM model_call_timings WHERE 1 = 1",
        );
        if since.is_some() {
            sql.push_str(" AND ts >= ?");
        }
        let mut q = sqlx::query(&sql);
        if let Some(s) = since {
            q = q.bind(s);
        }
        let rows = q.fetch_all(self.pool()).await?;

        let mut groups: std::collections::BTreeMap<(String, String, String), Vec<u64>> =
            std::collections::BTreeMap::new();
        for r in &rows {
            let source: String = r.get("source");
            let model_id: Option<String> = r.get("model_id");
            let step: Option<String> = r.get("step");
            let total_ms = nonneg_u64(r.get("total_ms"));
            groups
                .entry((
                    source,
                    model_id.unwrap_or_default(),
                    step.unwrap_or_default(),
                ))
                .or_default()
                .push(total_ms);
        }

        let mut out = Vec::with_capacity(groups.len());
        for ((source, model_id, step), mut ms) in groups {
            ms.sort_unstable();
            out.push(CallTimingSummaryRow {
                source,
                model_id,
                step,
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
    /// Review (ME4-CODEX-DEBT-10 #1): the count-based cap runs every
    /// `PRUNE_EVERY` writes (`agent24d/src/timing_recorder.rs`), so it must
    /// never full-table-scan while holding the writer lock. The obvious
    /// `id NOT IN (SELECT id FROM … ORDER BY id DESC LIMIT ?)` form forces
    /// SQLite to materialize the subquery into a Bloom filter and then scan
    /// every row of the table to test membership — ~26ms at 100k rows
    /// (confirmed via `EXPLAIN QUERY PLAN` and a timed benchmark during
    /// review). `id` is this table's own `INTEGER PRIMARY KEY` (the SQLite
    /// rowid), strictly increasing and never reused, so "keep the newest
    /// `max_rows` rows" can instead be phrased as "delete every row whose id
    /// is more than `max_rows` below the current max id" — `MAX(id)` is a
    /// single rightmost-leaf index lookup (not a scan) and the delete is an
    /// indexed PK range scan, both confirmed `SEARCH` (never `SCAN`) by
    /// `EXPLAIN QUERY PLAN` in `count_based_prune_never_full_table_scans`
    /// below (~1ms at 100k rows in the same benchmark, and a true no-op —
    /// no rows touched — once the table is already at/under the cap). Prior
    /// prunes can leave gaps in `id` (rows deleted by age), so this can keep
    /// slightly FEWER than `max_rows` rows when that happens; it can never
    /// keep more, which is all "cap" requires.
    ///
    /// # Errors
    /// Storage.
    pub async fn prune_call_timings(&self, cutoff_ts: &str, max_rows: u32) -> Result<u64> {
        let by_age = sqlx::query("DELETE FROM model_call_timings WHERE ts < ?")
            .bind(cutoff_ts)
            .execute(self.pool())
            .await?
            .rows_affected();
        let by_count = sqlx::query(COUNT_CAP_DELETE_SQL)
            .bind(i64::from(max_rows))
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(by_age + by_count)
    }
}

/// The exact DELETE `prune_call_timings` issues for its count-based cap —
/// pulled out to a constant so the regression test below
/// (`count_based_prune_never_full_table_scans`) can run
/// `EXPLAIN QUERY PLAN` on the SAME statement the production code path
/// executes, not a copy that could drift out of sync with it. `NULL - ?`
/// (an empty table) makes the whole `WHERE` clause `NULL`/false, so this is
/// also a safe no-op on an empty table without a separate branch.
const COUNT_CAP_DELETE_SQL: &str = "DELETE FROM model_call_timings WHERE id <= \
    (SELECT MAX(id) FROM model_call_timings) - ?";

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

    /// Review (ME4-CODEX-DEBT-10 #1): `prune_call_timings`'s count-based cap
    /// runs on EVERY `PRUNE_EVERY` (200) writes — at 100k rows that must
    /// never degrade into a full-table scan (it holds the writer lock while
    /// it runs). `EXPLAIN QUERY PLAN` on the exact DELETE this method issues
    /// must show an indexed `SEARCH`, never a `SCAN`, of
    /// `model_call_timings`.
    /// Review (ME4-CODEX-DEBT-10 #2): AgentEar's own per-turn breakdown
    /// (`agentear_timings.rs`) writes ONE row per `*_ms` field of a turn —
    /// `step: Some("llm")` for the actual model-answer latency (~900ms in
    /// this fixture) and `step: Some("total")` for the WHOLE turn including
    /// record/ASR/TTS/playback (~3500ms) — all sharing the same
    /// `(source, model_id)`. Grouping only by `(source, model_id)` blends
    /// these unrelated quantities into one p95, so a summary reader cannot
    /// tell "the model was slow" from "the speaker was still talking".
    #[tokio::test]
    async fn summary_does_not_blend_agentear_llm_latency_with_its_own_turn_total() {
        let store = Store::open_memory().await.unwrap();
        let source = "module:agentear".to_owned();
        let model_id = Some("Qwen3.6-35B-A3B-MLX-8bit".to_owned());
        // Five turns' worth of the REAL model-answer latency (the `llm_ms`
        // step) — this is the number a reader asking "how slow is the model"
        // actually wants.
        for ms in [800u64, 850, 900, 950, 1000] {
            store
                .record_call_timing(&NewCallTiming {
                    ts: "2026-09-27T00:00:00Z".to_owned(),
                    source: source.clone(),
                    model_id: model_id.clone(),
                    ok: true,
                    step: Some("llm".to_owned()),
                    total_ms: ms,
                    ..Default::default()
                })
                .await
                .unwrap();
        }
        // The SAME five turns' whole-turn total (record+asr+llm+tts+play) —
        // a much larger, unrelated number that must not leak into the `llm`
        // step's percentiles above.
        for ms in [3000u64, 3200, 3400, 3500, 3600] {
            store
                .record_call_timing(&NewCallTiming {
                    ts: "2026-09-27T00:00:00Z".to_owned(),
                    source: source.clone(),
                    model_id: model_id.clone(),
                    ok: true,
                    step: Some("total".to_owned()),
                    total_ms: ms,
                    ..Default::default()
                })
                .await
                .unwrap();
        }

        let summary = store.call_timing_summary(None).await.unwrap();
        let llm_group = summary
            .iter()
            .find(|r| {
                r.source == source && r.model_id == model_id.clone().unwrap() && r.step == "llm"
            })
            .expect("an `llm` step group, distinct from `total`, must exist");
        assert_eq!(
            llm_group.count, 5,
            "only the 5 `llm` rows, not the 5 `total` rows too"
        );
        assert_eq!(llm_group.p50_ms, 900, "median of the llm-only latencies");
        assert!(
            llm_group.p95_ms <= 1000,
            "the real model latency's p95 must not be inflated by the turn's `total` rows \
             (got {}, which would only be possible if `total` rows leaked in)",
            llm_group.p95_ms
        );

        let total_group = summary
            .iter()
            .find(|r| {
                r.source == source && r.model_id == model_id.clone().unwrap() && r.step == "total"
            })
            .expect("a `total` step group, distinct from `llm`, must exist");
        assert_eq!(total_group.count, 5);
        assert_eq!(total_group.p50_ms, 3400);
    }

    #[tokio::test]
    async fn count_based_prune_never_full_table_scans() {
        let store = Store::open_memory().await.unwrap();
        let p = crate::test_hooks::pool(&store);
        let plan_rows = sqlx::query(&format!("EXPLAIN QUERY PLAN {COUNT_CAP_DELETE_SQL}"))
            .fetch_all(p)
            .await
            .unwrap();
        let plan: String = plan_rows
            .iter()
            .map(|r| r.get::<String, _>("detail"))
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            !plan.to_uppercase().contains("SCAN"),
            "count-based prune must not full-table-scan model_call_timings; plan was: {plan}"
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
