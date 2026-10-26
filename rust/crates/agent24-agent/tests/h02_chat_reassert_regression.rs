//! H02 product-path regression for the re-assert audit proof (#1153, #1154),
//! driven through the real chat memory write path:
//! `RunManager::chat_remember_turn` -> `SessionMemory::remember` -> retain ->
//! `WriteGate` (SQLite file, real session log, no mocks of the memory layer).
//!
//! - #1153: a re-assert (forget, then remember again) must leave the CURRENT
//!   evidence's commit audit behind, so the next plain remember while the
//!   memory is still active is idempotent instead of `Conflict` -> `Failed`.
//! - #1154: a chain written before #1153 (valid commit -> retract -> reassert,
//!   but the current evidence's commit audit missing) must still be accepted.
//!
//! Same-second audit-ID collision (#1152) is NOT covered here: the chat path
//! cannot inject a clock and its evidence is unique per turn. That regression
//! is the fixed-clock writer unit test in `agent24-memory/src/writer.rs`.
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

async fn active_ids(kv: &KvStore) -> Vec<String> {
    kv.assertions()
        .beliefs_as_of(&BeliefQuery::owner(OWNER))
        .await
        .unwrap()
        .into_iter()
        .map(|b| b.id)
        .collect()
}

async fn kinds(kv: &KvStore, kind: &str) -> Vec<String> {
    kv.events()
        .scan(&EventQuery::owner(OWNER))
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event.kind == kind)
        .map(|e| e.event.id)
        .collect()
}

async fn remember(m: &RunManager, answer: &str) -> MemoryWriteOutcome {
    m.chat_remember_turn(SESSION, "remember espresso", answer)
        .await
}

async fn forget_active(kv: &KvStore) {
    let id = active_ids(kv).await.pop().expect("active assertion");
    kv.forget(OWNER, &id, &now_iso8601()).await.unwrap();
}

/// #1153: remember -> forget -> remember (re-assert) -> remember (while
/// active, new evidence) -> remember again. Every call is Saved, the re-assert
/// leaves the current evidence's commit audit behind, and the idempotent
/// retries append nothing.
#[tokio::test]
async fn chat_reassert_then_remember_while_active_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("mem.db")).await.unwrap();
    let m = manager(Store::open_memory().await.unwrap(), kv.clone());

    assert_eq!(remember(&m, "1").await, MemoryWriteOutcome::Saved);
    assert_eq!(kinds(&kv, "mem.write_decision").await.len(), 1);
    forget_active(&kv).await;
    assert_eq!(remember(&m, "2").await, MemoryWriteOutcome::Saved);
    // The re-assert records its own commit proof (#1153): one per evidence.
    assert_eq!(
        kinds(&kv, "mem.write_decision").await.len(),
        2,
        "re-assert must write the current evidence's commit audit"
    );
    assert_eq!(kinds(&kv, "assertion.reasserted").await.len(), 1);

    let before = kv
        .events()
        .scan(&EventQuery::owner(OWNER))
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event.kind != "message")
        .count();
    assert_eq!(remember(&m, "3").await, MemoryWriteOutcome::Saved);
    assert_eq!(remember(&m, "4").await, MemoryWriteOutcome::Saved);
    let after = kv
        .events()
        .scan(&EventQuery::owner(OWNER))
        .await
        .unwrap()
        .into_iter()
        .filter(|e| e.event.kind != "message")
        .count();
    assert_eq!(after, before, "idempotent remembers add no audit events");
    assert_eq!(active_ids(&kv).await.len(), 1);
}

/// #1154: a legitimate pre-#1153 chain (commit -> retract -> reassert) whose
/// current evidence has no commit audit. Built on the real path, then the one
/// audit the re-assert wrote is deleted, which is exactly the old on-disk
/// shape. A later chat remember must succeed without adding events.
#[tokio::test]
async fn chat_remember_accepts_a_legacy_reassert_chain_missing_the_current_commit_audit() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mem.db");
    let kv = KvStore::open(&path).await.unwrap();
    let m = manager(Store::open_memory().await.unwrap(), kv.clone());

    assert_eq!(remember(&m, "1").await, MemoryWriteOutcome::Saved);
    forget_active(&kv).await;
    assert_eq!(remember(&m, "2").await, MemoryWriteOutcome::Saved);

    // The newest write_decision is the re-assert's proof for evidence #2.
    let proof = kinds(&kv, "mem.write_decision").await.pop().unwrap();
    let mut side = SqliteConnectOptions::new()
        .filename(&path)
        .connect()
        .await
        .unwrap();
    let deleted = sqlx::query("DELETE FROM mem_events WHERE id = ?")
        .bind(&proof)
        .execute(&mut side)
        .await
        .unwrap()
        .rows_affected();
    side.close().await.unwrap();
    assert_eq!(deleted, 1, "the legacy shape must really be constructed");
    assert_eq!(kinds(&kv, "mem.write_decision").await.len(), 1);

    let before = kinds(&kv, "assertion.reasserted").await.len();
    assert_eq!(
        remember(&m, "3").await,
        MemoryWriteOutcome::Saved,
        "legacy chain must be accepted (bounded chain compatibility)"
    );
    assert_eq!(kinds(&kv, "assertion.reasserted").await.len(), before);
    assert_eq!(
        kinds(&kv, "mem.write_decision").await.len(),
        1,
        "compat path must not backfill or duplicate audits"
    );
    assert_eq!(active_ids(&kv).await.len(), 1);
}
