//! SL-5: the run pin is derived from the pointer in the run transaction, and
//! migration 0037 admits the `SkillText` message origin.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use agent24_protocol::{Run, RunInput, RunMode, RunStatus, Usage};
use agent24_store::test_hooks::pool;
use agent24_store::{
    MessageOrigin, MessageOriginLookup, OriginatedRunMessage, PendingRunMessage, SkillOrigin,
    SkillPinOutcome, SkillPinRequest, SourceClassification, SourceKind, SourceRef, Store,
    StoreError,
};

const NOW: &str = "2026-11-12T00:00:00Z";

fn run(id: &str) -> Run {
    Run {
        id: id.to_owned(),
        session_id: None,
        workspace_id: None,
        status: RunStatus::Queued,
        input: RunInput {
            prompt: "hi".to_owned(),
            workspace_id: None,
            model_override: None,
            mode: RunMode::Normal,
            knowledge_binding_id: None,
        },
        output: None,
        error: None,
        usage: Usage {
            prompt_tokens: 0,
            completion_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
        },
        schedule_id: None,
        created_at: NOW.to_owned(),
        started_at: None,
        ended_at: None,
    }
}

fn request(skill: &str, expected: &str) -> SkillPinRequest {
    SkillPinRequest {
        skill_id: skill.to_owned(),
        expected_body_revision_id: expected.to_owned(),
        providers_json: "{}".to_owned(),
    }
}

async fn revision(store: &Store, id: &str, skill: &str, origin: &str, kind: &str, digest: &str) {
    sqlx::query(
        "INSERT INTO skill_revision (revision_id, skill_id, origin, semver, kind, content_digest, content, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, x'61', ?)",
    )
    .bind(id)
    .bind(skill)
    .bind(origin)
    .bind(format!("1.0.{id}"))
    .bind(kind)
    .bind(digest)
    .bind(NOW)
    .execute(pool(store))
    .await
    .unwrap();
}

async fn point(store: &Store, skill: &str, body: &str, evals: &str) {
    sqlx::query(
        "INSERT OR REPLACE INTO skill_pointer (skill_id, body_revision_id, evals_revision_id, generation) \
         VALUES (?, ?, ?, 1)",
    )
    .bind(skill)
    .bind(body)
    .bind(evals)
    .execute(pool(store))
    .await
    .unwrap();
}

async fn count(store: &Store, table: &str) -> i64 {
    sqlx::query_scalar(&format!("SELECT COUNT(*) FROM {table}"))
        .fetch_one(pool(store))
        .await
        .unwrap()
}

async fn seeded() -> Store {
    let store = Store::open_memory().await.unwrap();
    revision(&store, "b1", "notes", "imported", "body", "digest-b1").await;
    revision(&store, "e1", "notes", "imported", "evals", "digest-e1").await;
    point(&store, "notes", "b1", "e1").await;
    store
}

#[tokio::test]
async fn the_pin_copies_the_digest_and_origin_from_the_rows() {
    let store = seeded().await;
    let outcome = store
        .insert_run_with_skill_pin(&run("run_1"), &request("notes", "b1"))
        .await
        .unwrap();
    let SkillPinOutcome::Pinned(pin) = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(pin.body_revision_id, "b1");
    assert_eq!(pin.content_digest, "digest-b1");
    assert_eq!(pin.origin, SkillOrigin::Imported);
    let read = store.get_run_skill_pin("run_1").await.unwrap().unwrap();
    assert_eq!(read.pin, pin);
    assert_eq!(read.content, b"a");
    assert_eq!(store.get_run_skill_pin("run_other").await.unwrap(), None);
}

#[tokio::test]
async fn no_pointer_writes_no_run_and_a_moved_pointer_writes_no_run() {
    let store = seeded().await;
    assert_eq!(
        store
            .insert_run_with_skill_pin(&run("run_1"), &request("other", "b1"))
            .await
            .unwrap(),
        SkillPinOutcome::NotActive
    );
    revision(&store, "b2", "notes", "imported", "body", "digest-b2").await;
    revision(&store, "e2", "notes", "imported", "evals", "digest-e2").await;
    point(&store, "notes", "b2", "e2").await;
    assert_eq!(
        store
            .insert_run_with_skill_pin(&run("run_2"), &request("notes", "b1"))
            .await
            .unwrap(),
        SkillPinOutcome::PointerMoved
    );
    assert_eq!(count(&store, "runs").await, 0);
    assert_eq!(count(&store, "run_skill_pin").await, 0);
}

