//! K1-6b.5c-2 (ADR-K1-02 §2.4): the run views a peer bridge relays.
//!
//! The MCP server and the ACP bridge hand run content to a process outside
//! the kernel, so they read runs through these endpoints instead of
//! `/api/v1/runs`. A run's prompt, output and error leave only when every
//! source tagged on it is Ordinary; a Restricted or Unknown source, no tags at
//! all, or a failed tag read all withhold it (David 2026-10-25, option 丙).
//!
//! Tool results are not tagged yet (ADR-K1-02 §2.1 says they should be;
//! 6b.5f), and what a tool returned travels on untagged: through a session's
//! memory of earlier turns (including `/chat` turns, which are not stored as
//! runs), through a remembered fact recalled elsewhere, through a self-wake
//! prompt. Until it is tagged, only a run that cannot have taken any of it in
//! is relayed: no tool call, no recalled memory, not started by a
//! kernel-created schedule, and no session context — no session, or it ran,
//! and the daemon runs, without session memory (David 2026-10-26, option A).

use agent24_protocol::{PeerRun, Run, RunStatus};
use agent24_store::{SourceClassification, SourceKind, strictest_classification};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::server::{AppState, error_response};

/// The only text a withheld run carries. Fixed, so nothing about the run's
/// sources or content can reach a peer through it.
pub(crate) const WITHHELD_NOTICE: &str = "this run used sources that are not ordinary, so its \
     content is not shared with connected tools; open it in Agent24 to see it";

/// The classification a peer may rely on for `run`. `Unknown` whenever the
/// run could have taken in content no tag describes, or its record cannot be
/// read.
async fn peer_classification(state: &AppState, run: &Run) -> SourceClassification {
    const UNKNOWN: SourceClassification = SourceClassification::Unknown;
    // Earlier turns of a session reach the model only through session memory.
    // Whether the run had it is fixed when it ran (its session-floor tag says
    // so, below); the daemon having it now covers a run whose tags cannot say.
    if run.session_id.is_some() && state.memory_kv.is_some() {
        return UNKNOWN;
    }
    // Every tool call is recorded before the tool runs, and a call that
    // cannot be recorded does not run.
    match state.store.list_tool_calls(&run.id).await {
        Ok(calls) if calls.is_empty() => {}
        Ok(_) => return UNKNOWN,
        Err(err) => {
            tracing::warn!(
                run_id = %run.id,
                error = %err,
                "run tool calls unreadable; withholding the run from peers"
            );
            return UNKNOWN;
        }
    }
    // A self-wake's prompt was written from its creating run's context. A
    // schedule the user created has no recorded origin.
    if let Some(schedule_id) = run.schedule_id.as_deref() {
        match state
            .store
            .schedule_origin_classification(schedule_id)
            .await
        {
            Ok(None) => {}
            Ok(Some(_)) => return UNKNOWN,
            Err(err) => {
                tracing::warn!(
                    run_id = %run.id,
                    error = %err,
                    "schedule source origin unreadable; withholding the run from peers"
                );
                return UNKNOWN;
            }
        }
    }
    let tags = match state.store.list_run_source_tags(&run.id).await {
        Ok(tags) => tags,
        Err(err) => {
            tracing::warn!(
                run_id = %run.id,
                error = %err,
                "run source tags unreadable; withholding the run from peers"
            );
            return UNKNOWN;
        }
    };
    // A recalled fact's stored classification cannot account for a tool
    // result it was remembered from; a session-floor tag means the run ran
    // with session memory, so earlier turns were in its context.
    if tags.iter().any(|tag| {
        matches!(
            tag.source.kind,
            SourceKind::MemoryRecall | SourceKind::SessionContext
        )
    }) {
        return UNKNOWN;
    }
    strictest_classification(tags.into_iter().map(|tag| tag.source.classification))
}

fn peer_view(run: Run, classification: SourceClassification) -> PeerRun {
    if classification != SourceClassification::Ordinary {
        return PeerRun {
            id: run.id,
            status: run.status,
            withheld: true,
            prompt: None,
            output: None,
            error: None,
            notice: Some(WITHHELD_NOTICE.to_owned()),
        };
    }
    // Tags are written one by one before the first model call, so a run
    // that has not ended may still gain a stricter one: no content yet.
    let ended = matches!(
        run.status,
        RunStatus::Completed | RunStatus::Failed | RunStatus::Cancelled
    );
    PeerRun {
        id: run.id,
        status: run.status,
        withheld: false,
        prompt: ended.then_some(run.input.prompt),
        output: if ended { run.output } else { None },
        error: if ended { run.error } else { None },
        notice: None,
    }
}

