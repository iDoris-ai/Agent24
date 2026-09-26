//! ME4-4.2.3a — per-module model callback usage, storage side. See
//! `docs/design/ME4-S2-model-callback.md`:
//! - §6.1 — the `module_model_usage` schema (migration
//!   `0008_module_model_usage.sql`) and this module's three `Store` methods.
//! - §6.2 — the counting rule (`served_by = "none"` never carries `calls_ok`
//!   or tokens; the CHECK in the migration enforces it).
//! - §6.3 — `UsageSink`/`UsageOutcome` (agentd's `model_callback.rs`, a
//!   different crate): this store has no dependency on it and does not know
//!   "how a call ended" — it only ever receives an already-decided
//!   [`ModelUsageDelta`] to add.
//!
//! Scope note (task ME4-4.2.3a is store-only, §10.2): the writer that turns
//! `UsageSink::record` calls into these upserts (`agentd/src/usage_recorder.rs`,
//! a bounded-channel background task) and the `GET /api/v1/usage?module=`
//! route both belong to ME4-4.2.3b and are NOT introduced here. This module
//! is exercised directly (no recorder, no route) by its own tests, and by
//! J11's store-level judgements: 50 concurrent writers summing exactly,
//! per-column saturation at `i64::MAX` (no wraparound), and the CHECK
//! constraints above rejecting illegal rows.
//!
//! **`day` is always caller-supplied** (this crate never reads a clock): it
//! must be today's UTC calendar day as `'YYYY-MM-DD'`. Producing it from the
//! right clock (UTC, not local time — two recorders in different timezones
//! must still land in the same bucket) is 4.2.3b's job, in the recorder that
//! calls [`Store::record_module_model_usage`]; this store only enforces the
//! *shape* (`CHECK (length(day) = 10)`), never the value.

use sqlx::Row;
use sqlx::sqlite::SqliteRow;

use crate::{Result, Store};

/// Which tier served a call, or that none did (§6.2). Only used on the write
/// side (`record_module_model_usage`) — the read side returns the stored
/// `TEXT` value as a plain `String` ([`ModelUsageRow::served_by`]), since a
/// row's tier is data the caller already trusts came from this table's own
/// CHECK-enforced closed set, not something a second Rust enum needs to
/// re-validate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServedBy {
    Local,
    Remote,
    None,
}

impl ServedBy {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Local => "local",
            Self::Remote => "remote",
            Self::None => "none",
        }
    }
}

/// One call's already-decided contribution to a (module, day, served_by)
/// row — the increment `record_module_model_usage` adds. `Default` gives the
/// all-zero delta (§6.2's "counts but no tokens" outcomes build on it, e.g.
/// `ModelUsageDelta { calls_cancelled: 1, ..Default::default() }`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelUsageDelta {
    pub calls_ok: u64,
    pub calls_failed: u64,
    pub calls_cancelled: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// One stored row, as read back. `served_by` is `"local"` | `"remote"` |
/// `"none"` (the table's CHECK-enforced closed set — see [`ServedBy`]'s doc
/// comment for why this is a plain `String` rather than that enum).
///
/// For [`Store::module_model_usage_totals`] (a `GROUP BY served_by` across
/// every day), `day` carries no single day and is always `String::new()` —
/// that query answers "how much, per tier, all-time", not "on which day";
/// callers of the totals method must not read `day` from its rows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelUsageRow {
    pub day: String,
    pub served_by: String,
    pub calls_ok: u64,
    pub calls_failed: u64,
    pub calls_cancelled: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// `i64::MAX` spelled out as a SQL literal (SQLite has no `i64::MAX`
/// constant): the saturation ceiling every counter clamps to (§6.1's "token
/// 和用 `min(…, i64::MAX)` 饱和" — done here with a `CASE` guard evaluated
/// *before* the addition, rather than a post-hoc `MIN`, so the addition
/// itself can never leave the `i64` range: SQLite silently promotes an
/// overflowing integer `+` to floating point instead of wrapping, which
/// would otherwise let a saturated column drift off `i64::MAX` by rounding
/// once it round-trips through a `REAL`).
const I64_MAX_SQL: &str = "9223372036854775807";

