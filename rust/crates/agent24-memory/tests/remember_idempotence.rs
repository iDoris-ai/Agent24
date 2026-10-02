#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_memory::assertion::{Assertion, AssertionStore, BeliefQuery};
use agent24_memory::event::{EventQuery, EventStore, Origin, Scope, Trust};
use agent24_memory::writer::{Candidate, MemoryWriter, WriteDecision};
use agent24_memory::{KvStore, MemoryError};
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

fn system_candidate(owner: &str, id: &str) -> Candidate {
    Candidate::new(
        id,
        Scope::owner(owner),
        "coffee",
        "favorite",
        json!("espresso"),
        Origin {
            source: "system".into(),
            trust: Trust::System,
        },
    )
    .with_evidence(vec!["system-evidence".into()])
}

fn stored(owner: &str, id: &str) -> Assertion {
    let mut assertion = Assertion::new(
        id,
        Scope::owner(owner),
        "coffee",
        "favorite",
        json!("espresso"),
        vec!["new-evidence".into()],
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

async fn valid_commit_audit(id: &str) -> agent24_memory::event::MemEvent {
    valid_audit_for(&stored("u1", id)).await
}

async fn valid_audit_for(assertion: &Assertion) -> agent24_memory::event::MemEvent {
    let source = KvStore::open_memory().await.unwrap();
    let candidate = Candidate::new(
        assertion.id.clone(),
        assertion.scope.clone(),
        assertion.subject.clone(),
        assertion.predicate.clone(),
        assertion.object.clone(),
        Origin {
            source: "user".into(),
            trust: Trust::UserSaid,
        },
    )
    .with_evidence(assertion.evidence.clone())
    .remember();
    source.write_gate().propose(vec![candidate]).await.unwrap();
    source
        .events()
        .scan(&EventQuery::owner(&assertion.scope.owner))
        .await
        .unwrap()
        .into_iter()
        .find(|event| event.event.kind == "mem.write_decision")
        .unwrap()
        .event
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
        kv.events()
            .append(&valid_audit_for(&old).await)
            .await
            .unwrap();
        kv.assertions().assert(&old).await.unwrap();
        let before = query(&kv, &old).await;
        let owner = old.scope.owner.clone();
        let events_before = kv.events().scan(&EventQuery::owner(&owner)).await.unwrap();
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
            kv.events().scan(&EventQuery::owner(&owner)).await.unwrap(),
            events_before,
            "{label} changed audit history"
        );
    }
}

