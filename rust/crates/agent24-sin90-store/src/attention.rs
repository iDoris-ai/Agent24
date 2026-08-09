//! Attention reconciliation (SIN90-domain.md §3.2).
//!
//! Two paths that MUST agree:
//! - [`Sin90Store::attention`] — pure replay of `sin90_events` for a window.
//!   Reads only self-contained event payloads; never joins the mutable
//!   blocks/directions tables, so editing a title or deleting a block later
//!   cannot rewrite history.
//! - [`Sin90Store::attention_apply_new_events`] / [`Sin90Store::attention_rebuild`]
//!   — the materialized `sin90_attention_daily`, advanced by a monotonic
//!   watermark so incremental folding equals a full rebuild.

use serde::{Deserialize, Serialize};
use sqlx::Row;

use crate::{Result, Sin90Store};

/// One direction's realized minutes over a window (from event replay).
/// `direction_id` is `""` for a block with no direction — the same
/// representation `sin90_attention_daily` uses, so the two paths line up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionRow {
    pub direction_id: String,
    /// Title as snapshotted in the event payload at completion time.
    pub direction_title: Option<String>,
    pub actual_min: i64,
}

/// One materialized (day, direction) bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttentionDailyRow {
    pub day: String,
    pub direction_id: String,
    pub actual_min: i64,
}

impl Sin90Store {
    /// Pure replay: realized minutes per direction for `[start, end)` (ISO-8601
    /// bounds, lexical compare == chronological on fixed-width UTC).
    pub async fn attention(&self, start: &str, end: &str) -> Result<Vec<AttentionRow>> {
        // Window filter uses the indexed `at` column (== payload.occurred_at by
        // construction). `MAX(title)` makes the label deterministic if a
        // direction was renamed mid-window (payloads still hold each snapshot);
        // `COALESCE(dir,'')` matches the materialized view's no-direction key;
        // the `, dir` tiebreaker makes ordering stable.
        let rows = sqlx::query(
            "SELECT COALESCE(json_extract(payload,'$.direction_id'),'') AS dir,
                    MAX(json_extract(payload,'$.direction_title'))       AS title,
                    CAST(COALESCE(SUM(json_extract(payload,'$.minutes')),0) AS INTEGER) AS actual
             FROM sin90_events
             WHERE entity = 'block' AND kind = 'transitioned' AND to_state = 'completed'
               AND at >= ? AND at < ?
             GROUP BY dir
             ORDER BY actual DESC, dir",
        )
        .bind(start)
        .bind(end)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AttentionRow {
                direction_id: r.get("dir"),
                direction_title: r.get("title"),
                actual_min: r.get("actual"),
            })
            .collect())
    }

    /// Fold every not-yet-applied completed-block event into
    /// `sin90_attention_daily`, then advance the watermark to the highest event
    /// seq considered. Idempotent: a second call with no new events is a no-op.
    pub async fn attention_apply_new_events(&self) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        let wm: i64 = sqlx::query(
            "SELECT applied_event_seq FROM sin90_attention_watermark WHERE only_row = 1",
        )
        .fetch_one(&mut *tx)
        .await?
        .get("applied_event_seq");
        let max_seq: i64 = sqlx::query("SELECT COALESCE(MAX(seq), 0) AS m FROM sin90_events")
            .fetch_one(&mut *tx)
            .await?
            .get("m");
        if max_seq <= wm {
            tx.commit().await?;
            return Ok(());
        }

        let rows = sqlx::query(
            "SELECT json_extract(payload,'$.occurred_at')  AS occurred_at,
                    json_extract(payload,'$.direction_id')  AS dir,
                    CAST(json_extract(payload,'$.minutes') AS INTEGER) AS minutes
             FROM sin90_events
             WHERE seq > ? AND seq <= ?
               AND entity = 'block' AND kind = 'transitioned' AND to_state = 'completed'
             ORDER BY seq",
        )
        .bind(wm)
        .bind(max_seq)
        .fetch_all(&mut *tx)
        .await?;

        for r in rows {
            // try_get so a malformed/NULL payload surfaces as an error, not a panic.
            let occurred_at: String = r.try_get("occurred_at")?;
            // NULL direction folds under "" so the NOT NULL PK is satisfied —
            // the same key `attention()` reports for no-direction blocks.
            let dir: Option<String> = r.try_get("dir")?;
            let dir = dir.unwrap_or_default();
            let minutes: i64 = r.try_get("minutes")?;
            let day = occurred_at.get(0..10).unwrap_or("").to_string();
            sqlx::query(
                "INSERT INTO sin90_attention_daily (day, direction_id, actual_min)
                 VALUES (?, ?, ?)
                 ON CONFLICT(day, direction_id) DO UPDATE SET actual_min = actual_min + excluded.actual_min",
            )
            .bind(&day)
            .bind(&dir)
            .bind(minutes)
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query(
            "UPDATE sin90_attention_watermark SET applied_event_seq = ? WHERE only_row = 1",
        )
        .bind(max_seq)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Deterministic rebuild: clear the view, reset the watermark to 0, replay
    /// everything. The invariant `incremental == rebuild` is asserted in tests.
    pub async fn attention_rebuild(&self) -> Result<()> {
        let mut tx = self.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query("DELETE FROM sin90_attention_daily")
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE sin90_attention_watermark SET applied_event_seq = 0 WHERE only_row = 1",
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        self.attention_apply_new_events().await
    }

    /// The materialized view, ordered — for inspection and the consistency test.
    pub async fn attention_daily(&self) -> Result<Vec<AttentionDailyRow>> {
        let rows = sqlx::query(
            "SELECT day, direction_id, actual_min FROM sin90_attention_daily
             ORDER BY day, direction_id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .into_iter()
            .map(|r| AttentionDailyRow {
                day: r.get("day"),
                direction_id: r.get("direction_id"),
                actual_min: r.get("actual_min"),
            })
            .collect())
    }
}
