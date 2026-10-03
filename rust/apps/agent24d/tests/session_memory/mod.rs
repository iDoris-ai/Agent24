//! M1-T05: real daemon run entry, forced compaction, disk reopen and exact replay.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod recall;

use super::*;
use crate::os_memory::{OrgId, SpaceId, partition_key};
use agent24_memory::{
    KvStore,
    event::{EventQuery, EventStore},
    replay::{replay_history, replayed_from_events},
    session::CompactionPolicy,
};
use agent24_models::{
    CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg, router::Tier,
};
use agent24_protocol::{Model, Run, RunStatus, Session, Usage};
use axum::{body::Body, extract::State, http::Request};
use std::{path::Path, sync::Mutex};

const SESSION: &str = "m1-t05-session";

#[derive(Default)]
struct Provider {
    received: Mutex<Vec<Vec<Msg>>>,
    summaries: Mutex<usize>,
    fail_summaries: bool,
    answer: Option<String>,
}

#[async_trait::async_trait]
impl ModelProvider for Provider {
    fn name(&self) -> &str {
        "m1-t05-mock"
    }

    async fn complete(
        &self,
        req: &CompletionRequest,
        _: &CancellationToken,
    ) -> Result<CompletionResponse, ModelError> {
        let summary = req
            .messages
            .first()
            .and_then(|m| m.content.as_deref())
            .is_some_and(|s| {
                s.starts_with("Summarize this conversation")
                    || s.starts_with("Update this running summary")
            });
        let content = if summary {
            let mut count = self.summaries.lock().unwrap();
            *count += 1;
            // Empty output makes the real RouterSummarizer fail deterministically.
            if self.fail_summaries {
                String::new()
            } else {
                format!("summary-{count}")
            }
        } else {
            let mut received = self.received.lock().unwrap();
            received.push(req.messages.clone());
            self.answer
                .clone()
                .unwrap_or_else(|| format!("answer-{}", received.len()))
        };
        Ok(CompletionResponse {
            message: Msg::assistant(Some(content), vec![]),
            usage: Usage::default(),
            model_id: Some("mock".into()),
        })
    }

    async fn models(&self, _: &CancellationToken) -> Result<Vec<Model>, ModelError> {
        Ok(vec![])
    }
}

async fn app(kv: KvStore, path: &Path, provider: Arc<Provider>) -> AppState {
    app_with_recall_budget(kv, path, provider, None).await
}

async fn app_with_recall_budget(
    kv: KvStore,
    path: &Path,
    provider: Arc<Provider>,
    recall_budget: Option<usize>,
) -> AppState {
    let max_recent = if provider.fail_summaries { 1 } else { 2 };
    let router = Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)]));
    let cancel = CancellationToken::new();
    // Use exactly the production constructor; never register a partition in setup.
    let mut memory = session_memory(kv, &router, &cancel)
        .await
        .unwrap()
        .with_policy(CompactionPolicy {
            max_recent,
            keep_recent: 1,
            max_summary_chars: 2000,
        });
    if let Some(budget) = recall_budget {
        memory = memory.with_recall_budget(budget);
    }
    let store = Store::open(&path.join("agent24.db")).await.unwrap();
    if store.get_session(SESSION).await.unwrap().is_none() {
        store
            .insert_session(&Session {
                workspace_id: None,
                id: SESSION.into(),
                title: String::new(),
                channel: "test".into(),
                created_at: "2026-10-01T00:00:00Z".into(),
                updated_at: "2026-10-01T00:00:00Z".into(),
            })
            .await
            .unwrap();
    }
    AppState::new(AppDeps {
        workspace_service: None,
        token: "test".into(),
        router,
        tools: agent24_tools::ToolRegistry::new(),
        store,
        shutdown: Shutdown::new(cancel),
        guardian: None,
        memory: Some(memory),
        mcp_servers: vec![],
        risk_overrides: StdArc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
            vec![],
        )),
        packages_root: Arc::new(path.to_path_buf()),
    })
}

async fn run(state: &AppState, prompt: &str) -> Run {
    run_in_session(state, SESSION, prompt).await
}

