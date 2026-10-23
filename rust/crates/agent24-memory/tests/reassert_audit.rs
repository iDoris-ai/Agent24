//! Fresh-transition repair controls; legacy audit repair remains a separate slice.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use agent24_core::util::now_iso8601;
use agent24_memory::assertion::AssertionStore;
use agent24_memory::event::{EventQuery, EventStore, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter};
use agent24_memory::{KvStore, MemoryError};
use serde_json::json;

fn candidate(evidence: &str) -> Candidate {
    Candidate::new(
        "reassert-audit",
        Scope::owner("owner"),
        "user",
        "prefers",
        json!("espresso"),
        Origin {
            source: "user".into(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(vec![evidence.into()])
    .remember()
}

async fn closed(kv: &KvStore) {
    kv.write_gate()
        .propose(vec![candidate("ev1")])
        .await
        .unwrap();
    kv.forget("owner", "reassert-audit", &now_iso8601())
        .await
        .unwrap();
}

#[tokio::test]
async fn fresh_reassert_records_the_current_authorized_commit_proof() {
    let kv = KvStore::open_memory().await.unwrap();
    closed(&kv).await;
    kv.write_gate()
        .propose(vec![candidate("ev2")])
        .await
        .unwrap();
    let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    assert!(before.iter().any(
        |e| e.event.kind == "mem.write_decision" && e.event.body["evidence"] == json!(["ev2"])
    ));
    kv.write_gate()
        .propose(vec![candidate("ev3")])
        .await
        .unwrap();
    assert_eq!(
        kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
        before
    );
}

#[tokio::test]
async fn reassert_reuses_an_existing_valid_commit_without_duplicate_audit() {
    let kv = KvStore::open_memory().await.unwrap();
    closed(&kv).await;
    kv.write_gate()
        .propose(vec![candidate("ev1")])
        .await
        .unwrap();
    kv.write_gate()
        .propose(vec![candidate("ev3")])
        .await
        .unwrap();
    let events = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event.kind == "mem.write_decision")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| e.event.kind == "assertion.reasserted")
            .count(),
        1
    );
}

