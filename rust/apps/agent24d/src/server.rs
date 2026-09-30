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

// A shutdown's budgets and deadlines — the HTTP drain's fixed 1.5s, the
// out-of-process modules' drain and stop grace (tunable: `A24_MODULE_DRAIN_MS`,
// `A24_MODULE_STOP_GRACE_MS`), the time to put the summary on disk, and the
// watchdog after all of them — live in `crate::lifecycle` (SHUT-1b). At the
// defaults `kill -TERM` ends the process within 2s (TASKS B2).

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
    /// T7b/ME-3e: module (gate/advise) approvals — a PARALLEL broker, its own
    /// table, its own async submit-then-poll model (design doc "现状" 4). Not
    /// `Option`: it needs only a `Store` and the WS hub, both of which exist
    /// unconditionally by the time `AppState::new` runs.
    pub module_approval_broker: Arc<crate::module_approval_broker::ModuleApprovalBroker>,
    pub usage: Arc<crate::routes::UsageCounters>,
    pub events: crate::events::EventsHub,
    /// ME4-desktop-model-ui: the timing writer's sink handle — built here
    /// (spawning its background task), same as `events`/`usage` above, so
    /// every clone of this `AppState` (including `/api/v1/chat`'s handler)
    /// shares the SAME writer task rather than each starting its own.
    pub timings: Arc<dyn crate::timing_recorder::TimingSink>,
    pub store: Store,
    /// What the mounter decided about each domain OS at startup (ME-2b).
    /// Held so `/api/v1/os` can report it — a mount verdict that only reached the
    /// log is invisible to the person who needs it. Includes modules that were
    /// never CONSTRUCTED (switched off, or whose constructor failed), because
    /// `agent24 os enable` needs a name to act on and a module that only appeared
    /// once it was already on could never be turned on.
    pub os_reports: Arc<Vec<crate::domain::MountReport>>,
    /// Where installed (out-of-process) packages live on disk (ME-3a). Computed
    /// once at startup (`agent24_os_packages::packages_root`, same call `serve`
    /// already made) and injected through [`AppDeps`] — unlike `os_reports`, this
    /// does not depend on the mount pass, so it is known before `AppState::new`
    /// runs and does not need the "empty until `serve` replaces it" dance.
    pub packages_root: Arc<std::path::PathBuf>,
    /// For each name whose catalogue entry was `Build::Package` at startup, the
    /// directory its manifest was read from (T8/ME-3g). NOT stored on
    /// `MountReport` — that struct is constructed directly by dozens of existing
    /// tests, and this is the one thing `enable`'s admission gate needs that
    /// `MountReport` does not carry: which names were ever a package at all, and
    /// where, so a currently-`Disabled` package's manifest can be re-checked
    /// on demand without guessing "not found in a rescan" means "compiled in".
    /// Empty until `serve` replaces it after the mount pass, same ordering
    /// reason as `os_reports` above.
    pub package_dirs: Arc<std::collections::HashMap<String, std::path::PathBuf>>,
    /// The live status of each out-of-process module, by name: what `agent24
    /// os list` reports beyond the startup verdict (a package can be mounted
    /// and since have given up).
    pub module_status: Arc<
        std::collections::HashMap<
            String,
            tokio::sync::watch::Receiver<agent24_os_proto::supervisor::Status>,
        >,
    >,
    /// The daemon's supervised modules, for `os disable` to stop one while
    /// the daemon runs (SUP-5). `None` when out-of-process modules cannot be
    /// started at all.
    pub supervisors: Option<Arc<crate::domain::Supervisors>>,
    /// Held across the config write and the hand-off of a hot disable's stop
    /// in `PATCH /api/v1/os/{name}` — not across the wait that follows — so
    /// concurrent toggles are applied in one order (SUP-5). The list a PATCH
    /// answers with is rendered after, and shows the state then.
    pub os_control: Arc<tokio::sync::Mutex<()>>,
    /// What `GET /api/v1/shutdown` answers — fixed at start-up (SHUT-1c).
    pub shutdown_report: Arc<agent24_protocol::ShutdownReport>,
    pub runs: Arc<agent24_agent::RunManager>,
    pub scheduler: Arc<agent24_scheduler::Scheduler>,
    /// ME4-1.3.1: the module deliverer `KernelTrigger`'s `Module` arm calls.
    /// Kept here (not only inside `KernelTrigger`, which `AppState` cannot
    /// see through its type-erased `Arc<dyn RunTrigger>`) so `serve` can call
    /// `set_supervisors` on the SAME instance right after `mount_all` returns
    /// (design §4.6).
    pub deliverer: Arc<crate::scheduler_deliver::ModuleDeliverer>,
    /// Live MCP server handles. This is an RAII guard, not data: dropping an
    /// McpServer kills its child process, which would silently break every tool
    /// it contributed. Never read on purpose — its job is to exist (M-E/E1b).
    #[allow(dead_code, reason = "RAII: keeps MCP child processes alive")]
    pub mcp_servers: Arc<Vec<Arc<agent24_mcp::McpServer>>>,
    /// Daemon-wide shutdown: handlers derive request tokens from it so
    /// shutdown cancels in-flight provider calls (run-level cancel joins in C2),
    /// and `POST /api/v1/shutdown` requests it.
    pub shutdown: Shutdown,
    /// A3-2b: the live attach registry (`docs/design/A3-ATTACHED-MODULE.md`
    /// §4–§5). Built here with `models: None` (no `ModelCallbackDeps` exists
    /// yet at `AppState::new` time — see that field's own doc); `serve`
    /// REPLACES this with a fresh one built from the real deps, hydrated from
    /// `attached.json`, right before spawning the attach listener — the same
    /// "start empty, replace once real data is ready" pattern `os_reports`
    /// uses just below, and for the same reason (this reassignment happens
    /// before `state` is ever cloned into the router).
    pub attach_registry: Arc<crate::attach_registry::AttachRegistry>,
}

/// A shutdown request, from anything that can make one — a signal, `POST
/// /api/v1/shutdown`, the server ending. [`Shutdown::request`] is synchronous
/// and does no I/O: it fixes the deadline, arms the watchdog, and cancels, in
/// that order — so nothing stuck, a stalled stderr say, keeps a shutdown from
/// starting or from being bounded (review of SUP-4, rounds 4 and 5). Clones
/// share one deadline and one watchdog.
#[derive(Clone)]
pub struct Shutdown {
    token: CancellationToken,
    /// When the shutdown began: fixed once, by whoever gets there first.
    began: Arc<std::sync::OnceLock<tokio::time::Instant>>,
    /// The modules' budgets its deadlines are derived from (SHUT-1b).
    params: crate::lifecycle::Params,
    armed: Arc<std::sync::atomic::AtomicBool>,
    /// `STARTING`, `READY` or `STOPPING`: readiness and a shutdown request
    /// decide, once and atomically, which came first (see
    /// [`Shutdown::commit_ready`]).
    phase: Arc<std::sync::atomic::AtomicU8>,
}

const STARTING: u8 = 0;
const READY: u8 = 1;
const STOPPING: u8 = 2;

impl Shutdown {
    #[cfg(test)]
    pub fn new(token: CancellationToken) -> Self {
        Self::with_params(token, crate::lifecycle::Params::default())
    }

    /// A controller whose deadlines follow `params` (SHUT-1b).
    #[must_use]
    pub fn with_params(token: CancellationToken, params: crate::lifecycle::Params) -> Self {
        Self {
            token,
            began: Arc::new(std::sync::OnceLock::new()),
            params,
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            phase: Arc::new(std::sync::atomic::AtomicU8::new(STARTING)),
        }
    }

    /// Begin the shutdown (idempotent).
    pub fn request(&self) {
        self.phase
            .store(STOPPING, std::sync::atomic::Ordering::SeqCst);
        arm_watchdog(self.deadlines().watchdog, &self.armed);
        self.token.cancel();
    }

