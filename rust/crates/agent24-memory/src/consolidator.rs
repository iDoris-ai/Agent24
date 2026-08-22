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
use crate::artifact::checksum;
use crate::event::{EventLog, EventQuery, EventStore, StoredEvent};

/// How many events one consolidation page scans. `run_once` pages to the end, so
/// the total is unbounded — unlike a single capped `scan`, which would silently
/// drop events past its limit (review #122 B2).
const CONSOLIDATE_PAGE: i64 = 10_000;

/// One consolidated insight over a group of events.
#[derive(Debug, Clone, PartialEq)]
pub struct Consolidation {
    pub id: String,
    pub owner: String,
    pub key: String,
    pub insight: String,
    /// f64, not f32: a count-based importance loses precision above 2^24 in f32
    /// (adjacent counts would compare equal), breaking strict ranking (review
    /// #122). Synths must return a FINITE value.
    pub importance: f64,
    pub source_events: Vec<String>,
    pub at: String,
}

/// Turns a group of events (all sharing a key) into an insight + importance.
/// The pluggable seam an LLM synth slots into.
///
/// **Contract: `synth` MUST be a deterministic, finite pure function of its
/// inputs** — the same `(key, events)` yields the same `(insight, importance)`,
/// and importance is finite (no NaN). The consolidation loop's idempotence and
/// "incremental == full" guarantees rest on this; an LLM-backed synth must pin
/// its output (temperature 0 / cached) to honor it (review #122 M1, same shape as
/// #121's "trust is an input invariant").
pub trait InsightSynth: Send + Sync {
    fn synth(&self, key: &str, events: &[&StoredEvent]) -> (String, f64);
}

/// Deterministic default: the insight names the group size, importance is the
/// count. Enough to test the loop; a real synth summarizes content.
#[derive(Debug, Clone, Copy, Default)]
pub struct CountSynth;

