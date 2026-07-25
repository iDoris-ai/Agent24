//! HTTP server: router, auth middleware, ready line, graceful shutdown.

use std::sync::Arc;
use std::time::Duration;

use agent24_models::router::ModelRouter;
use agent24_protocol::{ErrorBody, ErrorEnvelope, Health};
use agent24_store::Store;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use rand::RngCore;
use std::sync::Arc as StdArc;
use tokio_util::sync::CancellationToken;

/// Grace period for in-flight requests after a shutdown signal; the process
/// force-exits after this so `kill -TERM` always terminates within ~2s
/// (TASKS B2 acceptance).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct AppState {
    pub token: Arc<String>,
    /// D2 router: every model call goes through tier routing + health/cooldown,
    /// so a downed local provider backs off and a LocalOnly task never leaks.
    pub router: Arc<ModelRouter>,
    pub tools: Arc<agent24_tools::ToolRegistry>,
    /// H2: the user's risk overrides. Held here as well as inside the registry
    /// because the two need the SAME object — the registry resolves against it
    /// on every dispatch, and the CRUD handlers refresh it in place, so a rule
    /// the user adds governs the very next tool call without a restart.
    pub risk_overrides: StdArc<agent24_policy::overrides::RiskOverrideStore>,
    pub broker: Arc<agent24_policy::ApprovalBroker>,
    pub usage: Arc<crate::routes::UsageCounters>,
    pub events: crate::events::EventsHub,
    pub store: Store,
    pub runs: Arc<agent24_agent::RunManager>,
    pub scheduler: Arc<agent24_scheduler::Scheduler>,
    /// Live MCP server handles. This is an RAII guard, not data: dropping an
    /// McpServer kills its child process, which would silently break every tool
    /// it contributed. Never read on purpose — its job is to exist (M-E/E1b).
    #[allow(dead_code, reason = "RAII: keeps MCP child processes alive")]
    pub mcp_servers: Arc<Vec<Arc<agent24_mcp::McpServer>>>,
    /// Daemon-wide shutdown token; handlers derive request tokens from it so
    /// shutdown cancels in-flight provider calls (run-level cancel joins in C2)
    pub shutdown: CancellationToken,
}

/// Adapts the run manager to the scheduler's `RunTrigger` — a fired schedule
/// becomes a background run tagged with the schedule id.
struct RunManagerTrigger {
    runs: Arc<agent24_agent::RunManager>,
}

#[async_trait::async_trait]
impl agent24_scheduler::RunTrigger for RunManagerTrigger {
    async fn trigger(
        &self,
        action: &agent24_protocol::ScheduleAction,
        schedule_id: &str,
    ) -> Result<String, String> {
        let agent24_protocol::ScheduleAction::AgentRun {
            prompt,
            session_id,
            model_override,
        } = action;
        let create = agent24_protocol::RunCreate {
            session_id: session_id.clone(),
            prompt: prompt.clone(),
            model_override: model_override.clone(),
        };
        self.runs
            .start_run_with_schedule(create, Some(schedule_id.to_owned()))
            .await
            .map(|run| run.id)
            .map_err(|err| err.to_string())
    }
}

/// Build the D3 Guardian when the operator opts in with `A24_GUARDIAN=1`.
///
/// **Default OFF.** Letting a model auto-approve tool calls is a deliberate
/// operator choice, never a silent default — with no guardian every gated call
/// goes to a human exactly as before.
///
/// When on, risk is assessed by a LOCAL-ONLY model through the same [`ModelRouter`]
/// (the payload never leaves the device). `A24_GUARDIAN_ALWAYS_REVIEW` is a
/// comma-separated list of tool kinds that always require a human regardless of
/// the model's verdict; it defaults to `exec`, because `shell_exec` is arbitrary
/// code execution and deserves a human by default even with the guardian on.
fn build_guardian(router: &Arc<ModelRouter>) -> Option<StdArc<agent24_policy::guardian::Guardian>> {
    if !guardian_enabled(std::env::var("A24_GUARDIAN").ok().as_deref()) {
        return None;
    }
    let always_review =
        parse_always_review(std::env::var("A24_GUARDIAN_ALWAYS_REVIEW").ok().as_deref());
    let assessor = StdArc::new(agent24_policy::guardian::ModelRiskAssessor::new(
        Arc::clone(router),
    ));
    tracing::info!(
        "guardian enabled (always-review kinds: {})",
        always_review.join(",")
    );
    Some(StdArc::new(
        agent24_policy::guardian::Guardian::new(assessor).always_review(always_review),
    ))
}

