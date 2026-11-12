//! SL-4: activation and rollback through the route handler. Revisions are
//! written straight into the store so each test controls the exact pair.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use serde_json::{Value, json};

use super::*;

const SKILL: &str = "demo";

fn body_md(version: &str, text: &str) -> String {
    format!(
        "---\nname: demo\nversion: {version}\ndescription: d\nrequires: []\nevals: evals/c.jsonl\n---\n{text}\n"
    )
}

fn evals_text(expects: &[&str]) -> String {
    expects
        .iter()
        .map(|expect| json!({"input": "i", "expect": expect, "judge": "contains"}).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn put(
    store: &Store,
    origin: SkillOrigin,
    kind: SkillRevisionKind,
    semver: &str,
    text: &str,
) -> String {
    let row = NewSkillRevision {
        revision_id: uuid::Uuid::new_v4().to_string(),
        skill_id: SKILL.to_owned(),
        origin,
        semver: semver.to_owned(),
        kind,
        content_digest: agent24_skill::sha256_hex(text),
        content: text.as_bytes().to_vec(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };
    revision_id_of(store.insert_skill_revision(&row).await.unwrap())
}

async fn body(store: &Store, origin: SkillOrigin, version: &str, text: &str) -> String {
    put(
        store,
        origin,
        SkillRevisionKind::Body,
        version,
        &body_md(version, text),
    )
    .await
}

async fn evals(store: &Store, origin: SkillOrigin, version: &str, expects: &[&str]) -> String {
    put(
        store,
        origin,
        SkillRevisionKind::Evals,
        version,
        &evals_text(expects),
    )
    .await
}

async fn post(state: &AppState, id: &str, request: Value) -> (StatusCode, Value) {
    let response = post_skill_activate(
        State(state.clone()),
        AxumPath(id.to_owned()),
        Body::from(request.to_string()),
    )
    .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn pair(generation: i64, body: &str, evals: &str) -> Value {
    json!({
        "expected_generation": generation,
        "body_revision_id": body,
        "evals_revision_id": evals,
    })
}

async fn fixture() -> AppState {
    crate::server::tests::state().await
}

async fn pointer(store: &Store) -> Option<agent24_store::SkillPointer> {
    store.get_skill_pointer(SKILL).await.unwrap()
}

async fn history_len(store: &Store) -> usize {
    store.list_skill_activations(SKILL).await.unwrap().len()
}

#[tokio::test]
async fn first_activation_reruns_the_evals_and_binds_the_result() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let (status, value) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["result"], "activated");
    assert_eq!(value["unchanged"], false);
    assert_eq!(value["generation"], 1);
    assert_eq!(
        value["text_consistency"],
        json!({"consistent": 1, "total": 1})
    );
    let history = store.list_skill_activations(SKILL).await.unwrap();
    assert_eq!(history.len(), 1);
    let rerun: Value = serde_json::from_str(&history[0].cases_json).unwrap();
    assert_eq!(rerun[0]["passed"], true);
    assert_eq!(
        history[0].renderer_version,
        agent24_skill::RENDERER_VERSION.to_string()
    );
    // The audit row records ids, digest, code and origins, never the text.
    let audit = store.list_audit().await.unwrap();
    let entry = audit
        .iter()
        .find(|entry| entry.action == "skill.activate")
        .expect("activation audit row");
    assert_eq!(entry.detail["result_code"], "activated");
    assert_eq!(entry.detail["origin_to"], "imported");
    assert!(!entry.detail.to_string().contains("hello world"));
}

/// The server never reads a client verdict. Each of these is refused before
/// anything is read or written.
#[tokio::test]
async fn client_verdict_fields_are_refused() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    for (field, value) in [
        ("passed", json!(true)),
        ("score", json!(1.0)),
        ("body", json!("hello world")),
        ("summary", json!("激活")),
        ("origin", json!("user")),
    ] {
        let mut request = pair(0, &b1, &e1);
        request[field] = value;
        let (status, value) = post(&state, SKILL, request).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {value}");
        assert_eq!(value["error"]["code"], "invalid_request", "{field}");
    }
    assert!(pointer(&store).await.is_none());
    assert_eq!(history_len(&store).await, 0);
}

/// Mutation: build the binding without rerunning (trust the request) and the
/// first assertion goes green-to-red.
#[tokio::test]
async fn an_unaccepted_failure_is_409_with_the_server_summary_and_the_pointer_stays() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(
        &store,
        SkillOrigin::Imported,
        "1.0.0",
        &["hello world", "absent text"],
    )
    .await;
    let (status, value) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "skill_eval_failed");
    let cases = value["error"]["details"]["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 2);
    let failed: Vec<&str> = value["error"]["details"]["failed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|id| id.as_str().unwrap())
        .collect();
    assert_eq!(failed.len(), 1);
    assert!(pointer(&store).await.is_none());
    assert_eq!(history_len(&store).await, 0);

    // An id that is not a failure of this run does not stand in for it.
    let mut wrong = pair(0, &b1, &e1);
    wrong["accepted"] = json!([cases[0]["id"]]);
    let (status, value) = post(&state, SKILL, wrong).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "skill_eval_failed");
    // Too many is refused as well as too few.
    let mut extra = pair(0, &b1, &e1);
    extra["accepted"] = json!([failed[0], cases[0]["id"]]);
    let (status, _) = post(&state, SKILL, extra).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(pointer(&store).await.is_none());

    // The exact failed id, from the server's own summary, goes through.
    let mut accepted = pair(0, &b1, &e1);
    accepted["accepted"] = json!(failed);
    let (status, value) = post(&state, SKILL, accepted).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(
        value["text_consistency"],
        json!({"consistent": 1, "total": 2})
    );
    let history = store.list_skill_activations(SKILL).await.unwrap();
    let recorded: Vec<String> = serde_json::from_str(&history[0].accepted_json).unwrap();
    assert_eq!(recorded, failed);
}

