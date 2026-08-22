//! MD-5: the consolidation loop — background "sleep synthesis" that folds
//! episodic events into importance-ranked insights (SPEC-MD-ME §3 MD-5;
//! memobase/MemoryScope observation→insight, "the ceiling is the consolidation
//! mechanism, not the vector store").
//!
//! [`Consolidator::run_once`] groups an owner's events by key (the event `kind`),
//! synthesizes one [`Consolidation`] per group via a pluggable [`InsightSynth`],
//! and UPSERTs it. Each consolidation is a PURE FUNCTION of ALL events sharing its
//! key, which gives the three MD-5 acceptance properties for free:
//! - **idempotent** — re-running yields byte-identical rows (the id is stable and
//!   the `at` is the latest source event's time, not wall-clock);
//! - **incremental == full** — a consolidation depends only on the events that
//!   exist, not on how they were batched, so folding events in over several runs
//!   ends in the same state as one run over all of them;
//! - **importance-ranked** — [`Consolidator::insights`] returns them by importance.
//!
//! Insight SYNTHESIS is the pluggable, later-LLM-backed part ([`InsightSynth`]);
//! the default [`CountSynth`] is deterministic (count-weighted) so the loop
//! mechanics are tested without a model. Re-consolidating from all events every
//! run is O(events); a checkpoint-gated incremental pass is a future optimization
//! (the correctness properties above must survive it), noted not faked.

use async_trait::async_trait;
use sqlx::{Row, SqlitePool};

use crate::Result;
use crate::event::{EventLog, EventQuery, EventStore, StoredEvent};

/// One consolidated insight over a group of events.
#[derive(Debug, Clone, PartialEq)]
pub struct Consolidation {
    pub id: String,
    pub owner: String,
    pub key: String,
    pub insight: String,
    pub importance: f32,
    pub source_events: Vec<String>,
    pub at: String,
}

/// Turns a group of events (all sharing a key) into an insight + importance.
/// The pluggable seam an LLM synth slots into; the default is deterministic.
pub trait InsightSynth: Send + Sync {
    fn synth(&self, key: &str, events: &[&StoredEvent]) -> (String, f32);
}

/// Deterministic default: the insight names the group size, importance is the
/// count. Enough to test the loop; a real synth summarizes content.
#[derive(Debug, Clone, Copy, Default)]
pub struct CountSynth;

impl InsightSynth for CountSynth {
    fn synth(&self, key: &str, events: &[&StoredEvent]) -> (String, f32) {
        (
            format!("{} events of kind '{key}'", events.len()),
            events.len() as f32,
        )
    }
}

/// The consolidation loop.
#[async_trait]
pub trait Consolidator: Send + Sync {
    /// Re-consolidate `owner`'s events: one insight per event `kind`. Returns the
    /// number of consolidation groups written. Idempotent; incremental == full.
    async fn run_once(&self, owner: &str) -> Result<usize>;
    /// An owner's consolidations, most important first.
    async fn insights(&self, owner: &str) -> Result<Vec<Consolidation>>;
}

/// SQLite-backed [`Consolidator`] over the shared memory DB.
#[derive(Clone)]
pub struct EventConsolidator<S: InsightSynth + Clone> {
    pool: SqlitePool,
    synth: S,
}

impl<S: InsightSynth + Clone> EventConsolidator<S> {
    pub(crate) fn new(pool: SqlitePool, synth: S) -> Self {
        Self { pool, synth }
    }

    fn events(&self) -> EventLog {
        EventLog::new(self.pool.clone())
    }
}

