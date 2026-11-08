//! KB-SOP-01a (#1669): how strict a desktop conversation's sources were,
//! read only, for the knowledge entry admission (CONTRACT-OPS-v2 W8.1).
//!
//! `GET /api/v1/sessions/{id}/source-floor?run_id=…` folds, strictest first:
//! the floor of session `{id}` (a desktop conversation's id is the
//! `/api/v1/chat` session id its replies merge into); each listed run's own
//! tags; and the current floor of each run's session (a Knowledge turn runs in
//! a session of its own). The answer is one word, nothing of the conversation.
//!
//! Fail closed (decision 2026-11-09-floor-read-api, option B): a run this
//! daemon has no record of, a read that fails, a run without tags, or more
//! than [`MAX_RUNS`] run ids is `unknown`, never cut short and never
//! `ordinary`. A missing floor of `{id}` itself is `missing`, kept apart from
//! `unknown` only so each can be tested; both refuse. A run's session without
//! a floor adds nothing: the run loop keeps one only with session memory, and
//! without it no earlier turn reached the run, whose own tags then say all.
//!
//! Nothing here writes: no `merge_session_floor`, only SELECTs.

use agent24_store::{SourceClassification, strictest_classification};
use axum::Json;
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::server::{AppState, error_response};

/// At most this many runs are folded; more answers `unknown`.
pub(crate) const MAX_RUNS: usize = 64;

/// The answer, strictest last: `restricted` over `unknown` over `missing`
/// over `ordinary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum Reading {
    Ordinary,
    Missing,
    Unknown,
    Restricted,
}

impl From<SourceClassification> for Reading {
    fn from(classification: SourceClassification) -> Self {
        match classification {
            SourceClassification::Ordinary => Reading::Ordinary,
            SourceClassification::Restricted => Reading::Restricted,
            SourceClassification::Unknown => Reading::Unknown,
        }
    }
}

#[derive(serde::Serialize)]
struct SourceFloor {
    classification: Reading,
}

/// The run ids of the query; `None` when it has any other parameter.
fn run_ids(raw: Option<&str>) -> Option<Vec<String>> {
    let mut ids = Vec::new();
    for (key, value) in form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        if key != "run_id" {
            return None;
        }
        ids.push(value.into_owned());
    }
    Some(ids)
}

/// One run: its own tags and its session's current floor.
async fn run_reading(state: &AppState, run_id: &str) -> Reading {
    let run = match state.store.get_run(run_id).await {
        Ok(Some(run)) => run,
        Ok(None) => return Reading::Unknown,
        Err(err) => {
            tracing::warn!(run_id, error = %err, "run unreadable for source floor");
            return Reading::Unknown;
        }
    };
    let own = match state.store.run_policy_snapshot(&run.id).await {
        // No tags at all folds to Unknown.
        Ok(snapshot) => strictest_classification(snapshot.sources.iter().map(|s| s.classification)),
        Err(err) => {
            tracing::warn!(run_id, error = %err, "run source tags unreadable for source floor");
            return Reading::Unknown;
        }
    };
    let session = match run.session_id.as_deref() {
        None => None,
        Some(sid) => match state.store.session_floor(sid).await {
            Ok(floor) => floor,
            Err(err) => {
                tracing::warn!(run_id, error = %err, "run session floor unreadable");
                return Reading::Unknown;
            }
        },
    };
    Reading::from(strictest_classification(
        std::iter::once(own).chain(session),
    ))
}

async fn fold(state: &AppState, session_id: &str, runs: &[String]) -> Reading {
    if runs.len() > MAX_RUNS {
        return Reading::Unknown;
    }
    let mut worst = match state.store.session_floor(session_id).await {
        Ok(Some(floor)) => Reading::from(floor),
        Ok(None) => Reading::Missing,
        Err(err) => {
            tracing::warn!(error = %err, "session floor unreadable");
            return Reading::Unknown;
        }
    };
    for run_id in runs {
        if worst == Reading::Restricted {
            break;
        }
        worst = worst.max(run_reading(state, run_id).await);
    }
    worst
}

