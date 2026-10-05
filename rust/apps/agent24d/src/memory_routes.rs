//! `/api/v1/memory/*` (M1-T10) — the personal-memory REST surface: list /
//! search qualified assertions, retract one (`forget`), and a persistent
//! pause switch.
//!
//! **Personal space only.** Every handler reads `owner` from
//! [`crate::server::AppState::memory_owner`] — the SAME partition key the
//! agent run loop's [`agent24_agent::SessionMemory`] was built with (see
//! `crate::server::session_memory` and `AppState::new`'s derivation of
//! `memory_owner`/`memory_kv`). **`owner` is never accepted as a request
//! parameter** (`docs/research/MEMORY-STRATEGY.md` §4.1's T10 row): a caller
//! cannot name a different owner, and a module partition's id is simply not
//! found under this owner — `kv.forget`/`search_any` are owner-scoped all the
//! way down, so a module-partition id 404s exactly like a nonexistent one,
//! never leaking which case it was.

use agent24_core::util::now_iso8601;
use agent24_memory::KvStore;
use agent24_memory::assertion::{Assertion, AssertionStore, BeliefQuery, Forget};
use axum::extract::{Path, RawQuery, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::server::{AppState, error_response};

/// `GET /api/v1/memory/assertions` without `limit=`.
const DEFAULT_LIST_LIMIT: usize = 20;
/// The most rows any one call returns, `limit=` or not — same spirit as
/// `os_memory.rs`'s `MAX_RESULTS`: a cap the CONTRACT states, not a surprise a
/// caller discovers.
const MAX_LIST_LIMIT: usize = 200;

/// Shared guard for every handler below: resolve the daemon's own memory base
/// and the caller's personal owner, or answer 503 — never 404/500 — when this
/// daemon has none (e.g. `HOME` unset, or the memory file failed to open at
/// startup; see `open_memory_base`). A missing memory base is a daemon
/// configuration fact, not something about the specific assertion or setting
/// being asked for.
fn memory_ctx(state: &AppState) -> Option<(KvStore, String)> {
    match (state.memory_kv.clone(), state.memory_owner.clone()) {
        (Some(kv), Some(owner)) => Some((kv, owner)),
        _ => None,
    }
}

/// The 503 [`memory_ctx`] answers with when it is `None` — its own function
/// (not folded into a `Result<_, Response>`) so `memory_ctx` stays a small
/// `Option`: clippy's `result_large_err` flags a `Response`-sized `Err`
/// variant, and every caller already matches on `Option` just as easily.
fn memory_unavailable() -> Response {
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "memory_unavailable",
        "personal memory is not available on this daemon",
    )
}

/// At most one `q`, at most one `limit` (a positive integer) — anything else
/// is a 400, not a best-effort guess.
fn parse_list_query(raw: Option<&str>) -> Result<(Option<String>, Option<usize>), &'static str> {
    let mut q: Option<String> = None;
    let mut limit: Option<usize> = None;
    for (k, v) in form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        if k == "q" {
            if q.replace(v.into_owned()).is_some() {
                return Err("at most one q query parameter is allowed");
            }
        } else if k == "limit" {
            if limit.is_some() {
                return Err("at most one limit query parameter is allowed");
            }
            limit = Some(
                v.parse::<usize>()
                    .map_err(|_| "limit must be a positive integer")?,
            );
        }
    }
    Ok((q, limit))
}

/// The remembered text: the stored `object` as a plain string when it is one
/// (every M1 write is — `agent24_agent::retain::persist`'s `json!(object)`),
/// falling back to its JSON form so a future non-string object never panics
/// or silently vanishes here.
fn assertion_text(object: &Value) -> String {
    object
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| object.to_string())
}

/// `active` / `retracted` / `held`, per `docs/research/MEMORY-STRATEGY.md`
/// §4.1's T10 row. In practice every assertion [`list_assertions`] and
/// [`delete_assertion`] can reach is currently `active` — both
/// `AssertionStore::beliefs_as_of` and `Retriever::search_any` already filter
/// to current (`recorded_to IS NULL`) and qualified beliefs — but the field is
/// computed from the row's real state rather than hardcoded, so it stays
/// correct if a future caller passes `include_unqualified` or a historical
/// query through this same path.
fn assertion_status(a: &Assertion) -> &'static str {
    if !a.qualified {
        "held"
    } else if a.recorded_to.is_some() {
        "retracted"
    } else {
        "active"
    }
}

