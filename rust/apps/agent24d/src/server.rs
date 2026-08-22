//! HTTP server: router, auth middleware, ready line, graceful shutdown.

use std::sync::Arc;
use std::time::Duration;

use agent24_models::router::ModelRouter;
use agent24_protocol::Health;
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

/// The model ids on offer at startup, for the mount-time resource check.
///
/// Three properties, each of which cost a real bug to learn:
///
/// - **Enumerated once**, not per module: `ModelRouter::models` queries every
///   configured provider over the network, so per-module would multiply startup
///   latency by the module count and make one slow provider look like a module
///   fault.
/// - **Enumerated only when needed.** Zero times is better than once. Sin90
///   declares no models at all today, so an unconditional probe is pure startup
///   cost — and it lands after MCP's own 10s budget, inside the CLI's 15s ready
///   deadline. [`Self::skipped`] is what a daemon with nothing to check uses.
/// - **Completeness is tracked**, not inferred from emptiness. A partial failure
///   (one provider answers, another is down) still returns a non-empty union, so
///   a model served by the DOWN provider would be reported missing. "Install this
///   model" and "start your provider" are not the same instruction, and only one
///   of them would be right.
struct ModelCatalog(std::result::Result<Vec<String>, String>);

impl ModelCatalog {
    /// No admissible module declares any model, so nothing was asked.
    fn skipped() -> Self {
        Self(Err(
            "no module declares a model, so no provider was queried".to_owned(),
        ))
    }

    /// Ask every provider, bounded. A provider that hangs must not push the
    /// daemon past the CLI's ready deadline — a timed-out probe is `Unknown`,
    /// which is exactly the honest answer.
    async fn probe(router: &Arc<ModelRouter>, cancel: &CancellationToken) -> Self {
        const BUDGET: Duration = Duration::from_secs(3);
        let inv = match tokio::time::timeout(BUDGET, router.models_detailed(&cancel.child_token()))
            .await
        {
            Ok(inv) => inv,
            Err(_) => {
                return Self(Err(format!(
                    "model enumeration exceeded {}s",
                    BUDGET.as_secs()
                )));
            }
        };
        // INCOMPLETE, not empty, is the disqualifier. An empty list from providers
        // that all answered honestly means the models really are absent; a
        // non-empty list from a partial sweep proves nothing about what is missing.
        // (With NO providers configured at all, the sweep is vacuously complete and
        // the answer is an honest "nothing is available". Note it does not say WHY:
        // zero providers and providers that all answered with nothing are
        // indistinguishable once the count is dropped. A `NoProviders` variant is
        // worth adding when `agent24 os` has to explain this to a user.)
        if !inv.is_complete() {
            return Self(Err(format!(
                "{} provider(s) did not answer: {}",
                inv.failures.len(),
                inv.failures.join("; ")
            )));
        }
        Self(Ok(inv.models.into_iter().map(|m| m.id).collect()))
    }
}

