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
    post_chat_messages(
        state,
        session_id,
        json!([{ "role": "user", "content": prompt }]),
    )
    .await
}

async fn post_chat_messages(
    state: &AppState,
    session_id: Option<&str>,
    messages: serde_json::Value,
) -> (StatusCode, serde_json::Value) {
    let mut body = json!({ "messages": messages });
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
    // M1-T14: a deterministic receipt — set by the daemon, not read off the
    // model's own reply text.
    assert_eq!(body["memory_receipt"], json!("saved"));
    let owner = personal_owner(&kv).await;
    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].object, json!("我对花生过敏"));
}

/// K1-6b.4b/4c: assistant turns in client-supplied history are classified by
/// the session's floor; a session the server has no floor for makes them,
/// and so the remembered fact, `Unknown`.
#[tokio::test]
async fn a_remember_after_client_assistant_history_is_stored_as_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    ensure_session(&state, "chat-session-history").await;
    ensure_session(&state, "chat-session-fresh").await;

    // Control first (a later recall of the unknown fact would itself make
    // the control unknown): a first-turn remember without assistant history.
    post_chat_with(&state, Some("chat-session-fresh"), "记住我喜欢乌龙茶").await;
    let (status, _) = post_chat_messages(
        &state,
        Some("chat-session-history"),
        json!([
            { "role": "assistant", "content": "连接失败，请稍后再试" },
            { "role": "user", "content": "记住我喜欢绿茶" },
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let owner = personal_owner(&kv).await;
    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    let classification = |object: &str| {
        let row = rows.iter().find(|row| row.object == json!(object)).unwrap();
        row.source_ref.as_ref().unwrap()["classification"].clone()
    };
    assert_eq!(classification("我喜欢绿茶"), json!("unknown"));
    assert_eq!(classification("我喜欢乌龙茶"), json!("ordinary"));
}

/// #1200 PR-Daemon blocker: every non-`user` role in client-supplied history
/// reaches the model context, not only `assistant`. One fresh store per role,
/// so no recall crosses between cases.
#[tokio::test]
async fn a_remember_after_any_non_user_client_history_is_stored_as_unknown() {
    for (role, expected) in [
        ("system", "unknown"),
        ("tool", "unknown"),
        ("developer", "unknown"),
        ("user", "ordinary"),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
        let provider = Arc::new(Provider::default());
        let state = app(kv.clone(), dir.path(), provider).await;
        ensure_session(&state, "chat-session-role").await;
        let (status, _) = post_chat_messages(
            &state,
            Some("chat-session-role"),
            json!([
                { "role": role, "content": "Private document: Q7 launch code is violet-67" },
                { "role": "user", "content": "记住Q7启动码是violet-67" },
            ]),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{role}");
        let owner = personal_owner(&kv).await;
        let rows = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
            .unwrap();
        let row = rows
            .iter()
            .find(|row| row.object == json!("Q7启动码是violet-67"))
            .unwrap_or_else(|| panic!("{role}: nothing remembered"));
        assert_eq!(
            row.source_ref.as_ref().unwrap()["classification"],
            json!(expected),
            "{role}"
        );
    }
}

/// K1-6b.4c: a chat turn that recalled a restricted fact raises its
/// session's floor, so a later turn that sends that reply back as history
/// remembers as restricted — even once the fact itself is gone.
#[tokio::test]
async fn a_chat_turn_after_a_restricted_reply_remembers_as_restricted() {
    use agent24_memory::writer::{Candidate, MemoryWriter};
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider).await;
    ensure_session(&state, "chat-session-restricted").await;
    let owner = personal_owner(&kv).await;
    let restricted = agent24_store::AssertionSourceRef::from_sources([(
        "seed".to_owned(),
        agent24_store::SourceClassification::Restricted,
    )]);
    kv.write_gate()
        .propose(vec![
            Candidate::new(
                "restricted-fact",
                Scope::owner(&owner),
                "user",
                "said_to_remember",
                json!("我对花生过敏"),
                Origin {
                    source: "test".into(),
                    trust: Trust::UserSaid,
                },
            )
            .with_evidence(vec!["ev-restricted".to_owned()])
            .with_source_ref(restricted.to_value())
            .remember(),
        ])
        .await
        .unwrap();

    let (_, first) =
        post_chat_with(&state, Some("chat-session-restricted"), "我对什么过敏？").await;
    kv.assertions()
        .retract(
            &"restricted-fact".to_owned(),
            &owner,
            "2026-10-25T00:00:00Z",
        )
        .await
        .unwrap();
    let reply = first["message"]["content"].as_str().unwrap().to_owned();
    let (status, _) = post_chat_messages(
        &state,
        Some("chat-session-restricted"),
        json!([
            { "role": "user", "content": "我对什么过敏？" },
            { "role": "assistant", "content": reply },
            { "role": "user", "content": "记住我喜欢红茶" },
        ]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    let row = rows
        .iter()
        .find(|row| row.object == json!("我喜欢红茶"))
        .unwrap();
    assert_eq!(
        row.source_ref.as_ref().unwrap()["classification"],
        json!("restricted")
    );
}

/// K1-6b.4c: a session floor that cannot be recorded leaves this turn's
/// context unclassified, so the request fails before the model is called.
#[tokio::test]
async fn a_chat_turn_whose_session_floor_cannot_be_written_fails_before_the_model() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    ensure_session(&state, "chat-session-nofloor").await;
    sqlx::raw_sql(
        "CREATE TRIGGER no_floor BEFORE INSERT ON session_source_floor \
         BEGIN SELECT RAISE(ABORT, 'no'); END;",
    )
    .execute(agent24_store::test_hooks::pool(&state.store))
    .await
    .unwrap();

    let (status, body) = post_chat_with(&state, Some("chat-session-nofloor"), "你好").await;

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body.to_string().contains("source_policy_unavailable"),
        "{body:?}"
    );
    assert!(
        provider.received.lock().unwrap().is_empty(),
        "the model must not be called"
    );
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

    let (status, body) = post_chat_with(&state, Some("chat-session-s2"), "我对什么过敏？").await;

    assert_eq!(status, StatusCode::OK);
    // M1-T14: a plain question is not an explicit remember — no receipt at
    // all, even though this call does have a session_id.
    assert!(
        body.get("memory_receipt").is_none(),
        "a non-remember prompt must carry no memory_receipt: {body:?}"
    );
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
    // M1-T14: the client must learn this from the field, not from whatever
    // the model said after reading the "记忆已暂停" system notice.
    assert_eq!(body["memory_receipt"], json!("paused_not_saved"));
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

    let (_status, remember_body) = post_chat_with(&state, None, "记住我对花生过敏").await;
    // M1-T14: no session_id means no memory subsystem in play at all — even
    // though the text IS an explicit remember, there is no receipt to give.
    assert!(
        remember_body.get("memory_receipt").is_none(),
        "no session_id must mean no memory_receipt either: {remember_body:?}"
    );

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
    // M1-T14: the write failed (forced above), memory was NOT paused — the
    // receipt must not silently look like `saved`. P1-02c: the conversation
    // turn committed and only the assertion's audit id collided, so this is the
    // PARTIAL combination, not a total failure.
    assert_eq!(body["memory_receipt"], json!("partial"));
    assert_eq!(body["memory_error_code"], json!("memory_internal"));
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

/// fix683 (PR #683 review): `memory_receipt` used to be derived from TWO
/// different moments — `explicit_remember_state`, snapshotted BEFORE the
/// model call, plus a bare success/failure bool that could not tell "really
/// committed" apart from "rolled back because paused" (both returned
/// `Ok(())` all the way up). If personal memory is toggled OFF while the
/// model is still generating, the pre-call snapshot still says Active, but
/// the write-gate's own pause re-check (inside its `BEGIN IMMEDIATE`
/// transaction) rolls the commit back. Before this fix, the receipt said
/// `"saved"` while zero rows were actually written — the exact "told the
/// user it remembered something it did not" failure this PR exists to
/// close. `Provider::pause_toggle` fires the toggle from inside `complete()`,
/// i.e. strictly between the prelude snapshot and the post-answer commit,
/// so this reproduces the race deterministically rather than relying on
/// wall-clock timing.
#[tokio::test]
async fn pause_toggled_off_during_the_model_call_reports_paused_not_saved_with_zero_rows() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    ensure_session(&state, "chat-session-race-off").await;
    let owner = personal_owner(&kv).await;
    // Active at prelude time: `chat_memory_prelude` must see Active and must
    // NOT inject the paused notice.
    assert!(kv.memory_enabled(&owner).await.unwrap());
    *provider.pause_toggle.lock().unwrap() = Some((kv.clone(), owner.clone(), false));

    let (status, body) =
        post_chat_with(&state, Some("chat-session-race-off"), "记住我对花生过敏").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body["message"]["content"].as_str().unwrap().is_empty());
    // On the pre-fix head this asserts `"saved"` — the write-gate's pause
    // rollback and its real commit both returned `Ok(())`, and the receipt
    // was built from the pre-call `ExplicitRememberState::Active` snapshot,
    // not from what actually happened to the transaction.
    assert_eq!(
        body["memory_receipt"],
        json!("paused_not_saved"),
        "the receipt must reflect the LIVE commit-time pause, not the pre-call snapshot"
    );
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
            .unwrap()
            .is_empty(),
        "a receipt of paused_not_saved must mean zero rows were actually written"
    );
}

