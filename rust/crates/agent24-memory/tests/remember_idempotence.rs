#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_memory::KvStore;
use agent24_memory::assertion::{Assertion, AssertionStore, BeliefQuery};
use agent24_memory::event::{EventQuery, EventStore, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter, WriteDecision};
use serde_json::json;

fn candidate(owner: &str, id: &str) -> Candidate {
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
    .with_evidence(vec!["new-evidence".into()])
    .remember()
}

fn stored(owner: &str, id: &str) -> Assertion {
    let mut assertion = Assertion::new(
        id,
        Scope::owner(owner),
        "coffee",
        "favorite",
        json!("espresso"),
        vec!["old-evidence".into()],
    );
    assertion.valid_from = "1990-01-01T00:00:00Z".into();
    assertion.recorded_from = "1990-01-01T00:00:00Z".into();
    assertion
}

async fn query(kv: &KvStore, old: &Assertion) -> Vec<Assertion> {
    kv.assertions()
        .beliefs_as_of(
            &BeliefQuery::owner(&old.scope.owner)
                .valid_at(&old.valid_from)
                .recorded_at(&old.recorded_from)
                .with_unqualified(),
        )
        .await
        .unwrap()
}

async fn decision_audits(kv: &KvStore, owner: &str) -> usize {
    kv.events()
        .scan(&EventQuery::owner(owner))
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.event.kind == "mem.write_decision")
        .count()
}

#[tokio::test]
async fn only_a_matching_current_qualified_assertion_is_idempotent() {
    type Mutation = fn(&mut Assertion);
    let cases: [(&str, Mutation); 10] = [
        ("different owner", |a| a.scope = Scope::owner("other")),
        ("different content", |a| a.object = json!("tea")),
        ("different subject", |a| a.subject = "tea".into()),
        ("different predicate", |a| a.predicate = "dislikes".into()),
        ("different scope", |a| {
            a.scope.session = Some("other-session".into())
        }),
        ("unqualified", |a| a.qualified = false),
        ("future valid time", |a| {
            a.valid_from = "2999-01-01T00:00:00Z".into()
        }),
        ("expired valid time", |a| {
            a.valid_to = Some("2000-01-01T00:00:00Z".into())
        }),
        ("future recorded time", |a| {
            a.recorded_from = "2999-01-01T00:00:00Z".into()
        }),
        ("closed recorded time", |a| {
            a.recorded_to = Some("2000-01-01T00:00:00Z".into())
        }),
    ];

    for (label, mutate) in cases {
        let kv = KvStore::open_memory().await.unwrap();
        let mut old = stored("u1", "same-id");
        mutate(&mut old);
        kv.assertions().assert(&old).await.unwrap();
        let before = query(&kv, &old).await;
        assert_eq!(before.len(), 1, "{label} fixture must be observable");

        let result = kv
            .write_gate()
            .propose(vec![candidate("u1", "same-id")])
            .await;
        assert!(result.is_err(), "{label} must not be treated as idempotent");
        assert_eq!(
            query(&kv, &old).await,
            before,
            "{label} changed the stored assertion"
        );
        assert_eq!(
            decision_audits(&kv, "u1").await,
            0,
            "{label} wrote an audit"
        );
    }
}

#[tokio::test]
async fn matching_current_qualified_assertion_is_audited_idempotent_success() {
    let kv = KvStore::open_memory().await.unwrap();
    kv.write_gate()
        .propose(vec![candidate("u1", "same-id")])
        .await
        .unwrap();

    let repeated = candidate("u1", "same-id").with_evidence(vec!["different-evidence".into()]);
    let result = kv.write_gate().propose(vec![repeated]).await.unwrap();

    assert_eq!(result, vec![WriteDecision::Committed("same-id".into())]);
    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].evidence, vec!["new-evidence"]);
    assert_eq!(decision_audits(&kv, "u1").await, 1);
}
