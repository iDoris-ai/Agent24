use super::*;
use agent24_memory::{
    assertion::{Assertion, AssertionStore, BeliefQuery},
    event::{EventQuery, EventStore, Origin, Scope, Trust},
    writer::{Candidate, MemoryWriter, WriteDecision},
};
use agent24_protocol::EventBody;
use serde_json::json;

async fn personal_owner(kv: &KvStore) -> String {
    let org = OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
    partition_key(&org, &SpaceId::personal(LOCAL_USER))
}

async fn beliefs(kv: &KvStore, owner: &str, include_unqualified: bool) -> Vec<Assertion> {
    let mut query = BeliefQuery::owner(owner);
    if include_unqualified {
        query = query.with_unqualified();
    }
    kv.assertions().beliefs_as_of(&query).await.unwrap()
}

#[tokio::test]
async fn explicit_user_remember_writes_qualified_personal_assertion_with_event_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    run(&state, "记住我对花生过敏").await;

    let owner = personal_owner(&kv).await;
    let rows = beliefs(&kv, &owner, false).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].qualified);
    assert_eq!(rows[0].scope.owner, owner);
    assert_eq!(rows[0].subject, "user");
    assert_eq!(rows[0].predicate, "said_to_remember");
    assert_eq!(rows[0].object, json!("我对花生过敏"));
    assert_eq!(
        rows[0].id,
        agent24_memory::artifact::checksum(&format!("{owner}我对花生过敏"))
    );
    let events = kv
        .events()
        .scan(&EventQuery::owner(&owner).session(SESSION))
        .await
        .unwrap();
    let user_event = events
        .iter()
        .find(|e| e.event.kind == "message" && e.event.origin.trust == Trust::UserSaid)
        .unwrap();
    assert_eq!(rows[0].evidence, vec![user_event.event.id.clone()]);
}

#[tokio::test]
async fn repeating_same_explicit_remember_keeps_one_ledger_row() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    let mut events = state.events.subscribe();
    run(&state, "记住我对花生过敏").await;
    let owner = personal_owner(&kv).await;
    let original = beliefs(&kv, &owner, false).await;
    run(&state, "记住我对花生过敏").await;
    assert_eq!(beliefs(&kv, &owner, false).await, original);
    while let Ok((_, body)) = events.try_recv() {
        assert!(!matches!(body, EventBody::MemoryWriteFailed(_)));
    }

    assert_eq!(original.len(), 1);
}

#[tokio::test]
async fn remember_phrase_in_model_answer_does_not_create_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider {
        answer: Some("记住用户对花生过敏".into()),
        ..Provider::default()
    });
    let state = app(kv.clone(), dir.path(), provider).await;
    run(&state, "你好").await;

    let owner = personal_owner(&kv).await;
    assert!(beliefs(&kv, &owner, true).await.is_empty());
}

#[tokio::test]
async fn model_trust_is_held_and_excluded_from_default_recall_and_fts() {
    let kv = KvStore::open_memory().await.unwrap();
    let owner = "test-personal";
    let candidate = Candidate::new(
        "model-peanut",
        Scope::owner(owner),
        "user",
        "said_to_remember",
        json!("我对花生过敏"),
        Origin {
            source: "model".into(),
            trust: Trust::Model,
        },
    )
    .with_evidence(vec!["source-event".into()])
    .remember();
    assert_eq!(
        kv.write_gate().propose(vec![candidate]).await.unwrap(),
        vec![WriteDecision::Held("model-peanut".into())]
    );
    assert!(beliefs(&kv, owner, false).await.is_empty());
    let held = beliefs(&kv, owner, true).await;
    assert_eq!(held.len(), 1);
    assert!(!held[0].qualified);
    let hits = kv.retriever().search_any("花生", owner, 5).await.unwrap();
    assert!(hits.is_empty());
}

#[tokio::test]
async fn failed_append_turn_does_not_retain_or_leave_a_partial_conversation() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let kv = KvStore::open(&db).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    let owner = personal_owner(&kv).await;
    let mut events = state.events.subscribe();
    // Keep the ledger writable while forcing append_turn to reject its IDs.
    let assistant_id = agent24_memory::artifact::checksum(
        &serde_json::to_string(&(&owner, SESSION, 0u64, "assistant")).unwrap(),
    );
    kv.events()
        .append(&agent24_memory::event::MemEvent::new(
            assistant_id.clone(),
            Scope::owner(&owner).with_session(SESSION),
            "conflict",
            json!({}),
            Origin {
                source: "test".into(),
                trust: Trust::System,
            },
        ))
        .await
        .unwrap();

    run(&state, "记住我对花生过敏").await;
    assert!(beliefs(&kv, &owner, true).await.is_empty());
    let stored = kv
        .events()
        .scan(&EventQuery::owner(&owner).session(SESSION))
        .await
        .unwrap();
    assert_eq!(stored.len(), 1, "no partial conversation may be written");
    assert_eq!(stored[0].event.id, assistant_id);
    let mut memory_write_failed = false;
    while let Ok((_, body)) = events.try_recv() {
        memory_write_failed |= matches!(body, EventBody::MemoryWriteFailed(_));
    }
    assert!(memory_write_failed, "append failure must remain observable");
}