#[tokio::test]
async fn a_pointer_with_two_origins_is_a_typed_error_and_writes_nothing() {
    let store = Store::open_memory().await.unwrap();
    revision(&store, "b1", "notes", "user", "body", "d1").await;
    revision(&store, "e1", "notes", "imported", "evals", "d2").await;
    point(&store, "notes", "b1", "e1").await;
    let err = store
        .insert_run_with_skill_pin(&run("run_1"), &request("notes", "b1"))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::SkillOriginMismatch), "{err:?}");
    assert_eq!(count(&store, "runs").await, 0);
}

#[tokio::test]
async fn a_failed_run_insert_leaves_no_pin_and_workspace_runs_are_refused() {
    let store = seeded().await;
    let first = store
        .insert_run_with_skill_pin(&run("run_1"), &request("notes", "b1"))
        .await
        .unwrap();
    assert!(matches!(first, SkillPinOutcome::Pinned(_)));
    // The same run id again: the run insert fails, so no second pin and no second run.
    assert!(
        store
            .insert_run_with_skill_pin(&run("run_1"), &request("notes", "b1"))
            .await
            .is_err()
    );
    assert_eq!(count(&store, "runs").await, 1);
    assert_eq!(count(&store, "run_skill_pin").await, 1);
    // Bad session: nothing is written.
    let mut with_session = run("run_2");
    with_session.session_id = Some("missing".to_owned());
    assert!(matches!(
        store
            .insert_run_with_skill_pin(&with_session, &request("notes", "b1"))
            .await,
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(count(&store, "run_skill_pin").await, 1);
    // Only the declared object form is accepted for providers.
    let mut bad = request("notes", "b1");
    bad.providers_json = "[]".to_owned();
    assert!(
        store
            .insert_run_with_skill_pin(&run("run_3"), &bad)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_pin_that_disagrees_with_its_revision_is_refused_on_read() {
    let store = seeded().await;
    let _ = store
        .insert_run_with_skill_pin(&run("run_1"), &request("notes", "b1"))
        .await
        .unwrap();
    for (sql, what) in [
        (
            "UPDATE run_skill_pin SET content_digest = 'forged' WHERE run_id = 'run_1'",
            "digest",
        ),
        (
            "UPDATE run_skill_pin SET skill_id = 'other' WHERE run_id = 'run_1'",
            "skill",
        ),
        (
            "UPDATE run_skill_pin SET body_revision_id = 'e1', content_digest = 'digest-e1' WHERE run_id = 'run_1'",
            "kind",
        ),
    ] {
        sqlx::query(sql).execute(pool(&store)).await.unwrap();
        let err = store.get_run_skill_pin("run_1").await.unwrap_err();
        assert!(
            matches!(err, StoreError::SkillStateCorrupt(_)),
            "{what}: {err:?}"
        );
        sqlx::query(
            "UPDATE run_skill_pin SET skill_id = 'notes', body_revision_id = 'b1', content_digest = 'digest-b1' WHERE run_id = 'run_1'",
        )
        .execute(pool(&store))
        .await
        .unwrap();
        assert!(store.get_run_skill_pin("run_1").await.unwrap().is_some());
    }
}

#[tokio::test]
async fn migration_0037_admits_skill_text_and_keeps_the_other_kinds() {
    let store = Store::open_memory().await.unwrap();
    sqlx::query("INSERT INTO runs (id, status, input, usage, created_at) VALUES ('r', 'queued', '{}', '{}', ?)")
        .bind(NOW)
        .execute(pool(&store))
        .await
        .unwrap();
    let message = |origin| OriginatedRunMessage {
        message: PendingRunMessage {
            role: "system".to_owned(),
            content: Some("segment".to_owned()),
            tool_calls: serde_json::json!([]),
            tool_call_id: None,
        },
        origin,
    };
    store
        .append_originated_run_messages(
            "r",
            &[
                message(MessageOrigin::SkillText),
                message(MessageOrigin::HostSystem),
            ],
            NOW,
        )
        .await
        .unwrap();
    for (seq, expected) in [
        (0, MessageOrigin::SkillText),
        (1, MessageOrigin::HostSystem),
    ] {
        match store.read_run_message_origin("r", seq).await.unwrap() {
            MessageOriginLookup::Present(stored) => assert_eq!(stored.origin, expected),
            other => panic!("{other:?}"),
        }
    }
    // A SkillText row with a recall_ref is unreadable, like the other kinds.
    sqlx::query("UPDATE run_message_origins SET recall_ref = '[\"x\"]' WHERE seq = 0")
        .execute(pool(&store))
        .await
        .unwrap();
    assert!(matches!(
        store.read_run_message_origin("r", 0).await.unwrap(),
        MessageOriginLookup::Unreadable { seq: 0 }
    ));
    assert!(
        sqlx::query(
            "INSERT INTO run_message_origins (run_id, seq, schema_version, kind, instance_digest, recall_ref) \
             VALUES ('r', 5, 1, 'Bogus', 'x', NULL)"
        )
        .execute(pool(&store))
        .await
        .is_err()
    );
}

#[test]
fn the_skill_label_follows_the_stored_origin() {
    let imported = SourceRef::skill_text("notes", SkillOrigin::Imported, "d", NOW);
    assert_eq!(imported.kind, SourceKind::SkillText);
    assert_eq!(imported.classification, SourceClassification::Unknown);
    assert_eq!(imported.source_id, "skill_text:notes");
    assert_eq!(imported.revision_digest.as_deref(), Some("d"));
    let user = SourceRef::skill_text("notes", SkillOrigin::User, "d", NOW);
    assert_eq!(user.classification, SourceClassification::Ordinary);
    assert_eq!(user.mode, agent24_store::SourceMode::LocalOnly);
}

#[tokio::test]
async fn corrupt_pointer_rows_and_bad_requests_are_state_corrupt_not_conflict() {
    // The pointer's body slot holds an evals row.
    let store = Store::open_memory().await.unwrap();
    revision(&store, "e0", "notes", "imported", "evals", "d0").await;
    revision(&store, "e1", "notes", "imported", "evals", "d1").await;
    point(&store, "notes", "e0", "e1").await;
    let err = store
        .insert_run_with_skill_pin(&run("run_1"), &request("notes", "e0"))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::SkillStateCorrupt(_)), "{err:?}");
    // The pointer names another skill's body.
    let store = Store::open_memory().await.unwrap();
    revision(&store, "b9", "other", "imported", "body", "d9").await;
    revision(&store, "e9", "notes", "imported", "evals", "d8").await;
    point(&store, "notes", "b9", "e9").await;
    let err = store
        .insert_run_with_skill_pin(&run("run_1"), &request("notes", "b9"))
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::SkillStateCorrupt(_)), "{err:?}");
    // A request no caller should build.
    let mut bad = request("notes", "b9");
    bad.providers_json = "[]".to_owned();
    let err = store
        .insert_run_with_skill_pin(&run("run_2"), &bad)
        .await
        .unwrap_err();
    assert!(matches!(err, StoreError::SkillStateCorrupt(_)), "{err:?}");
    assert_eq!(count(&store, "runs").await, 0);
}