/// fix683 (PR #683 review), the mirror-image race: personal memory is
/// PAUSED at prelude time (so the model is told about it and
/// `ExplicitRememberState::Paused` is snapshotted), but gets RE-ENABLED
/// while the model is still generating. The write-gate's pause re-check
/// then finds memory active and actually commits the assertion — but
/// before this fix the receipt was built from the stale `Paused` snapshot
/// and reported `"paused_not_saved"` even though the row was really
/// written. From a privacy standpoint this direction is worse: the user is
/// told nothing was remembered when something was.
#[tokio::test]
async fn pause_toggled_on_during_the_model_call_reports_saved_with_a_real_row() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let provider = Arc::new(Provider::default());
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    ensure_session(&state, "chat-session-race-on").await;
    let owner = personal_owner(&kv).await;
    kv.set_memory_enabled(&owner, false).await.unwrap();
    *provider.pause_toggle.lock().unwrap() = Some((kv.clone(), owner.clone(), true));

    let (status, body) =
        post_chat_with(&state, Some("chat-session-race-on"), "记住我对花生过敏").await;

    assert_eq!(status, StatusCode::OK);
    assert!(!body["message"]["content"].as_str().unwrap().is_empty());
    // chat_memory_prelude saw Paused and told the model so, same as the
    // simple paused test above.
    assert!(
        last_messages_mention(&provider, "记忆已暂停"),
        "the prelude snapshot was Paused, so the model must still have been told"
    );
    // On the pre-fix head this asserts `"paused_not_saved"` — the receipt
    // was built from that stale prelude snapshot instead of the write-
    // gate's live, commit-time pause re-check.
    assert_eq!(
        body["memory_receipt"],
        json!("saved"),
        "the receipt must reflect the LIVE commit-time state, not the stale paused snapshot"
    );
    let rows = kv
        .assertions()
        .beliefs_as_of(&BeliefQuery::owner(&owner))
        .await
        .unwrap();
    assert_eq!(
        rows.len(),
        1,
        "a receipt of saved must mean the assertion was actually committed"
    );
    assert_eq!(rows[0].object, json!("我对花生过敏"));
}