/// One counter's saturating add, expanded for column `$col`. Used five times
/// (once per counter) to build [`RECORD_USAGE_SQL`].
fn saturating_add(col: &str) -> String {
    format!(
        "{col} = CASE WHEN {col} > {I64_MAX_SQL} - excluded.{col} \
                       THEN {I64_MAX_SQL} ELSE {col} + excluded.{col} END"
    )
}

/// §6.1: "写：一条语句 `INSERT … ON CONFLICT (module, day, served_by) DO
/// UPDATE SET x = x + excluded.x`" — SQLite serialises writers, and the
/// read-modify-write happens inside this one statement, so concurrent
/// recorders never lose an increment to a lost update (J11's "50 个并发写，
/// 合计恰为 50").
fn record_usage_sql() -> String {
    format!(
        "INSERT INTO module_model_usage \
             (module, day, served_by, calls_ok, calls_failed, calls_cancelled, \
              prompt_tokens, completion_tokens) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT (module, day, served_by) DO UPDATE SET {}, {}, {}, {}, {}",
        saturating_add("calls_ok"),
        saturating_add("calls_failed"),
        saturating_add("calls_cancelled"),
        saturating_add("prompt_tokens"),
        saturating_add("completion_tokens"),
    )
}

/// A `u64` delta field, clamped into `i64`'s range for binding — a delta
/// this large in one call never occurs in practice (each `UsageOutcome`
/// contributes at most one call and one provider's token count), but binding
/// must not panic if it ever did; clamping here (rather than at the SQL
/// layer) keeps the bound value always a valid, in-range `i64` literal.
fn clamp_i64(x: u64) -> i64 {
    i64::try_from(x).unwrap_or(i64::MAX)
}

/// The same `i64::MAX` ceiling the write side clamps to ([`I64_MAX_SQL`]),
/// applied again when folding several already-clamped rows together in Rust
/// (`module_model_usage_totals`, review M1): plain [`u64::saturating_add`]
/// only saturates at `u64::MAX`, which is roughly double `i64::MAX` — two
/// per-day rows each individually capped at `i64::MAX` would otherwise sum
/// to `2 * i64::MAX` (still a valid `u64`, no panic, but not the same
/// ceiling every stored row itself is held to). Capping the *sum* to the
/// same ceiling keeps "totals across days" indistinguishable, from the
/// caller's point of view, from "one more day that happened to be huge".
fn saturating_add_capped(a: u64, b: u64) -> u64 {
    let cap = u64::try_from(i64::MAX).unwrap_or(u64::MAX);
    a.saturating_add(b).min(cap)
}

/// A stored counter column, read back as `u64`. The table's own CHECK
/// constraints guarantee every stored value is `>= 0`, so a negative value
/// here should be unreachable through this crate's own writes; if one ever
/// shows up (e.g. a row written by something outside this module, or a
/// corrupted file), warn loudly and fall back to 0 rather than panicking a
/// read.
fn nonneg_u64(x: i64) -> u64 {
    u64::try_from(x).unwrap_or_else(|_| {
        tracing::warn!(
            value = x,
            "module_model_usage: read a negative counter — the table's CHECK constraints \
             should make this impossible; the data may be corrupted"
        );
        0
    })
}

fn row_with_day(r: &SqliteRow, day: String) -> ModelUsageRow {
    ModelUsageRow {
        day,
        served_by: r.get("served_by"),
        calls_ok: nonneg_u64(r.get("calls_ok")),
        calls_failed: nonneg_u64(r.get("calls_failed")),
        calls_cancelled: nonneg_u64(r.get("calls_cancelled")),
        prompt_tokens: nonneg_u64(r.get("prompt_tokens")),
        completion_tokens: nonneg_u64(r.get("completion_tokens")),
    }
}