#[tokio::test]
async fn an_empty_evals_file_cannot_activate() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = put(
        &store,
        SkillOrigin::Imported,
        SkillRevisionKind::Evals,
        "1.0.0",
        "",
    )
    .await;
    let (status, value) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "skill_eval_empty");
    assert!(pointer(&store).await.is_none());
}

#[tokio::test]
async fn changing_both_sides_of_the_same_origin_is_mixed_and_the_pointer_stays() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let b2 = body(&store, SkillOrigin::Imported, "2.0.0", "hello world two").await;
    let e2 = evals(&store, SkillOrigin::Imported, "2.0.0", &["hello world two"]).await;
    let (status, _) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, value) = post(&state, SKILL, pair(1, &b2, &e2)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], "skill_activation_mixed");
    let now = pointer(&store).await.unwrap();
    assert_eq!((now.body_revision_id, now.generation), (b1.clone(), 1));
    // One side at a time works.
    let (status, value) = post(&state, SKILL, pair(1, &b2, &e1)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(pointer(&store).await.unwrap().generation, 2);
}

#[tokio::test]
async fn a_stale_generation_is_a_pointer_conflict_even_when_both_sides_change() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let b2 = body(&store, SkillOrigin::Imported, "2.0.0", "hello world two").await;
    let e2 = evals(&store, SkillOrigin::Imported, "2.0.0", &["hello world two"]).await;
    let (status, _) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::OK);
    for generation in [0, 2] {
        let (status, value) = post(&state, SKILL, pair(generation, &b2, &e2)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{generation}: {value}");
        assert_eq!(value["error"]["code"], "skill_pointer_conflict");
    }
    // No pointer row compares as generation 0.
    let other_state = fixture().await;
    let other = other_state.store.clone();
    let b = body(&other, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e = evals(&other, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let (status, value) = post(&other_state, SKILL, pair(3, &b, &e)).await;
    assert_eq!(status, StatusCode::CONFLICT, "{value}");
    assert_eq!(value["error"]["code"], "skill_pointer_conflict");
    assert!(pointer(&store).await.unwrap().generation == 1);
    assert_eq!(history_len(&store).await, 1);
}

#[tokio::test]
async fn a_source_switch_may_change_both_sides_and_records_from_and_to() {
    let state = fixture().await;
    let store = state.store.clone();
    let ib = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let ie = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let ub = body(&store, SkillOrigin::User, "1.1.0", "user text").await;
    let ue = evals(&store, SkillOrigin::User, "1.1.0", &["user text"]).await;
    let (status, _) = post(&state, SKILL, pair(0, &ib, &ie)).await;
    assert_eq!(status, StatusCode::OK);
    let (status, value) = post(&state, SKILL, pair(1, &ub, &ue)).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["pointer"]["origin"], "user");
    let history = store.list_skill_activations(SKILL).await.unwrap();
    assert_eq!(history[1].origin_from, Some(SkillOrigin::Imported));
    assert_eq!(history[1].origin_to, SkillOrigin::User);
    // The same pair across origins is not a source switch.
    let mixed = pair(2, &ib, &ue);
    let (status, value) = post(&state, SKILL, mixed).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert_eq!(value["error"]["code"], "skill_origin_mismatch");
    assert_eq!(pointer(&store).await.unwrap().generation, 2);
}

#[tokio::test]
async fn repeating_the_same_target_is_idempotent() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let (status, _) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::OK);
    for _ in 0..2 {
        // A double click with the old generation, and a retry with the new one.
        for generation in [0, 1] {
            let (status, value) = post(&state, SKILL, pair(generation, &b1, &e1)).await;
            assert_eq!(status, StatusCode::OK, "{value}");
            assert_eq!(value["unchanged"], true);
            assert_eq!(value["result"], "unchanged");
            assert_eq!(value["generation"], 1);
            // L3: unchanged does not say this request's evals were stored.
            assert!(value["note"].as_str().unwrap().contains("没有被校验或落库"));
            assert_eq!(pointer(&store).await.unwrap().generation, 1);
            assert_eq!(history_len(&store).await, 1);
        }
    }
}

