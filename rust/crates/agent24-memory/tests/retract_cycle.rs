#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_core::util::now_iso8601;
use agent24_memory::KvStore;
use agent24_memory::assertion::{AssertionStore, BeliefQuery, Forget};
use agent24_memory::event::{EventQuery, EventStore, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter, WriteDecision};
use serde_json::json;

fn candidate(owner: &str, id: &str, evidence: &str) -> Candidate {
    Candidate::new(
        id,
        Scope::owner(owner),
        "coffee",
        "favorite",
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
async fn test_remember_retract_cycle_three_cycles_and_idempotence() {
    let kv = KvStore::open_memory().await.unwrap();
    let gate = kv.write_gate();

    // Cycle 1: remember -> retract
    let c1 = candidate("u1", "c1", "ev-1");
    let d1 = gate.propose(vec![c1]).await.unwrap();
    assert_eq!(d1, vec![WriteDecision::Committed("c1".into())]);

    let recall_1 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert_eq!(recall_1.len(), 1, "cycle 1: remembered belief is in recall");

    let forget_1 = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_1,
        Forget::Forgotten,
        "cycle 1: first forget succeeds"
    );

    let recall_after_forget_1 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert!(
        recall_after_forget_1.is_empty(),
        "cycle 1: retracted belief is excluded from recall"
    );

    // Idempotent no-op withdrawal after cycle 1
    let forget_1_repeat = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_1_repeat,
        Forget::AlreadyForgotten,
        "cycle 1: repeated forget is an idempotent no-op"
    );

    // Cycle 2: remember exact same content -> retract AGAIN
    let c2 = candidate("u1", "c1", "ev-2");
    let d2 = gate.propose(vec![c2]).await.unwrap();
    assert_eq!(
        d2,
        vec![WriteDecision::Committed("c1".into())],
        "cycle 2: re-assert same content reactivates"
    );

    let recall_2 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert_eq!(recall_2.len(), 1, "cycle 2: reasserted belief is in recall");

    // Cycle 2 retract: succeeds with unique transition identity
    let forget_2 = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_2,
        Forget::Forgotten,
        "cycle 2: second forget must succeed"
    );

    let recall_after_forget_2 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert!(
        recall_after_forget_2.is_empty(),
        "cycle 2: retracted belief is excluded from recall"
    );

    // Idempotent no-op withdrawal after cycle 2
    let forget_2_repeat = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_2_repeat,
        Forget::AlreadyForgotten,
        "cycle 2: repeated forget is an idempotent no-op"
    );

    // Cycle 3: remember exact same content a 3rd time -> retract AGAIN
    let c3 = candidate("u1", "c1", "ev-3");
    let d3 = gate.propose(vec![c3]).await.unwrap();
    assert_eq!(
        d3,
        vec![WriteDecision::Committed("c1".into())],
        "cycle 3: 3rd assert same content reactivates"
    );

    let recall_3 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert_eq!(recall_3.len(), 1, "cycle 3: reasserted belief is in recall");

    let forget_3 = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_3,
        Forget::Forgotten,
        "cycle 3: third forget must succeed"
    );

    let recall_after_forget_3 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert!(
        recall_after_forget_3.is_empty(),
        "cycle 3: retracted belief is excluded from recall"
    );

    // Idempotent no-op withdrawal after cycle 3
    let forget_3_repeat = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_3_repeat,
        Forget::AlreadyForgotten,
        "cycle 3: repeated forget is an idempotent no-op"
    );

    // Audit verification: transitions are properly recorded and have distinct IDs
    let all_events = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let retracted_events: Vec<_> = all_events
        .iter()
        .filter(|e| e.event.kind == "assertion.retracted")
        .collect();
    assert_eq!(
        retracted_events.len(),
        3,
        "exactly 3 retraction events recorded"
    );
    let mut retracted_ids: Vec<_> = retracted_events.iter().map(|e| &e.event.id).collect();
    let total_unique = {
        let len = retracted_ids.len();
        retracted_ids.sort();
        retracted_ids.dedup();
        retracted_ids.len() == len
    };
    assert!(
        total_unique,
        "each retraction event has a unique transition ID"
    );

    let reasserted_count = all_events
        .iter()
        .filter(|e| e.event.kind == "assertion.reasserted")
        .count();
    assert_eq!(
        reasserted_count, 2,
        "cycle 2 and cycle 3 each audited a reasserted event"
    );
}