impl Store {
    /// Add one call's outcome to its (module, day, served_by) aggregate,
    /// creating the row on first use (§6.1). Never reads before writing:
    /// the single `INSERT … ON CONFLICT … DO UPDATE` is the whole operation,
    /// so two concurrent recorders for the same key serialise on SQLite's
    /// own writer lock rather than racing a separate read-modify-write.
    ///
    /// An all-zero `delta` (review, L2) is a no-op that writes nothing — not
    /// even to create an empty row on first use. There is no `UsageOutcome`
    /// that legitimately produces a zero delta (every variant counts at
    /// least one call), so this only guards a caller bug or a future variant
    /// that forgets to set anything; without it, such a bug would silently
    /// litter `module_model_usage` with all-zero rows for modules that were
    /// never actually served.
    ///
    /// # Errors
    /// Storage — including a CHECK violation if `served_by = ServedBy::None`
    /// is combined with a non-zero `calls_ok`/token delta (§6.2: a call that
    /// never reached a provider cannot have been served or billed). This
    /// applies equally whether the row is being created or already exists
    /// (SQLite re-checks every `CHECK` constraint on the `ON CONFLICT DO
    /// UPDATE` path too, not just on `INSERT`).
    pub async fn record_module_model_usage(
        &self,
        module: &str,
        day: &str,
        served_by: ServedBy,
        delta: ModelUsageDelta,
    ) -> Result<()> {
        if delta == ModelUsageDelta::default() {
            return Ok(());
        }
        sqlx::query(&record_usage_sql())
            .bind(module)
            .bind(day)
            .bind(served_by.as_str())
            .bind(clamp_i64(delta.calls_ok))
            .bind(clamp_i64(delta.calls_failed))
            .bind(clamp_i64(delta.calls_cancelled))
            .bind(clamp_i64(delta.prompt_tokens))
            .bind(clamp_i64(delta.completion_tokens))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Daily detail (§6.1): every row for `module` on or after `since_day`
    /// (UTC 'YYYY-MM-DD', string-ordered — safe because the format is fixed
    /// width and zero-padded), newest day first, `served_by` alphabetical
    /// within a day. A module that never called, or whose rows are all
    /// older than `since_day`, gets an empty `Vec` — not an error.
    ///
    /// # Errors
    /// Storage.
    pub async fn module_model_usage(
        &self,
        module: &str,
        since_day: &str,
    ) -> Result<Vec<ModelUsageRow>> {
        let rows = sqlx::query(
            "SELECT day, served_by, calls_ok, calls_failed, calls_cancelled, \
                    prompt_tokens, completion_tokens \
             FROM module_model_usage \
             WHERE module = ? AND day >= ? \
             ORDER BY day DESC, served_by ASC",
        )
        .bind(module)
        .bind(since_day)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(|r| row_with_day(r, r.get("day"))).collect())
    }