#[tokio::test]
async fn rollback_restores_a_whole_pair_without_rerunning_or_deleting_pins() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    // Case two fails, and is accepted at activation.
    let e1 = evals(
        &store,
        SkillOrigin::Imported,
        "1.0.0",
        &["hello world", "absent"],
    )
    .await;
    let b2 = body(&store, SkillOrigin::Imported, "2.0.0", "hello world two").await;
    let (_, probe) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    let failed = probe["error"]["details"]["failed"].clone();
    let mut first = pair(0, &b1, &e1);
    first["accepted"] = failed.clone();
    let (status, first_value) = post(&state, SKILL, first).await;
    assert_eq!(status, StatusCode::OK, "{first_value}");
    let first_activation = first_value["activation_id"].as_str().unwrap().to_owned();
    let mut second = pair(1, &b2, &e1);
    second["accepted"] = failed;
    let (status, value) = post(&state, SKILL, second).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    let revisions_before = store.list_skill_revisions().await.unwrap().len();

    // No accepted list: a rerun would be 409 skill_eval_failed.
    let rollback = json!({"expected_generation": 2, "rollback_activation_id": first_activation});
    let (status, value) = post(&state, SKILL, rollback).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["result"], "rolled_back");
    assert_eq!(value["generation"], 3);
    let now = pointer(&store).await.unwrap();
    assert_eq!(now.body_revision_id, b1);
    assert_eq!(history_len(&store).await, 3);
    assert_eq!(
        store.list_skill_revisions().await.unwrap().len(),
        revisions_before
    );
    assert!(store.get_skill_revision(&b2).await.unwrap().is_some());
    let audit = store.list_audit().await.unwrap();
    assert!(audit.iter().any(|entry| entry.action == "skill.rollback"));

    // Repeating it is idempotent. An unknown or foreign id is 404.
    let again = json!({"expected_generation": 3, "rollback_activation_id": first_activation});
    let (status, value) = post(&state, SKILL, again).await;
    assert_eq!(status, StatusCode::OK, "{value}");
    assert_eq!(value["unchanged"], true);
    assert_eq!(history_len(&store).await, 3);
    let missing = json!({"expected_generation": 3, "rollback_activation_id": "nope"});
    let (status, value) = post(&state, SKILL, missing).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
    assert_eq!(value["error"]["code"], "not_found");
    let with_accepted = json!({
        "expected_generation": 3, "rollback_activation_id": first_activation, "accepted": ["x"]
    });
    let (status, _) = post(&state, SKILL, with_accepted).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn request_shapes_and_unknown_revisions_are_refused() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    for request in [
        json!({"expected_generation": 0}),
        json!({"expected_generation": 0, "body_revision_id": b1}),
        json!({"expected_generation": -1, "body_revision_id": b1, "evals_revision_id": e1}),
        json!({"expected_generation": 0, "body_revision_id": b1, "evals_revision_id": e1,
               "rollback_activation_id": "x"}),
    ] {
        let (status, value) = post(&state, SKILL, request.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{request}: {value}");
        assert_eq!(value["error"]["code"], "invalid_request");
    }
    let (status, value) = post(&state, SKILL, pair(0, "missing", &e1)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{value}");
    // A body id in the evals slot is the wrong kind.
    let (status, value) = post(&state, SKILL, pair(0, &b1, &b1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    // Another skill's revision is not this skill's.
    let (status, value) = post(&state, "other", pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{value}");
    assert!(pointer(&store).await.is_none());
}

/// A pointer row whose two sides have different origins is a bad stored row.
/// Reading it is 500 `internal`; only the write path's own pair check is the
/// 400 `skill_origin_mismatch`.
#[tokio::test]
async fn a_pointer_row_with_split_origins_reads_as_500() {
    let state = fixture().await;
    let store = state.store.clone();
    let ib = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let ie = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let ue = evals(&store, SkillOrigin::User, "1.1.0", &["hello world"]).await;
    sqlx::query(
        "INSERT INTO skill_pointer (skill_id, body_revision_id, evals_revision_id, generation) \
         VALUES (?, ?, ?, 1)",
    )
    .bind(SKILL)
    .bind(&ib)
    .bind(&ue)
    .execute(agent24_store::test_hooks::pool(&store))
    .await
    .unwrap();
    let (status, value) = post(&state, SKILL, pair(1, &ib, &ie)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{value}");
    assert_eq!(value["error"]["code"], "internal");
}

#[tokio::test]
async fn the_summary_comes_from_stored_rows_in_four_items() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let b2 = body(&store, SkillOrigin::Imported, "2.0.0", "hello world two").await;
    let ue = evals(&store, SkillOrigin::User, "9.0.0", &["hello world"]).await;
    let ub = body(&store, SkillOrigin::User, "9.0.0", "user text").await;
    let request = pair(0, &b1, &e1).to_string();
    let first = activation_summary(&store, SKILL, request.as_bytes())
        .await
        .unwrap();
    let digest = store
        .get_skill_revision(&b1)
        .await
        .unwrap()
        .unwrap()
        .content_digest;
    assert!(first.contains("\"demo\""), "{first}");
    assert!(first.contains("\"1.0.0\""), "{first}");
    assert!(first.contains(&digest[..8]), "{first}");
    assert!(!first.contains(&digest[..9]), "only 8 hex digits: {first}");
    assert!(first.contains("无当前指针"), "{first}");

    let (status, _) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::OK);
    let body_only = pair(1, &b2, &e1).to_string();
    let summary = activation_summary(&store, SKILL, body_only.as_bytes())
        .await
        .unwrap();
    assert!(summary.contains("\"2.0.0\""), "{summary}");
    assert!(summary.contains("评测集相对当前指针：无变化"), "{summary}");
    let switch = pair(1, &ub, &ue).to_string();
    let summary = activation_summary(&store, SKILL, switch.as_bytes())
        .await
        .unwrap();
    assert!(summary.contains("评测集相对当前指针：有变化"), "{summary}");
    assert!(summary.contains("来源切换：imported → user"), "{summary}");

    // A client-supplied summary field, a missing revision, an already-current
    // pair and a wrong skill all leave nothing to approve.
    let mut forged = pair(1, &b2, &e1);
    forged["summary"] = json!("看起来无害");
    assert!(
        activation_summary(&store, SKILL, forged.to_string().as_bytes())
            .await
            .is_none()
    );
    let unknown = pair(1, "missing", &e1).to_string();
    assert!(
        activation_summary(&store, SKILL, unknown.as_bytes())
            .await
            .is_none()
    );
    let current = pair(1, &b1, &e1).to_string();
    assert!(
        activation_summary(&store, SKILL, current.as_bytes())
            .await
            .is_none()
    );
    assert!(
        activation_summary(&store, "other", body_only.as_bytes())
            .await
            .is_none()
    );
}

#[tokio::test]
async fn a_rollback_summary_names_the_side_that_comes_back() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let b2 = body(&store, SkillOrigin::Imported, "2.0.0", "hello world two").await;
    let (_, first) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    let (status, _) = post(&state, SKILL, pair(1, &b2, &e1)).await;
    assert_eq!(status, StatusCode::OK);
    let request = json!({
        "expected_generation": 2, "rollback_activation_id": first["activation_id"]
    })
    .to_string();
    let summary = activation_summary(&store, SKILL, request.as_bytes())
        .await
        .unwrap();
    assert!(summary.starts_with("回滚技能"), "{summary}");
    assert!(summary.contains("换回正文"), "{summary}");
}

/// The detail lists the history a rollback names, with ids and codes only.
#[tokio::test]
async fn the_detail_lists_activations_without_cases_or_text() {
    let state = fixture().await;
    let store = state.store.clone();
    let b1 = body(&store, SkillOrigin::Imported, "1.0.0", "hello world").await;
    let e1 = evals(&store, SkillOrigin::Imported, "1.0.0", &["hello world"]).await;
    let (status, first) = post(&state, SKILL, pair(0, &b1, &e1)).await;
    assert_eq!(status, StatusCode::OK);
    let response = get_skill(State(state.clone()), AxumPath(SKILL.to_owned())).await;
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let detail: Value = serde_json::from_slice(&bytes).unwrap();
    let rows = detail["activations"].as_array().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["activation_id"], first["activation_id"]);
    assert_eq!(rows[0]["result_code"], "activated");
    assert_eq!(rows[0]["origin_from"], Value::Null);
    assert_eq!(rows[0]["origin_to"], "imported");
    assert!(rows[0].get("cases_json").is_none());
    assert!(!detail.to_string().contains("hello world"));
}