#[tokio::test]
async fn reassert_rejects_a_forged_expected_commit_and_leaves_state_closed() {
    let proof = KvStore::open_memory().await.unwrap();
    proof
        .write_gate()
        .propose(vec![candidate("ev2")])
        .await
        .unwrap();
    let mut audit = proof
        .events()
        .scan(&EventQuery::owner("owner"))
        .await
        .unwrap()[0]
        .event
        .clone();
    audit.body["reason"] = json!("forged");
    let kv = KvStore::open_memory().await.unwrap();
    closed(&kv).await;
    kv.events().append(&audit).await.unwrap();
    let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    let result = kv.write_gate().propose(vec![candidate("ev2")]).await;
    assert!(
        matches!(result, Err(MemoryError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(
        kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
        before
    );
    assert!(
        kv.assertions()
            .beliefs_as_of(&agent24_memory::assertion::BeliefQuery::owner("owner"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn rejected_reassert_proof_rolls_back_the_entire_transition() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    closed(&kv).await;
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    sqlx::query(
        "CREATE TRIGGER reject_reassert_proof BEFORE INSERT ON mem_events
        WHEN NEW.kind = 'mem.write_decision'
        BEGIN SELECT RAISE(ABORT, 'injected reassert proof failure'); END",
    )
    .execute(&pool)
    .await
    .unwrap();
    let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    let result = kv.write_gate().propose(vec![candidate("ev2")]).await;
    assert!(
        result
            .as_ref()
            .unwrap_err()
            .to_string()
            .contains("injected reassert proof failure")
    );
    assert_eq!(
        kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
        before
    );
    assert!(
        kv.assertions()
            .beliefs_as_of(&agent24_memory::assertion::BeliefQuery::owner("owner"))
            .await
            .unwrap()
            .is_empty()
    );
    pool.close().await;
}

#[tokio::test]
async fn historical_reassert_without_current_commit_retries_without_new_events() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    closed(&kv).await;
    kv.write_gate()
        .propose(vec![candidate("ev2")])
        .await
        .unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    // Reproduce the old ledger shape, whether or not new writes add the proof.
    let events = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    for event in events.iter().filter(|e| {
        e.event.kind == "mem.write_decision" && e.event.body["evidence"] == json!(["ev2"])
    }) {
        sqlx::query("DELETE FROM mem_events WHERE id = ?")
            .bind(&event.event.id)
            .execute(&pool)
            .await
            .unwrap();
    }
    let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    let retry = kv.write_gate().propose(vec![candidate("ev3")]).await;
    assert_eq!(
        kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
        before
    );
    assert!(
        retry.is_ok(),
        "historical legitimate reassert needs compatibility: {retry:?}"
    );
    pool.close().await;
}

async fn legacy_shape(kv: &KvStore, path: &std::path::Path) -> sqlx::SqlitePool {
    closed(kv).await;
    kv.write_gate()
        .propose(vec![candidate("ev2")])
        .await
        .unwrap();
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", path.display()))
        .await
        .unwrap();
    let events = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    for event in events.iter().filter(|e| {
        e.event.kind == "mem.write_decision" && e.event.body["evidence"] == json!(["ev2"])
    }) {
        sqlx::query("DELETE FROM mem_events WHERE id = ?")
            .bind(&event.event.id)
            .execute(&pool)
            .await
            .unwrap();
    }
    pool
}

#[tokio::test]
async fn forged_expected_audit_does_not_fall_back_to_an_older_chain() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let pool = legacy_shape(&kv, &path).await;
    let proof = KvStore::open_memory().await.unwrap();
    proof
        .write_gate()
        .propose(vec![candidate("ev2")])
        .await
        .unwrap();
    let mut forged = proof
        .events()
        .scan(&EventQuery::owner("owner"))
        .await
        .unwrap()[0]
        .event
        .clone();
    forged.body["reason"] = json!("forged");
    kv.events().append(&forged).await.unwrap();
    let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    let result = kv.write_gate().propose(vec![candidate("ev3")]).await;
    assert!(
        matches!(result, Err(MemoryError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(
        kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
        before
    );
    pool.close().await;
}

#[tokio::test]
async fn legacy_chain_missing_original_commit_or_reassert_is_rejected() {
    for drop_kind in ["mem.write_decision", "assertion.reasserted"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.sqlite");
        let kv = KvStore::open(&path).await.unwrap();
        let pool = legacy_shape(&kv, &path).await;
        sqlx::query("DELETE FROM mem_events WHERE kind = ? AND scope_owner = 'owner'")
            .bind(drop_kind)
            .execute(&pool)
            .await
            .unwrap();
        let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
        let result = kv.write_gate().propose(vec![candidate("ev3")]).await;
        assert!(
            matches!(result, Err(MemoryError::Conflict(_))),
            "{drop_kind}: {result:?}"
        );
        assert_eq!(
            kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
            before
        );
        pool.close().await;
    }
}

#[tokio::test]
async fn earlier_reject_audit_does_not_hide_a_later_commit_anchor() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("memory.sqlite");
    let kv = KvStore::open(&path).await.unwrap();
    let rejected = Candidate::new(
        "reassert-audit",
        Scope::owner("owner"),
        "user",
        "prefers",
        json!("espresso"),
        Origin {
            source: "web".into(),
            trust: Trust::WebFetch,
        },
    )
    .with_evidence(vec!["web-evidence".into()]);
    kv.write_gate().propose(vec![rejected]).await.unwrap();
    let pool = legacy_shape(&kv, &path).await;
    let before = kv.events().scan(&EventQuery::owner("owner")).await.unwrap();
    let retry = kv.write_gate().propose(vec![candidate("ev3")]).await;
    assert!(retry.is_ok(), "{retry:?}");
    assert_eq!(
        kv.events().scan(&EventQuery::owner("owner")).await.unwrap(),
        before
    );
    pool.close().await;
}
