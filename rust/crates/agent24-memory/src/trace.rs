//! MD-8: the symbolic task trace (H1/H2) — spill full tool output to a `ref` and
//! keep only a compact SYMBOLIC node in the prompt, with `node_id` drill-down
//! (SPEC-MD-ME §3 MD-8; TencentDB's symbolic graph + drill-down).
//!
//! The property that matters: **compression is RECOVERABLE, not truncating**.
//! [`TaskTrace::record`] stores the full body verbatim in a content-addressed ref
//! and returns a [`TraceNode`] holding a one-line `symbol` plus that `ref_id`.
//! [`TaskTrace::drill`] returns the ORIGINAL body byte-for-byte —
//! `symbolize(record(x)) → drill → x` for every x. Nothing is discarded, so a
//! trace can be 100% reconstructed ([`TaskTrace::expand_run`]).
//!
//! Refs are content-addressed WITHIN AN OWNER (`(scope_owner, ref_id)`): a tool
//! that emits the same output twice for one owner stores one body (dedup) while
//! each occurrence still gets its own node. Dedup deliberately does NOT cross
//! owners — a globally-content-addressed ref would be owned by whoever recorded
//! it first, and the second owner's `drill` would return `None` while
//! `expand_run` silently returned a SHORTER trace (review #125 B1).
//!
//! A node's identity is its natural key `(scope_owner, run_id, seq)` — one step
//! of one run — so re-recording a step is a clean idempotent upsert rather than a
//! collision against a second, competing identity (review #125 M1/M2).
//!
//! Scope: everything is owner-scoped — a foreign `node_id`/`ref_id` cannot drill
//! into another owner's trace (the #119 lesson).

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::Result;
use crate::artifact::checksum;

/// One symbolic step in a run's trace: what happened, in one line, plus the
/// drill-down handle to the full body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceNode {
    pub node_id: String,
    pub owner: String,
    pub run_id: String,
    pub seq: i64,
    pub kind: String,
    /// The compact line kept in the prompt.
    pub symbol: String,
    /// Drill-down target: the full body lives here, never truncated.
    pub ref_id: String,
}

/// How much a run's trace compressed: symbol bytes actually kept in-prompt vs the
/// full bodies preserved behind refs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraceStats {
    pub nodes: usize,
    pub symbol_bytes: usize,
    pub full_bytes: usize,
}

impl TraceStats {
    /// Fraction of the original bytes NOT carried in the prompt (0.0 if nothing
    /// was recorded). Purely informational — recoverability does not depend on it.
    pub fn compression_ratio(&self) -> f64 {
        if self.full_bytes == 0 {
            0.0
        } else {
            1.0 - (self.symbol_bytes as f64 / self.full_bytes as f64)
        }
    }
}

/// The symbolic trace store.
#[async_trait]
pub trait TaskTrace: Send + Sync {
    /// Record one step: spill `body` to a content-addressed ref and return the
    /// symbolic node. `seq` orders the step within `run_id`.
    async fn record(
        &self,
        owner: &str,
        run_id: &str,
        seq: i64,
        kind: &str,
        symbol: &str,
        body: &str,
    ) -> Result<TraceNode>;
    /// The run's symbolic nodes in order — what a prompt carries.
    async fn symbols(&self, owner: &str, run_id: &str) -> Result<Vec<TraceNode>>;
    /// Drill a node back down to its FULL original body (owner-scoped).
    async fn drill(&self, node_id: &str, owner: &str) -> Result<Option<String>>;
    /// Every step's full body, in order — proof the trace is 100% recoverable.
    async fn expand_run(&self, owner: &str, run_id: &str) -> Result<Vec<String>>;
    /// Compression stats for a run.
    async fn stats(&self, owner: &str, run_id: &str) -> Result<TraceStats>;
}

/// SQLite-backed [`TaskTrace`] over the shared memory DB.
#[derive(Clone)]
pub struct SymbolicTrace {
    pool: SqlitePool,
}

impl SymbolicTrace {
    pub(crate) fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }

    fn row_to_node(row: &sqlx::sqlite::SqliteRow) -> TraceNode {
        TraceNode {
            node_id: row.get("node_id"),
            owner: row.get("scope_owner"),
            run_id: row.get("run_id"),
            seq: row.get("seq"),
            kind: row.get("kind"),
            symbol: row.get("symbol"),
            ref_id: row.get("ref_id"),
        }
    }
}