/// Opt-in only: absent, empty, or anything other than `1`/`true` leaves the
/// guardian OFF (fail-safe — a typo must never silently enable auto-approval).
fn guardian_enabled(raw: Option<&str>) -> bool {
    raw.is_some_and(|v| {
        let v = v.trim();
        v == "1" || v.eq_ignore_ascii_case("true")
    })
}

/// Parse the always-review kind list, defaulting to `exec`. An explicitly empty
/// value yields an empty list (the operator deliberately allows every kind to be
/// considered for auto-approval).
fn parse_always_review(raw: Option<&str>) -> Vec<String> {
    raw.unwrap_or("exec")
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect()
}

/// Open the D1 session-memory KV store and pair it with a router-backed
/// summarizer. Returns `None` (memory off) if the store can't be opened — a
/// degraded daemon is better than one that won't start.
async fn open_session_memory(
    ephemeral: bool,
    router: &Arc<ModelRouter>,
    shutdown: &CancellationToken,
) -> Option<agent24_agent::SessionMemory> {
    let kv = if ephemeral {
        agent24_memory::KvStore::open_memory().await
    } else {
        let dir = agent24_protocol::state_file::state_dir()?;
        agent24_memory::KvStore::open(&dir.join("memory.db")).await
    };
    match kv {
        Ok(kv) => Some(agent24_agent::SessionMemory::new(
            kv,
            StdArc::new(agent24_agent::RouterSummarizer::new(
                Arc::clone(router),
                shutdown.clone(),
            )),
        )),
        Err(err) => {
            tracing::warn!("session memory unavailable ({err}); sessions will not remember");
            None
        }
    }
}

/// Everything [`AppState::new`] needs. The guardian and session memory are
/// INJECTED rather than read from env inside the constructor, so tests can wire
/// stubs; `serve` supplies the env-driven values.
pub struct AppDeps {
    pub token: String,
    pub router: Arc<ModelRouter>,
    pub tools: agent24_tools::ToolRegistry,
    pub store: Store,
    pub shutdown: CancellationToken,
    pub guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    pub memory: Option<agent24_agent::SessionMemory>,
    pub mcp_servers: Vec<Arc<agent24_mcp::McpServer>>,
    /// Pre-loaded user overrides (H2). Injected rather than loaded here so
    /// tests can wire an empty or hand-built set.
    pub risk_overrides: StdArc<agent24_policy::overrides::RiskOverrideStore>,
}

impl AppState {
    /// Build from [`AppDeps`]. Grouped into a struct rather than a long
    /// parameter list: the collaborators grew with each milestone (guardian,
    /// session memory, MCP servers) and positional args of the same shape are
    /// easy to transpose silently.
    pub fn new(deps: AppDeps) -> Self {
        let AppDeps {
            token,
            router,
            tools,
            store,
            shutdown,
            guardian,
            memory,
            mcp_servers,
            risk_overrides,
        } = deps;
        let events = crate::events::EventsHub::default();
        // Approval broker: emits onto the same WS hub; timeout from env
        // (A24_APPROVAL_TIMEOUT_SECS, default 300s)
        let timeout = std::env::var("A24_APPROVAL_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map_or(Duration::from_secs(300), Duration::from_secs);
        let hub = events.clone();
        let broker = agent24_policy::ApprovalBroker::with_guardian(
            store.clone(),
            StdArc::new(move |body| hub.broadcast(body)),
            timeout,
            guardian,
        );
        let tools = Arc::new(
            tools
                .with_risk_overrides(
                    StdArc::clone(&risk_overrides) as StdArc<dyn agent24_tools::RiskOverrides>
                )
                .with_gate(StdArc::new(agent24_policy::BrokerGate::new(StdArc::clone(
                    &broker,
                )))),
        );
        let runs = agent24_agent::RunManager::with_memory(
            store.clone(),
            Arc::clone(&router),
            Arc::clone(&tools),
            StdArc::new(events.clone()),
            shutdown.clone(),
            memory,
        );
        let sched_hub = events.clone();
        let scheduler = agent24_scheduler::Scheduler::new(
            store.clone(),
            StdArc::new(RunManagerTrigger {
                runs: Arc::clone(&runs),
            }),
            StdArc::new(move |body| sched_hub.broadcast(body)),
        );
        Self {
            risk_overrides,
            token: Arc::new(token),
            mcp_servers: Arc::new(mcp_servers),
            router,
            tools,
            broker,
            usage: Arc::new(crate::routes::UsageCounters::default()),
            events,
            store,
            runs,
            scheduler,
            shutdown,
        }
    }
}

impl AppState {
    /// Re-read the override set after the user changed it.
    ///
    /// A failed reload leaves the previous snapshot in place rather than
    /// clearing it: the old rules were user-authored too, and dropping them
    /// would silently re-tighten every tool the user had relaxed — surprising,
    /// though never unsafe.
    pub async fn reload_overrides(&self) {
        if let Err(err) = self.risk_overrides.reload(&self.store).await {
            tracing::error!("reloading risk overrides: {err}; keeping the previous set");
        }
    }
}

pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
        },
    };
    (status, Json(body)).into_response()
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        backend: "rust".to_owned(),
    })
}