#[tokio::test]
async fn a_tag_insert_failure_rolls_back_the_snapshot_messages() {
    use agent24_store::SnapshotRunMessage;
    let store = Store::open_memory().await.unwrap();
    sqlx::query("INSERT INTO runs (id, status, input, usage, created_at) VALUES ('r', 'queued', '{}', '{}', ?)")
        .bind(NOW)
        .execute(pool(&store))
        .await
        .unwrap();
    sqlx::raw_sql(
        "CREATE TRIGGER fail_tag BEFORE INSERT ON run_source_tags \
         BEGIN SELECT RAISE(ABORT, 'tag failed'); END",
    )
    .execute(pool(&store))
    .await
    .unwrap();
    let rows = [SnapshotRunMessage::Originated(OriginatedRunMessage {
        message: PendingRunMessage {
            role: "system".to_owned(),
            content: Some("segment".to_owned()),
            tool_calls: serde_json::json!([]),
            tool_call_id: None,
        },
        origin: MessageOrigin::SkillText,
    })];
    let tag = SourceRef::skill_text("notes", SkillOrigin::Imported, "d", NOW);
    assert!(
        store
            .append_snapshot_run_messages_tagged("r", &rows, &[(0, &tag)], NOW)
            .await
            .is_err()
    );
    assert_eq!(count(&store, "run_messages").await, 0);
    assert_eq!(count(&store, "run_message_origins").await, 0);
    assert_eq!(count(&store, "run_source_tags").await, 0);
}