#[tokio::test]
async fn test_retract_cross_owner_negative() {
    let kv = KvStore::open_memory().await.unwrap();
    let gate = kv.write_gate();

    let c1 = candidate("u1", "c1", "ev-1");
    gate.propose(vec![c1]).await.unwrap();

    // Cross-owner: u2 attempts to retract u1's assertion
    let forget_u2 = kv.forget("u2", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(
        forget_u2,
        Forget::NotFound,
        "foreign owner cannot retract another owner's assertion"
    );

    // u1's assertion remains in recall
    let recall_u1 = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert_eq!(
        recall_u1.len(),
        1,
        "u1's belief unaffected by foreign forget"
    );
}

#[tokio::test]
async fn test_retract_gap_preoccupied_suffix_allocates_unused_id_and_atomic_rollback() {
    let kv = KvStore::open_memory().await.unwrap();
    let gate = kv.write_gate();

    // Cycle 1: remember -> retract
    let c1 = candidate("u1", "c1", "ev-1");
    gate.propose(vec![c1]).await.unwrap();
    assert_eq!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap()
            .len(),
        1
    );

    let forget_1 = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(forget_1, Forget::Forgotten);
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap()
            .is_empty()
    );

    // Get the base retraction event ID
    let events_after_c1 = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let base_id = events_after_c1
        .iter()
        .find(|e| e.event.kind == "assertion.retracted")
        .unwrap()
        .event
        .id
        .clone();

    // Adversarial gap: external caller occupies base_id-2 using public EventStore::append
    let preoccupied_id = format!("{base_id}-2");
    let preoccupied_event = agent24_memory::event::MemEvent::new(
        preoccupied_id.clone(),
        Scope::owner("u1"),
        "external.preoccupied",
        serde_json::json!({ "note": "preoccupied gap" }),
        Origin {
            source: "external".into(),
            trust: Trust::System,
        },
    );
    kv.events().append(&preoccupied_event).await.unwrap();

    // Reactivate c1 in Cycle 2
    let c2 = candidate("u1", "c1", "ev-2");
    gate.propose(vec![c2]).await.unwrap();
    assert_eq!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap()
            .len(),
        1
    );

    // Cycle 2 retract:
    // With COUNT bug, count=2 selects base_id-2 which collides with the preoccupied event.
    // With repaired allocator, an actually unused ID (base_id-1) is chosen, withdrawal succeeds,
    // and recall is empty.
    let forget_2 = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(forget_2, Forget::Forgotten);
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap()
            .is_empty(),
        "belief must be excluded from recall"
    );

    // Idempotent repeat: already forgotten
    assert_eq!(
        kv.forget("u1", "c1", &now_iso8601()).await.unwrap(),
        Forget::AlreadyForgotten
    );

    // Verify preoccupied event was NOT overwritten or deleted
    let all_events = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let preoccupied = all_events.iter().find(|e| e.event.id == preoccupied_id);
    assert!(
        preoccupied.is_some(),
        "preoccupied event must remain intact"
    );
    assert_eq!(preoccupied.unwrap().event.kind, "external.preoccupied");

    // Cycle 3: Reactivate and retract again; now base_id, base_id-1, base_id-2 are occupied,
    // so allocator must choose base_id-3
    let c3 = candidate("u1", "c1", "ev-3");
    gate.propose(vec![c3]).await.unwrap();
    assert_eq!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap()
            .len(),
        1
    );

    let forget_3 = kv.forget("u1", "c1", &now_iso8601()).await.unwrap();
    assert_eq!(forget_3, Forget::Forgotten);
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap()
            .is_empty()
    );

    // Verify all retraction events have unique IDs
    let all_events_end = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let retract_events: Vec<_> = all_events_end
        .iter()
        .filter(|e| e.event.kind == "assertion.retracted")
        .collect();
    assert_eq!(
        retract_events.len(),
        3,
        "exactly 3 retraction events recorded"
    );
    let mut ids: Vec<_> = retract_events.iter().map(|e| &e.event.id).collect();
    let orig_len = ids.len();
    ids.sort();
    ids.dedup();
    assert_eq!(ids.len(), orig_len, "all retraction IDs are distinct");
}
