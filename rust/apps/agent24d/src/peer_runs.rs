//! K1-6b.5c-2 (ADR-K1-02 §2.4): the run views a peer bridge relays.
//!
//! The MCP server and the ACP bridge hand run content to a process outside
//! the kernel, so they read runs through these endpoints instead of
//! `/api/v1/runs`. A run's prompt, output and error leave only when every
//! source tagged on it is Ordinary; a Restricted or Unknown source, no tags at
//! all, or a failed tag read all withhold it (David 2026-10-25, option 丙).

use agent24_protocol::{PeerRun, Run, RunStatus};
use agent24_store::{SourceClassification, strictest_classification};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::server::{AppState, error_response};

/// The only text a withheld run carries. Fixed, so nothing about the run's
/// sources or content can reach a peer through it.
pub(crate) const WITHHELD_NOTICE: &str = "this run used sources that are not ordinary, so its \
     content is not shared with connected tools; open it in Agent24 to see it";

/// The strictest classification of a run's sources as a peer may rely on
/// them. A run that ended before all its tags were written (a failed tag
/// read or write stops it) has only some of them; the one source that can
/// shape such a run's own prompt is the self-wake schedule that wrote it, so
/// that schedule's recorded classification counts whether or not its tag
/// made it onto the run.
async fn peer_classification(state: &AppState, run: &Run) -> SourceClassification {
    let tags = match state.store.list_run_source_tags(&run.id).await {
        Ok(tags) => tags,
        Err(err) => {
            tracing::warn!(
                run_id = %run.id,
                error = %err,
                "run source tags unreadable; withholding the run from peers"
            );
            return SourceClassification::Unknown;
        }
    };
    let origin = match run.schedule_id.as_deref() {
        Some(schedule_id) => match state
            .store
            .schedule_origin_classification(schedule_id)
            .await
        {
            Ok(origin) => origin,
            Err(err) => {
                tracing::warn!(
                    run_id = %run.id,
                    error = %err,
                    "schedule source origin unreadable; withholding the run from peers"
                );
                return SourceClassification::Unknown;
            }
        },
        None => None,
    };
    strictest_classification(
        tags.into_iter()
            .map(|tag| tag.source.classification)
            .chain(origin),
    )
}

async fn peer_view(state: &AppState, run: Run) -> PeerRun {
    let classification = peer_classification(state, &run).await;
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

pub async fn list_peer_runs(State(state): State<AppState>) -> Response {
    let runs = match state.store.list_runs(None).await {
        Ok(runs) => runs,
        Err(err) => {
            tracing::error!("store error: {err}");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            );
        }
    };
    let mut views = Vec::with_capacity(runs.len());
    for run in runs {
        views.push(peer_view(&state, run).await);
    }
    Json(serde_json::json!({ "runs": views })).into_response()
}

pub async fn get_peer_run(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    match state.store.get_run(&id).await {
        Ok(Some(run)) => Json(peer_view(&state, run).await).into_response(),
        Ok(None) => error_response(StatusCode::NOT_FOUND, "not_found", &format!("run {id}")),
        Err(err) => {
            tracing::error!("store error: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "storage error",
            )
        }
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

    #[tokio::test]
    async fn a_self_wake_that_failed_before_its_origin_was_tagged_is_withheld() {
        let state = crate::server::tests::state().await;
        // The run failed after its user-input tag and before its schedule
        // origin tag: the only tag on it is Ordinary.
        let mut wake = run("run_wake", RunStatus::Failed);
        wake.schedule_id = Some("wake_1".to_owned());
        state.store.insert_run(&wake).await.unwrap();
        let tag = SourceRef::user_input("run_wake", now_iso8601());
        state
            .store
            .tag_run_source("run_wake", 0, &tag, &now_iso8601())
            .await
            .unwrap();
        state
            .store
            .record_schedule_origin(
                "wake_1",
                "run_origin",
                SourceClassification::Restricted,
                &now_iso8601(),
            )
            .await
            .unwrap();
        let (view, body) = peer_run(&state, "run_wake").await;
        assert_withheld(&view, &body);

        // Control: a schedule the user created has no recorded origin.
        let mut user = run("run_user_schedule", RunStatus::Failed);
        user.schedule_id = Some("user_1".to_owned());
        state.store.insert_run(&user).await.unwrap();
        let tag = SourceRef::user_input("run_user_schedule", now_iso8601());
        state
            .store
            .tag_run_source("run_user_schedule", 0, &tag, &now_iso8601())
            .await
            .unwrap();
        assert!(!peer_run(&state, "run_user_schedule").await.0.withheld);
    }

    #[tokio::test]
    async fn an_unknown_run_is_not_found() {
        let state = crate::server::tests::state().await;
        let (status, _) = get(&state, "/api/v1/peer/runs/run_missing").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