/// Authenticated shutdown (bearer token proves the caller owns this daemon —
/// unlike a pid from a possibly-stale state file, this can never kill an
/// unrelated reused-pid process). Used by `agent24 daemon stop`.
async fn shutdown_handler(State(state): State<AppState>) -> Response {
    tracing::info!("shutdown requested via /api/v1/shutdown");
    state.shutdown.cancel();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

async fn fallback() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route")
}

/// Bearer-token gate for everything except `GET /api/v1/health`
/// (SPEC-002 §4: health is the only unauthenticated endpoint — method
/// included, so a future POST on the same path never silently bypasses auth).
async fn auth(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/api/v1/health" {
        return next.run(req).await;
    }
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), state.token.as_bytes()));
    if authorized {
        next.run(req).await
    } else {
        error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        )
    }
}

/// Constant-time comparison — a timing oracle on a localhost token is a small
/// risk, but the cost of doing it right is one function.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/chat", post(crate::routes::post_chat))
        .route("/api/v1/models", get(crate::routes::get_models))
        .route("/api/v1/usage", get(crate::routes::get_usage))
        .route("/api/v1/tools", get(crate::routes::get_tools))
        .route(
            "/api/v1/tool-overrides",
            get(crate::overrides::list_overrides),
        )
        .route(
            "/api/v1/tool-overrides/{pattern}",
            axum::routing::put(crate::overrides::put_override)
                .delete(crate::overrides::delete_override),
        )
        .route("/api/v1/approvals", get(crate::approvals::list_approvals))
        .route(
            "/api/v1/approvals/{id}",
            get(crate::approvals::get_approval).post(crate::approvals::decide_approval),
        )
        .route(
            "/api/v1/schedules",
            get(crate::schedules::list_schedules).post(crate::schedules::create_schedule),
        )
        .route(
            "/api/v1/schedules/{id}",
            get(crate::schedules::get_schedule)
                .patch(crate::schedules::update_schedule)
                .delete(crate::schedules::delete_schedule),
        )
        .route(
            "/api/v1/schedules/{id}/run_now",
            axum::routing::post(crate::schedules::run_now),
        )
        .route("/api/v1/events", get(crate::events::ws_events))
        .route("/api/v1/shutdown", axum::routing::post(shutdown_handler))
        .route(
            "/api/v1/sessions",
            post(crate::runs::create_session).get(crate::runs::list_sessions),
        )
        .route("/api/v1/sessions/{id}", get(crate::runs::get_session))
        .route(
            "/api/v1/runs",
            post(crate::runs::create_run).get(crate::runs::list_runs),
        )
        .route("/api/v1/runs/{id}", get(crate::runs::get_run))
        .route("/api/v1/runs/{id}/cancel", post(crate::runs::cancel_run))
        .fallback(fallback)
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn serve(
    port: u16,
    ephemeral: bool,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    // Non-ephemeral daemons are singletons: hold an exclusive lifetime lock so
    // a concurrently-started second daemon fails fast instead of leaking as an
    // untracked process (review B6). Ephemeral instances skip both the lock
    // and the discovery file — they are private to one CLI invocation.
    let _singleton = if ephemeral {
        None
    } else {
        match agent24_protocol::state_file::try_acquire_singleton()? {
            Some(guard) => Some(guard),
            None => {
                return Err(std::io::Error::other(
                    "another agent24d is already running (singleton lock held)",
                ));
            }
        }
    };

    let token = generate_token();
    // Store: file-backed under ~/.agent24 (ephemeral instances get :memory:)
    let store = if ephemeral {
        Store::open_memory().await.map_err(std::io::Error::other)?
    } else {
        let dir = agent24_protocol::state_file::state_dir()
            .ok_or_else(|| std::io::Error::other("HOME not set"))?;
        Store::open(&dir.join("agent24.db"))
            .await
            .map_err(std::io::Error::other)?
    };
    // Fail-closed sweep BEFORE accepting any request: approvals left pending
    // by a previous process abort now (C1 primitive; ordering per its review)
    let swept = store
        .abort_lingering_approvals(&agent24_core::util::now_iso8601())
        .await
        .map_err(std::io::Error::other)?;
    if swept > 0 {
        tracing::warn!("aborted {swept} lingering pending approvals from a previous process");
    }
    let orphans = store
        .sweep_orphan_runs(&agent24_core::util::now_iso8601())
        .await
        .map_err(std::io::Error::other)?;
    if orphans > 0 {
        tracing::warn!("cancelled {orphans} orphan non-terminal runs from a previous process");
    }
    // Tool workspace: the fs whitelist root + shell cwd. Created up front so
    // the canonicalized whitelist is non-empty from the first request.
    let workspace = agent24_protocol::state_file::state_dir()
        .ok_or_else(|| std::io::Error::other("HOME not set"))?
        .join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let router = Arc::new(ModelRouter::from_env());
    let guardian = build_guardian(&router);
    // D1 session memory: a KV file next to the main store (ephemeral daemons get
    // an in-memory one). A failure here degrades to no memory rather than
    // refusing to start — sessions simply don't remember, as before.
    let memory = open_session_memory(ephemeral, &router, &cancel).await;

    // M-E/E1b: mount external MCP servers from ~/.agent24/mcp.json and register
    // their tools. Registered with `with()` so they are dispatchable, while
    // McpTool sets requires_approval = true so EVERY call still goes through the
    // C4 gate — the whitelist decides "may be dispatched", the gate decides
    // "may run this time". A broken server is logged and skipped, never fatal.
    let mut tools = agent24_tools::ToolRegistry::builtin(workspace);
    let mcp_servers = match crate::mcp::config_path() {
        Some(path) => match crate::mcp::load_config(&path) {
            Ok(cfg) => {
                let specs = cfg.specs();
                if specs.is_empty() {
                    Vec::new()
                } else {
                    let (servers, mcp_tools) = crate::mcp::mount(&specs, &cancel).await;
                    for tool in mcp_tools {
                        tools = tools.with(tool);
                    }
                    servers
                }
            }
            Err(err) => {
                tracing::error!("ignoring {}: {err}", path.display());
                Vec::new()
            }
        },
        None => Vec::new(),
    };

    // H2: load the user's risk overrides before the registry is frozen, so the
    // first dispatch already resolves against them. A read failure is logged
    // and treated as "no overrides" — which fails CLOSED (every tool keeps its
    // declared, more restrictive class), never open.
    let risk_overrides = StdArc::new(
        match agent24_policy::overrides::RiskOverrideStore::load(&store).await {
            Ok(loaded) => loaded,
            Err(err) => {
                tracing::error!("could not load risk overrides ({err}); continuing with none");
                agent24_policy::overrides::RiskOverrideStore::from_rows(Vec::new())
            }
        },
    );
    let state = AppState::new(AppDeps {
        token: token.clone(),
        router,
        tools,
        store,
        risk_overrides,
        shutdown: cancel.clone(),
        guardian,
        memory,
        mcp_servers,
    });
    // Scheduler tick loop: polls due schedules and fires runs. Cadence from
    // A24_SCHEDULER_TICK_SECS (default 10s; finest schedule granularity is a
    // minute, so a few seconds' latency is invisible).
    let tick_secs = std::env::var("A24_SCHEDULER_TICK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(10);
    let scheduler = Arc::clone(&state.scheduler);
    let sched_cancel = cancel.clone();
    tokio::spawn(scheduler.run(
        StdArc::new(agent24_scheduler::SystemClock),
        Duration::from_secs(tick_secs),
        sched_cancel,
    ));

    let router = build_router(state);

    // 127.0.0.1 only — never a public bind (SPEC-001 §9)
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let local = listener.local_addr()?;

    // SPEC-002 §4 ready line: parsers scan stdout for the first type=="ready"
    // JSON line. stdout carries nothing else (logs go to stderr).
    // Discovery state file BEFORE the ready line: a CLI that has seen the
    // ready line may immediately rely on attached-mode discovery.
    let daemon_pid = std::process::id();
    if !ephemeral
        && let Err(err) =
            agent24_protocol::state_file::write(&agent24_protocol::state_file::DaemonState {
                port: local.port(),
                token: token.clone(),
                pid: daemon_pid,
                version: env!("CARGO_PKG_VERSION").to_owned(),
            })
    {
        tracing::warn!("could not write daemon state file: {err}");
    }

    println!(
        "{}",
        serde_json::json!({
            "type": "ready",
            "port": local.port(),
            "token": token,
            "version": env!("CARGO_PKG_VERSION"),
        })
    );

    // Signal handling: SIGTERM (process managers) + SIGINT (Ctrl+C in dev)
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        let sigterm = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                match signal(SignalKind::terminate()) {
                    Ok(mut s) => {
                        s.recv().await;
                    }
                    Err(err) => {
                        // Never resolve on registration failure — resolving would
                        // be indistinguishable from a real signal and trigger an
                        // immediate graceful shutdown at startup.
                        tracing::error!("SIGTERM handler failed: {err}");
                        std::future::pending::<()>().await;
                    }
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };
        let sigint = async {
            if let Err(err) = tokio::signal::ctrl_c().await {
                // Mirror the SIGTERM arm: a registration failure must never be
                // indistinguishable from a real signal — park forever instead
                // of resolving the select and triggering a spurious shutdown.
                tracing::error!("SIGINT handler failed: {err}");
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = sigterm => {},
            () = sigint => {},
        }
        tracing::info!("shutdown signal received");
        signal_cancel.cancel();
    });

    let graceful_cancel = cancel.clone();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move { graceful_cancel.cancelled().await });

    // Force-exit backstop: once cancelled, in-flight requests get
    // SHUTDOWN_GRACE to finish, then the process exits regardless.
    let result = tokio::select! {
        result = server => result,
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {
            tracing::warn!("graceful shutdown exceeded {SHUTDOWN_GRACE:?}; forcing exit");
            Ok(())
        }
    };
    // Only remove our own state file — a newer daemon may have replaced it
    if !ephemeral {
        agent24_protocol::state_file::remove_if_owner(daemon_pid);
    }
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn state() -> AppState {
        state_with_guardian(None).await
    }

    async fn state_with_guardian(
        guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    ) -> AppState {
        AppState::new(AppDeps {
            token: "testtoken".to_owned(),
            router: Arc::new(ModelRouter::with_defaults(vec![])),
            tools: agent24_tools::ToolRegistry::new(),
            store: Store::open_memory().await.unwrap(),
            shutdown: CancellationToken::new(),
            guardian,
            memory: None,
            mcp_servers: Vec::new(),
            risk_overrides: StdArc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
                Vec::new(),
            )),
        })
    }

    /// A guardian whose assessor always returns the given verdict — lets us test
    /// the daemon's wiring without a live model.
    struct StubAssessor(agent24_policy::guardian::RiskLevel);

    #[async_trait::async_trait]
    impl agent24_policy::guardian::RiskAssessor for StubAssessor {
        async fn assess(
            &self,
            _input: &agent24_policy::guardian::AssessInput<'_>,
            _cancel: &CancellationToken,
        ) -> Result<agent24_policy::guardian::RiskAssessment, agent24_policy::guardian::AssessError>
        {
            Ok(agent24_policy::guardian::RiskAssessment {
                level: self.0,
                rationale: "stub".to_owned(),
            })
        }
    }

    fn stub_guardian(
        level: agent24_policy::guardian::RiskLevel,
        always_review: Vec<String>,
    ) -> StdArc<agent24_policy::guardian::Guardian> {
        StdArc::new(
            agent24_policy::guardian::Guardian::new(StdArc::new(StubAssessor(level)))
                .always_review(always_review),
        )
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["backend"], "rust");
        assert!(json["version"].as_str().is_some());
    }

    #[tokio::test]
    async fn post_to_health_path_requires_token() {
        // The auth exemption is GET-only — same path, other method: 401
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn other_routes_401_without_token_with_v1_envelope() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn wrong_token_401_correct_token_reaches_404_envelope() {
        let router = build_router(state().await);
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/definitely-not-a-route")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // authorized but unknown route → v1 404 envelope
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[test]
    fn token_is_32_bytes_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn guardian_is_off_unless_explicitly_enabled() {
        // Fail-safe: absent / empty / typo / "0" / "no" all leave it OFF.
        assert!(!guardian_enabled(None));
        assert!(!guardian_enabled(Some("")));
        assert!(!guardian_enabled(Some("0")));
        assert!(!guardian_enabled(Some("no")));
        assert!(!guardian_enabled(Some("ture"))); // typo must not enable
        // Only an explicit opt-in turns it on.
        assert!(guardian_enabled(Some("1")));
        assert!(guardian_enabled(Some("true")));
        assert!(guardian_enabled(Some("TRUE")));
        assert!(guardian_enabled(Some(" 1 ")));
    }

    #[test]
    fn always_review_defaults_to_exec_and_parses_lists() {
        // Default keeps shell_exec human-gated even with the guardian on.
        assert_eq!(parse_always_review(None), vec!["exec".to_owned()]);
        assert_eq!(
            parse_always_review(Some("exec, fs_write ,network")),
            vec![
                "exec".to_owned(),
                "fs_write".to_owned(),
                "network".to_owned()
            ]
        );
        // Explicitly empty = operator allows every kind to be auto-approvable.
        assert!(parse_always_review(Some("")).is_empty());
        assert!(parse_always_review(Some(" , ")).is_empty());
    }

    /// An approval row has a FK to its run, so escalation tests must seed one.
    async fn seed_run(store: &Store, id: &str) {
        let now = agent24_core::util::now_iso8601();
        store
            .insert_run(&agent24_protocol::Run {
                id: id.to_owned(),
                session_id: None,
                status: agent24_protocol::RunStatus::Running,
                input: agent24_protocol::RunInput {
                    prompt: "p".to_owned(),
                    model_override: None,
                },
                output: None,
                error: None,
                usage: agent24_protocol::Usage {
                    prompt_tokens: 0,
                    completion_tokens: 0,
                    total_tokens: 0,
                    cost_usd: 0.0,
                },
                schedule_id: None,
                created_at: now.clone(),
                started_at: Some(now),
                ended_at: None,
            })
            .await
            .unwrap();
    }

    /// Drive one gated call through the daemon's real broker.
    async fn gated_call(state: &AppState, tool: &str, kind: &str) -> agent24_policy::Verdict {
        state
            .broker
            .request(
                "run_1",
                Some("sess_1"),
                "tc_1",
                tool,
                kind,
                format!("{tool}: x"),
                serde_json::Map::new(),
                &CancellationToken::new(),
            )
            .await
    }

    #[tokio::test]
    async fn wired_guardian_auto_approves_low_risk_without_a_human() {
        // Codex follow-up: prove the daemon's broker really consults the injected
        // guardian. A low verdict on a non-always-review kind auto-approves with
        // NO approval row (nobody was asked) — and it returns immediately, so no
        // 300s human-approval path is involved.
        let state = state_with_guardian(Some(stub_guardian(
            agent24_policy::guardian::RiskLevel::Low,
            vec![],
        )))
        .await;
        let verdict = gated_call(&state, "fs_write", "fs_write").await;
        assert_eq!(verdict, agent24_policy::Verdict::Approved);
        assert!(state.store.list_approvals(None).await.unwrap().is_empty());
        let audits = state.store.list_audit().await.unwrap();
        assert!(audits.iter().any(|a| a.action == "approval.auto_approved"));
    }

    #[tokio::test]
    async fn wired_guardian_never_auto_approves_an_always_review_kind() {
        // The default always-review list keeps shell_exec ("exec") human-gated
        // even when the model says low. Escalation is audited; we cancel rather
        // than wait out the approval timeout.
        let state = state_with_guardian(Some(stub_guardian(
            agent24_policy::guardian::RiskLevel::Low,
            vec!["exec".to_owned()],
        )))
        .await;
        seed_run(&state.store, "run_1").await;
        let cancel = CancellationToken::new();
        let broker = Arc::clone(&state.broker);
        let c = cancel.clone();
        let waiter = tokio::spawn(async move {
            broker
                .request(
                    "run_1",
                    Some("sess_1"),
                    "tc_1",
                    "shell_exec",
                    "exec",
                    "shell_exec: rm -rf /".to_owned(),
                    serde_json::Map::new(),
                    &c,
                )
                .await
        });
        // A pending row must appear → it went to the human flow, not auto-approved.
        let mut pending = false;
        for _ in 0..200 {
            if !state
                .store
                .list_approvals(Some(agent24_protocol::ApprovalStatus::Pending))
                .await
                .unwrap()
                .is_empty()
            {
                pending = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pending, "always-review kind was not escalated to a human");
        cancel.cancel();
        let verdict = waiter.await.unwrap();
        assert!(
            matches!(verdict, agent24_policy::Verdict::Aborted(_)),
            "{verdict:?}"
        );
        let audits = state.store.list_audit().await.unwrap();
        assert!(
            audits
                .iter()
                .any(|a| a.action == "approval.guardian_escalated")
        );
    }

    #[tokio::test]
    async fn without_a_guardian_every_gated_call_still_asks_a_human() {
        // Default daemon (no guardian): unchanged behaviour — a pending row.
        let state = state().await;
        seed_run(&state.store, "run_1").await;
        let cancel = CancellationToken::new();
        let broker = Arc::clone(&state.broker);
        let c = cancel.clone();
        let waiter = tokio::spawn(async move {
            broker
                .request(
                    "run_1",
                    Some("sess_1"),
                    "tc_1",
                    "fs_write",
                    "fs_write",
                    "fs_write: x".to_owned(),
                    serde_json::Map::new(),
                    &c,
                )
                .await
        });
        let mut pending = false;
        for _ in 0..200 {
            if !state
                .store
                .list_approvals(Some(agent24_protocol::ApprovalStatus::Pending))
                .await
                .unwrap()
                .is_empty()
            {
                pending = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(pending, "no guardian, yet no human was asked");
        cancel.cancel();
        let _ = waiter.await;
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }

    // ── H2: risk overrides end to end ────────────────────────────────────────

    use agent24_tools::RiskOverrides as _;

    /// The whole point of H2 is that a rule the user writes governs the NEXT
    /// dispatch, with no restart. Anything less and the feature is a settings
    /// screen that lies. Asserted through the live registry the request path
    /// uses, not through the store.
    #[tokio::test]
    async fn a_stored_override_governs_the_next_dispatch() {
        let state = state().await;
        let store = state.store.clone();
        store
            .set_risk_override(
                "mcp_fs_*",
                agent24_protocol::RiskClass::Read,
                "cli",
                &agent24_core::util::now_iso8601(),
            )
            .await
            .unwrap();

        // Before the reload the daemon has not seen it …
        assert!(state.risk_overrides.is_empty());
        state.reload_overrides().await;
        // … and after, the same object the registry resolves against carries it.
        assert_eq!(state.risk_overrides.len(), 1);
        assert_eq!(
            state.risk_overrides.resolve("mcp_fs_read"),
            Some(agent24_protocol::RiskClass::Read)
        );
        assert_eq!(state.risk_overrides.resolve("shell_exec"), None);
    }

    /// A rule that names a builtin is STORED (the user said it) but must not
    /// take effect. Storing and applying are deliberately separate: the rule
    /// stays visible and revocable instead of being silently dropped at write
    /// time, while the registry keeps refusing to relax code we wrote.
    #[tokio::test]
    async fn an_override_naming_a_builtin_is_stored_but_never_applied() {
        let state = state().await;
        state
            .store
            .set_risk_override(
                "shell_exec",
                agent24_protocol::RiskClass::Read,
                "cli",
                &agent24_core::util::now_iso8601(),
            )
            .await
            .unwrap();
        state.reload_overrides().await;

        assert_eq!(
            state.risk_overrides.resolve("shell_exec"),
            Some(agent24_protocol::RiskClass::Read),
            "the rule is stored and listable"
        );
        let dir = tempfile::tempdir().unwrap();
        let reg = agent24_tools::ToolRegistry::builtin(dir.path().to_path_buf())
            .with_risk_overrides(
                StdArc::clone(&state.risk_overrides) as StdArc<dyn agent24_tools::RiskOverrides>
            );
        assert_eq!(
            reg.tool_risk_class("shell_exec"),
            Some(agent24_protocol::RiskClass::Exec),
            "but shell_exec is still exec — a builtin may be tightened, never relaxed"
        );
        assert!(reg.tool_requires_approval("shell_exec"));
    }
}