pub async fn get_source_floor(
    State(state): State<AppState>,
    Path(id): Path<String>,
    RawQuery(raw): RawQuery,
) -> Response {
    let Some(runs) = run_ids(raw.as_deref()) else {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "the only query parameter is run_id",
        );
    };
    let classification = fold(&state, &id, &runs).await;
    Json(SourceFloor { classification }).into_response()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_core::util::now_iso8601;
    use agent24_protocol::{Run, RunInput, RunMode, RunStatus, Usage};
    use agent24_store::SourceRef;
    use axum::body::Body;
    use axum::http::{Method, Request};
    use tower::ServiceExt;

    use SourceClassification::{Ordinary, Restricted, Unknown};

    const SECRET: &str = "SECRET-1669";

    async fn floor(state: &AppState, session: &str, classification: SourceClassification) {
        state
            .store
            .merge_session_floor(session, classification, false, &now_iso8601())
            .await
            .unwrap();
    }

    /// A completed run in `session` whose prompt and output carry [`SECRET`],
    /// tagged with `tags`.
    async fn run(state: &AppState, id: &str, session: Option<&str>, tags: &[SourceClassification]) {
        if let Some(sid) = session {
            // Runs reference sessions; a missing one is created.
            if state.store.get_session(sid).await.unwrap().is_none() {
                let now = now_iso8601();
                let session = agent24_protocol::Session {
                    id: sid.to_owned(),
                    title: format!("title {SECRET}"),
                    channel: "desktop".to_owned(),
                    workspace_id: None,
                    created_at: now.clone(),
                    updated_at: now,
                };
                state.store.insert_session(&session).await.unwrap();
            }
        }
        let now = now_iso8601();
        let record = Run {
            id: id.to_owned(),
            session_id: session.map(str::to_owned),
            workspace_id: None,
            status: RunStatus::Completed,
            input: RunInput {
                prompt: format!("prompt {SECRET}"),
                workspace_id: None,
                model_override: None,
                mode: RunMode::Normal,
                knowledge_binding_id: None,
            },
            output: Some(agent24_protocol::RunOutput {
                text: format!("output {SECRET}"),
            }),
            error: None,
            usage: Usage {
                prompt_tokens: 0,
                completion_tokens: 0,
                total_tokens: 0,
                cost_usd: 0.0,
            },
            schedule_id: None,
            created_at: now.clone(),
            started_at: Some(now.clone()),
            ended_at: Some(now),
        };
        state.store.insert_run(&record).await.unwrap();
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

    async fn call(state: &AppState, method: Method, uri: &str) -> (StatusCode, String) {
        let res = crate::server::build_router_with_modules(state.clone(), axum::Router::new())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Authorization", "Bearer testtoken")
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
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

    /// The classification for `session` and `runs`; the body must be exactly
    /// `{"classification": …}`.
    async fn read(state: &AppState, session: &str, runs: &[&str]) -> String {
        let query: String = runs
            .iter()
            .map(|r| format!("run_id={r}"))
            .collect::<Vec<_>>()
            .join("&");
        let (status, body) = call(
            state,
            Method::GET,
            &format!("/api/v1/sessions/{session}/source-floor?{query}"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), 1, "only the classification: {body}");
        assert!(!body.contains(SECRET), "{body}");
        object["classification"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn a_session_floor_alone_is_read_as_it_is_stored() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat_ord", Ordinary).await;
        floor(&state, "chat_res", Restricted).await;
        floor(&state, "chat_unk", Unknown).await;
        assert_eq!(read(&state, "chat_ord", &[]).await, "ordinary");
        assert_eq!(read(&state, "chat_res", &[]).await, "restricted");
        // N10d and N10e: unknown and missing are told apart.
        assert_eq!(read(&state, "chat_unk", &[]).await, "unknown");
        assert_eq!(read(&state, "chat_none", &[]).await, "missing");
    }

    #[tokio::test]
    async fn every_listed_run_and_its_session_count_and_the_strictest_wins() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        // A run whose session's floor is not kept (no session memory).
        run(&state, "run_ok", Some("rs_nofloor"), &[Ordinary, Ordinary]).await;
        run(&state, "run_solo", None, &[Ordinary]).await;
        assert_eq!(
            read(&state, "chat", &["run_ok", "run_solo"]).await,
            "ordinary"
        );

        // N10f: one restricted run among ordinary ones.
        run(&state, "run_secret", None, &[Ordinary, Restricted]).await;
        assert_eq!(
            read(&state, "chat", &["run_ok", "run_secret"]).await,
            "restricted"
        );
        // A run with no tags says nothing about what it took in.
        run(&state, "run_untagged", None, &[]).await;
        assert_eq!(read(&state, "chat", &["run_untagged"]).await, "unknown");
        // A missing conversation floor still loses to a stricter run.
        assert_eq!(read(&state, "chat_none", &["run_ok"]).await, "missing");
        assert_eq!(
            read(&state, "chat_none", &["run_secret"]).await,
            "restricted"
        );
    }

    /// N16d: the conversation and the run are Ordinary, but the run's own
    /// session (a Knowledge run session) has since become Restricted.
    #[tokio::test]
    async fn a_run_whose_session_floor_is_restricted_is_restricted() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        run(&state, "run_k", Some("run_session"), &[Ordinary]).await;
        floor(&state, "run_session", Ordinary).await;
        assert_eq!(read(&state, "chat", &["run_k"]).await, "ordinary");
        floor(&state, "run_session", Restricted).await;
        assert_eq!(read(&state, "chat", &["run_k"]).await, "restricted");
    }

    /// N16c (N10g): a run this daemon has no record of.
    #[tokio::test]
    async fn a_run_the_daemon_does_not_know_is_unknown() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        run(&state, "run_ok", None, &[Ordinary]).await;
        assert_eq!(
            read(&state, "chat", &["run_ok", "run_gone"]).await,
            "unknown"
        );
    }

    /// N16b: 65 ordinary runs are not cut down to 64 and passed.
    #[tokio::test]
    async fn more_runs_than_the_limit_is_unknown_never_cut_short() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        let ids: Vec<String> = (0..=MAX_RUNS).map(|i| format!("run_{i}")).collect();
        for id in &ids {
            run(&state, id, None, &[Ordinary]).await;
        }
        let all: Vec<&str> = ids.iter().map(String::as_str).collect();
        assert_eq!(read(&state, "chat", &all[..MAX_RUNS]).await, "ordinary");
        assert_eq!(read(&state, "chat", &all).await, "unknown");
    }

    #[tokio::test]
    async fn a_read_that_fails_is_unknown() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        run(&state, "run_ok", None, &[Ordinary]).await;
        sqlx::raw_sql("DROP TABLE run_source_tags")
            .execute(agent24_store::test_hooks::pool(&state.store))
            .await
            .unwrap();
        assert_eq!(read(&state, "chat", &["run_ok"]).await, "unknown");
        sqlx::raw_sql("DROP TABLE session_source_floor")
            .execute(agent24_store::test_hooks::pool(&state.store))
            .await
            .unwrap();
        assert_eq!(read(&state, "chat", &[]).await, "unknown");
    }

    #[tokio::test]
    async fn any_other_query_parameter_is_refused() {
        let state = crate::server::tests::state().await;
        let (status, body) = call(
            &state,
            Method::GET,
            "/api/v1/sessions/chat/source-floor?run_id=r&limit=1",
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    }

    /// N16a: the path takes no other method.
    #[tokio::test]
    async fn the_route_is_read_only() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            let (status, body) =
                call(&state, method.clone(), "/api/v1/sessions/chat/source-floor").await;
            assert!(
                status == StatusCode::METHOD_NOT_ALLOWED || status == StatusCode::NOT_FOUND,
                "{method}: {status} {body}"
            );
        }
    }

    /// N16 (store unchanged): with every write to the floor and tag tables
    /// refused, reads still answer, and the floor rows stay as they were.
    #[tokio::test]
    async fn reading_writes_nothing() {
        let state = crate::server::tests::state().await;
        floor(&state, "chat", Ordinary).await;
        run(&state, "run_k", Some("run_session"), &[Ordinary]).await;
        floor(&state, "run_session", Ordinary).await;
        let pool = agent24_store::test_hooks::pool(&state.store);
        let rows = || async {
            sqlx::query_as::<_, (String, String, String)>(
                "SELECT session_id, classification, updated_at FROM session_source_floor ORDER BY session_id",
            )
            .fetch_all(pool)
            .await
            .unwrap()
        };
        let migrations = || async {
            sqlx::query_scalar::<_, i64>("SELECT MAX(version) FROM _sqlx_migrations")
                .fetch_one(pool)
                .await
                .unwrap()
        };
        let (before, version) = (rows().await, migrations().await);
        for table in [
            "session_source_floor",
            "run_source_tags",
            "runs",
            "sessions",
        ] {
            for op in ["INSERT", "UPDATE", "DELETE"] {
                sqlx::raw_sql(&format!(
                    "CREATE TRIGGER lock_{table}_{op} BEFORE {op} ON {table} \
                     BEGIN SELECT RAISE(ABORT, 'read only'); END;"
                ))
                .execute(pool)
                .await
                .unwrap();
            }
        }
        assert_eq!(read(&state, "chat", &["run_k"]).await, "ordinary");
        assert_eq!(read(&state, "chat_none", &["run_gone"]).await, "unknown");
        assert_eq!(read(&state, "chat_none", &[]).await, "missing");
        assert_eq!(rows().await, before);
        assert_eq!(migrations().await, version);
    }
}