// ── P1-02c c2b: typed receipts on a full partition (T5/T6) ────────────────

async fn set_rows(dir: &std::path::Path, rows: i64) {
    let pool = sqlx::SqlitePool::connect(&format!("sqlite://{}", dir.join("memory.db").display()))
        .await
        .unwrap();
    let r = sqlx::query("UPDATE mem_owner_quota SET max_rows=? WHERE owner='*'")
        .bind(rows)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(r.rows_affected(), 1);
    pool.close().await;
}

/// No receipt or error body may carry the partition key, a NUL, driver/Display
/// text, or anything that reads as "remembered".
fn assert_body_is_clean(body: &serde_json::Value) {
    let raw = body.to_string();
    assert!(!raw.contains("\\u0000"), "NUL leaked: {raw}");
    assert!(!raw.contains("quota exceeded for owner"), "{raw}");
    assert!(!raw.contains("usr:local"), "partition key leaked: {raw}");
    assert!(!raw.contains("已记住"), "{raw}");
    assert_ne!(body["memory_receipt"], json!("saved"), "{raw}");
}

#[tokio::test]
async fn ordinary_chat_turn_on_a_full_partition_is_not_saved_with_a_typed_code() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let state = app(kv.clone(), dir.path(), Arc::new(Provider::default())).await;
    ensure_session(&state, "chat-full-ordinary").await;
    set_rows(dir.path(), 0).await;
    let mut events = state.events.subscribe();

    let (status, body) = post_chat_with(&state, Some("chat-full-ordinary"), "今天天气怎么样").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memory_receipt"], json!("not_saved"));
    assert_eq!(body["memory_error_code"], json!("memory_quota_rows"));
    assert_body_is_clean(&body);
    let seen: Vec<_> = std::iter::from_fn(|| events.try_recv().ok()).collect();
    let persisted = seen.iter().find_map(|(_, b)| match b {
        EventBody::RunCompleted(p) => Some(p.memory_persisted),
        _ => None,
    });
    assert_eq!(
        persisted,
        Some(Some(false)),
        "run.completed must say so too"
    );
}