#[async_trait]
impl TaskTrace for SymbolicTrace {
    async fn record(
        &self,
        owner: &str,
        run_id: &str,
        seq: i64,
        kind: &str,
        symbol: &str,
        body: &str,
    ) -> Result<TraceNode> {
        // Content-addressed WITHIN the owner (the table's key is
        // (scope_owner, ref_id)): identical bodies dedupe for one owner, but two
        // owners recording the same bytes each keep their own row.
        let ref_id = format!("ref-{}", &checksum(body)[..32]);
        // A derived handle for drill-down. Full 32-hex over (owner, run) so a
        // collision is not a realistic silent-overwrite path; the row's identity
        // is the natural key below, not this.
        let node_id = format!(
            "node-{}-{}",
            &checksum(&format!("{owner}\u{0}{run_id}"))[..32],
            seq
        );
        let now = agent24_core::util::now_iso8601();

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query(
            "INSERT INTO mem_trace_refs (scope_owner, ref_id, body, bytes, at)
             VALUES (?, ?, ?, ?, ?)
             ON CONFLICT(scope_owner, ref_id) DO NOTHING",
        )
        .bind(owner)
        .bind(&ref_id)
        .bind(body)
        .bind(body.len() as i64)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        // Conflict on the NATURAL key — re-recording a step is an idempotent
        // update, never a collision against a second identity (review #125 M1).
        sqlx::query(
            "INSERT INTO mem_trace_nodes
                 (scope_owner, run_id, seq, node_id, kind, symbol, ref_id, at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(scope_owner, run_id, seq) DO UPDATE SET
                 node_id = excluded.node_id, kind = excluded.kind,
                 symbol = excluded.symbol, ref_id = excluded.ref_id,
                 at = excluded.at",
        )
        .bind(owner)
        .bind(run_id)
        .bind(seq)
        .bind(&node_id)
        .bind(kind)
        .bind(symbol)
        .bind(&ref_id)
        .bind(&now)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;

        Ok(TraceNode {
            node_id,
            owner: owner.to_owned(),
            run_id: run_id.to_owned(),
            seq,
            kind: kind.to_owned(),
            symbol: symbol.to_owned(),
            ref_id,
        })
    }

    async fn symbols(&self, owner: &str, run_id: &str) -> Result<Vec<TraceNode>> {
        let rows = sqlx::query(
            "SELECT node_id, scope_owner, run_id, seq, kind, symbol, ref_id
             FROM mem_trace_nodes WHERE scope_owner = ? AND run_id = ?
             ORDER BY seq ASC",
        )
        .bind(owner)
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(Self::row_to_node).collect())
    }

    async fn drill(&self, node_id: &str, owner: &str) -> Result<Option<String>> {
        // Owner-scoped on BOTH the node and the ref: a foreign node_id cannot
        // reach another owner's body.
        let row = sqlx::query(
            "SELECT r.body AS body
             FROM mem_trace_nodes n
             JOIN mem_trace_refs r ON r.ref_id = n.ref_id AND r.scope_owner = n.scope_owner
             WHERE n.node_id = ? AND n.scope_owner = ?",
        )
        .bind(node_id)
        .bind(owner)
        .fetch_optional(&self.pool)
        .await?;
        Ok(row.map(|r| r.get::<String, _>("body")))
    }

