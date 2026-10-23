//! Explicit readiness probes; known-red contract tests are opt-in, never CI failures.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use agent24_core::util::now_iso8601;
use agent24_memory::assertion::{AssertionStore, BeliefQuery, Forget};
use agent24_memory::event::{EventQuery, EventStore, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter, WriteDecision};
use agent24_memory::{KvStore, MemoryError};
use serde_json::json;

fn candidate(owner: &str, evidence: &str) -> Candidate {
    Candidate::new(
        "p1-fact",
        Scope::owner(owner),
        "user",
        "said_to_remember",
        json!("espresso"),
        Origin {
            source: "user".into(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(vec![evidence.into()])
    .remember()
}

#[tokio::test]
#[ignore = "H02 contract probe: known-red baseline; run explicitly with --ignored"]
async fn h02_new_evidence_reassert_then_retry_must_succeed() {
    let kv = KvStore::open_memory().await.unwrap();
    let gate = kv.write_gate();
    gate.propose(vec![candidate("u1", "ev1")]).await.unwrap();
    assert_eq!(
        kv.forget("u1", "p1-fact", &now_iso8601()).await.unwrap(),
        Forget::Forgotten
    );
    assert_eq!(
        gate.propose(vec![candidate("u1", "ev2")]).await.unwrap(),
        vec![WriteDecision::Committed("p1-fact".into())]
    );
    let before = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let retry = gate.propose(vec![candidate("u1", "ev3")]).await;
    let after = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let beliefs = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    println!(
        "H02 retry={retry:?}; audit_kinds={:?}; active={}; evidence={:?}",
        after.iter().map(|e| &e.event.kind).collect::<Vec<_>>(),
        beliefs.len(),
        beliefs[0].evidence
    );
    assert_eq!(after.len(), before.len(), "retry must not append audit");
    assert_eq!(beliefs.len(), 1);
    assert_eq!(beliefs[0].evidence, vec!["ev2"]);
    assert!(
        retry.is_ok(),
        "identical current content after reassert must be idempotent: {retry:?}"
    );
}

#[tokio::test]
async fn h02_original_audit_retry_and_owner_controls() {
    let kv = KvStore::open_memory().await.unwrap();
    let gate = kv.write_gate();
    gate.propose(vec![candidate("u1", "ev1")]).await.unwrap();
    assert_eq!(
        gate.propose(vec![candidate("u1", "ev2")]).await.unwrap(),
        vec![WriteDecision::Committed("p1-fact".into())]
    );
    assert_eq!(
        kv.events()
            .scan(&EventQuery::owner("u1"))
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(matches!(
        gate.propose(vec![candidate("u2", "ev1")]).await,
        Err(MemoryError::Conflict(_))
    ));
    assert_eq!(
        kv.forget("u2", "p1-fact", &now_iso8601()).await.unwrap(),
        Forget::NotFound
    );
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u2"))
            .await
            .unwrap()
            .is_empty()
    );
    println!("H02 original audit retry succeeds; cross-owner write/forget rejected");
}

#[tokio::test]
async fn h02_missing_and_forged_audit_controls() {
    for mode in ["missing", "payload", "owner", "origin", "kind"] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("memory.sqlite");
        let kv = KvStore::open(&path).await.unwrap();
        let gate = kv.write_gate();
        gate.propose(vec![candidate("u1", "ev1")]).await.unwrap();
        let pool = sqlx::SqlitePool::connect(&format!("sqlite:{}", path.display()))
            .await
            .unwrap();
        let mut audit = kv.events().scan(&EventQuery::owner("u1")).await.unwrap()[0]
            .event
            .clone();
        sqlx::query("DELETE FROM mem_events WHERE kind='mem.write_decision'")
            .execute(&pool)
            .await
            .unwrap();
        match mode {
            "payload" => audit.body = json!({"forged":true}),
            "owner" => audit.scope = Scope::owner("u2"),
            "origin" => audit.origin.source = "forged".into(),
            "kind" => audit.kind = "forged".into(),
            _ => {}
        }
        if mode != "missing" {
            kv.events().append(&audit).await.unwrap();
        }
        let result = gate.propose(vec![candidate("u1", "ev2")]).await;
        println!("H02 audit control {mode}: {result:?}");
        assert!(matches!(result, Err(MemoryError::Conflict(_))));
        assert_eq!(
            kv.assertions()
                .beliefs_as_of(&BeliefQuery::owner("u1"))
                .await
                .unwrap()
                .len(),
            1
        );
        pool.close().await;
    }
}