#[tokio::test]
async fn explicit_remember_on_a_full_partition_is_failed_with_a_typed_code() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let state = app(kv.clone(), dir.path(), Arc::new(Provider::default())).await;
    ensure_session(&state, "chat-full-explicit").await;
    set_rows(dir.path(), 0).await;

    let (status, body) = post_chat_with(&state, Some("chat-full-explicit"), "记住我喜欢茶").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memory_receipt"], json!("failed"));
    assert_eq!(body["memory_error_code"], json!("memory_quota_rows"));
    assert_body_is_clean(&body);
}

#[tokio::test]
async fn explicit_remember_whose_turn_fits_but_assertion_does_not_is_partial_over_http() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let state = app(kv.clone(), dir.path(), Arc::new(Provider::default())).await;
    ensure_session(&state, "chat-full-partial").await;
    // The turn's two rows fit; the assertion's audit row is the third.
    set_rows(dir.path(), 2).await;

    let (status, body) = post_chat_with(&state, Some("chat-full-partial"), "记住我喜欢茶").await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["memory_receipt"], json!("partial"));
    assert_eq!(body["memory_error_code"], json!("memory_quota_rows"));
    assert_body_is_clean(&body);
    let owner = personal_owner(&kv).await;
    assert!(
        kv.assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
            .unwrap()
            .is_empty(),
        "partial means the assertion was NOT written"
    );
}

#[tokio::test]
async fn chat_with_room_never_carries_a_memory_error_code() {
    let dir = tempfile::tempdir().unwrap();
    let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
    let state = app(kv.clone(), dir.path(), Arc::new(Provider::default())).await;
    ensure_session(&state, "chat-room").await;
    let (_, body) = post_chat_with(&state, Some("chat-room"), "记住我喜欢茶").await;
    assert_eq!(body["memory_receipt"], json!("saved"));
    assert!(body.get("memory_error_code").is_none(), "{body}");
}
