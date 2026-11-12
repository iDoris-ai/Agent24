//! X11-2h acceptance ③. Desktop chat, ordinary `POST /runs`, and a schedule
//! tick do not add an inbound row whose disposition is anything but
//! `internal`. The schedule adds one `internal` row.
//!
//! ACP (`acp.rs`) and MCP (`agent24-mcp`) start a run through the same
//! `create_run` handler as ordinary `POST /api/v1/runs`. This calls that
//! handler in process. It does not spawn the daemon or a model.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;

use agent24_protocol::RunStatus;
use agent24_store::{InboundInsert, NewInboundEvent};
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};

use super::AppState;
use super::tests::state;

#[tokio::test]
async fn desktop_chat_plain_runs_and_scheduler_do_not_add_non_internal_inbound_rows() {
    let state = state().await;
    let stamp = "2026-11-11T00:00:00Z";
    let inserted = state
        .store
        .insert_inbound_event(&NewInboundEvent {
            id: "evt-seed-ignore".to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some("npub-seed".to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: stamp.to_owned(),
            received_at: stamp.to_owned(),
            text: Some("already here".to_owned()),
            meta: None,
            disposition: "ignore".to_owned(),
            disposition_reason: Some("not_allowlisted".to_owned()),
            policy_version: None,
            run_state: None,
        })
        .await
        .unwrap();
    assert_eq!(inserted, InboundInsert::Inserted);
    assert_eq!(count(&state, false).await, 1);
    assert_eq!(count(&state, true).await, 0);

    let chat = super::super::routes::post_chat(
        State(state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/v1/chat")
            .body(Body::from(
                r#"{"messages":[{"role":"user","content":"hello from chat"}]}"#,
            ))
            .unwrap(),
    )
    .await;
    let chat_status = chat.status();
    assert!(
        matches!(
            chat_status,
            StatusCode::SERVICE_UNAVAILABLE | StatusCode::INTERNAL_SERVER_ERROR
        ),
        "chat with no provider returned {chat_status}"
    );
    assert_eq!(
        count(&state, false).await,
        1,
        "chat added a non-internal row"
    );
    assert_eq!(count(&state, true).await, 0, "chat added an internal row");

    let created = super::super::runs::create_run(
        State(state.clone()),
        None,
        Request::builder()
            .method("POST")
            .uri("/api/v1/runs")
            .body(Body::from(r#"{"prompt":"hello from runs"}"#))
            .unwrap(),
    )
    .await;
    assert_eq!(created.status(), StatusCode::ACCEPTED);
    wait_until_ordinary_run_finished(&state).await;
    assert_eq!(
        count(&state, false).await,
        1,
        "POST /runs added a non-internal row"
    );
    assert_eq!(
        count(&state, true).await,
        0,
        "POST /runs added an internal row"
    );

    let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T10:00:00Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    state
        .scheduler
        .create(
            agent24_protocol::ScheduleCreate {
                name: "inbound-closeout".into(),
                enabled: true,
                spec: agent24_protocol::ScheduleSpec::Every { secs: 60 },
                action: agent24_protocol::ScheduleAction::AgentRun {
                    prompt: "scheduled".into(),
                    session_id: None,
                    model_override: None,
                },
                delivery: vec![],
            },
            now,
        )
        .await
        .unwrap();
    assert_eq!(
        state
            .scheduler
            .tick(now + chrono::Duration::seconds(61))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        count(&state, false).await,
        1,
        "scheduler added a non-internal row"
    );
    assert_eq!(
        count(&state, true).await,
        1,
        "scheduler should add one internal row"
    );
}

async fn count(state: &AppState, internal: bool) -> i64 {
    let sql = if internal {
        "SELECT COUNT(*) FROM inbound_events WHERE disposition = 'internal'"
    } else {
        "SELECT COUNT(*) FROM inbound_events WHERE disposition <> 'internal'"
    };
    sqlx::query_scalar(sql)
        .fetch_one(agent24_store::test_hooks::pool(&state.store))
        .await
        .unwrap()
}

async fn wait_until_ordinary_run_finished(state: &AppState) {
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    loop {
        let runs = state.store.list_runs(None).await.unwrap();
        let pending = runs.iter().any(|run| {
            run.schedule_id.is_none()
                && matches!(
                    run.status,
                    RunStatus::Queued | RunStatus::Running | RunStatus::AwaitingApproval
                )
        });
        if !pending && runs.iter().any(|run| run.schedule_id.is_none()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "ordinary run stayed queued or running: {runs:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}
