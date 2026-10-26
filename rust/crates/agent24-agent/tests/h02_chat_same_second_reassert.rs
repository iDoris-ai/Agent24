//! H02 product-path regression: the chat memory write path
//! (`RunManager::chat_remember_turn` -> `SessionMemory::remember` -> retain ->
//! `WriteGate`) must survive repeated same-second re-assertions of the same
//! memory without an audit-event ID collision.
//!
//! Background (docs/research/MEMORY-P1-READINESS-PROBES-20261023.md §8.3, §9):
//! the writer derived the `assertion.reasserted` audit ID from
//! `"{now}|{evidence}"` with second precision, so two re-assertions with the
//! same evidence inside one second hit SQLite 2067 (`mem_events.id` unique).
//! Fixed by #1152 (writer.rs allocates a free suffix under the write lock).
//!
//! The product path cannot inject a clock, and its evidence (the user turn
//! event ID) is a hash of `(owner, session, turn_no)`, so a collision needs
//! the same turn number twice. This test reaches that state on the REAL path
//! by deleting the session's message events between turns (the same
//! turn-number reuse a restart-and-retry produces), and proves the
//! same-second premise from the data itself: the second re-assert ID must be
//! the first one plus a `-N` suffix. If the wall clock rolls over mid-run the
//! attempt is discarded and retried, so the premise is never assumed.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use agent24_agent::{EventSink, MemoryWriteOutcome, RunManager, SessionMemory};
use agent24_core::util::now_iso8601;
use agent24_memory::assertion::{AssertionStore, BeliefQuery};
use agent24_memory::event::{EventQuery, EventStore};
use agent24_memory::{KvStore, session::Summarizer};
use agent24_models::Msg;
use agent24_models::router::ModelRouter;
use agent24_protocol::EventBody;
use agent24_store::Store;
use agent24_tools::ToolRegistry;
use async_trait::async_trait;
use sqlx::sqlite::SqliteConnectOptions;
use sqlx::{ConnectOptions, Connection};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const OWNER: &str = "h02-chat-owner";
const SESSION: &str = "h02-session";

struct NoSummary;
#[async_trait]
impl Summarizer for NoSummary {
    async fn summarize(&self, _: Option<&str>, _: &[Msg]) -> Result<String, String> {
        panic!("h02 test must not compact");
    }
}
struct Sink;
impl EventSink for Sink {
    fn emit(&self, _: EventBody) {}
}

fn manager(store: Store, kv: KvStore) -> Arc<RunManager> {
    RunManager::with_memory(
        store,
        Arc::new(ModelRouter::with_defaults(vec![])),
        Arc::new(ToolRegistry::new()),
        Arc::new(Sink),
        CancellationToken::new(),
        Some(SessionMemory::new(kv, Arc::new(NoSummary)).with_owner(OWNER.into())),
    )
}

/// Start a fresh wall-clock second so the whole cycle (milliseconds on an
/// in-process SQLite file) has the rest of the second to itself.
async fn align_to_second_start() {
    loop {
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_millis();
        if ms < 200 {
            return;
        }
        tokio::time::sleep(Duration::from_millis(u64::from(1000 - ms) + 5)).await;
    }
}

async fn active_ids(kv: &KvStore) -> Vec<String> {
    kv.assertions()
        .beliefs_as_of(&BeliefQuery::owner(OWNER))
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect()
}

async fn reassert_ids(kv: &KvStore) -> Vec<String> {
    kv.events()
        .scan(&EventQuery::owner(OWNER))
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event.kind == "assertion.reasserted")
        .map(|e| e.event.id)
        .collect()
}

/// One attempt. `Ok(Some(ids))` = premise met (same second, collision shape);
/// `Ok(None)` = the clock rolled over, discard and retry.
async fn attempt(dir: &std::path::Path, n: usize) -> Option<Vec<String>> {
    let path = dir.join(format!("mem-{n}.db"));
    let kv = KvStore::open(&path).await.unwrap();
    let m = manager(Store::open_memory().await.unwrap(), kv.clone());
    let mut side = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();

    align_to_second_start().await;
    // Turn 0: first remember (a plain commit, not a re-assert).
    assert_eq!(
        m.chat_remember_turn(SESSION, "remember espresso", "ok")
            .await,
        MemoryWriteOutcome::Saved
    );
    for round in 0..2 {
        let id = active_ids(&kv).await.pop().expect("active assertion");
        kv.forget(OWNER, &id, &now_iso8601()).await.unwrap();
        // Make the next chat turn reuse turn_no 0 => the same evidence event ID.
        sqlx::query("DELETE FROM mem_events WHERE kind = 'message'")
            .execute(&mut side)
            .await
            .unwrap();
        let out = m
            .chat_remember_turn(SESSION, "remember espresso", "again")
            .await;
        assert_eq!(
            out,
            MemoryWriteOutcome::Saved,
            "re-assert #{} through chat_remember_turn must not fail (audit ID collision?)",
            round + 1
        );
    }
    side.close().await.unwrap();

    let ids = reassert_ids(&kv).await;
    assert_eq!(
        ids.len(),
        2,
        "each re-assert is audited exactly once: {ids:?}"
    );
    assert_eq!(active_ids(&kv).await.len(), 1, "memory is active again");
    let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(distinct.len(), 2, "audit IDs must be distinct: {ids:?}");
    // Same-second premise: second ID = first ID + "-N" (same base hash).
    let same_second = ids[1].starts_with(&format!("{}-", ids[0]));
    same_second.then_some(ids)
}

#[tokio::test]
async fn chat_remember_same_second_reassert_gets_distinct_audit_ids() {
    let dir = tempfile::tempdir().unwrap();
    for n in 0..5 {
        if let Some(ids) = attempt(dir.path(), n).await {
            println!("H02 chat same-second reassert audit ids: {ids:?}");
            return;
        }
    }
    panic!("could not land two re-asserts in one wall-clock second in 5 attempts");
}

/// Control: with ordinary (unique) evidence per turn the product path keeps
/// working across forget/re-remember cycles; IDs differ without any suffix.
#[tokio::test]
async fn chat_remember_unique_evidence_cycles_stay_saved() {
    let kv = KvStore::open_memory().await.unwrap();
    let m = manager(Store::open_memory().await.unwrap(), kv.clone());
    assert_eq!(
        m.chat_remember_turn(SESSION, "remember espresso", "ok")
            .await,
        MemoryWriteOutcome::Saved
    );
    for _ in 0..3 {
        let id = active_ids(&kv).await.pop().unwrap();
        kv.forget(OWNER, &id, &now_iso8601()).await.unwrap();
        assert_eq!(
            m.chat_remember_turn(SESSION, "remember espresso", "again")
                .await,
            MemoryWriteOutcome::Saved
        );
    }
    let ids = reassert_ids(&kv).await;
    let distinct: std::collections::BTreeSet<_> = ids.iter().collect();
    assert_eq!(ids.len(), 3);
    assert_eq!(distinct.len(), 3);
}
