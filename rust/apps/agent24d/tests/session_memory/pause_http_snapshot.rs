//! R03: one real HTTP pause must not rewrite an existing run snapshot.
//!
//! Pause is `PUT /api/v1/memory/settings` (`put_memory_settings`). It is the
//! personal-memory switch, not `DELETE /api/v1/memory/assertions/{id}`.
//! After the switch flips off, the run's persisted `content`/`seq`, the
//! session history's original text, and the still-current assertion stay.

use super::*;
use agent24_memory::assertion::{AssertionStore, BeliefQuery};
use axum::http::StatusCode;
use serde_json::{Value, json};
use tower::ServiceExt;

const SESSION_ID: &str = "r03-http-pause-session";
const PROMPT: &str = "记住我对花生过敏";
const FACT: &str = "我对花生过敏";

fn snapshot_content_seq(rows: &[agent24_store::RunMessage]) -> Vec<(i64, Option<String>)> {
    rows.iter()
        .map(|row| (row.seq, row.content.clone()))
        .collect()
}

fn history_content_seq(events: &[agent24_memory::event::StoredEvent]) -> Vec<(i64, String)> {
    events
        .iter()
        .filter(|event| event.event.kind == "message")
        .map(|event| {
            let content = event
                .event
                .body
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            (event.seq, content)
        })
        .collect()
}

async fn session_events(kv: &KvStore, owner: &str) -> Vec<agent24_memory::event::StoredEvent> {
    kv.events()
        .scan(&EventQuery::owner(owner).session(SESSION_ID))
        .await
        .unwrap()
}

async fn http(
    state: &AppState,
    method: &str,
    uri: &str,
    body: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", format!("Bearer {}", state.token));
    if body.is_some() {
        builder = builder.header("content-type", "application/json");
    }
    let response = crate::server::build_router(state.clone())
        .oneshot(
            builder
                .body(Body::from(body.unwrap_or("").to_owned()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn http_pause_leaves_existing_run_snapshot_content_and_seq_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let state = app(kv.clone(), dir.path(), Arc::new(Provider::default())).await;
    ensure_session(&state, SESSION_ID).await;
    let owner = state
        .memory_owner
        .clone()
        .expect("the daemon fixture must inject the personal owner");
    assert!(
        kv.memory_enabled(&owner).await.unwrap(),
        "a fresh owner starts enabled, so the run below can persist a fact"
    );

    let created = run_in_session(&state, SESSION_ID, PROMPT).await;
    let snapshot_before = state.store.list_run_messages(&created.id).await.unwrap();
    let snapshot_pairs = snapshot_content_seq(&snapshot_before);
    assert!(
        snapshot_pairs
            .iter()
            .any(|(seq, content)| *seq >= 0 && content.as_deref() == Some(PROMPT)),
        "the persisted run snapshot must contain the original prompt: {snapshot_pairs:?}"
    );

    let history_before = session_events(&kv, &owner).await;
    let history_pairs = history_content_seq(&history_before);
    assert!(
        history_pairs.iter().any(|(_, content)| content == PROMPT),
        "session history must keep the original prompt text: {history_pairs:?}"
    );
    let replayed = replay_history(&kv.events(), &EventQuery::owner(&owner).session(SESSION_ID))
        .await
        .unwrap();
    assert!(
        replayed
            .messages
            .iter()
            .any(|message| message.role == "user" && message.content.as_deref() == Some(PROMPT)),
        "replay must still return the original user turn"
    );

    let beliefs = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    assert_eq!(beliefs.len(), 1, "the explicit remember must be current");
    assert_eq!(beliefs[0].object, json!(FACT));
    assert!(
        beliefs[0].recorded_to.is_none(),
        "the fact is active; retract would have closed recorded_to"
    );
    let belief_id = beliefs[0].id.clone();
    let recorded_from = beliefs[0].recorded_from.clone();
    let owner_events = kv.events().scan(&EventQuery::owner(&owner)).await.unwrap();
    assert!(
        owner_events
            .iter()
            .all(|event| event.event.kind != "assertion.retracted"),
        "no retract event exists before pause"
    );

    // Real router + auth, not `KvStore::set_memory_enabled`.
    let (status, body) = http(
        &state,
        "PUT",
        "/api/v1/memory/settings",
        Some(r#"{"enabled":false}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], json!(false));
    assert!(
        !kv.memory_enabled(&owner).await.unwrap(),
        "the HTTP pause must have persisted on the same owner"
    );

    let snapshot_after = state.store.list_run_messages(&created.id).await.unwrap();
    assert_eq!(
        snapshot_content_seq(&snapshot_after),
        snapshot_pairs,
        "HTTP pause must not change existing run snapshot content or seq"
    );

    let history_after = session_events(&kv, &owner).await;
    assert_eq!(
        history_content_seq(&history_after),
        history_pairs,
        "HTTP pause must leave the original history text and its event seq"
    );
    let replayed_after =
        replay_history(&kv.events(), &EventQuery::owner(&owner).session(SESSION_ID))
            .await
            .unwrap();
    assert_eq!(
        replayed_after
            .messages
            .iter()
            .map(|message| (message.role.clone(), message.content.clone()))
            .collect::<Vec<_>>(),
        replayed
            .messages
            .iter()
            .map(|message| (message.role.clone(), message.content.clone()))
            .collect::<Vec<_>>(),
        "replayed history must still be the pre-pause original"
    );

    let beliefs_after = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    assert_eq!(
        beliefs_after.len(),
        1,
        "pause must not drop the current belief"
    );
    assert_eq!(beliefs_after[0].id, belief_id);
    assert_eq!(beliefs_after[0].object, json!(FACT));
    assert_eq!(beliefs_after[0].recorded_from, recorded_from);
    assert!(
        beliefs_after[0].recorded_to.is_none(),
        "pause is not retract: recorded_to stays open"
    );
    let owner_events_after = kv.events().scan(&EventQuery::owner(&owner)).await.unwrap();
    assert!(
        owner_events_after
            .iter()
            .all(|event| event.event.kind != "assertion.retracted"),
        "pause must not append assertion.retracted"
    );

    let (list_status, listed) = http(&state, "GET", "/api/v1/memory/assertions", None).await;
    assert_eq!(list_status, StatusCode::OK);
    let items = listed["assertions"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], json!(belief_id));
    assert_eq!(items[0]["text"], json!(FACT));
    assert_eq!(
        items[0]["status"],
        json!("active"),
        "list still reports active; retract would remove or close the row"
    );
}