#[tokio::test]
async fn idempotent_collision_requires_original_evidence_and_matching_commit_audit() {
    type EventMutation = fn(&mut agent24_memory::event::MemEvent);
    let cases: [(&str, bool, Option<EventMutation>); 16] = [
        ("missing evidence", true, None),
        ("missing audit", false, None),
        ("empty evidence without audit", true, None),
        (
            "audit evidence mismatch",
            false,
            Some(|e| e.body["evidence"] = json!(["forged"])),
        ),
        (
            "audit content mismatch",
            false,
            Some(|e| e.body["object"] = json!("tea")),
        ),
        (
            "audit scope mismatch",
            false,
            Some(|e| e.scope.session = Some("other".into())),
        ),
        (
            "audit owner mismatch",
            false,
            Some(|e| e.scope.owner = "other".into()),
        ),
        (
            "audit id mismatch",
            false,
            Some(|e| e.id = "other-audit-id".into()),
        ),
        (
            "audit verdict mismatch",
            false,
            Some(|e| e.body["verdict"] = json!("hold")),
        ),
        (
            "audit origin mismatch",
            false,
            Some(|e| e.origin.source = "forged".into()),
        ),
        (
            "audit trust mismatch",
            false,
            Some(|e| e.origin.trust = Trust::UserSaid),
        ),
        (
            "audit body trust mismatch",
            false,
            Some(|e| e.body["trust"] = json!("Model")),
        ),
        (
            "audit remember mismatch",
            false,
            Some(|e| e.body["explicit_remember"] = json!(false)),
        ),
        (
            "audit candidate mismatch",
            false,
            Some(|e| e.body["candidate_id"] = json!("other-id")),
        ),
        (
            "audit reason mismatch",
            false,
            Some(|e| e.body["reason"] = json!("forged")),
        ),
        (
            "audit kind mismatch",
            false,
            Some(|e| e.kind = "other.kind".into()),
        ),
    ];

    let mut failures = Vec::new();
    for (label, missing_evidence, mutate_event) in cases {
        let kv = KvStore::open_memory().await.unwrap();
        let mut old = stored("u1", "same-id");
        if missing_evidence {
            old.evidence.clear();
        }
        kv.assertions().assert(&old).await.unwrap();
        if let Some(mutate) = mutate_event {
            let mut audit = valid_commit_audit("same-id").await;
            mutate(&mut audit);
            kv.events().append(&audit).await.unwrap();
        } else if label == "missing evidence" {
            let mut audit = valid_commit_audit("same-id").await;
            audit.body["evidence"] = json!([]);
            let canonical = format!("commit|same-id|coffee|favorite|{}|[]", json!("espresso"));
            audit.id = format!(
                "audit-same-id-{}",
                &agent24_memory::artifact::checksum(&canonical)[..16]
            );
            kv.events().append(&audit).await.unwrap();
        }

        let before_assertions = query(&kv, &old).await;
        let before_events = (
            kv.events().scan(&EventQuery::owner("u1")).await.unwrap(),
            kv.events().scan(&EventQuery::owner("other")).await.unwrap(),
        );
        let result = kv
            .write_gate()
            .propose(vec![candidate("u1", "same-id")])
            .await;

        let mut case_failures = Vec::new();
        if !matches!(result, Err(MemoryError::Conflict(_))) {
            case_failures.push(format!("expected Conflict, got {result:?}"));
        }
        if query(&kv, &old).await != before_assertions {
            case_failures.push("assertion changed".into());
        }
        let after_events = (
            kv.events().scan(&EventQuery::owner("u1")).await.unwrap(),
            kv.events().scan(&EventQuery::owner("other")).await.unwrap(),
        );
        if after_events != before_events {
            case_failures.push("events changed".into());
        }
        if !case_failures.is_empty() {
            failures.push(format!("{label}: {}", case_failures.join(", ")));
        }
    }
    assert!(
        failures.is_empty(),
        "invalid idempotent collisions accepted: {}",
        failures.join("; ")
    );
}

#[tokio::test]
async fn matching_current_qualified_assertion_is_audited_idempotent_success() {
    let kv = KvStore::open_memory().await.unwrap();
    kv.write_gate()
        .propose(vec![candidate("u1", "same-id")])
        .await
        .unwrap();

    let repeated = candidate("u1", "same-id").with_evidence(vec!["different-evidence".into()]);
    let before_assertions = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    let before_events = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let result = kv.write_gate().propose(vec![repeated]).await.unwrap();

    assert_eq!(result, vec![WriteDecision::Committed("same-id".into())]);
    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    assert_eq!(rows, before_assertions);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].evidence, vec!["new-evidence"]);
    assert_eq!(decision_audits(&kv, "u1").await, 1);
    assert_eq!(
        kv.events().scan(&EventQuery::owner("u1")).await.unwrap(),
        before_events
    );
}

#[tokio::test]
async fn system_commit_can_be_retried_by_explicit_user_remember() {
    let kv = KvStore::open_memory().await.unwrap();
    kv.write_gate()
        .propose(vec![system_candidate("u1", "system-id")])
        .await
        .unwrap();

    let before_assertions = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner("u1"))
        .await
        .unwrap();
    let before_events = kv.events().scan(&EventQuery::owner("u1")).await.unwrap();
    let result = kv
        .write_gate()
        .propose(vec![candidate("u1", "system-id")])
        .await
        .unwrap();

    assert_eq!(result, vec![WriteDecision::Committed("system-id".into())]);
    assert_eq!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner("u1"))
            .await
            .unwrap(),
        before_assertions
    );
    assert_eq!(
        kv.events().scan(&EventQuery::owner("u1")).await.unwrap(),
        before_events
    );
}
