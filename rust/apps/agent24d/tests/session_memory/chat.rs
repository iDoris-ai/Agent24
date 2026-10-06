//! M1-T12: `/api/v1/chat`'s optional `session_id` reuses the SAME
//! `SessionMemory` recall/retain machinery `/api/v1/runs` uses (not a
//! parallel implementation) — these tests exercise that surface directly,
//! the same way `recall.rs`/`retain.rs` exercise the run path: a real
//! router, a mock provider, and the actual HTTP handler (`crate::routes::post_chat`).

use super::*;
use agent24_memory::{
    assertion::{AssertionStore, BeliefQuery},
    event::{EventStore, MemEvent, Origin, Scope, Trust},
};
use agent24_protocol::EventBody;
use axum::http::StatusCode;
use serde_json::json;

async fn personal_owner(kv: &KvStore) -> String {
    let org = OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
    partition_key(&org, &SpaceId::personal(LOCAL_USER))
}

/// Calls the real `/api/v1/chat` handler directly (no auth middleware, same
/// as `run_in_session` calling `crate::runs::create_run` directly) and
/// returns (status, parsed body).
async fn post_chat_with(
    state: &AppState,
    session_id: Option<&str>,
    prompt: &str,
) -> (StatusCode, serde_json::Value) {
    let mut body = json!({ "messages": [{ "role": "user", "content": prompt }] });
    if let Some(sid) = session_id {
        body["session_id"] = json!(sid);
    }
    let response = crate::routes::post_chat(
        State(state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/v1/chat")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap(),
    )
    .await;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn last_messages_mention(provider: &Provider, needle: &str) -> bool {
    let calls = provider.received.lock().unwrap();
    let last = calls.last().expect("provider must have received a call");
    last.iter()
        .any(|m| m.content.as_deref().unwrap_or("").contains(needle))
}

#[tokio::test]
async fn explicit_remember_over_chat_stores_a_qualified_assertion() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    ensure_session(&state, "chat-session-a").await;

    let (status, body) = post_chat_with(&state, Some("chat-session-a"), "记住我对花生过敏").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body["message"]["content"].as_str().unwrap().is_empty());
    let owner = personal_owner(&kv).await;
    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].object, json!("我对花生过敏"));
}

#[tokio::test]
async fn a_later_chat_in_a_different_session_recalls_the_same_owners_fact() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    ensure_session(&state, "chat-session-s1").await;
    ensure_session(&state, "chat-session-s2").await;
    let mut events = state.events.subscribe();

    post_chat_with(&state, Some("chat-session-s1"), "记住我对花生过敏").await;

    let (status, _body) = post_chat_with(&state, Some("chat-session-s2"), "我对什么过敏？").await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        last_messages_mention(&provider, "花生"),
        "the provider request for session s2 must carry the recalled fact"
    );
    assert!(
        last_messages_mention(&provider, agent24_agent::RECALL_PREFIX),
        "the recalled fact must arrive as the annotated data block, not a bare string"
    );
    let recalled = std::iter::from_fn(|| events.try_recv().ok())
        .any(|(_, body)| matches!(body, EventBody::MemoryRecalled(_)));
    assert!(
        recalled,
        "memory.recalled must be emitted for the chat path too"
    );
}

#[tokio::test]
async fn paused_memory_skips_the_store_and_tells_the_model_so_over_chat() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    ensure_session(&state, "chat-session-paused").await;
    let owner = personal_owner(&kv).await;
    kv.set_memory_enabled(&owner, false).await.unwrap();
    let mut events = state.events.subscribe();

    let (status, body) =
        post_chat_with(&state, Some("chat-session-paused"), "记住我对花生过敏").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body["message"]["content"].as_str().unwrap().is_empty());
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
            .unwrap()
            .is_empty(),
        "paused memory must reject the write"
    );
    assert!(
        last_messages_mention(&provider, "记忆已暂停"),
        "the model must be told the write was skipped, same as the run path's notice"
    );
    let skipped = std::iter::from_fn(|| events.try_recv().ok())
        .any(|(_, body)| matches!(body, EventBody::MemoryWriteSkipped(_)));
    assert!(
        skipped,
        "memory.write_skipped must be emitted for the chat path too"
    );
}

#[tokio::test]
async fn chat_without_a_session_id_neither_recalls_nor_stores() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;

    post_chat_with(&state, None, "记住我对花生过敏").await;

    let owner = personal_owner(&kv).await;
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
            .unwrap()
            .is_empty(),
        "no session_id must mean no write at all — unchanged stateless behaviour"
    );

    let (status, _body) = post_chat_with(&state, None, "我对什么过敏？").await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        !last_messages_mention(&provider, agent24_agent::RECALL_PREFIX),
        "no session_id must mean no recall either"
    );
}

#[tokio::test]
async fn a_retain_failure_still_returns_200_with_content() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    let session_id = "chat-session-retain-failure";
    ensure_session(&state, session_id).await;
    let owner = personal_owner(&kv).await;
    let object = "我对花生过敏";
    // Same technique as retain.rs's `audit_id_conflict_...` test: pre-occupy
    // the audit event id `persist` will try to reserve for this turn's
    // candidate, forcing the retain write to fail deterministically without
    // touching any production code path.
    let candidate_id = agent24_memory::artifact::checksum(&format!("{owner}{object}"));
    let evidence = agent24_memory::artifact::checksum(
        &serde_json::to_string(&(owner.as_str(), session_id, 0u64, "user")).unwrap(),
    );
    let canonical = format!(
        "commit|{candidate_id}|user|said_to_remember|{}|{:?}",
        json!(object),
        vec![evidence]
    );
    let occupied_id = format!(
        "audit-{candidate_id}-{}",
        &agent24_memory::artifact::checksum(&canonical)[..16]
    );
    let occupied = MemEvent::new(
        occupied_id,
        Scope::owner(&owner),
        "test.audit_id_reservation",
        json!({"reservation": "preserve me"}),
        Origin {
            source: "test".into(),
            trust: Trust::System,
        },
    );
    kv.events().append(&occupied).await.unwrap();
    let mut events = state.events.subscribe();

    let (status, body) = post_chat_with(&state, Some(session_id), &format!("记住{object}")).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a retain failure must never drop the already-computed chat response"
    );
    assert!(!body["message"]["content"].as_str().unwrap().is_empty());
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
            .unwrap()
            .is_empty(),
        "the collision must have actually prevented the write"
    );
    let write_failed = std::iter::from_fn(|| events.try_recv().ok())
        .any(|(_, body)| matches!(body, EventBody::MemoryWriteFailed(_)));
    assert!(write_failed, "memory.write_failed must be observable");
}