impl crate::domain::ModelInventory for ModelCatalog {
    fn available(&self) -> std::result::Result<&[String], String> {
        match &self.0 {
            Ok(v) => Ok(v),
            Err(e) => Err(e.clone()),
        }
    }
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
            // Scheduled runs are unattended — plan mode needs a human to approve
            // the plan, so a fired schedule always runs Normal.
            mode: agent24_protocol::RunMode::Normal,
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

pub use agent24_domain::http::error_response;

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

/// The kernel router with NO domain OS mounted. Test-only since ME-1b: the real
/// startup path always goes through [`build_router_with_modules`], and a
/// production caller that skipped the mounter would silently serve a daemon with
/// no modules.
#[cfg(test)]
pub fn build_router(state: AppState) -> Router {
    build_router_with_modules(state, Router::new())
}

/// The kernel router with `modules` (from [`crate::domain::mount_all`]) folded in.
///
/// **The fold happens BEFORE `.layer(auth)` on purpose.** An axum layer applies
/// only to the routes already on the router, so nesting modules after the auth
/// layer would leave every module route unauthenticated — the modules would be a
/// hole in the daemon's only access control. `module_routes_are_behind_kernel_auth`
/// is the regression test; do not reorder these two lines.
pub fn build_router_with_modules(state: AppState, modules: Router) -> Router {
    let kernel = Router::new()
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
            "/api/v1/standing-grants",
            get(crate::overrides::list_standing_grants),
        )
        .route(
            "/api/v1/standing-grants/{id}",
            axum::routing::delete(crate::overrides::delete_standing_grant),
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
        .with_state(state.clone());

    // Modules first, auth last — see the doc comment above.
    kernel
        .merge(modules)
        .layer(middleware::from_fn_with_state(state, auth))
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
    // NOTE: the startup sweeps (durable-resume restore + orphan cancel) run
    // AFTER the state is built, below — the restore sweep needs the run manager.
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
    let mut tools = agent24_tools::ToolRegistry::builtin(workspace.clone());
    // H9: register the read-only explorer subagent. It runs against a registry
    // that holds ONLY read builtins and NOT itself, so the sub-run cannot write,
    // execute, or recurse — the guarantees are structural, not policy-checked.
    let explorer_tools = StdArc::new(agent24_tools::ToolRegistry::read_only(workspace));
    tools = tools.with(StdArc::new(agent24_agent::subagent::ExplorerSubagent::new(
        Arc::clone(&router),
        explorer_tools,
    )));
    // H5: register self-wake. It creates one-shot schedules in the same store the
    // scheduler reads, so an agent can schedule its own follow-ups; the woken run
    // is gated like any other (see self_wake module docs).
    tools = tools.with(StdArc::new(agent24_agent::self_wake::SelfWakeTool::new(
        store.clone(),
    )));
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

    // H3 durable-resume startup, BEFORE accepting any request and BEFORE the
    // orphan sweep: restore restorable parked approvals (re-broadcast + keep
    // pending) so their runs survive to be resumed when answered, and abort the
    // rest fail-closed. The orphan sweep then cancels every still-non-terminal
    // run whose approval did NOT survive — so the restore MUST come first.
    let (restored, aborted) = state.runs.restore_pending_approvals().await;
    if restored > 0 || aborted > 0 {
        tracing::info!(
            "durable resume: {restored} approval(s) restored, {aborted} aborted from a previous process"
        );
    }
    let orphans = state
        .store
        .sweep_orphan_runs(&agent24_core::util::now_iso8601())
        .await
        .map_err(std::io::Error::other)?;
    if orphans > 0 {
        tracing::warn!("cancelled {orphans} orphan non-terminal runs from a previous process");
    }

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

    // Domain OSes (ME-1b-b). THIS is the one place in the kernel that may name a
    // module: someone has to say which OS is installed, and a composition root
    // naming its components is not the coupling ADR-029 objects to. Everything
    // downstream — routing, the data directory, the event module, the capability
    // grant — is derived from the manifest, so `crate::domain` and
    // `build_router_with_modules` still contain no Sin90-shaped branch. A second
    // OS is another entry in `installed` — each built independently so one that
    // fails to construct cannot stop the others from mounting.
    let state_dir = agent24_protocol::state_file::state_dir()
        .ok_or_else(|| std::io::Error::other("HOME not set"))?;
    let mode = if ephemeral {
        // Ephemeral daemons get an in-memory store and NO migration: they are
        // private to one CLI invocation and must not touch the user's database.
        agent24_sin90_os::StorageMode::Memory
    } else {
        // Pre-ME-1b daemons kept Sin90 at `~/.agent24/sin90.db`. Handing that path
        // over as `legacy` is what stops an upgrading user from opening a
        // brand-new empty Sin90 while their real data sits one directory up; the
        // copy itself is a SQLite snapshot, not a file move (see
        // `Sin90Store::open_migrating_from`).
        agent24_sin90_os::StorageMode::Persistent {
            legacy: Some(state_dir.join("sin90.db")),
        }
    };
    let mut installed: Vec<StdArc<dyn agent24_domain::DomainModule>> = Vec::new();
    match agent24_sin90_os::Sin90Module::new(mode) {
        Ok(m) => installed.push(StdArc::new(m)),
        Err(err) => {
            // Only reachable if this build's compiled-in manifest is invalid. The
            // daemon still starts, and any OTHER installed module still mounts: a
            // broken domain OS is not a broken kernel, which is the whole point of
            // the boundary.
            tracing::error!("sin90 module could not be constructed ({err}); not mounting it");
        }
    }
    // An ephemeral daemon gets a throwaway root, NOT `~/.agent24/os`. The mounter
    // creates a directory for every module it mounts, and an in-memory module will
    // never writes into it — but an ephemeral instance's STORES were all in memory
    // before ME-1b-b, and quietly starting to create per-module directories under
    // the user's state dir would erode that. (It does still create
    // `~/.agent24/workspace` for tools; this is about not ADDING to what an
    // ephemeral run touches.) Keyed by pid so two concurrent ephemeral daemons
    // cannot collide.
    let os_root = if ephemeral {
        std::env::temp_dir().join(format!("agent24-ephemeral-{}", std::process::id()))
    } else {
        state_dir.join("os")
    };
    // The registry (ME-2). A MALFORMED os.json is fatal to the registry, not to
    // the daemon: falling back to defaults would mount modules the user had
    // explicitly disabled, so instead nothing is mounted and the reason is loud.
    // The registry (ME-2). An unreadable `os.json` is NOT fatal to the daemon and
    // NOT quietly ignored: the mounter degrades every ADMISSIBLE module to a 503
    // carrying `registry_invalid` (an inadmissible manifest is still refused first,
    // because that verdict does not depend on the config), so a client sees that the CONFIG is broken rather than
    // being sent to look at a module that is fine. Falling back to defaults would
    // mount something the user had switched off; mounting nothing would answer 404,
    // which reads as "this feature is gone".
    let os_config = crate::os_config::config_path()
        .ok_or_else(|| "HOME not set".to_owned())
        .and_then(|p| crate::os_config::OsConfig::load(&p));
    if let Err(why) = &os_config {
        tracing::error!("os.json could not be read ({why}); every admissible domain OS will 503");
    }
    // Probe only if some module that could actually mount declares a model.
    // Checking `enabled` too means disabling the one module that needs a model
    // also removes the startup cost of looking for it.
    //
    // This DUPLICATES admission logic that `mount_all` applies again, on purpose,
    // and the duplication is safe in both directions because it is an
    // OPTIMISATION, not a decision: if this predicate is too permissive we probe
    // when nobody needed it (wasted time), and if it is too strict a module that
    // does need the check gets `Unknown` instead of a definite answer (less
    // information, never wrong information). Neither can change what mounts.
    let needs_models = installed.iter().any(|m| {
        let man = m.manifest();
        !man.requires_models().is_empty()
            && man.is_mountable_in_process()
            && os_config.as_ref().is_ok_and(|c| c.is_enabled(man.name()))
    });
    let inventory = if needs_models {
        ModelCatalog::probe(&state.router, &cancel).await
    } else {
        ModelCatalog::skipped()
    };
    let (module_routes, reports) = crate::domain::mount_all(
        &installed,
        &os_root,
        &state.events,
        os_config.as_ref().map_err(String::as_str),
        &inventory,
    )
    .await;
    for r in &reports {
        tracing::info!(
            "domain OS {} at {}: {:?} (grants: {:?})",
            r.name,
            r.namespace,
            r.outcome,
            r.granted
        );
        match &r.resources {
            crate::domain::ResourceStatus::NotChecked
            | crate::domain::ResourceStatus::Satisfied => {}
            crate::domain::ResourceStatus::MissingModels(missing) => tracing::warn!(
                "domain OS {} declares models that are not available: {:?} — it is \
                 mounted, but features needing them will fail",
                r.name,
                missing
            ),
            crate::domain::ResourceStatus::Unknown(why) => tracing::warn!(
                "could not check {}'s declared models ({why}); NOT reporting them \
                 as missing, because an unreachable provider is not a missing model",
                r.name
            ),
        }
    }
    let router = build_router_with_modules(state, module_routes);

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
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// No provider answered — the honest default for a test daemon with no
    /// providers configured. Modules under test declare no models, so the check
    /// is Satisfied regardless; ME-2's resource cases have their own tests.
    struct NoModels;
    impl crate::domain::ModelInventory for NoModels {
        fn available(&self) -> std::result::Result<&[String], String> {
            Err("no providers in tests".to_owned())
        }
    }

    /// A daemon state wired for tests. `pub(crate)` so the mounter's tests in
    /// `domain.rs` can merge a module into the REAL kernel router — testing the
    /// degraded 503 against a stub router would not show which fallback wins.
    pub(crate) async fn state() -> AppState {
        state_with_guardian(None).await
    }

    /// A router with Sin90 mounted AS A DOMAIN OS — through `mount_all`, exactly
    /// like `serve` does it.
    ///
    /// The SPIKE-00 tests below deliberately still go over HTTP rather than
    /// calling the store: their job is to show that moving Sin90 behind
    /// `DomainModule` left the HEALTHY surface unchanged — same paths, same
    /// statuses, same bodies, and `proposal.applied` still reaching the kernel's
    /// bus with the right module and kind. What DID change is the unavailable
    /// surface: a degraded module now answers 503 for every path and method under
    /// its namespace, where the old inline guard produced 503 only on its own
    /// routes and left 404/405 for the rest.
    ///
    /// The `TempDir` is returned so the caller can hold it. With the in-memory
    /// store it is only the (empty) directory the mounter creates, so dropping it
    /// would not break anything today — but a test that switches to `Persistent`
    /// would silently lose its database the moment the handle went out of scope.
    async fn router_with_sin90() -> (Router, tempfile::TempDir) {
        router_with_sin90_mode(agent24_sin90_os::StorageMode::Memory).await
    }

    async fn router_with_sin90_mode(
        mode: agent24_sin90_os::StorageMode,
    ) -> (Router, tempfile::TempDir) {
        let st = state().await;
        let tmp = tempfile::tempdir().unwrap();
        let m: StdArc<dyn agent24_domain::DomainModule> =
            StdArc::new(agent24_sin90_os::Sin90Module::new(mode).unwrap());
        let (modules, _) = crate::domain::mount_all(
            &[m],
            tmp.path(),
            &st.events,
            Ok(&crate::os_config::OsConfig::default()),
            &NoModels,
        )
        .await;
        (build_router_with_modules(st, modules), tmp)
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

    /// A MOUNTED domain OS is behind the kernel's auth, exactly like a kernel
    /// route.
    ///
    /// This is the regression for the mount-order hazard in
    /// [`build_router_with_modules`]: an axum layer applies only to the routes
    /// already on the router, so the version of this that "obviously compiles" —
    /// `kernel.with_state(state).layer(auth).merge(modules)` — mounts every module
    /// route with NO authentication at all. That failure is silent: the module
    /// works, its tests pass, and the daemon simply serves a module's whole
    /// surface to anyone who can reach the port. Reorder those two lines and this
    /// test goes from 401 to 200.
    #[tokio::test]
    async fn module_routes_are_behind_kernel_auth() {
        use agent24_domain::{DomainModule, DomainOsManifest, KernelCtx};

        struct OpenModule(DomainOsManifest);
        #[async_trait::async_trait]
        impl DomainModule for OpenModule {
            fn manifest(&self) -> &DomainOsManifest {
                &self.0
            }
            async fn open_store(&self, _dir: &std::path::Path) -> agent24_domain::Result<()> {
                Ok(())
            }
            fn routes(&self, _ctx: StdArc<dyn KernelCtx>) -> Router {
                // Deliberately unauthenticated on its own: modules must not have
                // to implement auth, the kernel owns it.
                Router::new().route("/secret", get(|| async { "leaked" }))
            }
        }

        let st = state().await;
        let token = st.token.to_string();
        let tmp = tempfile::tempdir().unwrap();
        let m = StdArc::new(OpenModule(
            DomainOsManifest::from_yaml(
                "name: probe\nversion: \"0.1.0\"\nroute_namespace: /api/v1/probe\n\
                 event_module: probe\ndata_dir: ~/.agent24/os/probe/\n\
                 kernel_capabilities: [events]\nimpl_kind: in_process_crate\n",
            )
            .unwrap(),
        ));
        let (modules, reports) = crate::domain::mount_all(
            &[m as StdArc<dyn DomainModule>],
            tmp.path(),
            &st.events,
            Ok(&crate::os_config::OsConfig::default()),
            &NoModels,
        )
        .await;
        assert_eq!(reports[0].outcome, crate::domain::MountOutcome::Mounted);
        let router = build_router_with_modules(st, modules);

        // No token: 401, in the kernel's v1 envelope — not the module's 200.
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/probe/secret")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            res.status(),
            StatusCode::UNAUTHORIZED,
            "a module route without a token must 401 — if this is 200, modules \
             were nested AFTER the auth layer and are an authentication hole"
        );
        assert_eq!(body_json(res).await["error"]["code"], "unauthorized");

        // With the token: the module answers.
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/probe/secret")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
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
                    mode: agent24_protocol::RunMode::Normal,
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
                req(
                    "run_1",
                    Some("sess_1"),
                    "tc_1",
                    tool,
                    kind,
                    format!("{tool}: x"),
                    serde_json::Map::new(),
                ),
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
                    req(
                        "run_1",
                        Some("sess_1"),
                        "tc_1",
                        "shell_exec",
                        "exec",
                        "shell_exec: rm -rf /".to_owned(),
                        serde_json::Map::new(),
                    ),
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
                    req(
                        "run_1",
                        Some("sess_1"),
                        "tc_1",
                        "fs_write",
                        "fs_write",
                        "fs_write: x".to_owned(),
                        serde_json::Map::new(),
                    ),
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

    /// Pre-H4 request shape for the existing guardian coverage (no schedule, no
    /// target, never external — so the session-grant path stays as it was).
    fn req<'a>(
        run_id: &'a str,
        session_id: Option<&'a str>,
        tool_call_id: &'a str,
        tool: &'a str,
        kind: &'a str,
        summary: String,
        payload: serde_json::Map<String, serde_json::Value>,
    ) -> agent24_policy::ApprovalRequest<'a> {
        agent24_policy::ApprovalRequest {
            run_id,
            session_id,
            schedule_id: None,
            tool_call_id,
            tool,
            kind,
            risk: if kind == "exec" {
                agent24_protocol::RiskClass::Exec
            } else {
                agent24_protocol::RiskClass::WriteLocal
            },
            standing_target: None,
            summary,
            payload,
        }
    }

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