    /// All-time totals (§6.1): one row per `served_by` this module has ever
    /// used, summed across every day. At most three rows back (`local` /
    /// `remote` / `none`); a `served_by` the module never hit is simply
    /// absent — the caller (4.2.3b's `GET /api/v1/usage?module=`) fills in
    /// the missing keys as all-zero when building `by_served`. `day` on
    /// every returned row is `String::new()` (see [`ModelUsageRow`]'s doc
    /// comment) — this query has no single day to report.
    ///
    /// Review, M1: this does NOT sum with SQL's `SUM()` — a per-day column
    /// can independently be saturated to `i64::MAX` (this module's own
    /// saturating upsert allows it), and SQLite's `SUM()` does not saturate:
    /// it raises a hard `"integer overflow"` error the moment two summands
    /// would exceed `i64::MAX`, which two maxed-out days trivially would.
    /// Instead, every row for `module` is read back individually (already
    /// each `>= 0` and `<= i64::MAX` by the table's own CHECKs) and folded in
    /// Rust with [`saturating_add_capped`] — a saturating add capped at the
    /// same `i64::MAX` ceiling the write side clamps to, matching that clamp
    /// exactly rather than trading it for a read-side panic.
    ///
    /// # Errors
    /// Storage.
    pub async fn module_model_usage_totals(&self, module: &str) -> Result<Vec<ModelUsageRow>> {
        let rows = sqlx::query(
            "SELECT served_by, calls_ok, calls_failed, calls_cancelled, \
                    prompt_tokens, completion_tokens \
             FROM module_model_usage \
             WHERE module = ?",
        )
        .bind(module)
        .fetch_all(self.pool())
        .await?;

        let mut totals: std::collections::BTreeMap<String, ModelUsageRow> =
            std::collections::BTreeMap::new();
        for r in &rows {
            let served_by: String = r.get("served_by");
            let day = row_with_day(r, String::new());
            let acc = totals
                .entry(served_by.clone())
                .or_insert_with(|| ModelUsageRow {
                    day: String::new(),
                    served_by,
                    calls_ok: 0,
                    calls_failed: 0,
                    calls_cancelled: 0,
                    prompt_tokens: 0,
                    completion_tokens: 0,
                });
            acc.calls_ok = saturating_add_capped(acc.calls_ok, day.calls_ok);
            acc.calls_failed = saturating_add_capped(acc.calls_failed, day.calls_failed);
            acc.calls_cancelled = saturating_add_capped(acc.calls_cancelled, day.calls_cancelled);
            acc.prompt_tokens = saturating_add_capped(acc.prompt_tokens, day.prompt_tokens);
            acc.completion_tokens =
                saturating_add_capped(acc.completion_tokens, day.completion_tokens);
        }
        // BTreeMap keys iterate in ascending order, matching the old query's
        // `ORDER BY served_by ASC`.
        Ok(totals.into_values().collect())
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

    fn pool(store: &Store) -> &SqlitePool {
        crate::test_hooks::pool(store)
    }

    fn temp_db() -> (TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("s.db");
        (dir, path)
    }

    /// A fully-migrated WAL file database with `conns` connections — needed
    /// (unlike `Store::open_memory`'s single `:memory:` connection) for a
    /// genuine multi-connection concurrency test (mirrors
    /// `module_schedules.rs`'s `fresh`/`pool_migrated_up_to` for the same
    /// reason, C1.2).
    async fn fresh_wal(conns: u32) -> (Store, TempDir) {
        let (dir, path) = temp_db();
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
        sqlx::migrate!("./migrations").run(&pool).await.unwrap();
        (crate::test_hooks::from_pool(pool), dir)
    }

    fn one_ok() -> ModelUsageDelta {
        ModelUsageDelta {
            calls_ok: 1,
            ..Default::default()
        }
    }

    // ── J11 (store part) — 50 个并发写入，计数准确 ──────────────────────────

    #[tokio::test]
    async fn fifty_concurrent_writes_to_one_row_sum_to_exactly_fifty() {
        let (store, _dir) = fresh_wal(8).await;
        let day = "2026-09-26";
        // 50 writers for "m" and 10 for "other" race together: the assertion
        // on "m" is the judgement itself; "other" ending up at exactly 10
        // (not bled into by "m"'s 50, nor vice versa) is the positive
        // control — the upsert key is scoped correctly under contention, not
        // just "some total came out right by coincidence".
        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(60));
        let spawn_one = |module: &'static str| {
            let (store, barrier) = (store.clone(), barrier.clone());
            tokio::spawn(async move {
                barrier.wait().await;
                store
                    .record_module_model_usage(module, day, ServedBy::Local, one_ok())
                    .await
                    .unwrap();
            })
        };
        let mut handles = Vec::with_capacity(60);
        for _ in 0..50 {
            handles.push(spawn_one("m"));
        }
        for _ in 0..10 {
            handles.push(spawn_one("other"));
        }
        for h in handles {
            h.await.unwrap();
        }

        let m_rows = store.module_model_usage("m", day).await.unwrap();
        assert_eq!(m_rows.len(), 1);
        assert_eq!(
            m_rows[0].calls_ok, 50,
            "50 concurrent writers to the same key must sum to exactly 50, not lose or double-count any"
        );