async fn run_in_session(state: &AppState, session_id: &str, prompt: &str) -> Run {
    let response = crate::runs::create_run(
        State(state.clone()),
        Request::builder()
            .method("POST")
            .uri("/api/v1/runs")
            .body(Body::from(
                serde_json::json!({"session_id": session_id, "prompt": prompt}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), axum::http::StatusCode::ACCEPTED);
    let created: Run = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let current = state.store.get_run(&created.id).await.unwrap().unwrap();
            if !matches!(current.status, RunStatus::Running | RunStatus::Queued) {
                assert_eq!(current.status, RunStatus::Completed);
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("run must finish");
    created
}

async fn ensure_session(state: &AppState, session_id: &str) {
    if state.store.get_session(session_id).await.unwrap().is_none() {
        state
            .store
            .insert_session(&Session {
                workspace_id: None,
                id: session_id.into(),
                title: String::new(),
                channel: "test".into(),
                created_at: "2026-10-01T00:00:00Z".into(),
                updated_at: "2026-10-01T00:00:00Z".into(),
            })
            .await
            .unwrap();
    }
}

async fn assert_history(kv: &KvStore, owner: &str, expected: &[Msg]) {
    let log = kv.events();
    let query = EventQuery::owner(owner).session(SESSION);
    let mut cursor = 0;
    let mut events = Vec::new();
    loop {
        let page = log
            .scan(&query.clone().after(cursor).limit(2))
            .await
            .unwrap();
        if page.is_empty() {
            break;
        }
        cursor = page.last().unwrap().seq;
        events.extend(page);
    }
    assert!(events.iter().all(|e| e.event.scope.owner == owner));
    assert_eq!(
        replayed_from_events(&events).unwrap().messages,
        expected,
        "every original must survive, including those covered by summaries"
    );
    assert_eq!(
        replay_history(&log, &query).await.unwrap().messages,
        expected,
        "replay must return every original in order"
    );
    assert!(
        log.scan(&EventQuery::owner(LOCAL_USER).session(SESSION))
            .await
            .unwrap()
            .is_empty(),
        "bare user id must never own session events"
    );
}

async fn restart_and_replay(fail_summaries: bool) {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let mut original = Vec::new();
    // Only value snapshots cross this scope: no manager, KV, router or provider survives.
    let (owner, mut expected_context) = {
        let provider = Arc::new(Provider {
            fail_summaries,
            ..Provider::default()
        });
        let kv = KvStore::open(&db).await.unwrap();
        let state = app(kv.clone(), dir.path(), provider.clone()).await;
        for i in 1..=3 {
            let prompt = format!("question-{i}");
            run(&state, &prompt).await;
            original.extend([
                Msg::user(prompt),
                Msg::assistant(Some(format!("answer-{i}")), vec![]),
            ]);
        }
        let org = OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
        let owner = partition_key(&org, &SpaceId::personal(LOCAL_USER));
        let rows = kv.os_partitions_for_org(org.as_str()).await.unwrap();
        assert_eq!(
            rows.len(),
            1,
            "startup must register exactly one personal partition"
        );
        assert_eq!(
            (
                &rows[0].owner_key,
                rows[0].space_id.as_str(),
                rows[0].module_name.as_str(),
                rows[0].space_kind.as_str()
            ),
            (&owner, "usr:local", "@agent", "personal")
        );
        let before = kv.session_log().load_view(&owner, SESSION).await.unwrap();
        let count = *provider.summaries.lock().unwrap();
        assert!(count >= 2, "must actually attempt repeated compaction");
        let context = if fail_summaries {
            assert!(before.summary.is_none());
            assert_eq!(
                before.tail.into_iter().map(|(_, m)| m).collect::<Vec<_>>(),
                original
            );
            assert!(original.len() > 4, "must exceed 4 * max_recent");
            original[original.len() - 4..].to_vec()
        } else {
            assert_eq!(before.summary, Some(format!("summary-{count}")));
            assert_eq!(
                before.tail.len(),
                1,
                "keep a real uncovered tail alongside summary"
            );
            let mut context = vec![Msg::system(format!(
                "Summary of earlier conversation:\n{}",
                before.summary.unwrap()
            ))];
            context.extend(before.tail.into_iter().map(|(_, m)| m));
            context
        };
        let store_pool = agent24_store::test_hooks::pool(&state.store).clone();
        drop(state);
        store_pool.close().await;
        (owner, context)
    };
    let kv = KvStore::open(&db).await.unwrap();
    assert_history(&kv, &owner, &original).await;
    let provider = Arc::new(Provider {
        fail_summaries,
        ..Provider::default()
    });
    let state = app(kv.clone(), dir.path(), provider.clone()).await;
    run(&state, "question-4").await;
    expected_context.push(Msg::user("question-4"));
    assert_eq!(
        *provider.received.lock().unwrap(),
        vec![expected_context],
        "the fourth real run must receive exactly the pre-restart context plus its prompt"
    );
    original.extend([
        Msg::user("question-4"),
        Msg::assistant(Some("answer-1".into()), vec![]),
    ]);
    assert_history(&kv, &owner, &original).await;
}

#[path = "retain.rs"]
mod retain;

#[tokio::test]
async fn successful_compaction_replays_every_original_message_after_daemon_restart() {
    restart_and_replay(false).await;
}

#[tokio::test]
async fn repeated_summary_failure_keeps_all_original_messages_in_event_log() {
    restart_and_replay(true).await;
}