    // SPIKE-00 end-to-end through the router: create a direction + a 120-min
    // block, complete it, and read the attention reconciliation back — the HTTP
    // link computes 120. (The *purity* of the replay — unaffected by later edits
    // — is proven in agent24-sin90-store's `attention_replay_is_pure_snapshot`.)
    #[tokio::test]
    async fn sin90_spike00_loop_over_router() {
        let (router, _os_dir) = router_with_sin90().await;

        async fn post(router: &Router, uri: &str, body: serde_json::Value) -> Response {
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("Authorization", "Bearer testtoken")
                        .header("Content-Type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }

        let dir = body_json(
            post(
                &router,
                "/api/v1/sin90/directions",
                serde_json::json!({ "title": "Coding", "target_window": "2026-08" }),
            )
            .await,
        )
        .await;
        let dir_id = dir["id"].as_str().unwrap().to_owned();

        let blk = body_json(
            post(
                &router,
                "/api/v1/sin90/schedule-blocks",
                serde_json::json!({ "direction_id": dir_id, "planned_minutes": 120 }),
            )
            .await,
        )
        .await;
        let blk_id = blk["id"].as_str().unwrap().to_owned();

        for to in ["started", "completed"] {
            let res = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PATCH")
                        .uri(format!("/api/v1/sin90/schedule-blocks/{blk_id}"))
                        .header("Authorization", "Bearer testtoken")
                        .header("Content-Type", "application/json")
                        .body(Body::from(format!("{{\"to\":\"{to}\"}}")))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "transition to {to}");
        }

        // Wide, date-agnostic window (events stamp `at` = now).
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/sin90/attention?start=2000-01-01T00:00:00Z&end=2100-01-01T00:00:00Z")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let att = body_json(res).await;
        let rows = att["attention"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["actual_min"], 120);
        assert_eq!(rows[0]["direction_id"], dir_id);
        assert_eq!(rows[0]["direction_title"], "Coding");
    }

    // The GET list/detail reads: create through the router, then read the same
    // rows back — and a missing proposal id is a 404, not an empty success.
    #[tokio::test]
    async fn sin90_list_reads_round_trip_over_router() {
        let (router, _os_dir) = router_with_sin90().await;

        async fn post(router: &Router, uri: &str, body: serde_json::Value) -> Response {
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("Authorization", "Bearer testtoken")
                        .header("Content-Type", "application/json")
                        .body(Body::from(serde_json::to_vec(&body).unwrap()))
                        .unwrap(),
                )
                .await
                .unwrap()
        }
        async fn get(router: &Router, uri: &str) -> Response {
            router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(uri)
                        .header("Authorization", "Bearer testtoken")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        }

        let dir = body_json(
            post(
                &router,
                "/api/v1/sin90/directions",
                serde_json::json!({ "title": "Coding", "target_window": "2026-08" }),
            )
            .await,
        )
        .await;
        let dir_id = dir["id"].as_str().unwrap().to_owned();
        post(
            &router,
            "/api/v1/sin90/schedule-blocks",
            serde_json::json!({ "direction_id": dir_id, "planned_minutes": 45 }),
        )
        .await;
        assert_eq!(
            post(
                &router,
                "/api/v1/sin90/proposals",
                serde_json::json!({
                    "id": "p-read", "status": "pending", "source": "local_brain",
                    "ops": [{"op":"create_direction","title":"Z","target_window":"2026-08"}],
                    "rationale": null
                }),
            )
            .await
            .status(),
            StatusCode::ACCEPTED
        );

        let dirs = body_json(get(&router, "/api/v1/sin90/directions").await).await;
        assert_eq!(dirs["directions"].as_array().unwrap().len(), 1);
        assert_eq!(dirs["directions"][0]["id"], dir_id);

        let blocks = body_json(get(&router, "/api/v1/sin90/schedule-blocks").await).await;
        assert_eq!(blocks["blocks"].as_array().unwrap().len(), 1);
        assert_eq!(blocks["blocks"][0]["planned_minutes"], 45);

        let props = body_json(get(&router, "/api/v1/sin90/proposals").await).await;
        assert_eq!(props["proposals"].as_array().unwrap().len(), 1);
        assert_eq!(props["proposals"][0]["id"], "p-read");

        let one = get(&router, "/api/v1/sin90/proposals/p-read").await;
        assert_eq!(one.status(), StatusCode::OK);
        let one = body_json(one).await;
        assert_eq!(one["status"], "pending");
        assert_eq!(one["ops"].as_array().unwrap().len(), 1);

        let missing = get(&router, "/api/v1/sin90/proposals/nope").await;
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(missing).await["error"]["code"], "not_found");
    }

    // A block referencing a nonexistent direction is a client mistake (FK
    // violation) → 404, not the 500 a raw sqlx error would become.
    #[tokio::test]
    async fn sin90_bad_direction_is_404_not_500() {
        let (router, _os_dir) = router_with_sin90().await;
        let res = router
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/sin90/schedule-blocks")
                    .header("Authorization", "Bearer testtoken")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"direction_id":"NOPE","planned_minutes":30}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "not_found");
    }

    // A retried `accept` returns the same receipt but must NOT re-broadcast
    // `proposal.applied` — the receipt is idempotent, the notification too.
    #[tokio::test]
    async fn sin90_retry_accept_does_not_double_emit() {
        // Subscribe to the hub BEFORE mounting, and use the same state the module
        // was mounted against — the whole point is that the module's events still
        // reach the kernel's bus now that it emits through `KernelCtx`.
        let st = state().await;
        let mut rx = st.events.subscribe();
        let tmp = tempfile::tempdir().unwrap();
        let m: StdArc<dyn agent24_domain::DomainModule> = StdArc::new(
            agent24_sin90_os::Sin90Module::new(agent24_sin90_os::StorageMode::Memory).unwrap(),
        );
        let (modules, _) = crate::domain::mount_all(
            &[m],
            tmp.path(),
            &st.events,
            Ok(&crate::os_config::OsConfig::default()),
            &NoModels,
        )
        .await;
        let router = build_router_with_modules(st, modules);

        let submit = |router: Router| async move {
            router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/sin90/proposals")
                        .header("Authorization", "Bearer testtoken")
                        .header("Content-Type", "application/json")
                        .body(Body::from(
                            r#"{"id":"p1","status":"pending","source":"local_brain","ops":[{"op":"create_direction","title":"X","target_window":"2026-08"}],"rationale":null}"#,
                        ))
                        .unwrap(),
                )
                .await
                .unwrap()
        };
        assert_eq!(submit(router.clone()).await.status(), StatusCode::ACCEPTED);

        let accept = |router: Router| async move {
            router
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/v1/sin90/proposals/p1/accept")
                        .header("Authorization", "Bearer testtoken")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap()
        };
        assert_eq!(accept(router.clone()).await.status(), StatusCode::OK);
        assert_eq!(accept(router).await.status(), StatusCode::OK); // retry

        // Drain events; count only proposal.applied.
        let mut applied = 0;
        while let Ok((_, body)) = rx.try_recv() {
            if let agent24_protocol::EventBody::Module(m) = &body
                && m.module == "sin90"
                && m.kind == "proposal.applied"
            {
                applied += 1;
            }
        }
        assert_eq!(
            applied, 1,
            "exactly one proposal.applied despite two accepts"
        );
    }

    // The head-fix, now going through the real mount path: a daemon whose sin90
    // store fails to open must serve health but 503 every sin90 route — the kernel
    // does not depend on the module.
    //
    // The failure is REAL rather than injected: the module is pointed at a legacy
    // "database" that is not one, so its migration fails and `open_store` returns
    // Err. That exercises the whole chain — module error → `MountOutcome::Degraded`
    // → the kernel's own 503 under the namespace — instead of a hand-set `None`.
    #[tokio::test]
    async fn sin90_unavailable_503s_but_kernel_lives() {
        let broken = tempfile::tempdir().unwrap();
        let legacy = broken.path().join("not-a-database.db");
        std::fs::write(&legacy, b"definitely not sqlite").unwrap();
        let (router, _os_dir) = router_with_sin90_mode(agent24_sin90_os::StorageMode::Persistent {
            legacy: Some(legacy),
        })
        .await;

        for (method, uri) in [
            ("POST", "/api/v1/sin90/directions"),
            ("GET", "/api/v1/sin90/directions"),
            ("POST", "/api/v1/sin90/schedule-blocks"),
            ("GET", "/api/v1/sin90/schedule-blocks"),
            ("PATCH", "/api/v1/sin90/schedule-blocks/x"),
            ("POST", "/api/v1/sin90/proposals"),
            ("GET", "/api/v1/sin90/proposals"),
            ("GET", "/api/v1/sin90/proposals/x"),
            ("POST", "/api/v1/sin90/proposals/x/accept"),
            ("GET", "/api/v1/sin90/attention?start=a&end=b"),
        ] {
            let res = router
                .clone()
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
            assert_eq!(
                res.status(),
                StatusCode::SERVICE_UNAVAILABLE,
                "{method} {uri} must 503 when the module is down"
            );
            let json = body_json(res).await;
            assert_eq!(json["error"]["code"], "module_unavailable");
        }

        // The kernel is unaffected.
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    // A bare date (not the fixed-width timestamp) would drop a whole day under a
    // lexical window compare — reject it rather than silently under-count.
    #[tokio::test]
    async fn sin90_attention_rejects_non_fixed_width_bounds() {
        let (router, _os_dir) = router_with_sin90().await;
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/sin90/attention?start=2026-08-01&end=2026-08-11")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
    }
}