        let other_rows = store.module_model_usage("other", day).await.unwrap();
        assert_eq!(other_rows.len(), 1);
        assert_eq!(
            other_rows[0].calls_ok, 10,
            "positive control: a concurrently-written different module's count is exact and unaffected"
        );
    }

    // ── J11 (store part) — token 饱和到 i64::MAX，不回绕（v2, L3: 覆盖
    // calls_ok 之外的 prompt_tokens / completion_tokens 两列） ──────────────

    #[tokio::test]
    async fn calls_ok_and_token_counters_saturate_at_i64_max_instead_of_wrapping() {
        let (store, _dir) = fresh_wal(1).await;
        let day = "2026-09-26";

        // positive control: ordinary, far-from-the-ceiling adds accumulate
        // normally (proves the saturating CASE does not clamp everyday
        // values, only ones that would actually overflow).
        store
            .record_module_model_usage("m", day, ServedBy::Remote, one_ok())
            .await
            .unwrap();
        store
            .record_module_model_usage("m", day, ServedBy::Remote, one_ok())
            .await
            .unwrap();
        let normal = store.module_model_usage("m", day).await.unwrap();
        let remote = normal.iter().find(|r| r.served_by == "remote").unwrap();
        assert_eq!(remote.calls_ok, 2, "two ordinary +1s must add up to 2");

        // saturating path: one write already at i64::MAX on calls_ok AND
        // both token columns, then one more increment on each — every
        // counter must clamp independently at the ceiling, never overflow
        // into a negative (wrapped) value.
        let max = u64::try_from(i64::MAX).unwrap();
        store
            .record_module_model_usage(
                "m",
                day,
                ServedBy::Local,
                ModelUsageDelta {
                    calls_ok: max,
                    prompt_tokens: max,
                    completion_tokens: max,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .record_module_model_usage(
                "m",
                day,
                ServedBy::Local,
                ModelUsageDelta {
                    calls_ok: 5,
                    prompt_tokens: 5,
                    completion_tokens: 5,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let rows = store.module_model_usage("m", day).await.unwrap();
        let local = rows.iter().find(|r| r.served_by == "local").unwrap();
        assert_eq!(
            local.calls_ok, max,
            "adding past i64::MAX must saturate calls_ok at the ceiling, not wrap around"
        );
        assert_eq!(
            local.prompt_tokens, max,
            "adding past i64::MAX must saturate prompt_tokens at the ceiling, not wrap around"
        );
        assert_eq!(
            local.completion_tokens, max,
            "adding past i64::MAX must saturate completion_tokens at the ceiling, not wrap around"
        );
    }

    // ── M1 (review) — totals 汇总不走 SQL SUM，跨天饱和不报错 ────────────────

    #[tokio::test]
    async fn totals_saturate_across_days_without_a_sql_sum_overflow() {
        let (store, _dir) = fresh_wal(1).await;
        let max = u64::try_from(i64::MAX).unwrap();
        // Two DIFFERENT days, each independently saturated to i64::MAX on
        // prompt_tokens for the same (module, served_by). SQL's `SUM()`
        // summing these two columns would raise a hard "integer overflow"
        // error (verified: reverting `module_model_usage_totals` to a
        // `SUM()`-based query turns this test red — see the task's mutation
        // note). The Rust-side saturating fold must instead clamp at
        // i64::MAX and return normally.
        store
            .record_module_model_usage(
                "m",
                "2026-09-25",
                ServedBy::Local,
                ModelUsageDelta {
                    prompt_tokens: max,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .record_module_model_usage(
                "m",
                "2026-09-26",
                ServedBy::Local,
                ModelUsageDelta {
                    prompt_tokens: max,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // positive control: an ordinary (non-saturated) tier on the same
        // module sums normally across the same two days.
        store
            .record_module_model_usage("m", "2026-09-25", ServedBy::Remote, one_ok())
            .await
            .unwrap();
        store
            .record_module_model_usage("m", "2026-09-26", ServedBy::Remote, one_ok())
            .await
            .unwrap();

        let totals = store.module_model_usage_totals("m").await.unwrap();
        let local = totals.iter().find(|r| r.served_by == "local").unwrap();
        assert_eq!(
            local.prompt_tokens, max,
            "totals must saturate at i64::MAX across days, not error or wrap"
        );
        let remote = totals.iter().find(|r| r.served_by == "remote").unwrap();
        assert_eq!(
            remote.calls_ok, 2,
            "positive control: an ordinary tier's totals still sum normally across days"
        );
    }

    // ── L2 (review) — 全零增量是 no-op，不建行 ──────────────────────────────

    #[tokio::test]
    async fn an_all_zero_delta_writes_nothing() {
        let (store, _dir) = fresh_wal(1).await;
        let day = "2026-09-26";
        store
            .record_module_model_usage("m", day, ServedBy::Local, ModelUsageDelta::default())
            .await
            .unwrap();
        assert!(
            store.module_model_usage("m", day).await.unwrap().is_empty(),
            "an all-zero delta must not create an empty row"
        );

        // positive control: a real (non-zero) delta right after does create
        // exactly one row — this isn't a store that silently drops writes.
        store
            .record_module_model_usage("m", day, ServedBy::Local, one_ok())
            .await
            .unwrap();
        let rows = store.module_model_usage("m", day).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].calls_ok, 1);
    }

    // ── L4 (review) — 已存在的 none 行走 UPDATE 路径也被 CHECK 拒绝 ─────────

    #[tokio::test]
    async fn adding_tokens_to_an_existing_none_row_is_rejected_on_the_update_path() {
        let (store, _dir) = fresh_wal(1).await;
        let day = "2026-09-26";
        // establish a legal 'none' row first (an INSERT, not yet an UPDATE)
        store
            .record_module_model_usage(
                "m",
                day,
                ServedBy::None,
                ModelUsageDelta {
                    calls_failed: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // now hit the SAME key again with a token delta: this goes through
        // `ON CONFLICT DO UPDATE`, not `INSERT` — the CHECK must still fire.
        let err = store
            .record_module_model_usage(
                "m",
                day,
                ServedBy::None,
                ModelUsageDelta {
                    prompt_tokens: 3,
                    ..Default::default()
                },
            )
            .await;
        assert!(
            err.is_err(),
            "the CHECK must also reject the ON CONFLICT DO UPDATE path, not just a fresh INSERT"
        );

        // positive control: a further legal update (another calls_failed) to
        // the SAME existing row still succeeds, and the earlier rejected
        // update did not partially apply.
        store
            .record_module_model_usage(
                "m",
                day,
                ServedBy::None,
                ModelUsageDelta {
                    calls_failed: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let rows = store.module_model_usage("m", day).await.unwrap();
        let none = rows.iter().find(|r| r.served_by == "none").unwrap();
        assert_eq!(none.calls_failed, 2, "the rejected update wrote nothing");
        assert_eq!(
            none.prompt_tokens, 0,
            "the rejected token delta never landed"
        );
    }

    // ── J11 (store part) — CHECK 约束拒绝非法值 ──────────────────────────────

    #[tokio::test]
    async fn check_constraints_reject_illegal_rows() {
        let store = Store::open_memory().await.unwrap();
        let p = pool(&store);

        let bad_day_len = sqlx::query(
            "INSERT INTO module_model_usage (module, day, served_by) \
             VALUES ('m', '2026-9-26', 'local')", // 9 chars, not 10
        )
        .execute(p)
        .await;
        assert!(bad_day_len.is_err(), "day must be exactly 10 characters");

        let bad_served_by = sqlx::query(
            "INSERT INTO module_model_usage (module, day, served_by) \
             VALUES ('m', '2026-09-26', 'bogus')",
        )
        .execute(p)
        .await;
        assert!(
            bad_served_by.is_err(),
            "served_by must be one of local/remote/none"
        );

        let negative_calls_ok = sqlx::query(
            "INSERT INTO module_model_usage (module, day, served_by, calls_ok) \
             VALUES ('m', '2026-09-26', 'local', -1)",
        )
        .execute(p)
        .await;
        assert!(negative_calls_ok.is_err(), "calls_ok must be non-negative");

        let none_with_calls_ok = sqlx::query(
            "INSERT INTO module_model_usage (module, day, served_by, calls_ok) \
             VALUES ('m', '2026-09-26', 'none', 1)",
        )
        .execute(p)
        .await;
        assert!(
            none_with_calls_ok.is_err(),
            "a 'none' row can never carry calls_ok — nothing was served"
        );

        let none_with_tokens = sqlx::query(
            "INSERT INTO module_model_usage (module, day, served_by, prompt_tokens) \
             VALUES ('m', '2026-09-26', 'none', 5)",
        )
        .execute(p)
        .await;
        assert!(
            none_with_tokens.is_err(),
            "a 'none' row can never carry tokens — nothing was billed"
        );

        // positive controls: the legal shape of each rejected row above
        // inserts fine, and none of the rejected attempts above left a
        // partial row behind to conflict with these.
        let ok_none = sqlx::query(
            "INSERT INTO module_model_usage (module, day, served_by, calls_failed, calls_cancelled) \
             VALUES ('m', '2026-09-26', 'none', 1, 1)",
        )
        .execute(p)
        .await;
        assert!(
            ok_none.is_ok(),
            "a 'none' row with only calls_failed/calls_cancelled is legal"
        );
        let ok_local = sqlx::query(
            "INSERT INTO module_model_usage \
                 (module, day, served_by, calls_ok, prompt_tokens, completion_tokens) \
             VALUES ('m', '2026-09-26', 'local', 1, 10, 5)",
        )
        .execute(p)
        .await;
        assert!(
            ok_local.is_ok(),
            "a 'local' row with calls_ok and tokens is legal"
        );
    }

    // ── read-side sanity (not a J11 item, but exercises both queries) ──────

    #[tokio::test]
    async fn daily_detail_and_totals_agree_across_days_and_tiers() {
        let (store, _dir) = fresh_wal(1).await;
        store
            .record_module_model_usage(
                "m",
                "2026-09-25",
                ServedBy::Local,
                ModelUsageDelta {
                    calls_ok: 2,
                    prompt_tokens: 20,
                    completion_tokens: 10,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .record_module_model_usage(
                "m",
                "2026-09-26",
                ServedBy::Remote,
                ModelUsageDelta {
                    calls_ok: 1,
                    prompt_tokens: 5,
                    completion_tokens: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        store
            .record_module_model_usage(
                "m",
                "2026-09-26",
                ServedBy::None,
                ModelUsageDelta {
                    calls_failed: 1,
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        // since_day filters out the older day
        let since_26 = store.module_model_usage("m", "2026-09-26").await.unwrap();
        assert_eq!(since_26.len(), 2);
        assert!(since_26.iter().all(|r| r.day == "2026-09-26"));

        // every day is included with an early-enough since_day, newest first
        let all = store.module_model_usage("m", "2026-09-01").await.unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].day, "2026-09-26"); // newest day first

        let totals = store.module_model_usage_totals("m").await.unwrap();
        assert_eq!(totals.len(), 3); // local, none, remote
        let local = totals.iter().find(|r| r.served_by == "local").unwrap();
        assert_eq!(local.calls_ok, 2);
        assert_eq!(local.prompt_tokens, 20);
        assert_eq!(local.day, "", "totals rows carry no single day");
        let none = totals.iter().find(|r| r.served_by == "none").unwrap();
        assert_eq!(none.calls_failed, 1);

        // a module that never called anything gets empty results, not an error
        assert!(
            store
                .module_model_usage("never_called", "2026-01-01")
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .module_model_usage_totals("never_called")
                .await
                .unwrap()
                .is_empty()
        );
    }
}