/// Provenance for one assertion: its evidence event ids, and — looked up from
/// the FIRST evidence event, owner-scoped — which session recorded it. `None`
/// when there is no evidence, or the evidence event cannot be found under
/// this owner (never a different owner's event: [`agent24_memory::event::EventLog::get`]
/// is itself owner-scoped).
async fn assertion_json(kv: &KvStore, owner: &str, a: Assertion) -> Value {
    let session = match a.evidence.first() {
        Some(id) => kv
            .events()
            .get(owner, id)
            .await
            .ok()
            .flatten()
            .and_then(|stored| stored.event.scope.session),
        None => None,
    };
    json!({
        "id": a.id,
        "text": assertion_text(&a.object),
        "recorded_at": a.recorded_from,
        "status": assertion_status(&a),
        "evidence": a.evidence,
        "session": session,
    })
}

/// `GET /api/v1/memory/assertions?q=&limit=` — no `q`: the latest qualified
/// assertions (newest `recorded_from` first); with `q`: `search_any`, same
/// scope + qualified + current filters `agent24_memory::retriever` already
/// enforces.
pub async fn list_assertions(State(state): State<AppState>, RawQuery(raw): RawQuery) -> Response {
    let Some((kv, owner)) = memory_ctx(&state) else {
        return memory_unavailable();
    };
    let (q, limit) = match parse_list_query(raw.as_deref()) {
        Ok(v) => v,
        Err(msg) => return error_response(StatusCode::BAD_REQUEST, "invalid_request", msg),
    };
    let limit = limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, MAX_LIST_LIMIT);

    let assertions: Vec<Assertion> = match q.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(q) => match kv.retriever().search_any(q, &owner, limit).await {
            Ok(hits) => hits.into_iter().map(|h| h.assertion).collect(),
            Err(err) => {
                tracing::error!("memory search_any for an owner: {err}");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "could not search memory",
                );
            }
        },
        None => match kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner(&owner))
            .await
        {
            Ok(mut rows) => {
                // `beliefs_as_of` orders by subject for its own bi-temporal
                // reasons, not recency — re-sort newest-first ("the latest
                // qualified assertions", MEMORY-STRATEGY §4.1) before capping.
                rows.sort_by(|a, b| b.recorded_from.cmp(&a.recorded_from));
                rows.truncate(limit);
                rows
            }
            Err(err) => {
                tracing::error!("listing memory assertions for an owner: {err}");
                return error_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal",
                    "could not list memory",
                );
            }
        },
    };

    let mut items = Vec::with_capacity(assertions.len());
    for a in assertions {
        items.push(assertion_json(&kv, &owner, a).await);
    }
    Json(json!({ "assertions": items })).into_response()
}

/// `DELETE /api/v1/memory/assertions/{id}` — retract (`forget`).
/// `Forgotten`/`AlreadyForgotten` → 204 (repeatable); `NotFound` → 404,
/// identically whether `id` never existed or belongs to a module partition —
/// [`agent24_memory::KvStore::forget`] is owner-scoped, so this handler never
/// has to decide which case it is.
pub async fn delete_assertion(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some((kv, owner)) = memory_ctx(&state) else {
        return memory_unavailable();
    };
    match kv.forget(&owner, &id, &now_iso8601()).await {
        Ok(Forget::Forgotten | Forget::AlreadyForgotten) => StatusCode::NO_CONTENT.into_response(),
        Ok(Forget::NotFound) => {
            error_response(StatusCode::NOT_FOUND, "not_found", "no such assertion")
        }
        Err(err) => {
            tracing::error!("retracting assertion {id}: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not retract the assertion",
            )
        }
    }
}