    async fn expand_run(&self, owner: &str, run_id: &str) -> Result<Vec<String>> {
        let rows = sqlx::query(
            "SELECT r.body AS body
             FROM mem_trace_nodes n
             JOIN mem_trace_refs r ON r.ref_id = n.ref_id AND r.scope_owner = n.scope_owner
             WHERE n.scope_owner = ? AND n.run_id = ?
             ORDER BY n.seq ASC",
        )
        .bind(owner)
        .bind(run_id)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(|r| r.get::<String, _>("body")).collect())
    }

    async fn stats(&self, owner: &str, run_id: &str) -> Result<TraceStats> {
        let nodes = self.symbols(owner, run_id).await?;
        let symbol_bytes = nodes.iter().map(|n| n.symbol.len()).sum();
        let full_bytes = self
            .expand_run(owner, run_id)
            .await?
            .iter()
            .map(|b| b.len())
            .sum();
        Ok(TraceStats {
            nodes: nodes.len(),
            symbol_bytes,
            full_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;

    async fn trace() -> SymbolicTrace {
        KvStore::open_memory().await.unwrap().trace()
    }

    #[tokio::test]
    async fn record_then_drill_returns_the_full_body_verbatim() {
        let t = trace().await;
        let body = "line one\nline two\n\u{4f60}\u{597d} 🌍\nembedded \" quote";
        let node = t
            .record("u1", "run1", 0, "shell", "ran ls (4 lines)", body)
            .await
            .unwrap();
        let got = t.drill(&node.node_id, "u1").await.unwrap().unwrap();
        assert_eq!(got, body, "drill-down is byte-for-byte, not truncated");
    }

    #[tokio::test]
    async fn compression_is_recoverable_not_truncating() {
        // The core MD-8 acceptance: the prompt keeps small symbols, but 100% of
        // the original is recoverable.
        let t = trace().await;
        let bodies: Vec<String> = (0..5)
            .map(|i| format!("HUGE OUTPUT {i}: {}", "x".repeat(2000)))
            .collect();
        for (i, b) in bodies.iter().enumerate() {
            t.record("u1", "run1", i as i64, "tool", &format!("step {i}"), b)
                .await
                .unwrap();
        }
        let stats = t.stats("u1", "run1").await.unwrap();
        assert_eq!(stats.nodes, 5);
        assert!(
            stats.symbol_bytes < stats.full_bytes / 100,
            "symbols are tiny"
        );
        assert!(stats.compression_ratio() > 0.99);
        // …and every byte is still there.
        assert_eq!(
            t.expand_run("u1", "run1").await.unwrap(),
            bodies,
            "100% recoverable"
        );
    }

    #[tokio::test]
    async fn symbols_are_ordered_and_carry_drill_handles() {
        let t = trace().await;
        for i in 0..3 {
            t.record(
                "u1",
                "run1",
                i,
                "tool",
                &format!("s{i}"),
                &format!("body{i}"),
            )
            .await
            .unwrap();
        }
        let syms = t.symbols("u1", "run1").await.unwrap();
        assert_eq!(
            syms.iter().map(|n| n.symbol.as_str()).collect::<Vec<_>>(),
            vec!["s0", "s1", "s2"]
        );
        // Each symbol drills to its own body.
        for (i, n) in syms.iter().enumerate() {
            assert_eq!(
                t.drill(&n.node_id, "u1").await.unwrap().unwrap(),
                format!("body{i}")
            );
        }
    }

    #[tokio::test]
    async fn identical_bodies_dedupe_to_one_ref_but_keep_distinct_nodes() {
        let t = trace().await;
        let same = "identical tool output";
        let a = t
            .record("u1", "run1", 0, "tool", "first", same)
            .await
            .unwrap();
        let b = t
            .record("u1", "run1", 1, "tool", "second", same)
            .await
            .unwrap();
        assert_eq!(a.ref_id, b.ref_id, "content-addressed → one stored body");
        assert_ne!(a.node_id, b.node_id, "but two distinct occurrences");
        let refs: i64 = sqlx::query("SELECT COUNT(*) AS n FROM mem_trace_refs")
            .fetch_one(&t.pool)
            .await
            .unwrap()
            .get("n");
        assert_eq!(refs, 1);
        // Both still expand to the same full body.
        assert_eq!(t.expand_run("u1", "run1").await.unwrap(), vec![same, same]);
    }

    #[tokio::test]
    async fn identical_body_across_owners_is_fully_recoverable_by_both() {
        // B1: a globally content-addressed ref would be owned by whoever recorded
        // it first — the second owner's drill would be None and expand_run would
        // silently return a SHORTER trace. Dedup is per-owner, so both recover.
        let t = trace().await;
        let shared = "identical tool output";
        let a = t
            .record("alice", "runA", 0, "tool", "a-sym", shared)
            .await
            .unwrap();
        let b = t
            .record("bob", "runB", 0, "tool", "b-sym", shared)
            .await
            .unwrap();
        assert_eq!(
            t.drill(&a.node_id, "alice").await.unwrap().as_deref(),
            Some(shared)
        );
        assert_eq!(
            t.drill(&b.node_id, "bob").await.unwrap().as_deref(),
            Some(shared),
            "the second owner recovers its own body"
        );
        // Neither run is silently short, and stats count the same step set.
        assert_eq!(t.expand_run("bob", "runB").await.unwrap(), vec![shared]);
        let s = t.stats("bob", "runB").await.unwrap();
        assert_eq!(s.nodes, 1);
        assert_eq!(s.full_bytes, shared.len(), "denominator covers every step");
    }

    #[tokio::test]
    async fn expand_run_never_silently_drops_a_step() {
        // The same shape with MULTIPLE steps, one of them a body alice recorded
        // first: bob's run must expand to ALL of its steps.
        let t = trace().await;
        let shared = "same bytes";
        t.record("alice", "runA", 0, "tool", "a", shared)
            .await
            .unwrap();
        t.record("bob", "runB", 0, "tool", "b0", "unique-first")
            .await
            .unwrap();
        t.record("bob", "runB", 1, "tool", "b1", shared)
            .await
            .unwrap();
        assert_eq!(
            t.expand_run("bob", "runB").await.unwrap(),
            vec!["unique-first", shared],
            "no step is dropped by the owner-scoped join"
        );
        assert_eq!(t.stats("bob", "runB").await.unwrap().nodes, 2);
    }

    #[tokio::test]
    async fn re_recording_a_step_with_new_content_updates_it_cleanly() {
        // M1: the natural key (owner, run, seq) is the conflict identity, so
        // re-recording a step — even with different content, hence a different
        // ref — is an idempotent UPDATE, not a collision error.
        let t = trace().await;
        t.record("u1", "run1", 0, "tool", "v1", "BODY-ONE")
            .await
            .unwrap();
        t.record("u1", "run1", 0, "tool", "v2", "BODY-TWO")
            .await
            .unwrap();
        let syms = t.symbols("u1", "run1").await.unwrap();
        assert_eq!(syms.len(), 1, "still one step");
        assert_eq!(syms[0].symbol, "v2");
        assert_eq!(t.expand_run("u1", "run1").await.unwrap(), vec!["BODY-TWO"]);
    }

    #[tokio::test]
    async fn drill_is_owner_scoped() {
        let t = trace().await;
        let node = t
            .record("alice", "run1", 0, "tool", "secret step", "ALICE SECRET")
            .await
            .unwrap();
        // bob holding alice's node_id gets nothing.
        assert!(t.drill(&node.node_id, "bob").await.unwrap().is_none());
        assert_eq!(
            t.drill(&node.node_id, "alice").await.unwrap().unwrap(),
            "ALICE SECRET"
        );
    }

    #[tokio::test]
    async fn runs_and_owners_are_isolated() {
        let t = trace().await;
        t.record("u1", "runA", 0, "tool", "a", "bodyA")
            .await
            .unwrap();
        t.record("u1", "runB", 0, "tool", "b", "bodyB")
            .await
            .unwrap();
        t.record("u2", "runA", 0, "tool", "c", "bodyC")
            .await
            .unwrap();
        assert_eq!(t.expand_run("u1", "runA").await.unwrap(), vec!["bodyA"]);
        assert_eq!(t.expand_run("u1", "runB").await.unwrap(), vec!["bodyB"]);
        assert_eq!(t.expand_run("u2", "runA").await.unwrap(), vec!["bodyC"]);
    }

    #[tokio::test]
    async fn re_recording_the_same_step_is_idempotent() {
        let t = trace().await;
        t.record("u1", "run1", 0, "tool", "s", "body")
            .await
            .unwrap();
        t.record("u1", "run1", 0, "tool", "s", "body")
            .await
            .unwrap();
        assert_eq!(t.symbols("u1", "run1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn empty_owner_is_rejected() {
        let t = trace().await;
        let err = t
            .record("  ", "run1", 0, "tool", "s", "body")
            .await
            .unwrap_err();
        assert!(matches!(err, crate::MemoryError::Sqlx(_)), "{err}");
    }

    #[tokio::test]
    async fn stats_on_an_empty_run_are_zero_not_a_panic() {
        let t = trace().await;
        let s = t.stats("u1", "nope").await.unwrap();
        assert_eq!(s.nodes, 0);
        assert_eq!(s.compression_ratio(), 0.0, "no divide-by-zero");
    }
}