fn storage_error(err: agent24_store::StoreError) -> Response {
    tracing::error!("store error: {err}");
    error_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        "internal",
        "storage error",
    )
}

pub async fn list_peer_runs(State(state): State<AppState>) -> Response {
    let runs = match state.store.list_runs(None).await {
        Ok(runs) => runs,
        Err(err) => return storage_error(err),
    };
    let mut views = Vec::with_capacity(runs.len());
    for run in runs {
        let classification = peer_classification(&state, &run).await;
        views.push(peer_view(run, classification));
    }
    Json(serde_json::json!({ "runs": views })).into_response()
}

pub async fn get_peer_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_run(&id).await {
        Ok(Some(run)) => {
            let classification = peer_classification(&state, &run).await;
            Json(peer_view(run, classification)).into_response()
        }
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not_found", &format!("run {id}")),
        Err(err) => storage_error(err),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_core::util::now_iso8601;
    use agent24_protocol::{ErrorBody, RunInput, RunMode, RunOutput, Usage};
    use agent24_store::SourceRef;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    /// Every piece of run content a peer must not see when the run is
    /// withheld carries this marker.
    const SECRET: &str = "SECRET-7f3a";

    fn run(id: &str, status: RunStatus) -> Run {
        let now = now_iso8601();
        Run {
            id: id.to_owned(),
            session_id: None,
            workspace_id: None,
            status,
            input: RunInput {
                prompt: format!("prompt {SECRET}"),
                workspace_id: None,
                model_override: None,
                mode: RunMode::Normal,
            },
            output: (status == RunStatus::Completed).then(|| RunOutput {
                text: format!("output {SECRET}"),
            }),
            error: (status == RunStatus::Failed).then(|| ErrorBody {
                code: "internal".to_owned(),
                message: format!("error {SECRET}"),
                details: None,
                hint: Some(format!("hint {SECRET}")),
            }),
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
            },
            schedule_id: None,
            created_at: now.clone(),
            started_at: Some(now.clone()),
            ended_at: matches!(status, RunStatus::Completed | RunStatus::Failed).then_some(now),
        }
    }

    async fn insert(state: &AppState, id: &str, status: RunStatus, tags: &[SourceClassification]) {
        state.store.insert_run(&run(id, status)).await.unwrap();
        for (seq, classification) in (0_i64..).zip(tags) {
            let tag = SourceRef {
                source_id: format!("src_{seq}"),
                classification: *classification,
                ..SourceRef::user_input(id, now_iso8601())
            };
            state
                .store
                .tag_run_source(id, seq, &tag, &now_iso8601())
                .await
                .unwrap();
        }
    }

    async fn get(state: &AppState, path: &str) -> (StatusCode, String) {
        let res = crate::server::build_router_with_modules(state.clone(), axum::Router::new())
            .oneshot(
                Request::builder()
                    .uri(path)
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1024 * 1024)
            .await
            .unwrap();
        (status, String::from_utf8(bytes.to_vec()).unwrap())
    }

    async fn peer_run(state: &AppState, id: &str) -> (PeerRun, String) {
        let (status, body) = get(state, &format!("/api/v1/peer/runs/{id}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        (serde_json::from_str(&body).unwrap(), body)
    }

    fn assert_withheld(view: &PeerRun, body: &str) {
        assert!(view.withheld, "{body}");
        assert_eq!(view.notice.as_deref(), Some(WITHHELD_NOTICE));
        assert_eq!(
            (&view.prompt, &view.output, &view.error),
            (&None, &None, &None)
        );
        assert!(
            !body.contains(SECRET),
            "withheld content reached a peer: {body}"
        );
    }

    #[tokio::test]
    async fn an_ordinary_run_that_has_ended_is_relayed_in_full() {
        let state = crate::server::tests::state().await;
        let ordinary = [
            SourceClassification::Ordinary,
            SourceClassification::Ordinary,
        ];
        insert(&state, "run_done", RunStatus::Completed, &ordinary).await;
        insert(&state, "run_failed", RunStatus::Failed, &ordinary).await;

        let (done, body) = peer_run(&state, "run_done").await;
        assert!(!done.withheld, "{body}");
        assert_eq!(done.notice, None);
        assert_eq!(
            done.prompt.as_deref(),
            Some(format!("prompt {SECRET}").as_str())
        );
        assert_eq!(done.output.unwrap().text, format!("output {SECRET}"));

        let (failed, body) = peer_run(&state, "run_failed").await;
        assert!(!failed.withheld, "{body}");
        assert_eq!(failed.error.unwrap().message, format!("error {SECRET}"));
    }

    #[tokio::test]
    async fn a_run_with_any_source_that_is_not_ordinary_is_withheld_from_peers() {
        use SourceClassification::{Ordinary, Restricted, Unknown};
        let cases: [(&str, &[SourceClassification]); 4] = [
            ("restricted", &[Restricted]),
            ("unknown", &[Unknown]),
            (
                "one_restricted_among_ordinary",
                &[Ordinary, Restricted, Ordinary],
            ),
            ("untagged", &[]),
        ];
        let state = crate::server::tests::state().await;
        for (name, tags) in cases {
            for status in [RunStatus::Completed, RunStatus::Failed] {
                let id = format!("run_{name}_{status:?}");
                insert(&state, &id, status, tags).await;
                let (view, body) = peer_run(&state, &id).await;
                assert_eq!(view.status, status, "{name}");
                assert_withheld(&view, &body);
            }
        }
        let (status, list) = get(&state, "/api/v1/peer/runs").await;
        assert_eq!(status, StatusCode::OK);
        let runs: Vec<PeerRun> = serde_json::from_value(
            serde_json::from_str::<serde_json::Value>(&list).unwrap()["runs"].clone(),
        )
        .unwrap();
        assert_eq!(runs.len(), 8, "{list}");
        assert!(runs.iter().all(|run| run.withheld), "{list}");
        assert!(
            !list.contains(SECRET),
            "withheld content reached a peer: {list}"
        );
    }

    #[tokio::test]
    async fn a_run_whose_sources_cannot_be_read_is_withheld_from_peers() {
        let state = crate::server::tests::state().await;
        insert(
            &state,
            "run_x",
            RunStatus::Completed,
            &[SourceClassification::Ordinary],
        )
        .await;
        // Control: readable, the same run is relayed.
        assert!(!peer_run(&state, "run_x").await.0.withheld);

        agent24_store::test_hooks::fail_source_tag_reads(&state.store);
        let (view, body) = peer_run(&state, "run_x").await;
        assert_withheld(&view, &body);
        let (status, list) = get(&state, "/api/v1/peer/runs").await;
        assert_eq!(status, StatusCode::OK);
        assert!(list.contains("\"withheld\":true"), "{list}");
        assert!(
            !list.contains(SECRET),
            "withheld content reached a peer: {list}"
        );
    }

    #[tokio::test]
    async fn a_run_that_has_not_ended_carries_no_content_and_tightens_when_a_source_lands() {
        let state = crate::server::tests::state().await;
        insert(
            &state,
            "run_live",
            RunStatus::Running,
            &[SourceClassification::Ordinary],
        )
        .await;
        let (view, body) = peer_run(&state, "run_live").await;
        assert!(!view.withheld, "{body}");
        assert_eq!(
            (&view.prompt, &view.output, &view.error),
            (&None, &None, &None)
        );
        assert!(!body.contains(SECRET), "{body}");

        // A stricter source tagged later in the run (a session floor, a
        // recalled restricted fact) withholds it from then on.
        let floor =
            SourceRef::session_floor("sess_1", SourceClassification::Restricted, now_iso8601());
        state
            .store
            .tag_run_source("run_live", 1, &floor, &now_iso8601())
            .await
            .unwrap();
        state
            .store
            .transition_run(
                "run_live",
                RunStatus::Completed,
                agent24_store::RunPatch {
                    output: Some(RunOutput {
                        text: format!("output {SECRET}"),
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let (view, body) = peer_run(&state, "run_live").await;
        assert_eq!(view.status, RunStatus::Completed);
        assert_withheld(&view, &body);
    }

    /// A completed run carrying one Ordinary user-input tag.
    async fn ordinary(state: &AppState, mut run: Run) {
        let id = run.id.clone();
        run.status = RunStatus::Completed;
        state.store.insert_run(&run).await.unwrap();
        let tag = SourceRef::user_input(&id, now_iso8601());
        state
            .store
            .tag_run_source(&id, 0, &tag, &now_iso8601())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn a_run_started_by_a_kernel_created_schedule_is_withheld() {
        let state = crate::server::tests::state().await;
        // Even an Ordinary origin: the wake prompt was written from a context
        // whose tool results carry no tags. The run carries only its
        // user-input tag, as when it failed before its origin tag landed.
        for (schedule, origin) in [
            ("wake_restricted", SourceClassification::Restricted),
            ("wake_ordinary", SourceClassification::Ordinary),
        ] {
            let mut wake = run(&format!("run_{schedule}"), RunStatus::Completed);
            wake.schedule_id = Some(schedule.to_owned());
            ordinary(&state, wake).await;
            state
                .store
                .record_schedule_origin(schedule, "run_origin", origin, &now_iso8601())
                .await
                .unwrap();
            let (view, body) = peer_run(&state, &format!("run_{schedule}")).await;
            assert_withheld(&view, &body);
        }

        // Control: a schedule the user created has no recorded origin.
        let mut user = run("run_user_schedule", RunStatus::Completed);
        user.schedule_id = Some("user_1".to_owned());
        ordinary(&state, user).await;
        assert!(!peer_run(&state, "run_user_schedule").await.0.withheld);
    }

    #[tokio::test]
    async fn a_run_that_called_a_tool_is_withheld_until_tool_results_are_tagged() {
        let state = crate::server::tests::state().await;
        ordinary(&state, run("run_tool", RunStatus::Completed)).await;
        // Control: the same Ordinary run without a tool call is relayed.
        assert!(!peer_run(&state, "run_tool").await.0.withheld);

        state
            .store
            .insert_tool_call(&agent24_protocol::ToolCall {
                id: "tc_1".to_owned(),
                run_id: "run_tool".to_owned(),
                tool: "fs_read".to_owned(),
                input: serde_json::Map::new(),
                status: agent24_protocol::ToolCallStatus::Completed,
                output_summary: None,
                started_at: now_iso8601(),
                ended_at: Some(now_iso8601()),
            })
            .await
            .unwrap();
        let (view, body) = peer_run(&state, "run_tool").await;
        assert_withheld(&view, &body);
        let (_, list) = get(&state, "/api/v1/peer/runs").await;
        assert!(
            !list.contains(SECRET),
            "withheld content reached a peer: {list}"
        );
    }

    #[tokio::test]
    async fn a_run_that_recalled_a_memory_is_withheld() {
        let state = crate::server::tests::state().await;
        ordinary(&state, run("run_recall", RunStatus::Completed)).await;
        assert!(!peer_run(&state, "run_recall").await.0.withheld);

        // Stored as Ordinary: that cannot account for a tool result the fact
        // was remembered from.
        let recall =
            SourceRef::recalled_memory("a_1", SourceClassification::Ordinary, now_iso8601());
        state
            .store
            .tag_run_source("run_recall", 1, &recall, &now_iso8601())
            .await
            .unwrap();
        let (view, body) = peer_run(&state, "run_recall").await;
        assert_withheld(&view, &body);
    }

    #[tokio::test]
    async fn a_session_run_is_withheld_while_the_daemon_keeps_session_memory() {
        let mut state = crate::server::tests::state().await;
        state
            .store
            .insert_session(&agent24_protocol::Session {
                id: "sess_1".to_owned(),
                title: "t".to_owned(),
                channel: "cli".to_owned(),
                workspace_id: None,
                created_at: now_iso8601(),
                updated_at: now_iso8601(),
            })
            .await
            .unwrap();
        let mut turn = run("run_turn", RunStatus::Completed);
        turn.session_id = Some("sess_1".to_owned());
        ordinary(&state, turn).await;
        ordinary(&state, run("run_alone", RunStatus::Completed)).await;
        // Control: without session memory a session carries no earlier turns.
        assert!(!peer_run(&state, "run_turn").await.0.withheld);

        // The daemon kept session memory when this turn ran (its floor tag
        // says so) but not now: still withheld.
        let floor =
            SourceRef::session_floor("sess_1", SourceClassification::Ordinary, now_iso8601());
        let mut earlier = run("run_earlier", RunStatus::Completed);
        earlier.session_id = Some("sess_1".to_owned());
        ordinary(&state, earlier).await;
        state
            .store
            .tag_run_source("run_earlier", 1, &floor, &now_iso8601())
            .await
            .unwrap();
        let (view, body) = peer_run(&state, "run_earlier").await;
        assert_withheld(&view, &body);

        state.memory_kv = Some(agent24_memory::KvStore::open_memory().await.unwrap());
        let (view, body) = peer_run(&state, "run_turn").await;
        assert_withheld(&view, &body);
        // A run without a session has no earlier turns either way.
        assert!(!peer_run(&state, "run_alone").await.0.withheld);
        let (_, list) = get(&state, "/api/v1/peer/runs").await;
        let runs: Vec<PeerRun> = serde_json::from_value(
            serde_json::from_str::<serde_json::Value>(&list).unwrap()["runs"].clone(),
        )
        .unwrap();
        let withheld: Vec<(&str, bool)> = runs
            .iter()
            .map(|run| (run.id.as_str(), run.withheld))
            .collect();
        assert!(withheld.contains(&("run_turn", true)), "{withheld:?}");
        assert!(withheld.contains(&("run_alone", false)), "{withheld:?}");
    }

    #[tokio::test]
    async fn an_unknown_run_is_not_found() {
        let state = crate::server::tests::state().await;
        let (status, _) = get(&state, "/api/v1/peer/runs/run_missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