#[async_trait]
impl<S: InsightSynth + Clone> Consolidator for EventConsolidator<S> {
    async fn run_once(&self, owner: &str) -> Result<usize> {
        // Recompute from ALL of the owner's events, so the result depends only on
        // what exists — never on batching (incremental == full) or on how many
        // times we have run (idempotent).
        let all = self.events().scan(&EventQuery::owner(owner)).await?;

        // Group by kind, in a BTreeMap for a deterministic order.
        let mut groups: std::collections::BTreeMap<&str, Vec<&StoredEvent>> =
            std::collections::BTreeMap::new();
        for e in &all {
            groups.entry(e.event.kind.as_str()).or_default().push(e);
        }

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        for (key, events) in &groups {
            let (insight, importance) = self.synth.synth(key, events);
            // Deterministic provenance + timestamp: sorted source ids, latest at.
            let mut source_ids: Vec<&str> = events.iter().map(|e| e.event.id.as_str()).collect();
            source_ids.sort_unstable();
            let source_json = serde_json::to_string(&source_ids)?;
            let at = events
                .iter()
                .map(|e| e.event.at.as_str())
                .max()
                .unwrap_or("")
                .to_owned();
            let id = format!("consol-{owner}-{key}");
            sqlx::query(
                "INSERT INTO mem_consolidations
                     (id, scope_owner, consol_key, insight, importance, source_events, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(id) DO UPDATE SET
                     insight = excluded.insight, importance = excluded.importance,
                     source_events = excluded.source_events, at = excluded.at",
            )
            .bind(&id)
            .bind(owner)
            .bind(*key)
            .bind(&insight)
            .bind(importance as f64)
            .bind(&source_json)
            .bind(&at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(groups.len())
    }

    async fn insights(&self, owner: &str) -> Result<Vec<Consolidation>> {
        let rows = sqlx::query(
            "SELECT id, scope_owner, consol_key, insight, importance, source_events, at
             FROM mem_consolidations WHERE scope_owner = ?
             ORDER BY importance DESC, id ASC",
        )
        .bind(owner)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Consolidation {
                    id: r.get("id"),
                    owner: r.get("scope_owner"),
                    key: r.get("consol_key"),
                    insight: r.get("insight"),
                    importance: r.get::<f64, _>("importance") as f32,
                    source_events: serde_json::from_str(&r.get::<String, _>("source_events"))?,
                    at: r.get("at"),
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::KvStore;
    use crate::event::{MemEvent, Origin, Scope, Trust};

    async fn consolidator() -> (KvStore, EventConsolidator<CountSynth>) {
        let kv = KvStore::open_memory().await.unwrap();
        let c = kv.consolidator();
        (kv, c)
    }

    async fn append(kv: &KvStore, id: &str, owner: &str, kind: &str) {
        let ev = MemEvent::new(
            id,
            Scope::owner(owner),
            kind,
            serde_json::json!({"k": id}),
            Origin {
                source: "t".to_owned(),
                trust: Trust::UserSaid,
            },
        );
        kv.events().append(&ev).await.unwrap();
    }

    #[tokio::test]
    async fn consolidates_one_insight_per_kind() {
        let (kv, c) = consolidator().await;
        append(&kv, "a", "u1", "note").await;
        append(&kv, "b", "u1", "note").await;
        append(&kv, "d", "u1", "task").await;
        assert_eq!(c.run_once("u1").await.unwrap(), 2, "two kinds");
        let ins = c.insights("u1").await.unwrap();
        assert_eq!(ins.len(), 2);
        // "note" has 2 events, "task" has 1 → note ranks first (importance).
        assert_eq!(ins[0].key, "note");
        assert_eq!(ins[0].importance, 2.0);
        assert_eq!(ins[0].source_events, vec!["a", "b"]);
        assert_eq!(ins[1].key, "task");
    }

    #[tokio::test]
    async fn is_idempotent() {
        let (kv, c) = consolidator().await;
        append(&kv, "a", "u1", "note").await;
        append(&kv, "b", "u1", "note").await;
        c.run_once("u1").await.unwrap();
        let first = c.insights("u1").await.unwrap();
        // Re-run with no new events: byte-identical (stable id, deterministic at).
        c.run_once("u1").await.unwrap();
        let second = c.insights("u1").await.unwrap();
        assert_eq!(first, second, "re-run reproduces the same consolidations");
        assert_eq!(first.len(), 1);
    }

    #[tokio::test]
    async fn incremental_equals_full_rerun() {
        // Incremental: append a batch, run, append another, run.
        let (kv_inc, c_inc) = consolidator().await;
        append(&kv_inc, "a", "u1", "note").await;
        append(&kv_inc, "d", "u1", "task").await;
        c_inc.run_once("u1").await.unwrap();
        append(&kv_inc, "b", "u1", "note").await;
        append(&kv_inc, "e", "u1", "task").await;
        c_inc.run_once("u1").await.unwrap();
        let incremental = c_inc.insights("u1").await.unwrap();

        // Full: all events present, one run.
        let (kv_full, c_full) = consolidator().await;
        for (id, kind) in [("a", "note"), ("d", "task"), ("b", "note"), ("e", "task")] {
            append(&kv_full, id, "u1", kind).await;
        }
        c_full.run_once("u1").await.unwrap();
        let full = c_full.insights("u1").await.unwrap();

        assert_eq!(
            incremental, full,
            "incremental consolidation equals a full rebuild"
        );
    }

    #[tokio::test]
    async fn scope_isolation_zero_cross_owner_leak() {
        let (kv, c) = consolidator().await;
        append(&kv, "a", "alice", "note").await;
        append(&kv, "b", "bob", "note").await;
        c.run_once("alice").await.unwrap();
        c.run_once("bob").await.unwrap();
        let alice = c.insights("alice").await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(alice[0].source_events, vec!["a"], "only alice's events");
    }

    #[tokio::test]
    async fn importance_orders_insights() {
        let (kv, c) = consolidator().await;
        for id in ["n1", "n2", "n3"] {
            append(&kv, id, "u1", "frequent").await;
        }
        append(&kv, "r1", "u1", "rare").await;
        c.run_once("u1").await.unwrap();
        let ins = c.insights("u1").await.unwrap();
        assert_eq!(ins[0].key, "frequent", "3 events outranks 1");
        assert!(ins[0].importance > ins[1].importance);
    }

    #[tokio::test]
    async fn new_events_update_the_consolidation() {
        let (kv, c) = consolidator().await;
        append(&kv, "a", "u1", "note").await;
        c.run_once("u1").await.unwrap();
        assert_eq!(c.insights("u1").await.unwrap()[0].importance, 1.0);
        // A new event of the same kind re-derives the group (importance grows).
        append(&kv, "b", "u1", "note").await;
        c.run_once("u1").await.unwrap();
        let ins = c.insights("u1").await.unwrap();
        assert_eq!(ins.len(), 1, "still one group, updated not duplicated");
        assert_eq!(ins[0].importance, 2.0);
        assert_eq!(ins[0].source_events, vec!["a", "b"]);
    }
}