impl InsightSynth for CountSynth {
    fn synth(&self, key: &str, events: &[&StoredEvent]) -> (String, f64) {
        (
            format!("{} events of kind '{key}'", events.len()),
            events.len() as f64,
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

    /// Scan ALL of an owner's events, paging past the per-scan cap so nothing is
    /// silently dropped (review #122 B2). `page` is injectable for tests to force
    /// multiple pages without a huge corpus.
    async fn scan_all_paged(&self, owner: &str, page: i64) -> Result<Vec<StoredEvent>> {
        let log = self.events();
        let mut out = Vec::new();
        let mut cursor = 0i64;
        loop {
            let batch = log
                .scan(&EventQuery::owner(owner).after(cursor).limit(page))
                .await?;
            if batch.is_empty() {
                break;
            }
            let short = (batch.len() as i64) < page;
            if let Some(last) = batch.last() {
                cursor = last.seq;
            }
            out.extend(batch);
            if short {
                break;
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl<S: InsightSynth + Clone> Consolidator for EventConsolidator<S> {
    async fn run_once(&self, owner: &str) -> Result<usize> {
        // Recompute from ALL of the owner's events (paged, never truncated), so
        // the result depends only on what exists — never on batching (incremental
        // == full) or on how many times we have run (idempotent).
        let all = self.scan_all_paged(owner, CONSOLIDATE_PAGE).await?;

        // Group by kind, in a BTreeMap for a deterministic order.
        let mut groups: std::collections::BTreeMap<&str, Vec<&StoredEvent>> =
            std::collections::BTreeMap::new();
        for e in &all {
            groups.entry(e.event.kind.as_str()).or_default().push(e);
        }

        let mut tx = self.pool.begin_with("BEGIN IMMEDIATE").await?;
        // Drop consolidations whose key no longer has any events for this owner,
        // so the projection stays a faithful rebuild of the current log (e.g.
        // after a log repair/import removes a kind), not an append-only residue.
        let live_keys: Vec<String> = groups.keys().map(|k| (*k).to_owned()).collect();
        let placeholders = std::iter::repeat_n("?", live_keys.len())
            .collect::<Vec<_>>()
            .join(", ");
        let delete_sql = if live_keys.is_empty() {
            "DELETE FROM mem_consolidations WHERE scope_owner = ?".to_owned()
        } else {
            format!(
                "DELETE FROM mem_consolidations WHERE scope_owner = ? AND consol_key NOT IN ({placeholders})"
            )
        };
        let mut del = sqlx::query(&delete_sql).bind(owner);
        for k in &live_keys {
            del = del.bind(k);
        }
        del.execute(&mut *tx).await?;

        for (key, events) in &groups {
            let (insight, importance) = self.synth.synth(key, events);
            // Deterministic provenance + timestamp: sorted source ids, latest at.
            // (`at` uses lexicographic max, which is the time max because
            // MemEvent.at is canonical fixed-width UTC from now_iso8601.)
            let mut source_ids: Vec<&str> = events.iter().map(|e| e.event.id.as_str()).collect();
            source_ids.sort_unstable();
            let source_json = serde_json::to_string(&source_ids)?;
            let at = events
                .iter()
                .map(|e| e.event.at.as_str())
                .max()
                .unwrap_or("")
                .to_owned();
            // Collision-free display id: hash of owner+key (NUL-separated), so it
            // is not the ambiguous concat — the real identity is (owner, key).
            let id = format!("consol-{}", &checksum(&format!("{owner}\u{0}{key}"))[..16]);
            sqlx::query(
                "INSERT INTO mem_consolidations
                     (scope_owner, consol_key, id, insight, importance, source_events, at)
                 VALUES (?, ?, ?, ?, ?, ?, ?)
                 ON CONFLICT(scope_owner, consol_key) DO UPDATE SET
                     id = excluded.id, insight = excluded.insight,
                     importance = excluded.importance,
                     source_events = excluded.source_events, at = excluded.at",
            )
            .bind(owner)
            .bind(*key)
            .bind(&id)
            .bind(&insight)
            .bind(importance)
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
                    importance: r.get::<f64, _>("importance"),
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

    async fn append_at(kv: &KvStore, id: &str, owner: &str, kind: &str, at: &str) {
        let mut ev = MemEvent::new(
            id,
            Scope::owner(owner),
            kind,
            serde_json::json!({"k": id}),
            Origin {
                source: "t".to_owned(),
                trust: Trust::UserSaid,
            },
        );
        ev.at = at.to_owned();
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
        // Event timestamps are PINNED, because "incremental == full" needs the two
        // sides to be the SAME corpus and they were not. `MemEvent::new` stamps
        // `at` from the wall clock at SECOND resolution, and `Consolidation.at` is
        // the max `at` of its source events — so building two separate databases a
        // moment apart gave them different timestamps whenever the incremental
        // corpus's latest events and the full corpus's landed in different seconds.
        // (~1 failure in 15 under a loaded `cargo test --workspace`.) The
        // implementation was never at fault; the test's premise was.
        const EVENTS: [(&str, &str, &str); 4] = [
            ("a", "note", "2026-01-01T00:00:01Z"),
            ("d", "task", "2026-01-01T00:00:02Z"),
            ("b", "note", "2026-01-01T00:00:03Z"),
            ("e", "task", "2026-01-01T00:00:04Z"),
        ];

        // Incremental: append a batch, run, append another, run.
        let (kv_inc, c_inc) = consolidator().await;
        for (id, kind, at) in &EVENTS[..2] {
            append_at(&kv_inc, id, "u1", kind, at).await;
        }
        c_inc.run_once("u1").await.unwrap();
        for (id, kind, at) in &EVENTS[2..] {
            append_at(&kv_inc, id, "u1", kind, at).await;
        }
        c_inc.run_once("u1").await.unwrap();
        let incremental = c_inc.insights("u1").await.unwrap();

        // Full: all events present, one run.
        let (kv_full, c_full) = consolidator().await;
        for (id, kind, at) in &EVENTS {
            append_at(&kv_full, id, "u1", kind, at).await;
        }
        c_full.run_once("u1").await.unwrap();
        let full = c_full.insights("u1").await.unwrap();

        assert_eq!(
            incremental, full,
            "incremental consolidation equals a full rebuild"
        );
        // Pinning the inputs also lets us pin each group's `at` EXACTLY. Anything
        // looser passes for the wrong reasons: an `.all(at == 3 || at == 4)` form
        // is satisfied by an implementation that stamps every group with the
        // corpus-wide max, which violates the per-group rule at `run_once`. Being
        // exact here means the second incremental run really did advance each
        // group's `at`, and a future change that stamps from the clock fails
        // loudly instead of going flaky again.
        let by_key: std::collections::BTreeMap<&str, &str> = full
            .iter()
            .map(|c| (c.key.as_str(), c.at.as_str()))
            .collect();
        assert_eq!(
            by_key,
            std::collections::BTreeMap::from([
                ("note", "2026-01-01T00:00:03Z"),
                ("task", "2026-01-01T00:00:04Z"),
            ]),
            "each group's `at` is ITS OWN latest source event, not the clock and \
             not the corpus max: {full:?}"
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
    async fn cross_owner_with_hyphens_do_not_collide() {
        // B1: (owner="alice", kind="x-y") and (owner="alice-x", kind="y") must NOT
        // share a consolidation. The identity is the (owner, key) pair, not a
        // concatenated string.
        let (kv, c) = consolidator().await;
        append(&kv, "e1", "alice", "x-y").await;
        append(&kv, "e2", "alice-x", "y").await;
        c.run_once("alice").await.unwrap();
        c.run_once("alice-x").await.unwrap();

        let alice = c.insights("alice").await.unwrap();
        assert_eq!(alice.len(), 1);
        assert_eq!(
            alice[0].source_events,
            vec!["e1"],
            "alice keeps only her event"
        );
        let alice_x = c.insights("alice-x").await.unwrap();
        assert_eq!(alice_x.len(), 1);
        assert_eq!(
            alice_x[0].source_events,
            vec!["e2"],
            "alice-x is not clobbered"
        );
    }

    #[tokio::test]
    async fn scan_all_pages_past_the_cap() {
        // B2: a history longer than one page must be consolidated ENTIRELY. Drive
        // the pager with a small page so 25 events cross 3 pages (a single capped
        // scan would silently drop the tail).
        let (kv, c) = consolidator().await;
        for i in 0..25 {
            append(&kv, &format!("e{i}"), "u1", "note").await;
        }
        let all = c.scan_all_paged("u1", 10).await.unwrap();
        assert_eq!(all.len(), 25, "all pages recovered, not just the first");
    }

    #[tokio::test]
    async fn at_is_the_latest_source_event_time_not_wall_clock() {
        // Minor: the consolidation's `at` is the max source-event time — asserting
        // the mechanism directly, so it cannot pass with a wall-clock `at`.
        let (kv, c) = consolidator().await;
        append_at(&kv, "old", "u1", "note", "2020-01-01T00:00:00Z").await;
        append_at(&kv, "new", "u1", "note", "2023-06-01T00:00:00Z").await;
        c.run_once("u1").await.unwrap();
        assert_eq!(
            c.insights("u1").await.unwrap()[0].at,
            "2023-06-01T00:00:00Z"
        );
    }

    #[tokio::test]
    async fn stale_key_consolidation_is_removed_on_rebuild() {
        // Minor ④: a projection is rebuildable — a consolidation whose key no
        // longer has events (e.g. after a log repair) must not linger. Simulate by
        // deleting the events then re-running.
        let (kv, c) = consolidator().await;
        append(&kv, "a", "u1", "note").await;
        append(&kv, "b", "u1", "task").await;
        c.run_once("u1").await.unwrap();
        assert_eq!(c.insights("u1").await.unwrap().len(), 2);
        // Remove all 'task' events, re-run: the 'task' consolidation is dropped.
        sqlx::query("DELETE FROM mem_events WHERE scope_owner = 'u1' AND kind = 'task'")
            .execute(&c.pool)
            .await
            .unwrap();
        c.run_once("u1").await.unwrap();
        let ins = c.insights("u1").await.unwrap();
        assert_eq!(ins.len(), 1);
        assert_eq!(ins[0].key, "note");
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