/// `GET /api/v1/memory/settings`.
pub async fn get_memory_settings(State(state): State<AppState>) -> Response {
    let Some((kv, owner)) = memory_ctx(&state) else {
        return memory_unavailable();
    };
    match kv.memory_enabled(&owner).await {
        Ok(enabled) => Json(json!({ "enabled": enabled })).into_response(),
        Err(err) => {
            tracing::error!("reading memory settings: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not read memory settings",
            )
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PutMemorySettingsBody {
    pub enabled: bool,
}

/// `PUT /api/v1/memory/settings` — the persistent personal-memory switch.
/// Turning it off stops new writes
/// ([`agent24_agent::retain`]'s pause gate) and cross-session recall
/// ([`agent24_agent::SessionMemory::recall`]'s pause gate) from the moment
/// this returns; it never touches existing assertions, and the setting
/// survives a daemon restart (it is a row in the same `memory.db`).
pub async fn put_memory_settings(
    State(state): State<AppState>,
    Json(body): Json<PutMemorySettingsBody>,
) -> Response {
    let Some((kv, owner)) = memory_ctx(&state) else {
        return memory_unavailable();
    };
    match kv.set_memory_enabled(&owner, body.enabled).await {
        Ok(()) => Json(json!({ "enabled": body.enabled })).into_response(),
        Err(err) => {
            tracing::error!("writing memory settings: {err}");
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not store memory settings",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use agent24_memory::event::{EventStore, Origin, Scope, Trust};
    use agent24_memory::writer::{Candidate, MemoryWriter};
    use agent24_models::router::ModelRouter;
    use agent24_models::router::Tier;
    use agent24_models::{CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg};
    use agent24_protocol::{Model, RunStatus, Session, Usage};
    use agent24_store::Store;
    use axum::body::Body;
    use axum::http::Request;
    use std::sync::Arc;
    use std::sync::{Arc as StdArc, Mutex as StdMutex};
    use std::time::Duration;
    use tokio_util::sync::CancellationToken;

    async fn body_json(r: Response) -> (StatusCode, Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap();
        let v: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap()
        };
        (status, v)
    }

    async fn add_assertion(
        kv: &KvStore,
        owner: &str,
        id: &str,
        subject: &str,
        text: &str,
        evidence: Vec<String>,
    ) {
        kv.write_gate()
            .propose(vec![
                Candidate::new(
                    id,
                    Scope::owner(owner),
                    subject,
                    "said_to_remember",
                    json!(text),
                    Origin {
                        source: "test".into(),
                        trust: Trust::UserSaid,
                    },
                )
                .with_evidence(evidence)
                .remember(),
            ])
            .await
            .unwrap();
    }

    // ── memory unavailable (no memory base on this daemon) ─────────────────

    #[tokio::test]
    async fn every_handler_503s_when_memory_is_unavailable() {
        let state = crate::server::tests::state().await;
        assert!(state.memory_kv.is_none());

        assert_eq!(
            list_assertions(State(state.clone()), RawQuery(None))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            delete_assertion(State(state.clone()), Path("x".to_owned()))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            get_memory_settings(State(state.clone())).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            put_memory_settings(
                State(state.clone()),
                Json(PutMemorySettingsBody { enabled: false })
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    /// A daemon state with memory wired to a fresh in-memory base, for tests
    /// that only exercise the REST handlers directly (no agent run loop).
    async fn state_with_memory(owner: &str) -> (AppState, KvStore) {
        let mut state = crate::server::tests::state().await;
        let kv = KvStore::open_memory().await.unwrap();
        state.memory_kv = Some(kv.clone());
        state.memory_owner = Some(owner.to_owned());
        (state, kv)
    }

    // ── settings ─────────────────────────────────────────────────────────

    #[tokio::test]
    async fn settings_default_enabled_and_round_trips() {
        let (state, _kv) = state_with_memory("owner-1").await;

        let (status, body) = body_json(get_memory_settings(State(state.clone())).await).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            body["enabled"], true,
            "a fresh owner's memory is ON by default"
        );

        let (status, body) = body_json(
            put_memory_settings(
                State(state.clone()),
                Json(PutMemorySettingsBody { enabled: false }),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["enabled"], false);

        let (_, body) = body_json(get_memory_settings(State(state.clone())).await).await;
        assert_eq!(body["enabled"], false, "the PUT must have persisted");
    }

    // ── list / search ────────────────────────────────────────────────────

    #[tokio::test]
    async fn list_without_q_shows_the_latest_qualified_assertions_with_provenance() {
        let (state, kv) = state_with_memory("owner-list").await;

        // An event to carry as evidence, in a known session — list_assertions
        // must surface its session via this evidence id.
        let event_id = "ev-1".to_owned();
        kv.events()
            .append(&agent24_memory::event::MemEvent::new(
                event_id.clone(),
                Scope::owner("owner-list").with_session("sess-1"),
                "message",
                json!({"role": "user", "content": "记住我对花生过敏"}),
                Origin {
                    source: "test".into(),
                    trust: Trust::UserSaid,
                },
            ))
            .await
            .unwrap();
        add_assertion(
            &kv,
            "owner-list",
            "a1",
            "user",
            "我对花生过敏",
            vec![event_id],
        )
        .await;

        let (status, body) =
            body_json(list_assertions(State(state.clone()), RawQuery(None)).await).await;
        assert_eq!(status, StatusCode::OK);
        let items = body["assertions"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "a1");
        assert_eq!(items[0]["text"], "我对花生过敏");
        assert_eq!(items[0]["status"], "active");
        assert_eq!(items[0]["session"], "sess-1");
        assert_eq!(items[0]["evidence"], json!(["ev-1"]));

        // Negative control: a DIFFERENT owner's assertion never appears.
        add_assertion(&kv, "someone-else", "a2", "user", "不相关", vec![]).await;
        let (_, body) =
            body_json(list_assertions(State(state.clone()), RawQuery(None)).await).await;
        assert_eq!(body["assertions"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn list_with_q_filters_via_search_any() {
        let (state, kv) = state_with_memory("owner-search").await;
        // Non-empty evidence: `write_gate` downgrades an evidence-less
        // `UserSaid`+`remember` candidate to an unqualified "hold" (per
        // `agent24-memory::writer`'s own policy), and both `search_any` and
        // the default list exclude unqualified rows.
        add_assertion(
            &kv,
            "owner-search",
            "sky",
            "sky",
            "blue",
            vec!["ev-sky".into()],
        )
        .await;
        add_assertion(
            &kv,
            "owner-search",
            "grass",
            "grass",
            "green",
            vec!["ev-grass".into()],
        )
        .await;

        let (_, body) = body_json(
            list_assertions(State(state.clone()), RawQuery(Some("q=sky".to_owned()))).await,
        )
        .await;
        let items = body["assertions"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["id"], "sky");
    }

    #[tokio::test]
    async fn limit_is_honored_and_bad_query_params_are_400() {
        let (state, kv) = state_with_memory("owner-limit").await;
        for i in 0..5 {
            add_assertion(
                &kv,
                "owner-limit",
                &format!("a{i}"),
                "s",
                "v",
                vec![format!("ev-{i}")],
            )
            .await;
        }
        let (_, body) = body_json(
            list_assertions(State(state.clone()), RawQuery(Some("limit=2".to_owned()))).await,
        )
        .await;
        assert_eq!(body["assertions"].as_array().unwrap().len(), 2);

        assert_eq!(
            list_assertions(
                State(state.clone()),
                RawQuery(Some("limit=nope".to_owned()))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            list_assertions(State(state.clone()), RawQuery(Some("q=a&q=b".to_owned())))
                .await
                .status(),
            StatusCode::BAD_REQUEST
        );
        // Negative control for the OTHER guard in `parse_list_query`: a
        // duplicate `limit` is 400 too, not "last one wins".
        assert_eq!(
            list_assertions(
                State(state.clone()),
                RawQuery(Some("limit=1&limit=2".to_owned()))
            )
            .await
            .status(),
            StatusCode::BAD_REQUEST
        );
    }

    // ── delete / forget ──────────────────────────────────────────────────

    #[tokio::test]
    async fn delete_is_forgotten_then_already_forgotten_then_not_found() {
        let (state, kv) = state_with_memory("owner-del").await;
        add_assertion(&kv, "owner-del", "a1", "s", "v", vec![]).await;

        assert_eq!(
            delete_assertion(State(state.clone()), Path("a1".to_owned()))
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        // Repeated delete: AlreadyForgotten, still 204.
        assert_eq!(
            delete_assertion(State(state.clone()), Path("a1".to_owned()))
                .await
                .status(),
            StatusCode::NO_CONTENT
        );
        // Unknown id: 404.
        assert_eq!(
            delete_assertion(State(state.clone()), Path("nope".to_owned()))
                .await
                .status(),
            StatusCode::NOT_FOUND
        );
        // Retracted, so it drops out of the default list.
        let (_, body) =
            body_json(list_assertions(State(state.clone()), RawQuery(None)).await).await;
        assert!(body["assertions"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_module_partitions_id_404s_exactly_like_an_unknown_one() {
        // "Module partition" here is any owner key other than this request's
        // personal owner — `forget` is owner-scoped, so this handler has no
        // way to tell "exists under a different owner" from "never existed",
        // which is the point: no existence leak either way.
        let (state, kv) = state_with_memory("owner-personal").await;
        add_assertion(&kv, "v2\0module\0some-module", "shared", "s", "v", vec![]).await;

        let personal_404 = delete_assertion(State(state.clone()), Path("shared".to_owned())).await;
        let unknown_404 =
            delete_assertion(State(state.clone()), Path("never-existed".to_owned())).await;
        assert_eq!(personal_404.status(), StatusCode::NOT_FOUND);
        assert_eq!(unknown_404.status(), StatusCode::NOT_FOUND);
        let (_, a) = body_json(personal_404).await;
        let (_, b) = body_json(unknown_404).await;
        assert_eq!(a, b, "identical response either way — no existence leak");
    }

    // ── end-to-end: a real agent run loop, through create_run ──────────────
    //
    // TWO sessions, deliberately: the raw conversation history of the
    // session that said "记住…" legitimately keeps saying it on every later
    // turn in THAT session (M1's own contract: originals are never deleted),
    // which would make "does the last provider call mention the fact"
    // meaningless if asked in the SAME session. Recall is asked from a
    // SEPARATE, otherwise-unrelated session instead — exactly how
    // `apps/agent24d/tests/session_memory/recall.rs`'s own `RECALL_SESSION`
    // is kept apart from where the fact was written.

    const E2E_WRITE_SESSION: &str = "memrest-e2e-write-session";
    const E2E_ASK_SESSION: &str = "memrest-e2e-ask-session";

    #[derive(Default)]
    struct RecordingProvider {
        received: StdMutex<Vec<Vec<Msg>>>,
    }

    #[async_trait::async_trait]
    impl ModelProvider for RecordingProvider {
        fn name(&self) -> &str {
            "memrest-e2e-mock"
        }

        async fn complete(
            &self,
            req: &CompletionRequest,
            _: &CancellationToken,
        ) -> Result<CompletionResponse, ModelError> {
            let mut received = self.received.lock().unwrap();
            received.push(req.messages.clone());
            Ok(CompletionResponse {
                message: Msg::assistant(Some(format!("answer-{}", received.len())), vec![]),
                usage: Usage::default(),
                model_id: Some("mock".into()),
            })
        }

        async fn models(&self, _: &CancellationToken) -> Result<Vec<Model>, ModelError> {
            Ok(vec![])
        }
    }

    /// A daemon state with a REAL `SessionMemory` + agent run loop, backed by
    /// `kv`, with a mock provider recording every message it receives.
    /// Self-contained (does not reuse `apps/agent24d/tests/session_memory`'s
    /// harness, which M1-T07.1 is concurrently changing) — just enough to run
    /// `crate::runs::create_run` end to end.
    async fn e2e_app(
        kv: KvStore,
        dir: &std::path::Path,
        provider: StdArc<RecordingProvider>,
        owner: &str,
    ) -> AppState {
        let router = Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)]));
        let cancel = CancellationToken::new();
        let summarizer = StdArc::new(agent24_agent::RouterSummarizer::new(
            Arc::clone(&router),
            cancel.clone(),
        ));
        let memory = agent24_agent::SessionMemory::new(kv, summarizer).with_owner(owner.to_owned());
        let store = Store::open(&dir.join("agent24.db")).await.unwrap();
        for id in [E2E_WRITE_SESSION, E2E_ASK_SESSION] {
            store
                .insert_session(&Session {
                    workspace_id: None,
                    id: id.into(),
                    title: String::new(),
                    channel: "test".into(),
                    created_at: "2026-10-01T00:00:00Z".into(),
                    updated_at: "2026-10-01T00:00:00Z".into(),
                })
                .await
                .unwrap();
        }
        AppState::new(crate::server::AppDeps {
            token: "test".into(),
            router,
            tools: agent24_tools::ToolRegistry::new(),
            store,
            shutdown: crate::server::Shutdown::new(cancel),
            guardian: None,
            memory: Some(memory),
            workspace_service: None,
            mcp_servers: vec![],
            risk_overrides: StdArc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
                vec![],
            )),
            packages_root: Arc::new(dir.to_path_buf()),
        })
    }

    async fn run_in_session(state: &AppState, session_id: &str, prompt: &str) {
        let response = crate::runs::create_run(
            State(state.clone()),
            Request::builder()
                .method("POST")
                .uri("/api/v1/runs")
                .body(Body::from(
                    json!({"session_id": session_id, "prompt": prompt}).to_string(),
                ))
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let created: agent24_protocol::Run = serde_json::from_slice(
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
    }

    fn last_messages_mention(provider: &RecordingProvider, needle: &str) -> bool {
        let calls = provider.received.lock().unwrap();
        let last = calls.last().expect("provider must have been called");
        last.iter()
            .any(|m| m.content.as_deref().unwrap_or("").contains(needle))
    }

    #[tokio::test]
    async fn rest_delete_stops_future_recall() {
        let dir = tempfile::tempdir().unwrap();
        let kv = KvStore::open(&dir.path().join("memory.db")).await.unwrap();
        let owner = "memrest-e2e-delete-owner";
        let provider = StdArc::new(RecordingProvider::default());
        let state = e2e_app(kv.clone(), dir.path(), provider.clone(), owner).await;

        run_in_session(&state, E2E_WRITE_SESSION, "记住我对花生过敏").await;
        let rows = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner(owner))
            .await
            .unwrap();
        assert_eq!(rows.len(), 1, "the explicit remember must have persisted");
        let assertion_id = rows[0].id.clone();

        // Sanity (positive control): a full-question run, in a DIFFERENT
        // session, recalls it before retract.
        run_in_session(&state, E2E_ASK_SESSION, "你好").await;
        run_in_session(&state, E2E_ASK_SESSION, "我对什么过敏？").await;
        assert!(
            last_messages_mention(&provider, "花生过敏"),
            "must be recalled before retract"
        );

        // REST list shows it with provenance.
        let (_, listed) =
            body_json(list_assertions(State(state.clone()), RawQuery(None)).await).await;
        let items = listed["assertions"].as_array().unwrap();
        let item = items
            .iter()
            .find(|i| i["id"] == json!(assertion_id))
            .expect("listed");
        assert_eq!(item["text"], "我对花生过敏");
        assert_eq!(item["session"], E2E_WRITE_SESSION);

        // Retract via REST.
        let del = delete_assertion(State(state.clone()), Path(assertion_id)).await;
        assert_eq!(del.status(), StatusCode::NO_CONTENT);

        // End-to-end: a fresh question, same ask session, no longer recalls it.
        run_in_session(&state, E2E_ASK_SESSION, "你好").await;
        run_in_session(&state, E2E_ASK_SESSION, "我对什么过敏？").await;
        assert!(
            !last_messages_mention(&provider, "花生过敏"),
            "retracted fact must not be recalled"
        );
    }

    #[tokio::test]
    async fn rest_pause_blocks_write_and_recall_and_survives_restart() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("memory.db");
        let owner = "memrest-e2e-pause-owner";
        {
            let kv = KvStore::open(&db).await.unwrap();
            let provider = StdArc::new(RecordingProvider::default());
            let state = e2e_app(kv.clone(), dir.path(), provider.clone(), owner).await;

            let put = put_memory_settings(
                State(state.clone()),
                Json(PutMemorySettingsBody { enabled: false }),
            )
            .await;
            assert_eq!(put.status(), StatusCode::OK);

            run_in_session(&state, E2E_WRITE_SESSION, "记住我对花生过敏").await;
            let rows = kv
                .assertions()
                .beliefs_as_of(&BeliefQuery::owner(owner))
                .await
                .unwrap();
            assert!(rows.is_empty(), "paused memory must reject the write");

            run_in_session(&state, E2E_ASK_SESSION, "你好").await;
            run_in_session(&state, E2E_ASK_SESSION, "我对什么过敏？").await;
            assert!(
                !last_messages_mention(&provider, "花生过敏"),
                "paused memory must not recall anything (nothing was written to recall)"
            );
        }

        // Simulate a daemon restart: fresh KvStore over the same file.
        let reopened = KvStore::open(&db).await.unwrap();
        assert!(
            !reopened.memory_enabled(owner).await.unwrap(),
            "the pause must survive a daemon restart"
        );
    }
}