    /// Declare the daemon ready — unless a shutdown was requested first.
    /// `false`: it was, and nothing may say this daemon is ready. One atomic
    /// step against [`Shutdown::request`], so the two cannot both win: a
    /// check followed by the ready line let a shutdown land in between and the
    /// CLI report a daemon started that was already exiting (review of SUP-4,
    /// rounds 5 and 7).
    #[must_use]
    pub fn commit_ready(&self) -> bool {
        self.phase
            .compare_exchange(
                STARTING,
                READY,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Every deadline of this shutdown, from the moment it began — fixed by
    /// the first [`Shutdown::request`], or, for a token cancelled some other
    /// way, by the first look after it. Every path that sees the cancel calls
    /// `request` before reading these, so the first to arrive fixes it, once
    /// (SHUT-1b).
    #[must_use]
    pub fn deadlines(&self) -> crate::lifecycle::Deadlines {
        let began = *self.began.get_or_init(tokio::time::Instant::now);
        self.params.deadlines(began)
    }

    /// When the HTTP drain must be done.
    #[must_use]
    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadlines().http
    }

    /// The budgets this shutdown gives modules.
    #[must_use]
    pub fn params(&self) -> crate::lifecycle::Params {
        self.params
    }

    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// A token cancelled with the shutdown.
    #[must_use]
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// Resolves when a module still being stopped by `os disable` must be
    /// killed: the budget any module gets once the shutdown begins — its
    /// drain and its stop grace — so a disable's longer drain neither holds
    /// the shutdown past its bound nor gets less than a module the shutdown
    /// stops itself (SUP-5). An absolute instant after the moment the
    /// shutdown began, which is stored, not derived: measured from whenever
    /// this was first polled, a late poll put it past the shutdown's own
    /// deadline, which then stopped waiting for it (review of SUP-5, rounds
    /// 2 and 3; SHUT-1b).
    pub fn modules_cut_off(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let shutdown = self.clone();
        async move {
            shutdown.token.cancelled().await;
            let began = shutdown.deadlines().began;
            tokio::time::sleep_until(began + shutdown.params.drain + shutdown.params.stop_grace)
                .await;
        }
    }
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

/// Adapts the run manager and the module deliverer to the scheduler's
/// `RunTrigger` (design `docs/design/ME4-S1-scheduler-callback.md` §3.3) — a
/// fired schedule becomes either a background run tagged with the schedule
/// id (`AgentRun`) or a real kernel request into the module's live
/// `Generation` (`Module`, ME4-1.3.1).
///
/// Named `KernelTrigger` (not `RunManagerTrigger`, its ME4-1.2.2b2/b3 working
/// name) because it now speaks for BOTH arms of the kernel's own trigger
/// interface, not just `RunManager`: the `AgentRun` arm is the original
/// `RunManagerTrigger` body, byte-identical, wrapped to classify into
/// `FireOutcome` (`Ok(run_id)` -> `AgentRun`, `Err(e)` -> `Failed`). The
/// `Module` arm delegates to `ModuleDeliverer` (`scheduler_deliver.rs`), which
/// is `Deferred(MountPending)` for every fire until `server::serve` calls
/// `ModuleDeliverer::set_supervisors` right after `mount_all` returns (design
/// §4.6) — never a failure either way (§4.1: none of `DeferReason`'s variants
/// are the module's fault). Only the DELIVERY PUMP
/// (`agent24_scheduler::deliveries::DeliveryPump`) ever calls this arm for a
/// module row — the tick itself never does (design §3.2).
struct KernelTrigger {
    runs: Arc<agent24_agent::RunManager>,
    deliverer: Arc<crate::scheduler_deliver::ModuleDeliverer>,
}

#[async_trait::async_trait]
impl agent24_scheduler::RunTrigger for KernelTrigger {
    async fn trigger(
        &self,
        invocation: &agent24_scheduler::ScheduleInvocation,
    ) -> agent24_scheduler::FireOutcome {
        match &invocation.target {
            agent24_scheduler::InvocationTarget::AgentRun(action) => {
                let agent24_protocol::ScheduleAction::AgentRun {
                    prompt,
                    session_id,
                    model_override,
                } = action;
                let create = agent24_protocol::RunCreate {
                    session_id: session_id.clone(),
                    prompt: prompt.clone(),
                    model_override: model_override.clone(),
                    // Scheduled runs are unattended — plan mode needs a human
                    // to approve the plan, so a fired schedule always runs
                    // Normal.
                    mode: agent24_protocol::RunMode::Normal,
                };
                match self
                    .runs
                    .start_run_with_schedule(create, Some(invocation.schedule_id.clone()))
                    .await
                {
                    Ok(run) => agent24_scheduler::FireOutcome::AgentRun { run_id: run.id },
                    Err(err) => agent24_scheduler::FireOutcome::Failed {
                        reason: err.to_string(),
                    },
                }
            }
            agent24_scheduler::InvocationTarget::Module { owner, fire_id } => {
                self.deliverer
                    .deliver(
                        owner,
                        fire_id,
                        invocation.trigger.as_str(),
                        &agent24_scheduler::next_fire::fmt_iso(invocation.scheduled_for),
                        &agent24_scheduler::next_fire::fmt_iso(invocation.fired_at),
                    )
                    .await
            }
        }
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

/// The logical user a local daemon's memory belongs to.
///
/// Agent24 is one person's 24/7 agent, so there is exactly one — and inventing an
/// identity system to have a nicer-looking constant would be worse than naming
/// the assumption. It matters here only because it is one half of a module's
/// memory partition key, and because that key is VERSIONED: when real user
/// identity arrives it comes from the kernel, and the catalog in
/// [`crate::os_memory`] is what lets the existing partitions be reattributed
/// rather than guessed at.
const LOCAL_USER: &str = "local";

/// Open the memory base — the KV store the D1 session memory and the domain-OS
/// memory partitions both live in. Returns `None` (memory off) if it cannot be
/// opened: a degraded daemon is better than one that won't start.
///
/// ONE store, handed to both. `KvStore` is `Clone` over a shared pool, so this is
/// one database and one connection pool rather than two competing for the same
/// file — and the two uses are separated by their owner keys, not by their
/// handles (see [`crate::os_memory`]).
async fn open_memory_base(ephemeral: bool) -> Option<agent24_memory::KvStore> {
    let kv = if ephemeral {
        agent24_memory::KvStore::open_memory().await
    } else {
        let dir = agent24_protocol::state_file::state_dir()?;
        agent24_memory::KvStore::open(&dir.join("memory.db")).await
    };
    match kv {
        Ok(kv) => Some(kv),
        Err(err) => {
            tracing::warn!(
                "memory base unavailable ({err}); sessions will not remember and no \
                 domain OS will be lent memory"
            );
            None
        }
    }
}

/// Pair the memory base with a router-backed summarizer for D1 session memory.
fn session_memory(
    kv: agent24_memory::KvStore,
    router: &Arc<ModelRouter>,
    shutdown: &CancellationToken,
) -> agent24_agent::SessionMemory {
    agent24_agent::SessionMemory::new(
        kv,
        StdArc::new(agent24_agent::RouterSummarizer::new(
            Arc::clone(router),
            shutdown.clone(),
        )),
    )
}

/// Everything [`AppState::new`] needs. The guardian and session memory are
/// INJECTED rather than read from env inside the constructor, so tests can wire
/// stubs; `serve` supplies the env-driven values.
pub struct AppDeps {
    pub token: String,
    pub router: Arc<ModelRouter>,
    pub tools: agent24_tools::ToolRegistry,
    pub store: Store,
    pub shutdown: Shutdown,
    pub guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    pub memory: Option<agent24_agent::SessionMemory>,
    pub mcp_servers: Vec<Arc<agent24_mcp::McpServer>>,
    /// Pre-loaded user overrides (H2). Injected rather than loaded here so
    /// tests can wire an empty or hand-built set.
    pub risk_overrides: StdArc<agent24_policy::overrides::RiskOverrideStore>,
    /// Where installed packages live (T8/ME-3g) — see [`AppState::packages_root`].
    /// Injected because `serve` already computes it before it needs anything
    /// else `AppState::new` builds; a test must supply a real temp directory,
    /// never an empty path pretending to be one.
    pub packages_root: Arc<std::path::PathBuf>,
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
            packages_root,
        } = deps;
        // ME4-desktop-model-ui: spawned here (not in `serve()`) so every
        // `AppState` — including the one every unit test builds via this
        // same `new()` — gets a real (if test-scale) writer task, and
        // `/api/v1/chat` (routes.rs) can reach it as `state.timings` exactly
        // like `state.events`/`state.usage`.
        let (timings, _timing_writer_handle) =
            crate::timing_recorder::TimingRecorder::spawn(store.clone());
        let events = crate::events::EventsHub::default();
        // ME4-desktop-model-ui follow-up (review M2): a passive observer on
        // the WS bus, recording through the SAME `timings` sink/writer as
        // `_a24/model/complete`/`/api/v1/chat` — see `agentear_timings.rs`'s
        // doc comment for why this is NOT wired through the RPC path.
        let _agentear_timing_bridge_handle =
            crate::agentear_timings::spawn_agentear_timing_bridge(events.clone(), timings.clone());
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
            shutdown.token().clone(),
            memory,
        );
        let sched_hub = events.clone();
        let deliverer = StdArc::new(crate::scheduler_deliver::ModuleDeliverer::new(
            crate::scheduler_deliver::PRODUCTION_LIMITS,
        ));
        let scheduler = agent24_scheduler::Scheduler::new(
            store.clone(),
            StdArc::new(KernelTrigger {
                runs: Arc::clone(&runs),
                deliverer: StdArc::clone(&deliverer),
            }),
            StdArc::new(move |body| sched_hub.broadcast(body)),
        );
        // T7b/ME-3e: a PARALLEL broker, its own table, its own async
        // submit-then-poll model — deliberately not built from `broker`
        // above (design doc "现状" 4).
        let module_approval_broker =
            crate::module_approval_broker::ModuleApprovalBroker::new(store.clone(), events.clone());
        let attach_registry = Arc::new(crate::attach_registry::AttachRegistry::new(
            crate::attach_registry::AttachDeps {
                scheduler: scheduler.clone(),
                // No `ModelCallbackDeps` exists yet at this point (it needs
                // the `UsageRecorder` `serve` spawns later) — `serve` builds
                // the real registry once that exists and replaces this one,
                // hydrated from `attached.json` (see this field's own doc on
                // `AppState`).
                models: None,
                approval_broker: module_approval_broker.clone(),
                events: events.clone(),
            },
        ));
        Self {
            risk_overrides,
            token: Arc::new(token),
            mcp_servers: Arc::new(mcp_servers),
            router,
            tools,
            broker,
            module_approval_broker,
            usage: Arc::new(crate::routes::UsageCounters::default()),
            events,
            timings,
            store,
            // Empty until `serve` replaces it after the mount pass, which happens
            // before the router (and therefore any request handler) can clone this
            // state. Note that a clone taken EARLIER keeps the empty Arc forever —
            // replacing the original does not reach clones — which is why the
            // assignment is ordered ahead of router construction rather than left
            // to chance.
            os_reports: Arc::new(Vec::new()),
            packages_root,
            // Empty for the same reason and until the same moment as `os_reports`
            // above — see that field's comment.
            package_dirs: Arc::new(std::collections::HashMap::new()),
            module_status: Arc::new(std::collections::HashMap::new()),
            supervisors: None,
            os_control: Arc::new(tokio::sync::Mutex::new(())),
            shutdown_report: Arc::new(crate::lifecycle::report(
                &crate::lifecycle::Params::default(),
                &[],
                None,
            )),
            runs,
            scheduler,
            deliverer,
            shutdown,
            attach_registry,
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

pub use agent24_domain::http::{error_response, error_response_with_hint};

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
    state.shutdown.request();
    tracing::info!("shutdown requested via /api/v1/shutdown");
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// `GET /api/v1/shutdown` (SHUT-1c): the shutdown budgets in effect, what
/// was warned about them, and the daemon before this one.
async fn shutdown_report(State(state): State<AppState>) -> Json<agent24_protocol::ShutdownReport> {
    Json((*state.shutdown_report).clone())
}

async fn fallback() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route")
}

/// Bearer-token gate for everything except `GET /api/v1/health`
/// (SPEC-002 §4: health is the only unauthenticated endpoint — method
/// included, so a future POST on the same path never silently bypasses auth).
///
/// # If you are adding a kernel-private header, name it `X-A24-*`
///
/// An out-of-process module is reached through
/// [`agent24_os_proto::proxy`](../../../crates/agent24-os-proto/src/proxy.rs),
/// which strips every `X-A24-*` header in both directions by PREFIX, and strips
/// `Authorization` / `Cookie` from a hand-written list. The prefix rule keeps up
/// on its own; the list does not. A kernel-private header introduced here under
/// any other name reaches modules verbatim, and nothing will report it.
///
/// This note lives beside the auth middleware rather than beside the proxy
/// because the person adding such a header is reading this file
/// (SPEC-ME3-OUT-OF-PROCESS §2.1).
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
        .route("/api/v1/timings", get(crate::routes::get_timings))
        .route(
            "/api/v1/timings/summary",
            get(crate::routes::get_timings_summary),
        )
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
        // T7b/ME-3e: module (gate/advise) approvals — a parallel surface to
        // `/api/v1/approvals` above, see `crate::module_approvals`.
        .route(
            "/api/v1/module-approvals",
            get(crate::module_approvals::list_module_approvals),
        )
        .route(
            "/api/v1/module-approvals/{id}",
            get(crate::module_approvals::get_module_approval)
                .post(crate::module_approvals::decide_module_approval),
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
        .route(
            "/api/v1/schedules/{id}/suspend",
            axum::routing::post(crate::schedules::suspend_schedule),
        )
        .route(
            "/api/v1/schedules/{id}/resume",
            axum::routing::post(crate::schedules::resume_schedule),
        )
        .route("/api/v1/events", get(crate::events::ws_events))
        // A3-2a (`docs/design/A3-ATTACHED-MODULE.md` §3.2): register/rotate,
        // list and revoke attached modules. `attached` is reserved in
        // `RESERVED_KERNEL_SEGMENTS` (domain.rs) precisely because it is a
        // literal `/api/v1/` segment here — see that constant's own doc and
        // `reserved_segments_match_the_kernel_routes_exactly`.
        .route(
            "/api/v1/attached",
            post(crate::attached_routes::post_attached).get(crate::attached_routes::list_attached),
        )
        .route(
            "/api/v1/attached/{name}",
            axum::routing::delete(crate::attached_routes::delete_attached)
                .patch(crate::attached_routes::patch_attached),
        )
        // Domain-OS registry (ME-2b). The daemon owns `os.json`; see `os_routes`.
        .route("/api/v1/os", get(crate::os_routes::list_os))
        .route(
            "/api/v1/os/{name}",
            axum::routing::patch(crate::os_routes::patch_os),
        )
        // FU-61: transient stop, does not touch os.json (unlike the PATCH
        // above) — used by `agent24 os uninstall`'s hot-disable step.
        .route(
            "/api/v1/os/{name}/stop",
            axum::routing::post(crate::os_routes::stop_now_os),
        )
        // A3-3 (`docs/design/A3-ATTACHED-MODULE.md` §6): reverse commands to
        // an attached module, on the SAME connection it registered over —
        // see `crate::attach_commands`. Attached modules never mount routes
        // of their own (§2's table: "入站 REST 反代...无"), so this literal
        // segment can never collide with a module's own surface.
        .route(
            "/api/v1/os/{name}/commands/{command}",
            axum::routing::post(crate::attach_commands::post_command),
        )
        .route(
            "/api/v1/shutdown",
            axum::routing::post(shutdown_handler).get(shutdown_report),
        )
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

/// A3-2b / Codex A3 follow-up (design §5.5): fills `cell` with `registry`,
/// then immediately re-checks `cancel`, revoking every live generation if a
/// shutdown raced ahead of the fill.
///
/// `serve`'s `stopping` task checks `cell` (a `OnceLock<Arc<AttachRegistry>>`)
/// exactly ONCE, the instant shutdown is requested, and only calls
/// `revoke_all()` if it finds the cell already filled. If shutdown instead
/// races ahead of the fill — `stopping`'s single check runs, finds the cell
/// empty, and gives up — nothing else would ever call `revoke_all()` for
/// that daemon: the fill that follows a moment later would be the LAST thing
/// to ever touch the registry, leaving its live generations/tokens (none
/// yet, this early, but hydration just before the fill may already have
/// populated `entries` with disk-registered modules) "live" forever, and the
/// attach listener started just after would keep accepting/handshaking
/// connections for a daemon that is meant to be going away. Re-checking
/// `cancel` immediately after the fill catches exactly that ordering.
/// `revoke_all()` is idempotent (see its own doc), so calling it here is a
/// harmless no-op on the OTHER ordering, where `stopping`'s own check
/// already found the cell filled and revoked everything itself — between the
/// two checks, every interleaving of "shutdown requested" vs "cell filled"
/// is covered by exactly one of them.
///
/// A free function — not inlined into `serve` — so the two
/// `stopping`-task-races-the-fill regression tests in this file's `tests`
/// module call this SAME code, instead of re-describing the logic next to
/// it (a reverse mutation removing the re-check below must turn those tests
/// red; see their doc comments).
fn fill_attach_registry_and_recheck(
    cell: &std::sync::OnceLock<Arc<crate::attach_registry::AttachRegistry>>,
    registry: &Arc<crate::attach_registry::AttachRegistry>,
    cancel: &CancellationToken,
) {
    let _ = cell.set(Arc::clone(registry));
    if cancel.is_cancelled() {
        registry.revoke_all();
    }
}

pub async fn serve(
    port: u16,
    ephemeral: bool,
    cancel: CancellationToken,
) -> Result<(), std::io::Error> {
    // The shutdown controller, and the signals that request it, before
    // anything else: a SIGTERM during startup — which can take seconds (the
    // store, MCP servers, model probing) — runs this bounded shutdown rather
    // than the default action, which ends the daemon as signal-killed, a crash
    // to a supervisor such as launchd (review of SUP-4, round 6).
    // The modules' budgets, read first — no I/O, and a bad value is only a
    // warning (SHUT-1b): the shutdown controller's deadlines follow them.
    let (params, config_warnings) = crate::lifecycle::Params::from_process_env();
    let mut config_warnings = config_warnings;
    let shutdown = Shutdown::with_params(cancel.clone(), params);
    // Signal handling: SIGTERM (process managers) + SIGINT (Ctrl+C in dev).
    // Registered HERE — before any module process can be started — and
    // synchronously: a SIGTERM arriving while packages start must run this
    // shutdown, which stops them, not the default action, which ends the daemon
    // and leaves them running (review of SUP-4, round 1).
    // A registration that fails is fatal HERE, before any module runs: a
    // daemon that cannot hear SIGTERM cannot stop its modules when asked to
    // (review of SUP-4, round 2).
    #[cfg(unix)]
    let (mut sigterm, mut sigint) = {
        use tokio::signal::unix::{SignalKind, signal};
        (
            signal(SignalKind::terminate())?,
            signal(SignalKind::interrupt())?,
        )
    };
    // The one shutdown deadline. A signal fixes it BEFORE it cancels, so it is
    // the moment of the signal; any other way shutdown starts (an HTTP
    // shutdown, the server ending) fixes it at the first look after the cancel
    // — a scheduling delay later, which the watchdog below bounds (review of
    // SUP-4, round 3).
    let signal_shutdown = shutdown.clone();
    tokio::spawn(async move {
        #[cfg(unix)]
        tokio::select! {
            _ = sigterm.recv() => {},
            _ = sigint.recv() => {},
        }
        #[cfg(not(unix))]
        if let Err(err) = tokio::signal::ctrl_c().await {
            tracing::error!("SIGINT handler failed: {err}");
            std::future::pending::<()>().await;
        }
        // The request first; the log line last — a stalled stderr must not be
        // what keeps the shutdown from starting.
        signal_shutdown.request();
        tracing::info!("shutdown signal received");
    });
    // Logged only now, with the signals registered: a stalled stderr must not
    // widen the window in which a SIGTERM takes the default action (review of
    // SHUT-1b, round 1).
    for warning in &config_warnings {
        tracing::warn!("{warning}");
    }
    tracing::info!(
        drain_ms = params.drain.as_millis(),
        stop_grace_ms = params.stop_grace.as_millis(),
        exit_bound_ms = params.exit_bound().as_millis(),
        "shutdown budgets for out-of-process modules; a SIGTERM ends this daemon within {}ms \
         (2000ms at the defaults)",
        params.exit_bound().as_millis()
    );
    // For a token cancelled other than through `Shutdown::request` (a child
    // token's owner, say): the watchdog is armed at the first look after it.
    {
        let observed = shutdown.clone();
        tokio::spawn(async move {
            observed.token().cancelled().await;
            observed.request();
        });
    }

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
    // SHUT-1b: what the previous daemon left, then this one's marker — right
    // after the lock, before the shutdown's task exists, so a shutdown always
    // finds it. A start-up that fails from here drops the guard, which removes
    // it: a start that failed is not a shutdown that did not finish. (A
    // shutdown requested before this point — during the lock — can still be
    // ended by the watchdog before the marker exists; that daemon never got
    // as far as starting anything, and leaves no evidence.) A marker that
    // cannot be written still gets its run a summary; the warning is kept.
    // Ephemeral daemons neither read nor write either file.
    let mut evidence = None;
    let marker = if ephemeral {
        None
    } else {
        agent24_protocol::state_file::state_dir().map(|state| {
            let run = crate::lifecycle::run_dir(&state);
            // FU-67: this process now holds the singleton lock, so any temp
            // file a prior life of this same daemon left behind is safe to
            // sweep here — but a predecessor's shutdown `persist` can still
            // be mid-write for a while after it dropped the lock (its own
            // timeout, worst case tens of seconds under a raised budget), so
            // the sweep itself age-gates what it touches; see
            // `lifecycle::ORPHAN_MIN_AGE`, not "nothing else can be writing
            // one" — that was never quite true.
            crate::lifecycle::sweep_orphaned_temp_files(&run);
            let (previous, last) = crate::lifecycle::evidence(&run);
            if let Some(warning) = previous.warning(&run) {
                tracing::warn!("{warning}");
            }
            evidence = Some((run.clone(), previous, last));
            let (guard, warning) = crate::lifecycle::MarkerGuard::create(&run);
            if let Some(warning) = warning {
                tracing::warn!("{warning}");
                config_warnings.push(warning);
            }
            guard
        })
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
    let memory_base = open_memory_base(ephemeral).await;
    let memory = memory_base
        .clone()
        .map(|kv| session_memory(kv, &router, &cancel));

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
    let report = crate::lifecycle::report(
        &params,
        &config_warnings,
        evidence
            .as_ref()
            .map(|(dir, previous, last)| (dir.as_path(), previous, last.clone())),
    );
    // T8/ME-3g: moved ahead of `AppState::new` (it used to be computed much
    // later, alongside the catalogue) so it can be injected through `AppDeps`
    // the same way every other dependency is — neither of these two lines
    // needs anything `AppState::new` builds, so there is no ordering hazard in
    // moving them here; the `catalogue`/`with_discovered` call below reuses
    // this same `packages_root`, not a second one.
    let state_dir = agent24_protocol::state_file::state_dir()
        .ok_or_else(|| std::io::Error::other("HOME not set"))?;
    let packages_root = Arc::new(agent24_os_packages::packages_root(&state_dir, ephemeral));
    let mut state = AppState::new(AppDeps {
        token: token.clone(),
        router,
        tools,
        store,
        risk_overrides,
        shutdown: shutdown.clone(),
        guardian,
        memory,
        mcp_servers,
        packages_root: Arc::clone(&packages_root),
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

    // ME4-1.3.1 (design §3.2/§4.6, S1-6): the scheduler's tick loop AND
    // delivery pump are spawned AFTER `mount_all` returns, below — not here.
    // Before `mount_all` there is no `ProcessHost`/`Supervisors` for a module
    // fire to be delivered into, and no `InstalledOwners` catalogue for the
    // tick to gate delivery-row recording on; starting either loop first
    // would let a tick land on a module row before either exists.

    // T7b/ME-3e: the periodic module-approval timeout scan (design doc
    // decision 5) — a plain periodic task, not a per-row timer, on the same
    // `CancellationToken`/`tokio::spawn` pattern as the scheduler above.
    state
        .module_approval_broker
        .spawn_scan(cancel.child_token());

    // Domain OSes (ME-1b-b). THIS is the one place in the kernel that may name a
    // module: someone has to say which OS is installed, and a composition root
    // naming its components is not the coupling ADR-029 objects to. Everything
    // downstream — routing, the data directory, the event module, the capability
    // grant — is derived from the manifest, so `crate::domain` and
    // `build_router_with_modules` still contain no module-shaped branch. An OS is
    // another entry in the CATALOGUE below — each with its own builder, which the
    // mounter calls only if that module is admissible and enabled, so one that
    // fails to construct cannot stop the others.
    //
    // T11: this build compiles in no domain OS at all — Sin90 (the one that used
    // to live here as `agent24-sin90-os`) now ships from `iDoris-ai/Sin90` as an
    // out-of-process package, discovered below by `with_discovered` like any
    // other third-party OS, not hardcoded into this catalogue. A future
    // compiled-in OS is still just another entry in this `Vec`.
    let catalogue: Vec<crate::domain::Installed> = Vec::new();

    // ME-3a: the catalogue is no longer only what was compiled in. The merge is a
    // free function so it can be tested without standing up a daemon — see
    // `with_discovered`. `packages_root` was computed earlier, before
    // `AppState::new`, and injected there too — this reuses that same value.
    let catalogue = with_discovered(catalogue, &packages_root);

    let os_config_path =
        crate::os_config::config_path().ok_or_else(|| std::io::Error::other("HOME not set"))?;
    let os_config = crate::os_config::OsConfig::load(&os_config_path);
    if let Err(why) = &os_config {
        tracing::error!("os.json could not be read ({why}); every admissible domain OS will 503");
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
    // A model declaration lives in a manifest, and reading a manifest means
    // constructing the module — which is exactly what the catalogue exists to
    // avoid. So the probe is skipped: this build compiles in no domain OS at all
    // (T11), and paying a multi-provider network sweep at every startup to
    // discover that would be worse than the `Unknown` it would avoid.
    //
    // This is a CONSTANT, not a predicate, and deliberately so — writing a
    // predicate over data the catalogue does not carry would look like a check
    // while always answering the same thing. When a module that needs models
    // arrives, `Installed` gains a `requires_models` field and this becomes a real
    // predicate over it.
    // Packages carry their manifest already (it was read at discovery), so a
    // package that declares a model can be known without constructing
    // anything: probe if any does. Compiled-in modules still cannot say without
    // being built, and none in this build declares one.
    let needs_models = catalogue.iter().any(|e| {
        matches!(&e.build, crate::domain::Build::Package(p) if !p.manifest.requires_models().is_empty())
            && os_config.as_ref().is_ok_and(|c| c.is_enabled(&e.name))
    });
    let inventory = if needs_models {
        ModelCatalog::probe(&state.router, &cancel).await
    } else {
        ModelCatalog::skipped()
    };
    // A module gets memory only if the base actually opened. No base, no lease,
    // no handle — rather than a handle that fails on every call.
    let lease = match memory_base {
        Some(kv) => crate::domain::MemoryLease::open(LOCAL_USER, kv).await,
        None => None,
    };
    // SUP-4: what out-of-process modules are started with. Its callback
    // sockets live under the state directory — or, for an ephemeral daemon,
    // under its throwaway root. Directories left by daemons that are gone are
    // cleared first (FU-56). A daemon that cannot set this up still runs; its
    // packages degrade, with the reason.
    // 127.0.0.1 only — never a public bind (SPEC-001 §9). Bound BEFORE any
    // module is started: a port that is taken fails startup here, rather than
    // after packages are running, on a path that would leave them to the
    // runtime's teardown instead of the ordered stop (review of SUP-4, round 4).
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let local = listener.local_addr()?;

    let host = process_host(
        if ephemeral { &os_root } else { &state_dir },
        params.stop_grace,
    );
    if let Err(why) = &host {
        tracing::error!("out-of-process domain OS modules cannot be started: {why}");
    }
    // Modules are owned by the shutdown from the moment each starts — this
    // task exists before the first one does — so a shutdown that begins while
    // later packages are still being mounted stops the earlier ones, within
    // the same bound (review of SUP-4, round 3). It drains and then stops
    // them, alongside the HTTP drain, by the modules' deadline (TASKS B2): a
    // module still stopping at the deadline is dropped with its supervisor,
    // which SIGKILLs its group — and a module reads EOF on its callback
    // connection as the end of its run (D1), so one that outlives this
    // process exits on its own.
    let registry = host.as_ref().ok().map(|h| h.supervisors.clone());
    let stop_shutdown = shutdown.clone();
    // Handed over here, synchronously, before the task exists: from now on
    // only a summary on disk removes the marker (SHUT-1b).
    let marker = marker.map(crate::lifecycle::MarkerGuard::into_stopping);
    // A3-2b (design §5.5, M2): the REAL attach registry (with model deps)
    // does not exist yet at this point in `serve` — it needs the
    // `UsageRecorder` built further down. This cell is filled in once it is,
    // right before the attach listener starts; the `stopping` task below only
    // ever reads it AFTER a shutdown was requested, which cannot happen
    // before that fill (a shutdown racing in during startup finds the cell
    // empty and has nothing to revoke — the listener that would let a module
    // attach has not started either in that same window).
    let attach_registry_cell: Arc<
        std::sync::OnceLock<Arc<crate::attach_registry::AttachRegistry>>,
    > = Arc::new(std::sync::OnceLock::new());
    let stopping_attach_registry = Arc::clone(&attach_registry_cell);
    let stopping = tokio::spawn(async move {
        stop_shutdown.token().cancelled().await;
        // Whoever cancelled, the shutdown has begun: fixed here if nothing
        // fixed it yet (SHUT-1b).
        stop_shutdown.request();
        // §5.5: revoke every attached module's live generation and drop its
        // grants (the last `ModelCallbackDeps` clone with it) in the SAME
        // phase the out-of-process supervisors' `close()` runs in, below —
        // and, transitively, before `stop_usage_writer` (every call site of
        // it awaits THIS task first).
        if let Some(registry) = stopping_attach_registry.get() {
            registry.revoke_all();
        }
        let deadlines = stop_shutdown.deadlines();
        // The discovery state goes first, off this task: a watchdog exit later
        // must not leave a state file pointing at a daemon that is gone. (The
        // singleton lock is held until the process exits, so no second daemon
        // can start in the meantime.)
        if !ephemeral {
            let pid = std::process::id();
            // A thread of its own rather than the blocking pool, which a
            // package-tree scan can have busy. Best effort all the same: on a
            // stalled filesystem nothing can promise it before the watchdog,
            // and a reader of a stale file still checks that its pid is alive.
            let _ = std::thread::Builder::new()
                .name("state-file-cleanup".to_owned())
                .spawn(move || agent24_protocol::state_file::remove_if_owner(pid));
        }
        let closed = registry.map(|r| r.close()).unwrap_or_default();
        // What each stop records, taken before the stops consume their
        // handles; read again at the modules' deadline (SHUT-1b).
        let tracked: Vec<_> = closed
            .running
            .iter()
            .map(|s| {
                (
                    s.name.clone(),
                    crate::lifecycle::Reason::Shutdown,
                    s.handle.stop_record(),
                )
            })
            .chain(closed.disabling.iter().map(|d| {
                (
                    d.name.clone(),
                    crate::lifecycle::Reason::Disable,
                    d.record.clone(),
                )
            }))
            .collect();
        let params = stop_shutdown.params();
        if tokio::time::timeout_at(deadlines.modules, stop_supervisors(closed, params.drain))
            .await
            .is_err()
        {
            tracing::warn!(
                "out-of-process modules were still stopping at the deadline; their supervisors \
                 were dropped (SIGKILL attempted, exit unconfirmed)"
            );
        }
        let records: Vec<_> = tracked
            .iter()
            .map(|(name, reason, record)| (name.clone(), *reason, record.snapshot()))
            .collect();
        let summary = crate::lifecycle::Summary::new(
            marker
                .as_ref()
                .map_or("ephemeral", crate::lifecycle::StoppingMarker::instance_id),
            &params,
            deadlines.began.elapsed(),
            &records,
        );
        // Every evidence line names its state directory (review of SHUT-1b,
        // round 3): two daemons with different HOMEs log side by side.
        let evidence = marker
            .as_ref()
            .map_or_else(|| "ephemeral".to_owned(), |m| m.dir().display().to_string());
        tracing::info!(evidence = %evidence, "{}", summary.describe());
        if let Some(marker) = marker {
            let job =
                tokio::task::spawn_blocking(move || crate::lifecycle::persist(&summary, &marker));
            match tokio::time::timeout_at(deadlines.persist, job).await {
                Ok(Ok(Ok(()))) => {}
                Ok(Ok(Err(e))) => tracing::error!(
                    "the shutdown summary could not be written in {evidence} ({e}); the next \
                     start will not be able to confirm this shutdown"
                ),
                Ok(Err(e)) => {
                    tracing::error!("writing the shutdown summary in {evidence} failed: {e}");
                }
                Err(_) => tracing::error!(
                    "the shutdown summary was not on disk in {evidence} by its deadline; the \
                     next start will not be able to confirm this shutdown"
                ),
            }
        }
    });
    // ME4-4.2.3b (design §6.3): the production usage sink. `hard_stop`
    // mirrors `modules_cut_off()`'s own shape — the shutdown token cancelled,
    // then the SAME `deadlines().modules` instant (cut-off + `CONFIRM`) —
    // rather than reusing `modules_cut_off()` itself: that future is a fresh
    // "began" read the FIRST time it's polled, and this recorder's hard stop
    // must line up with `stop_usage_writer`'s own `deadlines().modules` call
    // below, not with whenever this particular future happens to be polled.
    // `deadlines()` itself is idempotent (`Shutdown::began` is a `OnceLock`),
    // so both reads agree regardless.
    let usage_hard_stop = {
        let shutdown = shutdown.clone();
        async move {
            shutdown.token().cancelled().await;
            tokio::time::sleep_until(shutdown.deadlines().modules).await;
        }
    };
    let (usage_recorder, usage_writer) =
        crate::usage_recorder::UsageRecorder::spawn(state.store.clone(), usage_hard_stop);
    // A3-2b (design §5.5, M2): built ONCE here — not a second, structurally
    // equivalent copy — so the attach registry's model calls are cancelled by
    // the EXACT SAME `modules_cut_off()` cancellation tree a mounted
    // package's calls are ("同一棵树"), and so its `usage` sender is the same
    // `Arc` `stop_usage_writer` waits to see dropped. Cloned (not moved) into
    // `CallbackDeps` below — `ModelCallbackDeps` is `Clone` by design for
    // exactly this "more than one grantor needs the same deps" case.
    let model_deps = crate::model_callback::ModelCallbackDeps {
        router: state.router.clone(),
        usage: usage_recorder.clone(),
        cancel_root: crate::model_callback::spawn_cancel_root(shutdown.modules_cut_off()),
        admission: crate::model_callback::ModelAdmission::new(
            crate::model_callback::MODEL_MAX_IN_FLIGHT_GLOBAL,
            crate::model_callback::MODEL_MAX_IN_FLIGHT_PER_MODULE,
        ),
        events: state.events.clone(),
        timings: state.timings.clone(),
    };
    let (module_routes, reports, partitions) = crate::domain::mount_all(
        &catalogue,
        &os_root,
        &state.events,
        os_config.as_ref().map_err(String::as_str),
        &inventory,
        lease.as_ref(),
        host.as_ref().map_err(String::as_str),
        &state.module_approval_broker,
        crate::domain::CallbackDeps {
            scheduler: state.scheduler.clone(),
            // ME4-4.2.2b2 (design §2.4/§10.1): `router` is the SAME kernel
            // router `/api/v1/chat` uses — each grant takes its own
            // `with_separate_health()` view of it (v2 H2), never routing
            // through it directly. `cancel_root` fires at
            // `modules_cut_off()`, not at the start of shutdown (§3.3): a
            // module's in-flight inference lives exactly as long as its own
            // drain allows. ME4-4.2.3b: the usage sink is now the real
            // `UsageRecorder` — `serve` waits for its writer below, AFTER the
            // supervisors have stopped, via `stop_usage_writer`. Cloned (not
            // moved) here: `serve` keeps its own `usage_recorder` binding
            // alive to pass to `stop_usage_writer` later — that function
            // itself is what drops the LAST reference, right before it
            // awaits the writer's join (its own doc comment explains why
            // that ordering matters).
            models: Some(model_deps.clone()),
        },
    )
    .await;

    // ME4-1.3.1 (design §4.6): right after `mount_all` returns — before the
    // tick loop or the delivery pump ever run — set the two handles they and
    // `KernelTrigger`'s `Module` arm depend on.
    //
    // `InstalledOwners` gets every name `mount_all` was given (mounted,
    // disabled in os.json, refused — anything the catalogue discovered), NOT
    // only what mounted successfully (design v2, M5): a disabled entry is
    // still "installed" and its schedules must keep pre-advancing even though
    // no delivery row is recorded for them.
    state
        .scheduler
        .installed_owners()
        .set(catalogue.iter().map(|entry| entry.name.clone()).collect());
    // `host` being `Err` means this daemon cannot start out-of-process
    // modules at all (design §4.6, v2 L5): the deliverer's `OnceLock` is left
    // unset, so every module fire stays `Deferred(MountPending)` until its
    // 24h TTL — the tick loop below still starts unconditionally, so AgentRun
    // rows are unaffected.
    if let Ok(h) = &host {
        state.deliverer.set_supervisors(h.supervisors.clone());
    }

    // Scheduler tick loop: polls due schedules, pre-advances, and fires
    // AgentRun rows / records module deliveries. Cadence from
    // A24_SCHEDULER_TICK_SECS (default 10s; finest schedule granularity is a
    // minute, so a few seconds' latency is invisible).
    let tick_secs = std::env::var("A24_SCHEDULER_TICK_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|s| *s > 0)
        .unwrap_or(10);
    let tick_scheduler = Arc::clone(&state.scheduler);
    let tick_cancel = cancel.clone();
    tokio::spawn(tick_scheduler.run(
        StdArc::new(agent24_scheduler::SystemClock),
        Duration::from_secs(tick_secs),
        tick_cancel,
    ));
    // ME4-1.3.1 (design §5.4): the delivery pump — independent of the tick,
    // its own cadence, driven by the same real clock. Cancelled by the SAME
    // `CancellationToken` the tick loop uses: on shutdown, an attempt still in
    // flight is aborted (its `JoinSet` is dropped) rather than awaited, and
    // its delivery row is left `pending`/`deferred` for the next start
    // (design §4.6/§5.4, judgement C4.12).
    let pump = agent24_scheduler::deliveries::DeliveryPump::new(Arc::clone(&state.scheduler));
    let pump_cancel = cancel.clone();
    tokio::spawn(pump.run(StdArc::new(agent24_scheduler::SystemClock), pump_cancel));

    for p in partitions.partitions() {
        tracing::info!(
            "domain OS {} was lent a memory partition for user {}",
            p.module,
            p.user
        );
    }
    // From the DURABLE catalog, not from what mounted just now, and not by
    // matching keys — which is the whole reason the catalog exists. This number
    // deliberately includes partitions belonging to modules that are disabled or
    // no longer installed: those are the ones an operator would otherwise never
    // learn about, and the ones an export or erase path must not miss.
    if let Some(l) = lease.as_ref() {
        match crate::os_memory::OsMemoryCatalog::durable_for_org(&l.kv, &l.org).await {
            Ok(rows) if !rows.is_empty() => {
                // By physical KEY, not by module name. After a key-version change a
                // mounted module gets a NEW partition while its historical rows keep
                // the same module name — matching on the name would report those as
                // live and hide exactly the leftovers this log exists to surface.
                let live: std::collections::HashSet<&str> = partitions
                    .partitions()
                    .iter()
                    .map(|p| p.key.as_str())
                    .collect();
                let dormant = rows
                    .iter()
                    .filter(|r| !live.contains(r.owner_key.as_str()))
                    .count();
                // "not mounted now" describes the PARTITION, not the module. The
                // comment above already said why and the message used to
                // contradict it: after a re-key, the leftover row keeps the same
                // module name while that module is mounted perfectly well under
                // its new key, so calling those rows "modules not mounted" is
                // false exactly where an operator most needs the truth.
                tracing::info!(
                    "org {} owns {} domain-OS memory partition(s) besides {LOCAL_USER}'s \
                     own memory; {dormant} of them are not mounted under that key now \
                     (a disabled or uninstalled module, a rename, or an older key version)",
                    l.org.as_str(),
                    rows.len()
                );
            }
            Ok(_) => {}
            Err(e) => tracing::warn!("could not read the domain-OS memory catalog: {e}"),
        }
    }
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
    // Hand the verdicts to the state BEFORE the router clones it, so `/api/v1/os`
    // can report what the mounter actually decided rather than re-deriving it.
    state.os_reports = Arc::new(reports);
    // T8/ME-3g: same ordering reason as `os_reports` just above — `catalogue`
    // is still in scope (only borrowed by `mount_all`, never moved), so this is
    // a cheap O(n) derivation, not a second discovery pass.
    state.package_dirs = Arc::new(
        catalogue
            .iter()
            .filter_map(|entry| match &entry.build {
                crate::domain::Build::Package(package) => {
                    Some((entry.name.clone(), package.dir.clone()))
                }
                crate::domain::Build::InProcess(_) => None,
            })
            .collect(),
    );
    state.module_status = Arc::new(
        host.as_ref()
            .map(|h| h.supervisors.statuses())
            .unwrap_or_default(),
    );
    state.supervisors = host.as_ref().ok().map(|h| h.supervisors.clone());
    state.shutdown_report = Arc::new(report);
    // A3-2b (design §4/§5): the real attach registry — same "start empty,
    // replace once real data is ready" pattern as `os_reports` above, and for
    // the same ordering reason (this runs before `state` is cloned into the
    // router). Hydrated from `attached.json` BEFORE the listener takes its
    // first connection (`AttachRegistry::hydrate`'s own doc: a handshake
    // racing an unloaded name would otherwise see a spurious `auth_failed`).
    state.attach_registry = Arc::new(crate::attach_registry::AttachRegistry::new(
        crate::attach_registry::AttachDeps {
            scheduler: state.scheduler.clone(),
            models: Some(model_deps),
            approval_broker: state.module_approval_broker.clone(),
            events: state.events.clone(),
        },
    ));
    // Review H1: an EPHEMERAL daemon (`agent24 ...` without a resident
    // daemon, or an `os_local` helper spawning one just for one CLI call)
    // must NOT touch `~/.agent24/attach/agent24d.sock` at all — that path is
    // the REAL, resident daemon's. A short-lived daemon hydrating it and
    // binding the real socket would make the resident daemon's own listener
    // degrade (`bind`'s own "already accepting connections" check), or worse
    // — if the resident daemon is not up yet — actually WIN the bind, so a
    // real AgentEar connects to a process that exits moments later, silently
    // losing every event/usage row it would have recorded. Attach support is
    // simply not offered from an ephemeral daemon; hydration and the
    // listener are both skipped.
    if !ephemeral {
        if let Some(path) = crate::attached::config_path()
            && let Err(e) = state.attach_registry.hydrate(&path)
        {
            tracing::error!("could not hydrate the attach registry from {path:?}: {e}");
        }
        // Filled before the listener starts (see the cell's own comment
        // above): the `stopping` task's `revoke_all` can now find it. See
        // [`fill_attach_registry_and_recheck`]'s own doc comment for why the
        // fill is immediately followed by a re-check of `cancel`.
        fill_attach_registry_and_recheck(&attach_registry_cell, &state.attach_registry, &cancel);
        if let Some(path) = crate::attached::socket_path() {
            tokio::spawn(crate::attach_listener::run(
                Arc::clone(&state.attach_registry),
                path,
                shutdown.child_token(),
            ));
        } else {
            tracing::error!("attach listener not started: HOME is not set, no socket path to bind");
        }
    }
    let router = build_router_with_modules(state, module_routes);

    // A shutdown that began during startup ends it here, before anything says
    // this daemon is ready: its modules are stopped, and no state file or
    // ready line advertises a daemon that is about to exit (review of SUP-4,
    // round 4). Checked again, atomically, before the ready line below.
    if cancel.is_cancelled() {
        let _ = stopping.await;
        crate::usage_recorder::stop_usage_writer(
            usage_recorder,
            usage_writer,
            shutdown.deadlines().modules,
        )
        .await;
        return Ok(());
    }

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
                generation: String::new(),
                auth_mode: agent24_protocol::state_file::AuthMode::LegacySingleToken,
            })
    {
        tracing::warn!("could not write daemon state file: {err}");
    }

    // The state file is written; now readiness and a shutdown request race
    // for one atomic flag. Lost to a shutdown: take the file back — the
    // cleanup at the shutdown's start may have run before it existed — and
    // print no ready line.
    if !shutdown.commit_ready() {
        if !ephemeral {
            agent24_protocol::state_file::remove_if_owner(daemon_pid);
        }
        let _ = stopping.await;
        crate::usage_recorder::stop_usage_writer(
            usage_recorder,
            usage_writer,
            shutdown.deadlines().modules,
        )
        .await;
        return Ok(());
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

    let graceful_cancel = cancel.clone();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move { graceful_cancel.cancelled().await });

    // Force-exit backstop: once cancelled, in-flight requests get
    // the HTTP window (1.5s) to finish, then the process exits regardless.
    let result = tokio::select! {
        result = server => result,
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep_until(shutdown.deadline()).await;
        } => {
            tracing::warn!(
                "graceful shutdown exceeded {:?}; forcing exit",
                crate::lifecycle::HTTP_GRACE
            );
            Ok(())
        }
    };
    // The server can end without a cancel (an accept error); the modules stop
    // either way. The wait is bounded by the task itself.
    shutdown.request();
    let _ = stopping.await;
    // ME4-4.2.3b (design §6.3/v3.1 M-2, J19): AFTER the out-of-process
    // supervisors have stopped — every in-flight call's outcome (including
    // ones the cut-off itself cancelled) has by now either reached the
    // recorder's channel or never will — wait for the writer to drain it,
    // up to the SAME `deadlines().modules` instant its own hard stop uses.
    // Skipping this call, or not awaiting it, is exactly the bug J19 exists
    // to catch: a record queued but never written before the process exits.
    crate::usage_recorder::stop_usage_writer(
        usage_recorder,
        usage_writer,
        shutdown.deadlines().modules,
    )
    .await;
    // Only remove our own state file — a newer daemon may have replaced it
    if !ephemeral {
        agent24_protocol::state_file::remove_if_owner(daemon_pid);
    }
    result
}

/// FU-92 follow-up (security hardening): verify — or, if absent, create — the
/// top-level fallback directory [`callback_root`] names under the shared,
/// world-writable `/tmp`. Its name is a hash of `root`, which is predictable
/// (`root` is just `$HOME/.agent24` or similar), so any other local user could
/// pre-create it — or plant a symlink at that name pointing anywhere — before
/// this daemon ever starts, hijacking the callback socket that would otherwise
/// go under it (eavesdropping on / impersonating the module handshake).
///
/// This check has to happen HERE, before [`agent24_os_proto::endpoint::CallbackDir::create`]
/// touches `<fallback>/run`: that call's own directory setup uses
/// `std::fs::DirBuilder::create` with `recursive(true)`, which walks straight
/// through an existing symlink at an intermediate component (the same as
/// `mkdir -p` — it treats "resolves to a directory" as "already there") rather
/// than refusing it.
///
/// Fails closed: an existing entry that is not exactly a real directory, owned
/// by this process's own euid, mode exactly `0700` (no group/other bits, no
/// special bits) is refused with an actionable message — never silently
/// reused, never chmod'd/fixed in place (unlike `private_dir` in
/// `agent24-os-proto`, which tightens a loose directory of ours; here, on a
/// shared `/tmp`, "ours" itself cannot be trusted without the check), and
/// never retried at some OTHER path.
///
/// # Errors
///
/// A message naming the problem and telling the operator to remove the
/// offending path and restart.
fn secure_fallback_dir(path: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|e| format!("could not create {}: {e}", path.display())),
        Err(e) => Err(format!("could not check {}: {e}", path.display())),
        Ok(meta) if meta.file_type().is_symlink() => Err(format!(
            "{} is a symlink — another local user may have planted it to intercept this \
             daemon's callback socket; remove it and restart",
            path.display()
        )),
        Ok(meta) if !meta.is_dir() => Err(format!(
            "{} exists and is not a directory; remove it and restart",
            path.display()
        )),
        Ok(meta) => {
            let me = rustix::process::geteuid().as_raw();
            if meta.uid() != me {
                return Err(format!(
                    "{} is owned by uid {}, not this process's {me} — another local user may \
                     have created it to intercept this daemon's callback socket; remove it and \
                     restart",
                    path.display(),
                    meta.uid()
                ));
            }
            // All twelve mode bits, like `agent24-os-proto`'s `check_private`: no
            // group/other bits AND no sticky/setuid/setgid bit either.
            if meta.mode() & 0o7777 != 0o700 {
                return Err(format!(
                    "{} has mode {:04o}, not 0700 — another local user could read or replace \
                     what goes under it; remove it and restart",
                    path.display(),
                    meta.mode() & 0o7777
                ));
            }
            Ok(())
        }
    }
}

/// FU-92: `<root>/run/<pid>/<n>.sock` must fit macOS's 103-byte `sun_path`
/// limit ([`agent24_os_proto::endpoint::MAX_SOCKET_PATH`]). A long `$HOME` (or,
/// for an ephemeral daemon, a long `$TMPDIR`) otherwise fails every
/// out-of-process module's start — with a message that only said "too long",
/// not by how much or what to do about it.
///
/// Rather than lose the whole out-of-process subsystem over a HOME the user
/// did not pick for this reason, fall back to a short path under `/tmp`
/// (a literal `/tmp`, not `$TMPDIR`, which on macOS is itself often long)
/// when `root` would not fit — named by a hash of `root` so distinct state
/// directories, and repeated ephemeral runs, do not collide. Only the
/// transient callback-socket directory moves: `os.json`, `daemon.json`,
/// packages and memory all stay under the real `root`. The fallback
/// directory itself is checked by [`secure_fallback_dir`] before use — never
/// blindly reused — since its name is predictable on a shared `/tmp`.
///
/// Codex ME4-CODEX-DEBT-10 (#528 High): the hash used to name that directory
/// must be [`fallback_dir_hash`], NOT `std::collections::hash_map::DefaultHasher`
/// — its own doc explicitly does not promise the same algorithm across Rust
/// versions (nor even across runs of the same binary — it is randomly seeded
/// unless a `Hasher` is built directly, which this WAS doing, keeping the
/// algorithm fixed but not its stability guarantee), so a toolchain upgrade
/// could silently rename every existing user's fallback directory. That
/// rename is itself harmless (see [`fallback_dir_hash`]'s own doc for why),
/// but there is no reason to accept it when a fixed algorithm is one call
/// away.
///
/// # Errors
///
/// If `root` is too long AND its fallback directory is not safe to use (see
/// [`secure_fallback_dir`]). Does not retry at any other path.
fn callback_root(root: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let projected = root
        .join("run")
        .join(std::process::id().to_string())
        .join(format!("{}.sock", u64::MAX));
    if projected.as_os_str().len() <= agent24_os_proto::endpoint::MAX_SOCKET_PATH {
        return Ok(root.to_owned());
    }
    let fallback =
        std::path::PathBuf::from("/tmp").join(format!("a24-run-{:016x}", fallback_dir_hash(root)));
    secure_fallback_dir(&fallback).map_err(|why| {
        format!(
            "{} is too long for callback sockets (over the {}-byte macOS limit), and its \
             short-path fallback {} is not safe to use: {why}",
            root.display(),
            agent24_os_proto::endpoint::MAX_SOCKET_PATH,
            fallback.display(),
        )
    })?;
    tracing::warn!(
        "{} is too long for callback sockets (over the {}-byte macOS limit); using {} instead \
         for this run. To use {} directly, point Agent24 at a shorter data directory (e.g. a \
         shorter $HOME).",
        root.display(),
        agent24_os_proto::endpoint::MAX_SOCKET_PATH,
        fallback.display(),
        root.display(),
    );
    Ok(fallback)
}

/// A stable (across Rust versions, and across runs of the same binary — see
/// [`callback_root`]'s own doc for why `DefaultHasher` is not either) hash of
/// `root`, folded to 64 bits: the first 8 bytes of its sha256 digest, read as
/// a big-endian `u64`. Only used to NAME the `/tmp` fallback directory
/// [`callback_root`] picks when `root` itself is too long for a socket path
/// — this only has to be stable and collision-resistant enough that two
/// different `root`s (almost) never land on the same fallback name; it is
/// not a security boundary itself ([`secure_fallback_dir`] is what actually
/// keeps another local user out of a name they might guess or collide with).
///
/// A directory named by the OLD `DefaultHasher`-based scheme is simply
/// abandoned by a build using this function instead (its contents are the
/// transient callback-socket directory only — `run/<pid>/<n>.sock` files a
/// live daemon recreates from scratch on every start, per [`callback_root`]'s
/// own doc — so nothing is lost, and [`agent24_os_proto::endpoint::remove_stale`]
/// cleans up anything left behind under the old name once no process holds
/// it open).
#[must_use]
fn fallback_dir_hash(root: &std::path::Path) -> u64 {
    use sha2::Digest;
    let digest = sha2::Sha256::digest(root.as_os_str().as_encoded_bytes());
    let mut first8 = [0u8; 8];
    first8.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(first8)
}

/// What out-of-process modules are started with: the callback directory under
/// `root` (stale ones cleared first, FU-56; relocated by [`callback_root`] if
/// `root` itself is too long), this binary as the trampoline, and the
/// daemon's stop grace.
fn process_host(
    root: &std::path::Path,
    stop_grace: Duration,
) -> Result<crate::domain::ProcessHost, String> {
    let root = &callback_root(root)?;
    for gone in agent24_os_proto::endpoint::remove_stale(root) {
        tracing::info!(
            "removed the callback directory of a daemon that is gone: {}",
            gone.display()
        );
    }
    let callback_dir = agent24_os_proto::endpoint::CallbackDir::create(root)
        .map_err(|e| format!("callback directory: {e}"))?;
    // Checked now, for the longest name a socket there can get, rather than
    // failing every module's start — and then its restarts — one by one: a long
    // `TMPDIR` (an ephemeral daemon's root) can put every socket over the limit.
    // Should not trigger given `callback_root` above, but kept as a safety net
    // — e.g. if `/tmp` itself were ever remapped to something long.
    let longest = callback_dir.path().join(format!("{}.sock", u64::MAX));
    let longest_len = longest.as_os_str().len();
    if longest_len > agent24_os_proto::endpoint::MAX_SOCKET_PATH {
        return Err(format!(
            "callback sockets under {} would be {longest_len} bytes, over the {}-byte macOS \
             limit — point Agent24 at a shorter data directory (e.g. a shorter $HOME) and \
             restart",
            callback_dir.path().display(),
            agent24_os_proto::endpoint::MAX_SOCKET_PATH
        ));
    }
    // This binary, which hands a module over at the top of `main`
    // (`run_as_trampoline_if_asked`), so it needs no arguments of its own. By
    // path: a module restarted after the binary was replaced or moved runs the
    // new one, or fails to start — upgrading the daemon means restarting it.
    let program = std::env::current_exe().map_err(|e| format!("this binary's path: {e}"))?;
    Ok(crate::domain::ProcessHost {
        callback_dir: StdArc::new(callback_dir),
        trampoline: agent24_os_proto::launch::Trampoline {
            program,
            args: Vec::new(),
        },
        timings: agent24_os_proto::supervisor::Timings {
            stop_grace,
            ..agent24_os_proto::supervisor::Timings::default()
        },
        supervisors: StdArc::new(crate::domain::Supervisors::default()),
    })
}

/// Arm the hard bound, once: at `at` — the watchdog instant of `crate::lifecycle` — the process
/// ends, whatever is stuck — a module's directory on a stalled filesystem
/// during startup, a blocking task. A native thread, so no scheduling of the
/// runtime can delay it; an absolute instant, so arming it late does not move
/// it; no I/O when it fires, so a stalled stderr cannot hold it; and exit
/// status 0, because a shutdown was asked for — a supervisor such as launchd
/// must not read it as a crash and start the daemon again (review of SUP-4,
/// round 4). A thread the OS refuses to create ends the process at once,
/// rather than losing the shutdown (round 5). Modules it did not get to stop
/// read EOF on their callback connections, which ends their runs (D1).
fn arm_watchdog(at: tokio::time::Instant, armed: &std::sync::atomic::AtomicBool) {
    if armed
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }
    let at = at.into_std();
    let spawned = std::thread::Builder::new()
        .name("shutdown-watchdog".to_owned())
        .spawn(move || {
            let now = std::time::Instant::now();
            if at > now {
                std::thread::sleep(at - now);
            }
            std::process::exit(0);
        });
    if spawned.is_err() {
        std::process::exit(0);
    }
}

/// Stop every supervised module, the way SPEC §4 stops one, concurrently for
/// all: DRAINING first — new proxied requests refused, the ones in flight left
/// to finish, for up to `drain` — then REVOKING and the process stop.
/// Each supervisor does both itself (`drain_and_stop`), so a module that was
/// starting is stopped without ever serving and none is restarted into a
/// generation nobody drained (review of SUP-4, round 2). Logs any stop that
/// was not clean. The caller bounds the whole of it.
///
/// Draining first is what keeps a shutdown from answering a request a module
/// is in the middle of `request_abandoned` — and a client that then retries a
/// write the module did complete (review of SUP-4, round 1).
async fn stop_supervisors(closed: crate::domain::Closed, drain: Duration) {
    let mut stops = tokio::task::JoinSet::new();
    for s in closed.running {
        stops.spawn(async move { (s.name, s.handle.drain_and_stop(drain).await) });
    }
    while let Some(done) = stops.join_next().await {
        match done {
            Ok((_, Ok(()))) => {}
            Ok((name, Err(e))) => tracing::error!("domain OS {name:?} did not stop cleanly: {e}"),
            Err(e) => tracing::error!("stopping a domain OS failed: {e}"),
        }
    }
    // Stops `os disable` began: each ends by `Shutdown::modules_cut_off` at
    // the latest, with a SIGKILL of its module attempted by then — attempted,
    // not confirmed (SUP-5).
    for stop in closed.disabling {
        let _ = stop.task.await;
    }
}

/// Append packages found on disk to a build-time catalogue.
///
/// Everything the caller passes in is a build-time entry. Everything this adds
/// was found on disk AFTER the binary was built, which is the only shape that can
/// demonstrate "installing a third-party domain OS needs no kernel change" — a
/// mock appended to the `vec!` in `serve` would prove nothing, because reaching
/// that `vec!` means editing and rebuilding the daemon.
///
/// A discovered package is NOT constructed here, and cannot be: an out-of-process
/// module has no Rust type. It becomes a [`crate::domain::Build::Package`], which
/// the mounter starts under a supervisor — only if it is admissible and enabled —
/// and which appears in `agent24 os list` either way.
///
/// **Order is load-bearing.** Discovered entries go AFTER the built-in ones, and
/// `mount_all` claims names first-come-first-served, so a disk package cannot
/// shadow a compiled-in module by taking its name. The second claimant gets an
/// explicit refusal rather than silently winning.
fn with_discovered(
    mut catalogue: Vec<crate::domain::Installed>,
    packages_root: &std::path::Path,
) -> Vec<crate::domain::Installed> {
    // FU-41. Checked HERE, at the one place packages are read, rather than at
    // startup: a check somewhere else can be true when it runs and false when
    // the directory is used, and this is the moment of use.
    //
    // A refusal SKIPS discovery entirely and keeps the compiled-in catalogue.
    // That is the safe direction — a daemon with only its built-in modules still
    // works — and it is loud, because from ME-3b-3 a package in a directory
    // somebody else can write decides what this process executes.
    if let Err(e) = agent24_os_packages::ensure_packages_root(packages_root) {
        tracing::error!("{e}; no disk packages will be loaded this run");
        return catalogue;
    }
    let scan = agent24_os_packages::discovery::scan(packages_root);
    for r in &scan.refused {
        tracing::warn!(
            "domain OS package at {} was not loaded: {}",
            r.dir.display(),
            r.why
        );
    }
    for d in scan.found {
        let name = d.manifest.name().to_owned();
        let version = d.manifest.version().to_owned();
        tracing::info!(
            "discovered domain OS {name:?} v{version} at {}",
            d.dir.display()
        );
        catalogue.push(crate::domain::Installed {
            name,
            version,
            build: crate::domain::Build::Package(Box::new(crate::domain::Package {
                manifest: d.manifest,
                dir: d.dir,
                digest: d.digest,
            })),
        });
    }
    catalogue
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    // ---- ME4-4.2.2b2 H1 (Opus review round on top of `bb6fb0e`) -------------

    /// A real end-to-end daemon test
    /// (`apps/agent24d/tests/me4_model_shutdown_wiring.rs`) empirically
    /// CANNOT reliably pin this one line: `Shutdown::request()` also revokes
    /// the module's `Generation` as part of stopping it — design
    /// §3.3's OWN "代次撤销" row, pre-existing `os-proto` machinery that
    /// independently cancels an in-flight model call within tens of
    /// milliseconds of the SAME `spawn_cancel_root(shutdown.modules_cut_off())`
    /// cut-off. Measured with a real subprocess module (SIGTERM-ignoring, one
    /// proxied request kept in flight so its own drain doesn't finish early)
    /// and a real hung TCP provider: correct code closes the provider
    /// connection ~630–670ms after `POST /api/v1/shutdown`; wiring the cancel
    /// root to `CancellationToken::new()` (never fires) still closes it at
    /// ~670–760ms via the OTHER path — overlapping ranges, not a reliable
    /// red/green signal for CI. This structural test is the deterministic
    /// fallback the design's own review round asked for when the daemon-level
    /// test can't be made to pin it: it pins the EXACT call, so a mutation to
    /// either half — the constructor (`CancellationToken::new()` instead of
    /// `spawn_cancel_root`) or the argument (anything other than
    /// `shutdown.modules_cut_off()`, e.g. `shutdown.token().cancelled()` to
    /// fire at the START of shutdown instead of at cut-off) — turns it red.
    #[test]
    fn the_model_callback_cancel_root_is_spawned_from_modules_cut_off() {
        // Scoped to `serve()`'s OWN body — never to the whole file, which
        // would make this test tautological (the string above, in this
        // test's own source, would always make a whole-file `.contains`
        // pass regardless of what `serve()` actually does). Same technique
        // as `module_routes_are_behind_kernel_auth`'s sibling
        // `build_router_with_modules` scan below: find the function, cut at
        // the first column-zero `}` (every brace inside a rustfmt'd function
        // body is indented).
        let src = include_str!("server.rs");
        let start = src.find("pub async fn serve(").expect("serve must exist");
        let body = &src[start..];
        let end = body
            .find("\n}\n")
            .expect("function must be brace-terminated");
        let body = &body[..end];
        assert!(
            body.contains("spawn_cancel_root(shutdown.modules_cut_off())"),
            "serve() must build ModelCallbackDeps.cancel_root as \
             spawn_cancel_root(shutdown.modules_cut_off()) — design \
             docs/design/ME4-S2-model-callback.md §3.3"
        );
    }

    // ---- ME4-4.2.3b (design §6.3/v3.1 M-2, J19 variant 2a) -------------------

    /// J19's structural half: `stop_usage_writer` must be called from
    /// `serve()`'s MAIN shutdown path (the one every real shutdown takes)
    /// strictly AFTER `let _ = stopping.await;` — the point at which the
    /// out-of-process supervisors have finished stopping, so every in-flight
    /// call's outcome (including ones the cut-off cancelled) has already
    /// either reached the recorder's channel or never will. Calling it
    /// earlier would race the very cancellations it exists to wait out; not
    /// calling it at all is the bug this test exists to catch.
    ///
    /// Review, M1: two hardenings over the original version of this test —
    /// (a) `//`-comment lines are stripped from the scanned source FIRST, so
    /// commenting the call out (and `drop`-ping the values it would have
    /// consumed, to keep the function compiling) does not fool a plain
    /// substring search; (b) the match is the FULL call with its exact
    /// arguments, not just the bare function name, so swapping in a
    /// differently-shaped call (wrong argument, wrong order) also turns this
    /// red. Mutation verified: replacing the real call with
    /// `// crate::usage_recorder::stop_usage_writer(...)` plus
    /// `drop(usage_recorder); drop(usage_writer);` turns this red; reverting
    /// turns it green again.
    #[test]
    fn stop_usage_writer_is_called_after_the_supervisors_have_stopped() {
        let src = include_str!("server.rs");
        let start = src.find("pub async fn serve(").expect("serve must exist");
        let body = &src[start..];
        let end = body
            .find("\n}\n")
            .expect("function must be brace-terminated");
        let body = &body[..end];
        // Strip `//`-comments line by line (this scans ONLY `serve`'s own
        // source, which has no `//` inside a string literal near this area,
        // so a naive split is safe here) — a commented-out call must be
        // invisible to the search below.
        let code_only: String = body
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n");
        // Unique to the main shutdown path — the two early-return copies
        // (during startup) never call `shutdown.request()` right before
        // their own `stopping.await`.
        let stopping_at = code_only
            .find("shutdown.request();\n    let _ = stopping.await;")
            .expect("the main shutdown path must request, then wait for stopping");
        let after = &code_only[stopping_at..];
        // The exact rustfmt shape of the real call (verified against the
        // source at the time this test was written — a reformat that keeps
        // the same call would need this literal updated too, which is the
        // point: it is not just checking the function name).
        let exact_call = "crate::usage_recorder::stop_usage_writer(\n        usage_recorder,\n        \
                           usage_writer,\n        shutdown.deadlines().modules,\n    )\n    .await;";
        assert!(
            after.contains(exact_call),
            "serve()'s main shutdown path must call \
             crate::usage_recorder::stop_usage_writer(usage_recorder, usage_writer, \
             shutdown.deadlines().modules).await AFTER `let _ = stopping.await;` — design \
             docs/design/ME4-S2-model-callback.md §6.3/v3.1 M-2"
        );
    }

    // ---- ME-3a: the wiring itself, not just the scanner ---------------------

    /// T7b/ME-3e: a throwaway broker for `mount_all` tests that don't care
    /// about approval behavior — built on the SAME hub the test passes as
    /// `mount_all`'s `events`.
    async fn test_approval_broker(
        hub: &crate::events::EventsHub,
    ) -> std::sync::Arc<crate::module_approval_broker::ModuleApprovalBroker> {
        crate::module_approval_broker::ModuleApprovalBroker::new(
            agent24_store::Store::open_memory().await.unwrap(),
            hub.clone(),
        )
    }

    fn pkg(root: &std::path::Path, name: &str) {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(agent24_os_packages::discovery::MANIFEST_FILE),
            format!(
                "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
                 event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
                 kernel_capabilities: [events]\nimpl_kind: out_of_process_provider\n\
                 spawn:\n  command: bin/mod\n"
            ),
        )
        .unwrap();
    }

    /// Readiness and a shutdown request cannot both win: requested first, the
    /// daemon never says it is ready; ready first, the shutdown still happens
    /// (review of SUP-4, round 7).
    #[test]
    fn readiness_and_a_shutdown_request_decide_once() {
        let stopping_first = Shutdown::new(CancellationToken::new());
        stopping_first.token().cancel(); // no watchdog in a test process
        stopping_first
            .phase
            .store(STOPPING, std::sync::atomic::Ordering::SeqCst);
        assert!(
            !stopping_first.commit_ready(),
            "ready after a shutdown request"
        );

        let ready_first = Shutdown::new(CancellationToken::new());
        assert!(ready_first.commit_ready());
        assert!(!ready_first.commit_ready(), "ready twice");
        assert!(!ready_first.token().is_cancelled());
    }

    fn built_in(name: &str) -> crate::domain::Installed {
        crate::domain::Installed {
            name: name.to_owned(),
            version: "9.9.9".to_owned(),
            build: crate::domain::Build::InProcess(Box::new(|| {
                Err("built-in, not constructed in this test".to_owned())
            })),
        }
    }

    #[test]
    fn a_package_placed_after_the_build_reaches_the_catalogue() {
        // The scanner having found it is not the same claim as the daemon having
        // USED it. This is the wiring, which was previously only assertable by
        // reading `serve`.
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");

        let out = super::with_discovered(vec![built_in("sin90")], root.path());
        let names: Vec<&str> = out.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["sin90", "cos72"]);
    }

    /// FU-41's wiring. The library gained `ensure_packages_root`; a library
    /// function nobody calls closes nothing.
    ///
    /// The refusal must SKIP discovery and keep the built-ins — a daemon with
    /// only its compiled-in modules still works, while one that loaded a package
    /// from a directory anyone could write would be executing a program of their
    /// choosing (the manifest carries a `spawn` command as of ME-3b-3).
    #[test]
    fn a_world_writable_packages_root_is_not_read_at_all() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o777)).unwrap();

        let out = super::with_discovered(vec![built_in("sin90")], root.path());
        let names: Vec<&str> = out.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["sin90"],
            "a package in an unsafe root was loaded"
        );

        // Control: the SAME root with the SAME package, tightened, is read. So
        // the skip above is about the mode — not about the package being
        // unreadable for some other reason, which would make this test pass for
        // a reason that has nothing to do with FU-41.
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let out = super::with_discovered(vec![built_in("sin90")], root.path());
        let names: Vec<&str> = out.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["sin90", "cos72"]);
    }

    #[test]
    fn a_disk_package_cannot_shadow_a_built_in_module() {
        // Order is the whole mechanism: `mount_all` claims names first-come,
        // first-served, so a disk package named `sin90` must land AFTER the
        // compiled-in one and lose. If discovery ever moved ahead of the built-ins
        // — or the mounter's claim became last-wins — a dropped-in directory could
        // take over a kernel module's namespace silently.
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "sin90");

        let out = super::with_discovered(vec![built_in("sin90")], root.path());
        assert_eq!(out.len(), 2, "both entries exist; the mounter decides");
        assert_eq!(out[0].version, "9.9.9", "the built-in must come FIRST");
        assert_eq!(out[1].version, "0.1.0");
    }

    #[test]
    fn discovery_failing_does_not_empty_the_built_in_catalogue() {
        // A missing or unreadable packages root must not cost the user the modules
        // that were compiled in. "No third-party packages" is the normal case.
        let out = super::with_discovered(
            vec![built_in("sin90")],
            std::path::Path::new("/nonexistent/agent24-packages"),
        );
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].name, "sin90");
    }

    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// A disable's stop is cut off at a fixed instant after the shutdown
    /// began — the budget any module gets — however late its waiter is first
    /// polled (review of SUP-5, round 2). The token is cancelled directly:
    /// `request` would arm the watchdog, which ends this process.
    #[tokio::test(start_paused = true)]
    async fn a_disables_stop_is_cut_off_at_a_fixed_instant_after_the_shutdown_began() {
        let shutdown = Shutdown::new(CancellationToken::new());
        let began = tokio::time::Instant::now();
        let _ = shutdown.deadlines();
        shutdown.token().cancel();
        tokio::time::sleep(Duration::from_millis(250)).await;
        shutdown.modules_cut_off().await;
        let p = crate::lifecycle::Params::default();
        assert_eq!(began.elapsed(), p.drain + p.stop_grace);
    }

    /// The shutdown waits for the stops `os disable` began, not only for the
    /// modules it stops itself (SUP-5): those end when the shutdown's module
    /// budget cuts them short, and a daemon that exited first would leave the
    /// kill unsent.
    #[tokio::test]
    async fn the_shutdown_waits_for_the_stops_disables_began() {
        let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop = {
            let done = done.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(100)).await;
                done.store(true, std::sync::atomic::Ordering::SeqCst);
            })
        };
        stop_supervisors(
            crate::domain::Closed {
                running: Vec::new(),
                disabling: vec![crate::domain::Disabling {
                    name: "x".into(),
                    record: agent24_os_proto::stop_record::StopRecordHandle::default(),
                    task: stop,
                }],
            },
            Duration::ZERO,
        )
        .await;
        assert!(done.load(std::sync::atomic::Ordering::SeqCst));
    }

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

    async fn state_with_guardian(
        guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    ) -> AppState {
        AppState::new(AppDeps {
            token: "testtoken".to_owned(),
            router: Arc::new(ModelRouter::with_defaults(vec![])),
            tools: agent24_tools::ToolRegistry::new(),
            store: Store::open_memory().await.unwrap(),
            shutdown: Shutdown::new(CancellationToken::new()),
            guardian,
            memory: None,
            mcp_servers: Vec::new(),
            risk_overrides: StdArc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
                Vec::new(),
            )),
            // A real (if immediately-dropped) temp directory, not an empty
            // path standing in for one — T8/ME-3g's admission gate never
            // reaches for this unless a test explicitly populates
            // `package_dirs`, so nothing here actually gets read by default.
            packages_root: Arc::new(tempfile::tempdir().expect("tempdir").path().to_path_buf()),
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
        let entry = crate::domain::Installed {
            name: "probe".to_owned(),
            version: "0.1.0".to_owned(),
            build: crate::domain::Build::InProcess(Box::new(move || {
                Ok(m.clone() as StdArc<dyn DomainModule>)
            })),
        };
        let (modules, reports, _) = crate::domain::mount_all(
            &[entry],
            tmp.path(),
            &st.events,
            Ok(&crate::os_config::OsConfig::default()),
            &NoModels,
            None,
            Err("no process host in this test"),
            &test_approval_broker(&st.events).await,
            crate::domain::CallbackDeps {
                scheduler: st.scheduler.clone(),
                models: None,
            },
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

    /// An OUT-OF-PROCESS module is behind the kernel's auth too — and an
    /// unauthenticated caller does not even cause a connection to it.
    ///
    /// The 401 itself is already covered for in-process modules by
    /// [`module_routes_are_behind_kernel_auth`]; what is new here is that the
    /// thing being protected is a socket to another program. "401" and "the
    /// module never heard about it" are different claims: a proxy mounted as a
    /// FALLBACK, or one that dialled upstream before the layer ran, could answer
    /// 401 to the client while the module had already seen the request — and a
    /// module that logs or acts on what it receives is then reachable by anyone
    /// who can reach the port. So the assertion is on the upstream's connection
    /// count, not only on the status.
    ///
    /// The daemon does not mount a proxy in production yet: that is SUP-4,
    /// which starts a supervisor per module and hands its `Current` to
    /// `proxy::mount` — the address travels inside each run's `Generation`
    /// (SUP-3b), not through the mount. This pins the composition
    /// (`proxy::mount` + `build_router_with_modules`) before that consumer
    /// exists, which is the point at which the mount order is still cheap to
    /// get right.
    #[tokio::test]
    async fn a_proxied_module_is_behind_kernel_auth_and_is_not_even_dialled() {
        use std::sync::atomic::{AtomicUsize, Ordering};

        let connections = StdArc::new(AtomicUsize::new(0));
        // Pid plus a nanosecond timestamp: a pid-only name left a stale node
        // for a later run reusing this pid to collide with (review of
        // FU-60, round 1).
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let upstream =
            std::env::temp_dir().join(format!("a24-zzproxy-{}-{nanos}.sock", std::process::id()));
        let listener = tokio::net::UnixListener::bind(&upstream).unwrap();
        let counter = connections.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let _ = socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nmodule")
                    .await;
                let _ = socket.shutdown().await;
            }
        });

        let st = state().await;
        let token = st.token.to_string();
        let module = agent24_os_proto::drain::Generation::serving_at(upstream);
        assert!(module.ready());
        let modules = agent24_os_proto::proxy::mount(
            Router::new(),
            "/api/v1/zzproxy",
            agent24_os_proto::drain::Current::new(module),
        );
        let router = build_router_with_modules(st, modules);

        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/zzproxy/anything")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(res).await["error"]["code"], "unauthorized");
        assert_eq!(
            connections.load(Ordering::SeqCst),
            0,
            "the module was dialled for a request that had no token"
        );

        // The control. Without it, zero connections is also what a proxy
        // pointed at nothing produces, and the 401 proves nothing about the
        // mount order.
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/zzproxy/anything")
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(connections.load(Ordering::SeqCst), 1);
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

    // ── ME4-1.2.2b (top-level cut): KernelTrigger, review H1 ─────────────────

    fn kernel_trigger_for_tests(runs: Arc<agent24_agent::RunManager>) -> KernelTrigger {
        KernelTrigger {
            runs,
            // Unset `ModuleDeliverer` — every module fire this trigger sees
            // in these tests is `Deferred(MountPending)` (design §4.6), same
            // as before ME4-1.3.1 wired a real deliverer behind it.
            deliverer: Arc::new(crate::scheduler_deliver::ModuleDeliverer::new(
                crate::scheduler_deliver::PRODUCTION_LIMITS,
            )),
        }
    }

    async fn test_run_manager() -> Arc<agent24_agent::RunManager> {
        agent24_agent::RunManager::new(
            agent24_store::Store::open_memory().await.unwrap(),
            Arc::new(ModelRouter::with_defaults(vec![])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            CancellationToken::new(),
        )
    }

    fn module_invocation(
        schedule_id: &str,
        trigger: agent24_scheduler::FireTrigger,
    ) -> agent24_scheduler::ScheduleInvocation {
        let now = chrono::Utc::now();
        agent24_scheduler::ScheduleInvocation {
            schedule_id: schedule_id.to_owned(),
            scheduled_for: now,
            fired_at: now,
            trigger,
            target: agent24_scheduler::InvocationTarget::Module {
                owner: agent24_scheduler::ModuleScheduleKey {
                    owner_module: "mod-a".to_owned(),
                    module_key: "k".to_owned(),
                },
                fire_id: agent24_scheduler::FireId::derive(trigger, schedule_id, now),
            },
        }
    }

    /// Review H1: `KernelTrigger`'s `Module` arm is a stub until ME4-1.3.1
    /// wires a real `ModuleDeliverer` — every module target it is asked to
    /// fire must come back `Deferred(MountPending)`, never a failure (design
    /// §4.1: none of `DeferReason`'s variants are the module's fault, and a
    /// mis-wired daemon must never burn a module's failure budget before the
    /// real deliverer even exists).
    #[tokio::test]
    async fn kernel_trigger_module_arm_is_deferred_mount_pending() {
        let trigger = kernel_trigger_for_tests(test_run_manager().await);
        let invocation = module_invocation("sch_test", agent24_scheduler::FireTrigger::Tick);
        let outcome = agent24_scheduler::RunTrigger::trigger(&trigger, &invocation).await;
        assert_eq!(
            outcome,
            agent24_scheduler::FireOutcome::Deferred {
                reason: agent24_scheduler::DeferReason::MountPending
            }
        );
    }

    /// Review H1: the `AgentRun` arm's failure path — `RunManager` refusing
    /// the run (here: a `session_id` that was never created, which
    /// `start_run_with_schedule` rejects with `SessionNotFound` before
    /// anything else happens) — must classify as `FireOutcome::Failed`, not
    /// panic or silently swallow the error.
    #[tokio::test]
    async fn kernel_trigger_agent_run_failure_maps_to_failed() {
        let trigger = kernel_trigger_for_tests(test_run_manager().await);
        let now = chrono::Utc::now();
        let invocation = agent24_scheduler::ScheduleInvocation {
            schedule_id: "sch_test".to_owned(),
            scheduled_for: now,
            fired_at: now,
            trigger: agent24_scheduler::FireTrigger::Tick,
            target: agent24_scheduler::InvocationTarget::AgentRun(
                agent24_protocol::ScheduleAction::AgentRun {
                    prompt: "x".to_owned(),
                    session_id: Some("sess_nonexistent".to_owned()),
                    model_override: None,
                },
            ),
        };
        let outcome = agent24_scheduler::RunTrigger::trigger(&trigger, &invocation).await;
        match outcome {
            agent24_scheduler::FireOutcome::Failed { reason } => {
                assert!(
                    reason.contains("sess_nonexistent"),
                    "expected the SessionNotFound reason to name the missing session, got: {reason}"
                );
            }
            other => panic!("expected Failed for a rejected run, got {other:?}"),
        }
    }

    /// Positive control for the AgentRun arm's SUCCESS path (the failure
    /// test above only proves half of H1's classification): a run that
    /// `RunManager` actually accepts must come back `AgentRun{run_id}`.
    #[tokio::test]
    async fn kernel_trigger_agent_run_success_maps_to_agent_run() {
        let trigger = kernel_trigger_for_tests(test_run_manager().await);
        let now = chrono::Utc::now();
        let invocation = agent24_scheduler::ScheduleInvocation {
            schedule_id: "sch_test".to_owned(),
            scheduled_for: now,
            fired_at: now,
            trigger: agent24_scheduler::FireTrigger::Tick,
            target: agent24_scheduler::InvocationTarget::AgentRun(
                agent24_protocol::ScheduleAction::AgentRun {
                    prompt: "x".to_owned(),
                    session_id: None,
                    model_override: None,
                },
            ),
        };
        let outcome = agent24_scheduler::RunTrigger::trigger(&trigger, &invocation).await;
        match outcome {
            agent24_scheduler::FireOutcome::AgentRun { run_id } => {
                assert!(run_id.starts_with("run_"), "{run_id}");
            }
            other => panic!("expected AgentRun for an accepted run, got {other:?}"),
        }
    }

    // ---- FU-92 follow-up: secure_fallback_dir / callback_root hardening ----
    // The fallback directory `callback_root` may pick lives under the shared,
    // world-writable `/tmp` with a PREDICTABLE name (a hash of `root`) — so
    // another local user could race to plant something at that name before
    // this daemon starts. These tests are for `secure_fallback_dir` itself
    // (the pure filesystem check) and for its wiring into `callback_root`
    // (so a mutation that drops the call is caught, not just the helper in
    // isolation). The "normal case" — nothing planted, a real mount — is
    // covered end to end by `daemon_modules.rs`'s long-HOME blackbox test.

    /// Mirrors `callback_root`'s own hash formula, only so tests can find (and
    /// clean up before/after) the exact path it will pick for a given `root`
    /// — never asserted on for its OWN sake.
    fn hashed_fallback_path(root: &std::path::Path) -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp").join(format!("a24-run-{:016x}", fallback_dir_hash(root)))
    }

    /// Codex ME4-CODEX-DEBT-10 (#528 High): golden value locking
    /// [`fallback_dir_hash`] to sha256, not `std::collections::hash_map::DefaultHasher`
    /// (whose own doc does not promise this algorithm across Rust versions).
    /// A fixed input's fallback directory name must never silently change
    /// again — if this test ever needs updating, that is a deliberate,
    /// visible break, not a toolchain upgrade nobody noticed.
    #[test]
    fn fallback_dir_hash_is_a_stable_golden_value() {
        let root = std::path::Path::new("/home/test/very/long/path/for/golden/hash/test");
        assert_eq!(
            format!("{:016x}", fallback_dir_hash(root)),
            "2ffb6c3d38a488a3",
            "fallback_dir_hash's output for a fixed input must never change — if it did, \
             every existing user's fallback directory name would silently change with it"
        );
    }

    /// Nothing there yet: created fresh, exactly `0700`.
    #[test]
    fn secure_fallback_dir_creates_a_fresh_directory_0700() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        secure_fallback_dir(&path).expect("a fresh directory is fine");
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode()
            & 0o7777;
        assert_eq!(mode, 0o700, "{mode:o}");
    }

    /// A restart re-checking its OWN directory from a previous run (still
    /// `0700`, still ours) must not be refused — only an unsafe entry is.
    #[test]
    fn secure_fallback_dir_accepts_its_own_directory_again() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        secure_fallback_dir(&path).unwrap();
        secure_fallback_dir(&path).expect("our own 0700 directory is fine the second time");
    }

    /// A pre-existing directory at that name with looser permissions — as
    /// another local user racing the predictable name could leave — is
    /// refused, not silently tightened in place (unlike `agent24-os-proto`'s
    /// `private_dir`, which DOES tighten a loose directory of ours: on a
    /// shared `/tmp`, "ours" cannot be trusted here without the check first).
    #[test]
    fn secure_fallback_dir_refuses_a_preexisting_loose_directory() {
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        std::fs::create_dir(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = secure_fallback_dir(&path).expect_err("a 0755 directory must be refused");
        assert!(err.contains("0700"), "{err}");
    }

    /// A pre-existing symlink at that name — pointing anywhere, even nowhere
    /// that exists — is refused, never followed (which is exactly what
    /// `DirBuilder::create(..).recursive(true)` — used one layer down by
    /// `CallbackDir::create` — would otherwise walk straight through).
    #[test]
    fn secure_fallback_dir_refuses_a_preexisting_symlink() {
        let base = tempfile::tempdir().unwrap();
        let path = base.path().join("fallback");
        let elsewhere = base.path().join("elsewhere");
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        let err = secure_fallback_dir(&path).expect_err("a symlink must be refused");
        assert!(err.contains("symlink"), "{err}");
        assert!(
            !elsewhere.exists(),
            "the symlink target must never be created"
        );
    }

    /// Wiring check: `callback_root` itself must refuse an unsafe pre-existing
    /// fallback directory rather than silently reusing it or trying some
    /// other path (mutation check: dropping the `secure_fallback_dir` call
    /// from `callback_root` turns this green when it must be red).
    #[test]
    fn callback_root_refuses_an_unsafe_preexisting_fallback_directory() {
        // A root that does not exist on disk — `callback_root` never touches
        // `root` itself in the too-long branch, only hashes it — but long
        // enough that `<root>/run/<pid>/<u64::MAX>.sock` cannot fit, so the
        // `/tmp` fallback branch is the one under test. Unique to this test
        // so it cannot collide with the other `callback_root`/blackbox tests.
        let long_root = std::path::PathBuf::from(format!(
            "/callback-root-hardening-test-{}-{}",
            std::process::id(),
            "x".repeat(150)
        ));
        let fallback = hashed_fallback_path(&long_root);
        let _ = std::fs::remove_dir_all(&fallback);
        let _ = std::fs::remove_file(&fallback);

        let got = callback_root(&long_root).expect("a fresh fallback directory is fine");
        assert_eq!(got, fallback);

        // Replace the now-created directory with a symlink, as a racing local
        // user could have done between two runs of this daemon.
        std::fs::remove_dir_all(&fallback).unwrap();
        std::os::unix::fs::symlink("/nonexistent-elsewhere-for-this-test", &fallback).unwrap();
        let err = callback_root(&long_root).expect_err("a symlinked fallback must be refused");
        assert!(err.contains("symlink"), "{err}");

        let _ = std::fs::remove_file(&fallback);
    }

    // ── Review (Codex A3 follow-up, §5.5): the `attach_registry_cell` race ──
    //
    // `serve`'s `stopping` task checks `attach_registry_cell` (a
    // `OnceLock<Arc<AttachRegistry>>`) exactly ONCE, the instant shutdown is
    // requested, and only calls `revoke_all()` if it finds the cell already
    // filled. `serve` itself fills the cell and re-checks `cancel` right
    // after via [`fill_attach_registry_and_recheck`] — both tests below call
    // THAT function directly (not a re-implementation of it) against a REAL
    // `AttachRegistry` (via `state()`), driving it through both possible
    // interleavings with `stopping`'s own one-shot check. Driving the entire
    // `serve()` startup through this precise interleaving would need to win
    // an actual OS-scheduling race, which cannot be made deterministic for
    // CI; calling the extracted function directly is what makes a reverse
    // mutation of it (deleting the re-check) turn these tests red — see
    // each test's own doc comment.

    /// The bug: shutdown requested before the registry is ever filled into
    /// the cell. Without the fix, `registry.is_closed()` would stay `false`
    /// forever — the attach listener would keep accepting and handshaking
    /// connections for a daemon that is meant to be going away. Confirmed
    /// red under the reverse mutation (deleting the `if cancel.is_cancelled()
    /// { registry.revoke_all(); }` re-check inside
    /// `fill_attach_registry_and_recheck`): `registry.is_closed()` stayed
    /// `false` at the final assertion.
    #[tokio::test]
    async fn a_shutdown_that_races_ahead_of_the_registry_fill_is_still_revoked() {
        let state = state().await;
        let registry = Arc::clone(&state.attach_registry);
        let cancel = CancellationToken::new();
        let cell: std::sync::OnceLock<Arc<crate::attach_registry::AttachRegistry>> =
            std::sync::OnceLock::new();

        // `stopping`'s own one-shot check, landing before the fill: shutdown
        // is already requested, but the cell is still empty.
        cancel.cancel();
        if let Some(r) = cell.get() {
            r.revoke_all();
        }
        assert!(
            !registry.is_closed(),
            "sanity check on the test itself: the premature check must find nothing to revoke"
        );

        // The startup fill, immediately followed by the FIXED re-check —
        // the actual production function, not a copy of its body.
        fill_attach_registry_and_recheck(&cell, &registry, &cancel);

        assert!(
            registry.is_closed(),
            "a shutdown that raced ahead of the registry fill must still end up revoked"
        );
    }

    /// The ordinary ordering, as a control: the fill happens first, well
    /// before any shutdown, and `stopping`'s own check (not the re-check
    /// inside `fill_attach_registry_and_recheck`) is what revokes it. That
    /// re-check is then a harmless no-op — `revoke_all` is idempotent, and
    /// `cancel` is not yet cancelled at the point the re-check runs.
    #[tokio::test]
    async fn the_ordinary_ordering_the_stopping_tasks_own_check_still_revokes() {
        let state = state().await;
        let registry = Arc::clone(&state.attach_registry);
        let cancel = CancellationToken::new();
        let cell: std::sync::OnceLock<Arc<crate::attach_registry::AttachRegistry>> =
            std::sync::OnceLock::new();

        assert!(!cancel.is_cancelled());
        fill_attach_registry_and_recheck(&cell, &registry, &cancel);
        assert!(
            !registry.is_closed(),
            "no shutdown requested yet — the re-check must not revoke early"
        );

        cancel.cancel();
        if let Some(r) = cell.get() {
            r.revoke_all();
        }
        assert!(registry.is_closed());
    }
}
