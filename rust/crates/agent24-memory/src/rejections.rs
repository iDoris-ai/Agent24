//! P1-02c c3 (design D3/D4): failed-write audit trail and session gap ranges.
//!
//! `mem_write_rejections` is an audit trail, not a retry queue and not part of
//! the quota: it is rate-limited (one row per owner + code per 60 s, the rest
//! folded into `suppressed_count`) and ring-capped at 1000 rows per owner.
//! `mem_session_gaps` keeps one row per contiguous run of unsaved turns and is
//! NEVER ring-trimmed, so a gap survives the audit ring overwriting its
//! rejection rows.
//!
//! Recording is best effort. A rejection is often caused by a full disk or
//! quota, which can fail this very write; [`Rejections::note`] therefore logs a
//! failure and returns, and the caller's original error is never replaced.

use sqlx::SqlitePool;

use crate::artifact::checksum;
use crate::{Clock, MemoryErrorCode, Result, SystemClock};

/// Rows kept per owner in `mem_write_rejections` (oldest overwritten first).
pub const MAX_REJECTION_ROWS: i64 = 1000;
/// Window within which a repeat (same owner + code) only bumps the counter.
pub const RATE_LIMIT_SECS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnerKind {
    Personal,
    Module,
}

impl OwnerKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Personal => "personal",
            Self::Module => "module",
        }
    }
}

/// What was refused. `rejected_kind` is the event kind, never its body.
#[derive(Debug, Clone)]
pub struct Rejection<'a> {
    pub owner_kind: OwnerKind,
    /// The raw partition key; stored only as a digest.
    pub owner_key: &'a str,
    pub code: MemoryErrorCode,
    pub rejected_kind: Option<&'a str>,
    pub count_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectionRow {
    pub code: String,
    pub rejected_kind: Option<String>,
    pub at_epoch: i64,
    pub suppressed_count: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionGap {
    pub session_id: String,
    pub turn_from: i64,
    pub turn_to: i64,
    pub code: String,
    pub first_at: i64,
    pub last_at: i64,
}

#[derive(Clone)]
pub struct Rejections {
    pool: SqlitePool,
}

/// Opaque owner id: a digest prefix, so the partition key (which embeds the
/// user id and a NUL byte) never lands in the audit table.
fn opaque_owner(owner_key: &str) -> String {
    checksum(owner_key)[..16].to_owned()
}

impl Rejections {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    /// Best-effort record: a failure here is logged and swallowed so it can never
    /// replace the error that caused the rejection.
    pub async fn note(&self, r: &Rejection<'_>) {
        if let Err(e) = self.record_using_clock(r, &SystemClock).await {
            tracing::warn!(code = r.code.as_str(), error = %e, "could not record write rejection");
        }
    }

    pub async fn record_using_clock(&self, r: &Rejection<'_>, clock: &dyn Clock) -> Result<()> {
        let now = clock.now_epoch_secs() as i64;
        let owner = opaque_owner(r.owner_key);
        let code = r.code.as_str();
        let mut tx = self.pool.begin().await?;
        let folded = sqlx::query(
            "UPDATE mem_write_rejections SET suppressed_count = suppressed_count + 1
             WHERE id = (SELECT MAX(id) FROM mem_write_rejections
                         WHERE owner_id = ? AND code = ? AND at_epoch > ? - ?)",
        )
        .bind(&owner)
        .bind(code)
        .bind(now)
        .bind(RATE_LIMIT_SECS)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if folded == 0 {
            sqlx::query(
                "INSERT INTO mem_write_rejections
                   (owner_kind, owner_id, code, dimension, rejected_kind, count_at, at_epoch)
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(r.owner_kind.as_str())
            .bind(&owner)
            .bind(code)
            .bind(r.code.dimension())
            .bind(r.rejected_kind)
            .bind(r.count_at)
            .bind(now)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM mem_write_rejections WHERE owner_id = ? AND id <=
                   COALESCE((SELECT id FROM mem_write_rejections WHERE owner_id = ?
                             ORDER BY id DESC LIMIT 1 OFFSET ?), 0)",
            )
            .bind(&owner)
            .bind(&owner)
            .bind(MAX_REJECTION_ROWS)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Newest first.
    pub async fn list(&self, owner_key: &str) -> Result<Vec<RejectionRow>> {
        let rows = sqlx::query_as::<_, (String, Option<String>, i64, i64)>(
            "SELECT code, rejected_kind, at_epoch, suppressed_count FROM mem_write_rejections
             WHERE owner_id = ? ORDER BY id DESC",
        )
        .bind(opaque_owner(owner_key))
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(
                |(code, rejected_kind, at_epoch, suppressed_count)| RejectionRow {
                    code,
                    rejected_kind,
                    at_epoch,
                    suppressed_count,
                },
            )
            .collect())
    }

    /// Record that `turn` of `session_id` was not saved. A turn adjacent to (or
    /// inside) an existing gap extends/merges into it; anything else opens a new
    /// gap. Never ring-trimmed. Each turn is expected to fail once, in order.
    pub async fn record_session_gap_using_clock(
        &self,
        session_id: &str,
        turn: i64,
        code: MemoryErrorCode,
        clock: &dyn Clock,
    ) -> Result<()> {
        let now = clock.now_epoch_secs() as i64;
        let mut tx = self.pool.begin().await?;
        let touched = sqlx::query(
            "UPDATE mem_session_gaps SET
               turn_from = MIN(turn_from, ?1), turn_to = MAX(turn_to, ?1), last_at = ?3
             WHERE id = (SELECT MAX(id) FROM mem_session_gaps WHERE session_id = ?2
                         AND turn_from - 1 <= ?1 AND ?1 <= turn_to + 1)",
        )
        .bind(turn)
        .bind(session_id)
        .bind(now)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if touched == 0 {
            sqlx::query(
                "INSERT INTO mem_session_gaps
                   (session_id, turn_from, turn_to, code, first_at, last_at)
                 VALUES (?, ?, ?, ?, ?, ?)",
            )
            .bind(session_id)
            .bind(turn)
            .bind(turn)
            .bind(code.as_str())
            .bind(now)
            .bind(now)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn note_session_gap(&self, session_id: &str, turn: i64, code: MemoryErrorCode) {
        if let Err(e) = self
            .record_session_gap_using_clock(session_id, turn, code, &SystemClock)
            .await
        {
            tracing::warn!(code = code.as_str(), error = %e, "could not record session gap");
        }
    }

    pub async fn session_gaps(&self, session_id: &str) -> Result<Vec<SessionGap>> {
        let rows = sqlx::query_as::<_, (i64, i64, String, i64, i64)>(
            "SELECT turn_from, turn_to, code, first_at, last_at FROM mem_session_gaps
             WHERE session_id = ? ORDER BY turn_from",
        )
        .bind(session_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(turn_from, turn_to, code, first_at, last_at)| SessionGap {
                session_id: session_id.to_owned(),
                turn_from,
                turn_to,
                code,
                first_at,
                last_at,
            })
            .collect())
    }
}
