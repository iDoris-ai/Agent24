//! HTTP server: router, ready line, graceful shutdown.
//! Bearer auth middleware lives in `capabilities/middleware.rs`.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use agent24_models::router::ModelRouter;
use agent24_protocol::state_file::AuthMode;
use agent24_store::Store;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use rand::RngCore;
use std::sync::Arc as StdArc;
use tokio_util::sync::CancellationToken;

fn workspace_recovery_instant_at(
    time: std::time::SystemTime,
) -> std::io::Result<agent24_store::WorkspaceInstant> {
    let text = agent24_core::util::iso8601_millis_at(time).map_err(std::io::Error::other)?;
    agent24_store::WorkspaceInstant::parse(&text).map_err(std::io::Error::other)
}

// A shutdown's budgets and deadlines — the HTTP drain's fixed 1.5s, the
// out-of-process modules' drain and stop grace (tunable: `A24_MODULE_DRAIN_MS`,
// `A24_COMPONENT_STOP_GRACE_MS`), the time to put the summary on disk, and the
// watchdog after all of them — live in `crate::lifecycle` (SHUT-1b). At the
// defaults `kill -TERM` ends the process within 16.5s, or about 50.6s while a managed iDoris holds the summary open (ADR-038 D5 raised the stop grace to 15s; TASKS B2's 2s now holds only with a lowered grace).

/// Test seam: park the first enable rescan, and record whether the config
/// write still holds `os_control`.
#[cfg(test)]
pub(crate) struct OsEnableProbe {
    pub park_first_rescan: std::sync::atomic::AtomicBool,
    pub rescans: std::sync::atomic::AtomicUsize,
    pub rescan_entered: tokio::sync::Notify,
    pub release_rescan: tokio::sync::Notify,
    pub write_saw_lock: std::sync::atomic::AtomicBool,
    pub writes: std::sync::atomic::AtomicUsize,
}

#[cfg(test)]
impl OsEnableProbe {
    fn new() -> Self {
        Self {
            park_first_rescan: std::sync::atomic::AtomicBool::new(false),
            rescans: std::sync::atomic::AtomicUsize::new(0),
            rescan_entered: tokio::sync::Notify::new(),
            release_rescan: tokio::sync::Notify::new(),
            write_saw_lock: std::sync::atomic::AtomicBool::new(false),
            writes: std::sync::atomic::AtomicUsize::new(0),
        }
    }
}

#[derive(Clone)]
pub struct AppState {
    pub os_signature: agent24_os_packages::signature::Policy,
    pub token: Arc<String>,
    /// Legacy and capability authority are mutually exclusive. Tests and
    /// existing callers continue to construct legacy state through
    /// [`AppState::new`]; production capability mode replaces it before the
    /// router is built.
    pub auth_mode: AuthMode,
    /// X11-2g: legacy daemon started with `A24_DEV_TOOL=1`. The wechat POST
    /// answers 404 unless this is set. Capabilities mode leaves it false and
    /// the auth middleware answers 404 before the handler.
    pub dev_wechat_events: bool,
    pub capabilities: Option<crate::capabilities::CapabilityStore>,
    pub first_party: Option<Arc<crate::capabilities::first_party::FirstPartyAuthority>>,
    pub presence_provider: Option<Arc<dyn crate::capabilities::presence::PresenceProvider>>,
    pub module_surfaces: Arc<Vec<crate::capabilities::delegation::ModuleSurface>>,
    pub delegations: Arc<crate::capabilities::delegation::Delegations>,
    /// KB-HOST-01: the Knowledge bindings the product host activated, in
    /// memory only (see `agent24_knowledge::host`).
    pub knowledge_host: Arc<agent24_knowledge::host::KnowledgeHostRegistry>,
    /// CORE-2 AgentEar S3: the desktop confirmation queue behind
    /// `/api/v1/presence/challenges/*`.
    pub presence_desktop: Option<Arc<crate::presence_desktop::DesktopPresence>>,
    /// D2 router: every model call goes through tier routing + health/cooldown,
    /// so a downed local provider backs off and a LocalOnly task never leaks.
    pub router: Arc<ModelRouter>,
    pub idoris_host: Option<Arc<agent24_idoris::idoris_host::IdorisHost>>,
    pub idoris_unavailable: bool,
    /// Where the Hyphae daemon supervisor lives, once `serve` has one (`/api/v1/components`).
    pub comm_slot: Option<Arc<crate::comm_routes::CommDaemonSlot>>,
    /// What the desktop reported about the components it owns (ADR-038 D5).
    pub component_reports: Arc<crate::component_routes::ComponentReports>,
    pub managed_idoris: Option<Arc<agent24_idoris::managed_idoris::ManagedIdoris>>,
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
    /// Enable's rescan replaces one module's startup outcome. `os_reports` is
    /// shared as an immutable snapshot; this map is the live refusal record
    /// `/components` and the next enable read. A name absent here still uses
    /// the snapshot.
    pub os_outcome_overrides:
        Arc<std::sync::Mutex<std::collections::HashMap<String, crate::domain::MountOutcome>>>,
    /// Namespaces that were actually mounted (including disabled and degraded).
    /// Capability auth uses this when axum did not record a `MatchedPath`
    /// (`nest_service` / fallback nests). Refused mounts are absent. Empty
    /// until `serve` fills it, same ordering as `os_reports`.
    pub module_namespaces: Arc<BTreeSet<String>>,
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
    /// What a module tool's card is made from (K1-6a-G §2.1); `None`
    /// until `serve` assembles the module tools.
    pub module_tool_cards: Option<crate::consent_routes::ModuleToolCards>,
    /// How module tool decisions are taken; `None` (the default, and always
    /// on an ephemeral daemon) answers every decision 403.
    pub module_grants: Option<crate::consent_routes::GrantSettings>,
    /// Held across an enable's rescan, refusal-record update, and config
    /// write (the rescan runs only after the lock is taken), and across a
    /// disable's config write plus the hand-off of its stop — not across
    /// the wait that follows — so concurrent toggles run in one order
    /// (SUP-5, PR-3c). The list a PATCH answers with is rendered after.
    /// A module tool decision holds it from its checks to its write, and a
    /// stop for the whole stop (K1-6a-G §2.1 F7).
    pub os_control: Arc<tokio::sync::Mutex<()>>,
    /// Test seam for "rescan and the config write share one `os_control`
    /// hold". Production builds do not carry it.
    #[cfg(test)]
    pub(crate) os_enable_probe: Arc<OsEnableProbe>,
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
    /// D5/D9 for every configured MCP server, including ones that failed to
    /// start or were switched off (ADR-038 C7). Empty until `serve` replaces
    /// it, before the router clones this state.
    pub mcp_roster: Arc<crate::mcp::McpRoster>,
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
    pub attach_registry: Arc<agent24_attach::attach_registry::AttachRegistry>,
    /// M1-T10: the memory base, for `/api/v1/memory/*` — `None` exactly when
    /// `memory_owner` is `None` (memory unavailable; see [`open_memory_base`]).
    /// A SEPARATE handle from the one the run loop got (both are clones of the
    /// same pool via [`agent24_memory::KvStore`]'s `Clone`), because the
    /// `SessionMemory` that owns the run loop's handle is moved into
    /// [`agent24_agent::RunManager`] and no longer reachable from `AppState`.
    pub memory_kv: Option<agent24_memory::KvStore>,
    /// M1-T10: the personal partition key `memory_kv`'s handlers read and
    /// write — the SAME owner [`session_memory`] computed for the run loop.
    /// Never accepted from a request: the daemon injects it from session
    /// identity, per `docs/research/MEMORY-STRATEGY.md` §4.1's T10 row.
    pub memory_owner: Option<String>,
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
    idoris_budget: Arc<std::sync::atomic::AtomicBool>,
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
            idoris_budget: Arc::new(std::sync::atomic::AtomicBool::new(false)),
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
        let mut deadlines = self.params.deadlines(began);
        if self.idoris_budget.load(std::sync::atomic::Ordering::SeqCst) {
            deadlines.persist = deadlines
                .persist
                .max(began + agent24_idoris::idoris_host::SHUTDOWN_BUDGET);
            deadlines.watchdog =
                deadlines.http.max(deadlines.persist) + crate::lifecycle::WATCHDOG_MARGIN;
        }
        deadlines
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

    /// Resolves when in-flight model calls of modules and attached modules must be
    /// cancelled: the drain plus the per-call cancel grace
    /// ([`crate::lifecycle::CALL_CANCEL_GRACE`], ADR-038 D6), capped by the stop grace when that
    /// is shorter and never stretched by it. Attached modules get no SIGTERM, so a 15s stop
    /// grace must not keep their calls alive. Stored
    /// instant, like [`Shutdown::modules_cut_off`].
    pub fn calls_cut_off(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let shutdown = self.clone();
        async move {
            shutdown.token.cancelled().await;
            tokio::time::sleep_until(shutdown.deadlines().calls).await;
        }
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
                    idoris_headroom_confirmed: false,
                    session_id: session_id.clone(),
                    workspace_id: None,
                    prompt: prompt.clone(),
                    model_override: model_override.clone(),
                    // Scheduled runs are unattended — plan mode needs a human
                    // to approve the plan, so a fired schedule always runs
                    // Normal.
                    mode: agent24_protocol::RunMode::Normal,
                    knowledge_binding_id: None,
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

#[cfg(test)]
#[path = "../tests/session_memory/mod.rs"]
mod session_memory_tests;

#[cfg(test)]
#[path = "../tests/inbound_admission/positive.rs"]
mod inbound_admission_positive;

/// Pair the memory base with a router-backed summarizer for D1 session memory.
/// The session owner is the durable personal partition recorded by the OS
/// memory catalog, so replay and module memory use the same org identity.
async fn session_memory(
    kv: agent24_memory::KvStore,
    router: &Arc<ModelRouter>,
    shutdown: &CancellationToken,
) -> Result<agent24_agent::SessionMemory, String> {
    let org = crate::os_memory::OrgId::from_store(
        kv.ensure_org_for_user(LOCAL_USER)
            .await
            .map_err(|err| err.to_string())?,
    );
    let owner = crate::os_memory::OsMemoryCatalog::default()
        .ensure_personal_recorded(&org, LOCAL_USER, &kv)
        .await?;

    Ok(agent24_agent::SessionMemory::new(
        kv,
        StdArc::new(agent24_agent::RouterSummarizer::new(
            Arc::clone(router),
            shutdown.clone(),
        )),
    )
    .with_owner(owner))
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
    pub workspace_service: Option<StdArc<agent24_workspace::WorkspaceService>>,
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
            workspace_service,
            mcp_servers,
            risk_overrides,
            packages_root,
        } = deps;
        // M1-T10: the memory REST surface needs a handle to the SAME base and
        // owner the run loop below gets — captured here, before `memory` is
        // moved into `RunManager::with_memory_and_workspace`, so no AppDeps
        // field (and no change to any of its many construction sites) is
        // needed for this.
        let memory_kv = memory.as_ref().map(|m| m.kv().clone());
        let memory_owner = memory.as_ref().map(|m| m.owner().to_owned());
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
            agent24_attach::agentear_timings::spawn_agentear_timing_bridge(
                &events,
                timings.clone(),
            );
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
        let runs = agent24_agent::RunManager::with_memory_and_workspace(
            store.clone(),
            Arc::clone(&router),
            Arc::clone(&tools),
            StdArc::new(events.clone()),
            shutdown.token().clone(),
            memory,
            workspace_service,
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
        let attach_registry = Arc::new(agent24_attach::attach_registry::AttachRegistry::new(
            agent24_attach::attach_registry::AttachDeps {
                // No `ModelCallbackDeps` exists yet at this point (it needs
                // the `UsageRecorder` `serve` spawns later) — `serve` builds
                // the real registry once that exists and replaces this one,
                // hydrated from `attached.json` (see this field's own doc on
                // `AppState`).
                grants: Arc::new(crate::attach_grant::DaemonAttachGrantBuilder {
                    scheduler: scheduler.clone(),
                    models: None,
                    approval_broker: module_approval_broker.clone(),
                    events: events.clone(),
                }),
            },
        ));
        Self {
            os_signature: os_signature_policy(),
            risk_overrides,
            token: Arc::new(token),
            auth_mode: AuthMode::LegacySingleToken,
            dev_wechat_events: false,
            capabilities: None,
            first_party: None,
            presence_provider: None,
            module_surfaces: Arc::new(Vec::new()),
            delegations: Arc::new(crate::capabilities::delegation::Delegations::default()),
            knowledge_host: Arc::new(agent24_knowledge::host::KnowledgeHostRegistry::default()),
            presence_desktop: None,
            mcp_servers: Arc::new(mcp_servers),
            mcp_roster: Arc::new(crate::mcp::McpRoster::default()),
            router,
            managed_idoris: None,
            idoris_host: None,
            idoris_unavailable: false,
            comm_slot: None,
            component_reports: Arc::default(),
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
            os_outcome_overrides: Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            module_namespaces: Arc::new(BTreeSet::new()),
            packages_root,
            // Empty for the same reason and until the same moment as `os_reports`
            // above — see that field's comment.
            package_dirs: Arc::new(std::collections::HashMap::new()),
            module_status: Arc::new(std::collections::HashMap::new()),
            supervisors: None,
            module_tool_cards: None,
            module_grants: None,
            os_control: Arc::new(tokio::sync::Mutex::new(())),
            #[cfg(test)]
            os_enable_probe: Arc::new(OsEnableProbe::new()),
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
            memory_kv,
            memory_owner,
        }
    }
}

impl AppState {
    /// The outcome enable and `/components` should show. The override wins
    /// when a rescan has replaced the startup snapshot.
    pub(crate) fn os_outcome(
        &self,
        name: &str,
        fallback: &crate::domain::MountOutcome,
    ) -> crate::domain::MountOutcome {
        self.os_outcome_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(name)
            .cloned()
            .unwrap_or_else(|| fallback.clone())
    }

    pub(crate) fn set_os_outcome(&self, name: &str, outcome: crate::domain::MountOutcome) {
        self.os_outcome_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(name.to_owned(), outcome);
    }

    /// Drop a rescan overlay so the startup snapshot shows again.
    pub(crate) fn clear_os_outcome(&self, name: &str) {
        self.os_outcome_overrides
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(name);
    }

    pub(crate) fn enable_capability_auth(&mut self, store: crate::capabilities::CapabilityStore) {
        self.token = Arc::new(String::new());
        self.auth_mode = AuthMode::Capabilities;
        self.first_party = Some(Arc::new(
            crate::capabilities::first_party::FirstPartyAuthority::new(store.clone()),
        ));
        self.capabilities = Some(store);
    }

    /// Share one queue between enrolment confirmation and ProductHost HTTP.
    fn enable_desktop_presence(&mut self) {
        let presence = Arc::new(crate::presence_desktop::DesktopPresence::new());
        self.presence_provider = Some(presence.clone());
        self.presence_desktop = Some(presence);
    }

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

#[path = "handlers.rs"]
mod handlers;

use crate::capabilities::middleware::auth;
use handlers::{
    health, mint_bridge_capability, mint_creative_capability, revoke_capability, shutdown_handler,
    shutdown_report,
};

async fn fallback() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route")
}

async fn establish_module_delegation(
    State(state): State<AppState>,
    axum::extract::Path(name): axum::extract::Path<String>,
) -> Response {
    if state.auth_mode != AuthMode::Capabilities {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "Delegation requires capabilities mode",
        );
    }
    let Some(surface) = state.module_surfaces.iter().find(|s| s.name == name) else {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "No registered module surface",
        );
    };
    match state.delegations.establish(surface) {
        Ok(()) => (
            StatusCode::OK,
            axum::Json(serde_json::json!({"delegated":true,"name":name})),
        )
            .into_response(),
        Err(_) => error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Cannot persist delegation",
        ),
    }
}

#[cfg(test)]
#[path = "tests/first_party_http.rs"]
mod first_party_http_tests;

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
    if let Some(authority) = &state.first_party {
        authority.set_provider(state.presence_provider.clone());
        authority.set_module_surfaces(state.module_surfaces.clone());
    }
    let kernel = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/components", get(crate::component_routes::list))
        .route(
            "/api/v1/components/{id}/report",
            post(crate::component_routes::report),
        )
        .route("/api/v1/idoris/status", get(crate::idoris_routes::status))
        .route(
            "/api/v1/idoris/install",
            post(crate::idoris_routes::install),
        )
        .route(
            "/api/v1/idoris/connect",
            post(crate::idoris_routes::connect),
        )
        .route(
            "/api/v1/idoris/omlx/candidates",
            get(crate::idoris_routes::candidates),
        )
        .route("/api/v1/idoris/start", post(crate::idoris_routes::start))
        .route("/api/v1/idoris/stop", post(crate::idoris_routes::stop))
        .route(
            "/api/v1/idoris/operations/{id}",
            get(crate::idoris_routes::operation),
        )
        .route(
            "/api/v1/idoris/operations/{id}/cancel",
            post(crate::idoris_routes::cancel),
        )
        .route("/api/v1/chat", post(crate::routes::post_chat))
        .route("/api/v1/models", get(crate::routes::get_models))
        .route(
            "/api/v1/decide/profile",
            get(crate::decide_routes::get_decide_profile),
        )
        .route(
            "/api/v1/capabilities/creative",
            post(mint_creative_capability),
        )
        .route("/api/v1/capabilities/bridge", post(mint_bridge_capability))
        .route(
            "/api/v1/capabilities/{capability_id}/revoke",
            post(revoke_capability),
        )
        // The desktop's generic proxy refuses these by their raw path
        // (`reachesHostInterface`); that holds only while routing matches the
        // raw path, so never decode or normalize the path before routing.
        .route(
            "/api/v1/host/knowledge/bindings/{binding_id}",
            axum::routing::put(crate::knowledge_routes::change)
                .delete(crate::knowledge_routes::change),
        )
        .route(
            "/api/v1/host/knowledge/bindings/{binding_id}/credential",
            axum::routing::put(crate::knowledge_routes::push_credential)
                .delete(crate::knowledge_routes::revoke_credential),
        )
        .route("/api/v1/usage", get(crate::routes::get_usage))
        .route("/api/v1/timings", get(crate::routes::get_timings))
        .route(
            "/api/v1/timings/summary",
            get(crate::routes::get_timings_summary),
        )
        .route("/api/v1/tools", get(crate::routes::get_tools))
        .route("/api/v1/module-consents", get(crate::consent_routes::list))
        .route(
            "/api/v1/module-consents/export",
            get(crate::consent_routes::export),
        )
        .route(
            "/api/v1/module-consents/modules/{module}",
            axum::routing::delete(crate::consent_routes::revoke_module),
        )
        .route(
            "/api/v1/module-consents/modules/{module}/tools/{tool}",
            axum::routing::delete(crate::consent_routes::revoke_tool)
                .post(crate::consent_routes::decide),
        )
        .route(
            "/api/v1/module-consents/modules/{module}/tools/{tool}/card",
            get(crate::consent_routes::card),
        )
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
        // M1-T10: personal-memory REST — list/search, retract (forget), and
        // the persistent pause switch. Personal space only; `owner` is never
        // a request parameter (`crate::memory_routes` module doc).
        .route(
            "/api/v1/memory/assertions",
            get(crate::memory_routes::list_assertions),
        )
        .route(
            "/api/v1/memory/assertions/{id}",
            axum::routing::delete(crate::memory_routes::delete_assertion),
        )
        .route(
            "/api/v1/memory/settings",
            get(crate::memory_routes::get_memory_settings)
                .put(crate::memory_routes::put_memory_settings),
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
        .route(
            "/api/v1/inbound/allowlist",
            get(crate::inbound::get_allowlist).put(crate::inbound::put_allowlist),
        )
        .route(
            "/api/v1/inbound/policy",
            get(crate::inbound::get_policy).put(crate::inbound::put_policy),
        )
        .route(
            "/api/v1/inbound/events",
            get(crate::inbound::get_events).post(crate::inbound::post_wechat_event),
        )
        // Skill loader SL-3. `{id}` is the only path parameter. The scan
        // route is a noun collection (ADR-045 N5). Activate is SL-4.
        .route(
            "/api/v1/skills",
            get(crate::skill_routes::list_skills).post(crate::skill_routes::post_skill),
        )
        .route(
            "/api/v1/skills/scans",
            post(crate::skill_routes::post_skill_scan),
        )
        .route("/api/v1/skills/{id}", get(crate::skill_routes::get_skill))
        .route(
            "/api/v1/skills/{id}/activate",
            post(crate::skill_routes::post_skill_activate),
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
            "/api/v1/os/trusted-teams",
            get(crate::os_routes::trusted_teams),
        )
        .route(
            "/api/v1/os/trusted-teams/{team}",
            axum::routing::put(crate::os_routes::trust_team).delete(crate::os_routes::untrust_team),
        )
        .route(
            "/api/v1/os/{name}/delegation",
            post(establish_module_delegation),
        )
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
        // KB-SOP-01a (#1669): read only; see source_floor.rs.
        .route(
            "/api/v1/sessions/{id}/source-floor",
            get(crate::source_floor::get_source_floor),
        )
        .route(
            "/api/v1/runs",
            post(crate::runs::create_run).get(crate::runs::list_runs),
        )
        .route("/api/v1/runs/{id}", get(crate::runs::get_run))
        .route("/api/v1/runs/{id}/cancel", post(crate::runs::cancel_run))
        // K1-6b.5c-2: the redacted views the MCP server and ACP bridge relay.
        .route("/api/v1/peer/runs", get(crate::peer_runs::list_peer_runs))
        .route(
            "/api/v1/peer/runs/{id}",
            get(crate::peer_runs::get_peer_run),
        )
        .fallback(fallback);

    let kernel = if state.auth_mode == AuthMode::Capabilities {
        kernel
            .route(
                "/api/v1/presence/challenges/next",
                get(crate::presence_desktop::get_next),
            )
            .route(
                "/api/v1/presence/challenges/{id}",
                post(crate::presence_desktop::post_decision),
            )
    } else {
        kernel
    };

    // Modules first, auth last — see the doc comment above.
    kernel
        .with_state(state.clone())
        .merge(modules)
        .layer(middleware::from_fn_with_state(state, auth))
}

/// The legacy-mode `ready` line. With `omit_token` (B3 / S-04: the LaunchAgent
/// persists stdout to a log file) the bearer is left out — it is still in the
/// 0600 `daemon.json`, which is where the CLI of a service-run daemon reads it.
fn legacy_ready_line(
    port: u16,
    token: &str,
    omit_token: bool,
    generation: &str,
) -> serde_json::Value {
    let mut ready = serde_json::json!({
        "type": "ready",
        "port": port,
        "auth_mode": "legacy_single_token",
        "generation": generation,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if !omit_token {
        ready["token"] = serde_json::json!(token);
    }
    ready
}

/// B1 (REVIEW-TODO PR-B, R4 / C-03): DNS-rebinding and browser-CSRF defence for
/// the REST surface. Layered OUTSIDE [`build_router`] by `serve` (the only
/// place the real bound port is known), so it runs before `auth` and covers
/// `/api/v1/health` and the module reverse-proxy routes alike. It only inspects
/// headers — the request reaching a module is untouched, and the proxy builds
/// its own fixed upstream `Host` (`agent24-os-proto::proxy`).
///
/// - `Host` must be `127.0.0.1:{port}`, `localhost:{port}` or `[::1]:{port}`
///   (ASCII case-insensitive); any other value is 403 `bad_host`, and no host
///   at all (an HTTP/1.0 request) is 400 `bad_host`. (HTTP/2 carries the authority in the URI, which is used when there is no
///   `Host` header.)
/// - An `Origin` header other than the literal `null` is 403 `forbidden_origin`
///   (browsers always send it on cross-origin requests; the CLI/desktop main
///   process/bridges never do). `Origin: null` stays allowed here; the WS
///   upgrade rejects any `Origin` itself.
pub fn guard_host_origin(router: Router, port: u16) -> Router {
    let allowed: StdArc<[String; 3]> = StdArc::new([
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]);
    router.layer(middleware::from_fn_with_state(allowed, host_origin_guard))
}

async fn host_origin_guard(
    State(allowed): State<StdArc<[String; 3]>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.as_str().to_owned()));
    match host.as_deref() {
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "bad_host",
                "A Host header is required",
            );
        }
        Some(h) if !allowed.iter().any(|a| a.eq_ignore_ascii_case(h)) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "bad_host",
                "Host header is not this daemon's loopback address",
            );
        }
        Some(_) => {}
    }
    if req
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|o| o.as_bytes() != b"null")
    {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Browser-originated requests are not allowed",
        );
    }
    next.run(req).await
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
    cell: &std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>>,
    registry: &Arc<agent24_attach::attach_registry::AttachRegistry>,
    cancel: &CancellationToken,
) {
    let _ = cell.set(Arc::clone(registry));
    if cancel.is_cancelled() {
        registry.revoke_all();
    }
}

const APPROVAL_RECOVERY_SCAN_INTERVAL: Duration = Duration::from_secs(10);

fn spawn_approval_recovery_scan(
    broker: Arc<agent24_policy::ApprovalBroker>,
    runs: Arc<agent24_agent::RunManager>,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    spawn_approval_recovery_scan_every(broker, runs, APPROVAL_RECOVERY_SCAN_INTERVAL, cancel)
}

fn spawn_approval_recovery_scan_every(
    broker: Arc<agent24_policy::ApprovalBroker>,
    runs: Arc<agent24_agent::RunManager>,
    interval: Duration,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
            let now = agent24_core::util::now_iso8601();
            match broker.timeout_expired(&now).await {
                Ok(expired) if expired > 0 => {
                    tracing::warn!("timed out {expired} expired approval(s)");
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::error!("expired approval scan failed; will retry: {err}");
                }
            }
            match runs.recover_timed_out_approval_runs().await {
                Ok(cancelled) if cancelled > 0 => {
                    tracing::warn!(
                        "cancelled {cancelled} token-less run(s) after approval timeout"
                    );
                }
                Ok(_) => {}
                Err(err) => {
                    tracing::error!("timed-out run recovery scan failed; will retry: {err}");
                }
            }
        }
    })
}

/// Set once the ready line has gone out (SPEC-002 §4): from then on stdout
/// carries nothing, not even a `failed` line.
pub static READY_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Another daemon holds the singleton lock: the start that loses a
/// concurrent-start race. Its `failed` line says `already_running`, so a
/// launcher knows to look for the winner instead of reporting an error.
#[derive(Debug)]
pub struct AlreadyRunning;

impl std::fmt::Display for AlreadyRunning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another agent24d is already running (singleton lock held)")
    }
}

impl std::error::Error for AlreadyRunning {}

#[path = "serve_steps.rs"]
mod serve_steps;

#[allow(
    clippy::too_many_arguments,
    reason = "startup options include both enrollment and injected iDoris dependencies"
)]
pub async fn serve(
    port: u16,
    ephemeral: bool,
    auth_mode: AuthMode,
    host_bootstrap_stdio: bool,
    enroll_endpoint: bool,
    ready_omit_token: bool,
    cancel: CancellationToken,
    idoris_dependencies: Option<agent24_idoris::managed_idoris::ManagedDependencies>,
) -> Result<(), std::io::Error> {
    let startup = std::time::Instant::now();
    tracing::debug!(ephemeral, "startup: serve");
    let (
        mode,
        module_grants,
        mut host_ready,
        params,
        mut config_warnings,
        shutdown,
        idoris_root,
        idoris_install_root,
    ) = serve_steps::install_shutdown_controller(
        auth_mode,
        host_bootstrap_stdio,
        enroll_endpoint,
        ephemeral,
        &cancel,
    )?;
    let _singleton = serve_steps::acquire_singleton(ephemeral)?;
    let (evidence, marker) = serve_steps::install_run_marker(ephemeral, &mut config_warnings);
    let (daemon_generation, token, capability_store, product_host_token) =
        serve_steps::mint_startup_credentials(auth_mode, mode)?;
    // CORE-5 5f-3: a marker or an unfinished upgrade docks here, before any
    // store, task or listener of the business daemon exists. `ENABLED` is
    // false, so this is skipped and startup is exactly the code below.
    if !ephemeral
        && let Some(dir) = agent24_protocol::state_file::state_dir()
        && let Some(docking) =
            crate::docked_startup::pre_start(crate::docked_startup::guard_enabled(), &dir).await
    {
        let launch = crate::docked_startup::Launch {
            data_dir: &dir,
            port,
            auth_mode,
            enroll: mode != StartupMode::Legacy,
            generation: daemon_generation,
            capabilities: capability_store,
            ready: match host_ready.take() {
                Some(writer) => crate::docked_startup::ReadyOut::Host(writer),
                None => crate::docked_startup::ReadyOut::Stdout,
            },
            shutdown,
            lock: _singleton,
        };
        return crate::docked_startup::serve_docked(launch, docking).await;
    }

    let (
        store,
        workspace,
        router,
        managed_idoris,
        idoris_unavailable,
        idoris_host,
        idoris_stopping,
    ) = serve_steps::open_store_workspace_and_idoris(
        ephemeral,
        &startup,
        idoris_root,
        idoris_install_root,
        &idoris_dependencies,
        &cancel,
    )
    .await?;
    let (guardian, memory_base, memory) =
        serve_steps::open_guardian_and_memory(ephemeral, &router, &cancel, &startup).await;
    let (state_dir, packages_root, catalogue, integrity_refusals) =
        serve_steps::discover_packages(ephemeral, &cancel).await?;
    let (
        tools,
        module_registry,
        module_tool_runtime,
        _module_tool_runtime_dyn,
        module_tool_supervisors,
        _module_authorizations,
        module_tool_cards,
        _advert_view_dyn,
        knowledge_host,
        mcp_servers,
        mcp_roster,
    ) = serve_steps::register_startup_tools(workspace, &store, &router, &catalogue, &cancel).await;
    let (report, mut state) = serve_steps::assemble_app_state(
        &token,
        router,
        tools,
        store,
        &shutdown,
        guardian,
        memory,
        mcp_servers,
        &packages_root,
        &params,
        &config_warnings,
        &evidence,
        &state_dir,
        knowledge_host,
        managed_idoris,
        idoris_host,
        idoris_unavailable,
        capability_store,
        mode,
        mcp_roster,
        ephemeral,
    )
    .await?;
    let os_config = serve_steps::recover_durable_runs(
        &state,
        ephemeral,
        &cancel,
        &module_tool_runtime,
        &module_registry,
    )
    .await?;
    let (os_root, inventory, lease) = serve_steps::prepare_os_mount(
        ephemeral,
        &state_dir,
        &catalogue,
        &os_config,
        &state,
        &cancel,
        memory_base,
    )
    .await;
    let (listener, local) = serve_steps::bind_loopback(&startup, port).await?;
    let host = serve_steps::start_process_host(
        ephemeral,
        &os_root,
        &state_dir,
        params,
        &shutdown,
        &module_tool_supervisors,
    );
    let (attach_registry_cell, comm_daemon_slot, stopping) =
        serve_steps::spawn_ordered_shutdown(&shutdown, &host, ephemeral, idoris_stopping, marker);
    let (usage_recorder, usage_writer, model_deps) =
        serve_steps::start_usage_and_model_deps(&shutdown, &state);
    let (module_routes, reports, partitions, package_dirs) = serve_steps::mount_domain_os(
        &catalogue,
        &os_root,
        &state,
        &os_config,
        &inventory,
        &lease,
        &host,
        &integrity_refusals,
        &model_deps,
    )
    .await;
    serve_steps::arm_scheduler_after_mount(&mut state, &catalogue, &host, &cancel);
    serve_steps::publish_mount_and_attach(
        &mut state,
        &partitions,
        &lease,
        reports,
        package_dirs,
        &host,
        module_tool_cards,
        &cancel,
        module_grants,
        report,
        model_deps,
        ephemeral,
        &attach_registry_cell,
        &shutdown,
    )
    .await;
    // COMM-2a: `/api/v1/comm/*` — merged into `module_routes` (not a literal
    // route inside `build_router_with_modules` itself) so this stays behind
    // kernel auth exactly like every other module route, and so the wiring
    // itself lives in its own file (`comm_routes.rs`); see that file's own
    // doc comment for the `RESERVED_KERNEL_SEGMENTS` gap this leaves.
    // PR #626 review, High #1: `build` now registers the supervisor it
    // creates into `comm_daemon_slot` ITSELF, synchronized with the
    // `stopping` task's own `close()` — no more racing a `set()` here
    // after the fact against a shutdown that may have already decided
    // there was nothing to stop.
    let comm_router =
        crate::comm_routes::build(&state_dir, params.stop_grace, &comm_daemon_slot).await;
    state.comm_slot = Some(Arc::clone(&comm_daemon_slot));
    // X11-2e: the poller reads the CLI and the opt-in again every round.
    // Default is not to pull. A Hyphae binary configured after startup is
    // picked up on the next round.
    let inbound_slot = Arc::clone(&comm_daemon_slot);
    serve_steps::start_hyphae_inbound(
        &state,
        move || inbound_slot.hyphae_cli(),
        shutdown.child_token(),
    )
    .await;

    serve_steps::fill_module_surfaces(&mut state, &catalogue);
    let enroll_authority = state.first_party.clone();
    // The user-directory scan runs after the listener is accepting. It must
    // not sit on the path that publishes the ready line.
    let skill_scan_store = state.store.clone();
    let router = build_router_with_modules(state, module_routes.merge(comm_router));

    let still = serve_steps::advertise_readiness(
        &cancel,
        stopping,
        usage_recorder,
        usage_writer,
        &shutdown,
        ephemeral,
        mode,
        &state_dir,
        auth_mode,
        local,
        &token,
        &daemon_generation,
        enroll_authority,
        ready_omit_token,
        &startup,
        product_host_token,
        &mut host_ready,
    )
    .await?;
    let Some(still) = still else {
        return Ok(());
    };
    let serve_steps::StillUp {
        stopping,
        usage_recorder,
        usage_writer,
        daemon_pid,
        enroll: _enroll,
    } = still;
    let graceful_cancel = cancel.clone();
    let router = guard_host_origin(router, local.port());
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move { graceful_cancel.cancelled().await });
    tokio::spawn(async move {
        // Debug builds only, and only this startup scan. HTTP rescan does not wait.
        #[cfg(debug_assertions)]
        crate::skill_routes::pause_scan_for_tests().await;
        let user_skills = crate::skill_routes::scan_installed_user_skills(&skill_scan_store).await;
        if !user_skills.errors.is_empty() {
            tracing::warn!(
                skipped = user_skills.errors.len(),
                "user skill directory scan skipped entries"
            );
        }
    });

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

/// How this process is started (CORE-2 D8). The single source of the
/// bootstrap / enroll matrix; `serve` acts on the returned mode only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StartupMode {
    /// Single-token daemon, unchanged.
    Legacy,
    /// Capability daemon owned by a trusted parent over stdio pipes: the
    /// parent-EOF lease applies and the enrolment endpoint opens by itself.
    HostChild,
    /// Capability daemon run by a service manager: no pipes, no parent lease,
    /// no host credential; the enrolment endpoint is the only way in.
    Service,
}

/// Only a host-child daemon has a trusted pipe to hand a `ProductHost`
/// credential to; a service never mints one (CORE-2 R5, T17).
fn mints_product_host(mode: StartupMode) -> bool {
    mode == StartupMode::HostChild
}

/// Validate the auth / bootstrap / enroll combination. Mutually exclusive by
/// construction, not by argument order; bootstrap together with enroll is
/// refused as ambiguous (§9 Q5).
fn validate_auth_startup(
    auth_mode: AuthMode,
    host_bootstrap_stdio: bool,
    enroll_endpoint: bool,
    ephemeral: bool,
) -> Result<StartupMode, std::io::Error> {
    use std::io::{Error, ErrorKind};
    match (auth_mode, host_bootstrap_stdio, enroll_endpoint) {
        (AuthMode::LegacySingleToken, false, false) => Ok(StartupMode::Legacy),
        (AuthMode::LegacySingleToken, true, _) => Err(Error::new(
            ErrorKind::InvalidInput,
            "host bootstrap stdio is valid only in capability mode",
        )),
        (AuthMode::LegacySingleToken, false, true) => Err(Error::new(
            ErrorKind::InvalidInput,
            "--enroll-endpoint is valid only in capability mode",
        )),
        (AuthMode::Capabilities, true, false) => Ok(StartupMode::HostChild),
        (AuthMode::Capabilities, false, false) => Err(Error::new(
            ErrorKind::PermissionDenied,
            "capability mode requires trusted host bootstrap stdio or --enroll-endpoint",
        )),
        (AuthMode::Capabilities, true, true) => Err(Error::new(
            ErrorKind::InvalidInput,
            "host bootstrap stdio and --enroll-endpoint are mutually exclusive",
        )),
        (AuthMode::Capabilities, false, true) if ephemeral => Err(Error::new(
            ErrorKind::InvalidInput,
            "--enroll-endpoint needs a discoverable daemon and cannot be ephemeral",
        )),
        (AuthMode::Capabilities, false, true) => Ok(StartupMode::Service),
    }
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
    // CallbackDir canonicalizes before binding. Measure that same path: on
    // macOS /var and /tmp expand to /private/var and /private/tmp.
    let resolved = root.canonicalize().unwrap_or_else(|_| root.to_owned());
    let projected = resolved
        .join("run")
        .join(std::process::id().to_string())
        .join(format!("{}.sock", u64::MAX));
    if projected.as_os_str().len() <= agent24_os_proto::endpoint::MAX_SOCKET_PATH {
        return Ok(resolved);
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
    shutdown: CancellationToken,
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
        signature: os_signature_policy(),
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
        shutdown,
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
/// A panicked scan is not an empty catalogue. An empty catalogue means no
/// packages were installed; a join failure means startup did not learn that.
fn catalogue_from_join<T>(joined: Result<T, tokio::task::JoinError>) -> Result<T, std::io::Error> {
    joined.map_err(|err| {
        tracing::error!("discovering domain OS packages failed: {err}");
        std::io::Error::other(format!("discovering domain OS packages failed: {err}"))
    })
}

/// Register package tools unless shutdown has already cancelled the token.
/// A package the scan accepted is not published after that.
pub(crate) fn register_discovered_module_tools(
    catalogue: &[crate::domain::Installed],
    cancel: &CancellationToken,
    module_registry: &mut agent24_domain::tool::ModuleToolRegistry,
    module_operations: &mut std::collections::HashSet<(String, String)>,
) {
    if cancel.is_cancelled() {
        tracing::info!("not registering domain OS tools: shutdown already cancelled");
        return;
    }
    for entry in catalogue {
        if let crate::domain::Build::Package(package) = &entry.build {
            if let Err(error) =
                module_registry.register_module(entry.name.as_str(), package.manifest.tools())
            {
                tracing::warn!(module = entry.name, %error, "module tools were refused");
                continue;
            }
            module_operations.extend(
                package
                    .manifest
                    .tools()
                    .iter()
                    .map(|tool| (entry.name.clone(), tool.operation().to_owned())),
            );
        }
    }
}

/// Drop tools filled before the publish point if the token has since fired.
pub(crate) fn module_tools_if_live(
    cancel: &CancellationToken,
    registry: agent24_domain::tool::ModuleToolRegistry,
    operations: std::collections::HashSet<(String, String)>,
) -> (
    agent24_domain::tool::ModuleToolRegistry,
    std::collections::HashSet<(String, String)>,
) {
    if cancel.is_cancelled() {
        tracing::info!("not publishing domain OS tools: shutdown already cancelled");
        (
            agent24_domain::tool::ModuleToolRegistry::new(),
            std::collections::HashSet::new(),
        )
    } else {
        (registry, operations)
    }
}

fn os_signature_policy() -> agent24_os_packages::signature::Policy {
    #[cfg(test)]
    {
        agent24_os_packages::test_support::signature_policy()
    }
    #[cfg(not(test))]
    {
        agent24_os_packages::signature::Policy::default()
    }
}

struct DiscoveryPass {
    catalogue: Vec<crate::domain::Installed>,
    /// Integrity refusals whose directory is a package, not the packages root.
    /// The root's own unreadability stays a log line: it is not a component.
    integrity_refusals: Vec<agent24_os_packages::discovery::Refused>,
}

/// Catalogue directories plus the directory of each integrity refusal, under
/// the component name [`crate::domain::integrity_refusal_reports`] assigned.
/// Enable rescans that directory. A mounted name keeps the catalogue path.
fn package_dirs_for_startup(
    catalogue: &[crate::domain::Installed],
    integrity_refusals: &[agent24_os_packages::discovery::Refused],
    mounted: &[crate::domain::MountReport],
) -> (
    Vec<crate::domain::MountReport>,
    std::collections::HashMap<String, std::path::PathBuf>,
) {
    let rows = crate::domain::integrity_refusal_reports(integrity_refusals, mounted);
    let mut dirs = std::collections::HashMap::new();
    for entry in catalogue {
        if let crate::domain::Build::Package(package) = &entry.build {
            dirs.insert(entry.name.clone(), package.dir.clone());
        }
    }
    for (row, refused) in rows
        .iter()
        .zip(integrity_refusals.iter().filter(|r| r.integrity.is_some()))
    {
        dirs.entry(row.name.clone())
            .or_insert_with(|| refused.dir.clone());
    }
    (rows, dirs)
}

#[cfg(test)]
fn with_discovered(
    catalogue: Vec<crate::domain::Installed>,
    packages_root: &std::path::Path,
    cancel: &CancellationToken,
) -> DiscoveryPass {
    with_discovered_policy(catalogue, packages_root, cancel, &os_signature_policy())
}
fn with_discovered_policy(
    mut catalogue: Vec<crate::domain::Installed>,
    packages_root: &std::path::Path,
    cancel: &CancellationToken,
    signature: &agent24_os_packages::signature::Policy,
) -> DiscoveryPass {
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
        return DiscoveryPass {
            catalogue,
            integrity_refusals: Vec::new(),
        };
    }
    let scan = agent24_os_packages::discovery::scan_with_policy(packages_root, cancel, signature);
    let mut integrity_refusals = Vec::new();
    for dir in &scan.cancelled {
        tracing::info!(
            "domain OS package at {} was not loaded: verification cancelled",
            dir.display()
        );
    }
    for r in &scan.refused {
        tracing::warn!(
            "domain OS package at {} was not loaded: {}",
            r.dir.display(),
            r.why
        );
        if r.integrity.is_some() && r.dir.as_path() != packages_root {
            integrity_refusals.push(r.clone());
        }
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
    DiscoveryPass {
        catalogue,
        integrity_refusals,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    #[test]
    fn a_refused_documents_package_is_recorded_under_that_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("documents");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("domain-os.yml"),
            "name: documents\nversion: \"1.2.3\"\nroute_namespace: /api/v1/documents\n\
             event_module: documents\ndata_dir: ~/.agent24/os/documents/\n\
             kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
             spawn:\n  command: bin/documents\n",
        )
        .unwrap();
        let scan = agent24_os_packages::discovery::scan(
            root.path(),
            &tokio_util::sync::CancellationToken::new(),
        );
        let refusals: Vec<_> = scan
            .refused
            .into_iter()
            .filter(|refused| refused.integrity.is_some() && refused.dir != root.path())
            .collect();
        let (rows, dirs) = super::package_dirs_for_startup(&[], &refusals, &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "documents");
        assert_eq!(
            dirs.get("documents").map(std::path::PathBuf::as_path),
            Some(dir.as_path())
        );
    }

    #[test]
    fn workspace_recovery_clock_preserves_fractional_millis() {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_900);
        let instant = super::workspace_recovery_instant_at(now).unwrap();
        assert_eq!(instant.as_str(), "1970-01-01T00:00:01.900Z");
    }

    // ---- ME4-4.2.2b2 H1 (Opus review round on top of `bb6fb0e`) -------------

    /// A real end-to-end daemon test
    /// (`apps/agent24d/tests/me4_model_shutdown_wiring.rs`) empirically
    /// CANNOT reliably pin this one line: `Shutdown::request()` also revokes
    /// the module's `Generation` as part of stopping it — design
    /// §3.3's OWN "代次撤销" row, pre-existing `os-proto` machinery that
    /// independently cancels an in-flight model call within tens of
    /// milliseconds of the SAME `spawn_cancel_root(shutdown.calls_cut_off())`
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
    fn the_model_callback_cancel_root_is_spawned_from_calls_cut_off() {
        // Scoped to `serve()`'s OWN body — never to the whole file, which
        // would make this test tautological (the string above, in this
        // test's own source, would always make a whole-file `.contains`
        // pass regardless of what `serve()` actually does). Same technique
        // as `module_routes_are_behind_kernel_auth`'s sibling
        // `build_router_with_modules` scan below: find the function, cut at
        // the first column-zero `}` (every brace inside a rustfmt'd function
        // body is indented).
        let server = include_str!("server.rs");
        let steps = include_str!("serve_steps.rs");
        let serve = &server[server
            .find("pub async fn serve(")
            .expect("serve must exist")..];
        let serve = &serve[..serve.find("\n}\n").expect("serve must be brace-terminated")];
        assert!(
            serve.contains("start_usage_and_model_deps("),
            "serve() must build model deps through start_usage_and_model_deps"
        );
        let marker = "fn start_usage_and_model_deps(";
        let start = steps.find(marker).expect("start_usage_and_model_deps");
        let body = &steps[start..];
        let body = &body[..body
            .find("\n}\n")
            .expect("function must be brace-terminated")];
        assert!(
            body.contains("spawn_cancel_root(shutdown.calls_cut_off())"),
            "serve() must build ModelCallbackDeps.cancel_root as \
             spawn_cancel_root(shutdown.calls_cut_off()) — design \
             docs/design/ME4-S2-model-callback.md §3.3"
        );
    }

    /// #1202 G6 (Codex A5c-2a round 2): `serve` builds each module tool's
    /// authorization through `tool_authorization`, which refuses a module
    /// not on the grants protocol, never `store_authorization` directly,
    /// which would let a grant from an older daemon hold. S2-1: that absence
    /// covers `serve()` itself and all of `serve_steps.rs`. Scanned with
    /// `//`-comments stripped, as
    /// `stop_usage_writer_is_called_after_the_supervisors_have_stopped`.
    #[test]
    fn serve_authorizes_module_tools_by_protocol() {
        let server = include_str!("server.rs");
        let steps = include_str!("serve_steps.rs");
        let serve = &server[server
            .find("pub async fn serve(")
            .expect("serve must exist")..];
        let serve = &serve[..serve.find("\n}\n").expect("serve must be brace-terminated")];
        let tools_call = serve
            .find("register_startup_tools(")
            .expect("serve must register tools");
        let publish_call = serve
            .find("publish_mount_and_attach(")
            .expect("serve must publish the mount");
        assert!(
            tools_call < publish_call,
            "tool authorization is built before the grant-expiry notifier"
        );
        let step = |name: &str| {
            let marker = format!("fn {name}(");
            let start = steps.find(&marker).unwrap_or_else(|| panic!("{name}"));
            let rest = &steps[start..];
            rest[..rest
                .find("\n}\n")
                .expect("function must be brace-terminated")]
                .to_owned()
        };
        let code = |src: &str| {
            src.lines()
                .map(|line| line.split("//").next().unwrap_or(""))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let tools = code(&step("register_startup_tools"));
        let publish = code(&step("publish_mount_and_attach"));
        assert!(tools.contains("crate::module_tool_runtime::tool_authorization("));
        // The notifier asks the same cards (A5c-2b).
        assert!(publish.contains("cards.on_grants_protocol(module)"));
        assert!(
            !code(serve).contains("store_authorization("),
            "serve() must not call store_authorization directly"
        );
        assert!(
            !code(steps).contains("store_authorization("),
            "serve_steps must not call store_authorization directly"
        );
    }

    /// #1202 G6 (A5c-2b): a module is on the grants protocol when a tool of
    /// it takes grants; one whose tools do not, or with none, is not.
    #[tokio::test]
    async fn the_cards_say_which_modules_are_on_the_grants_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, _) = decision_state(&running, &dir.path().join("os.json")).await;
        let cards = state.module_tool_cards.unwrap();
        assert!(cards.on_grants_protocol("doc-module"));
        assert!(!cards.on_grants_protocol("old-module"));
        assert!(!cards.on_grants_protocol("absent"));
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
        agent24_os_packages::test_support::seal_exit_stub(&dir, "bin/mod").unwrap();
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

    /// A scan thread that panics is a failed start, not an empty catalogue.
    /// An empty catalogue is what "no packages installed" looks like.
    #[tokio::test]
    async fn a_panicked_package_scan_is_a_startup_error() {
        let joined = tokio::task::spawn_blocking(|| {
            panic!("scan blew up");
        })
        .await;
        let err = super::catalogue_from_join(joined).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("discovering domain OS packages failed"),
            "{msg}"
        );
        assert!(msg.contains("panicked"), "{msg}");
    }

    /// A shutdown during discovery must not put the package on the catalogue.
    /// Passing `CancellationToken::new()` instead of the caller's token
    /// discovers `cos72` and turns this red.
    #[test]
    fn a_cancelled_discovery_does_not_catalogue_the_package() {
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");
        let cancel = CancellationToken::new();
        cancel.cancel();
        let out = super::with_discovered(vec![built_in("sin90")], root.path(), &cancel);
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
        assert_eq!(names, vec!["sin90"]);
    }

    #[test]
    fn a_package_placed_after_the_build_reaches_the_catalogue() {
        // The scanner having found it is not the same claim as the daemon having
        // USED it. This is the wiring, which was previously only assertable by
        // reading `serve`.
        let root = tempfile::tempdir().unwrap();
        pkg(root.path(), "cos72");

        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
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

        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
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
        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        let names: Vec<&str> = out.catalogue.iter().map(|i| i.name.as_str()).collect();
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

        let out = super::with_discovered(
            vec![built_in("sin90")],
            root.path(),
            &CancellationToken::new(),
        );
        assert_eq!(
            out.catalogue.len(),
            2,
            "both entries exist; the mounter decides"
        );
        assert_eq!(
            out.catalogue[0].version, "9.9.9",
            "the built-in must come FIRST"
        );
        assert_eq!(out.catalogue[1].version, "0.1.0");
    }

    #[test]
    fn discovery_failing_does_not_empty_the_built_in_catalogue() {
        // A missing or unreadable packages root must not cost the user the modules
        // that were compiled in. "No third-party packages" is the normal case.
        let out = super::with_discovered(
            vec![built_in("sin90")],
            std::path::Path::new("/nonexistent/agent24-packages"),
            &CancellationToken::new(),
        );
        assert_eq!(out.catalogue.len(), 1);
        assert_eq!(out.catalogue[0].name, "sin90");
    }

    use super::*;
    use axum::http::Method;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    // These kernel paths do not yet have OpenAPI entries. Keep each reason and
    // owning follow-up beside the path: M2-02/M2-04 remove their entries when
    // those contracts land; the remaining exceptions have separate owners.
    const UNDOCUMENTED_KERNEL_ROUTES: &[&str] = &[
        "/capabilities/creative", // OD-M11 (PAUSED): capability contract is out of scope.
        "/events", // WebSocket protocol is documented by protocol/events.schema.json.
        // KB-HOST-01: the desktop host's private interface (UI-BRIDGE U1.5), not a client contract.
        "/host/knowledge/bindings/{binding_id}",
        "/host/knowledge/bindings/{binding_id}/credential",
        // CORE-2 AgentEar S3: the desktop main process's confirmation channel, ProductHost
        // only (slice §3.4) — private to the desktop, not a client contract.
        "/presence/challenges/next",
        "/presence/challenges/{id}",
    ];

    // Module-owned contract paths appear in OpenAPI but are not literal routes
    // in build_router_with_modules. `/sin90/*` is an external contract
    // (ADR-045 D5, Q4 (iii)): it stays here, tagged `x-external-contract`, and the
    // implementation is owned by iDoris-ai/Sin90.
    const MODULE_SURFACE_PATHS: &[&str] = &[
        "/sin90/directions",
        "/sin90/schedule-blocks",
        "/sin90/schedule-blocks/{id}",
        "/sin90/proposals",
        "/sin90/proposals/{id}",
        "/sin90/proposals/{id}/accept",
        "/sin90/attention",
        // Documenting OS (ADR-DOC-02 §4), served by rust/apps/agent24-documents.
        "/documents/capabilities",
        "/documents/documents",
        "/documents/documents/{document_id}",
        "/documents/uploads",
        "/documents/uploads/{upload_id}/chunks",
        "/documents/imports",
        "/documents/jobs/{job_id}",
        "/documents/jobs/{job_id}/cancel",
        "/documents/jobs/{job_id}/retry",
        "/documents/documents/{document_id}/revisions/{revision}/pages/{page}",
        "/documents/documents/{document_id}/revisions/{revision}/text",
        "/documents/documents/{document_id}/find",
        "/documents/documents/{document_id}/extractions",
        "/documents/extractions/{extraction_id}",
    ];

    // Module-internal channel (ADR-045 D6): `_a24/*` is kernel <-> module plumbing,
    // not a client API. It is deliberately not in OpenAPI (api-client would
    // generate it as a callable surface) and the kernel proxy answers 404 to
    // clients for it (`agent24-os-proto` RESERVED_SEGMENT). Every `_a24` literal
    // in a module source must be listed here, so the gate does not have a blind
    // spot for routes that are neither in OpenAPI nor in an exemption table.
    const MODULE_INTERNAL_CHANNEL_ROUTES: &[(&str, &str)] = &[
        ("POST", "/_a24/tools/{op}"),
        ("POST", "/_a24/authorization/revoked"),
    ];

    const DOCUMENTS_ROUTE_SOURCE: (&str, &str) = (
        include_str!("../../agent24-documents/src/lib.rs"),
        "pub fn router(",
    );

    // COMM routes are mounted by server::serve, outside the scanned builder, so
    // their literals are scanned from the agent24-comm sources instead
    // (`comm_route_literals`); they are covered by the same gate, no exemption.
    const COMM_ROUTE_SOURCES: [(&str, &str); 3] = [
        (
            include_str!("../../../crates/agent24-comm/src/router.rs"),
            "pub fn router(",
        ),
        (
            include_str!("../../../crates/agent24-comm/src/groupchat.rs"),
            "pub(crate) fn routes(",
        ),
        (include_str!("comm_routes.rs"), "fn runtime_router("),
    ];

    const HTTP_METHODS: [&str; 7] = ["get", "put", "post", "delete", "patch", "head", "options"];

    /// Calls other than the HTTP methods that may follow a route at its top level: they
    /// configure the handler, they do not add or hide a method.
    const ROUTE_MODIFIERS: [&str; 3] = ["layer", "route_layer", "with_state"];

    /// `(METHOD, path)` pairs for the `.route("<path>", <handlers>)` literals inside the
    /// function starting at `anchor`. Methods are the `get(..)`/`post(..)`/... calls at the
    /// top level of the route's own argument list, so `get(a).post(b)` yields both while a
    /// nested `.layer(..)` argument is not read. The scan is fail-closed: a non-literal path,
    /// a route with no readable method, or any other top-level call that could add or hide a
    /// method (`any`, `on`, `merge`, `trace`, ...) panics instead of dropping out of the gate.
    fn route_ops(source: &str, anchor: &str) -> Vec<(String, String)> {
        let start = source
            .find(anchor)
            .unwrap_or_else(|| panic!("{anchor} must exist"));
        let body = &source[start..];
        let body = &body[..body
            .find("\n}\n")
            .expect("function must be brace-terminated")];
        let mut ops = Vec::new();
        for (offset, _) in body.match_indices(".route(") {
            let args = &body[offset + ".route(".len()..];
            let trimmed = args.trim_start();
            assert!(
                trimmed.starts_with('"'),
                "route path must be a string literal, got: {:.40}",
                trimmed
            );
            let open = args.len() - trimmed.len() + 1;
            let path_len = args[open..].find('"').expect("route path literal end");
            let path = args[open..open + path_len].to_owned();
            let chars: Vec<char> = args[open + path_len + 1..].chars().collect();
            let (mut depth, mut i, mut found) = (1_i32, 0, 0);
            while i < chars.len() && depth > 0 {
                let c = chars[i];
                if c == '/' && chars.get(i + 1) == Some(&'/') {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    continue;
                }
                if c == '/' && chars.get(i + 1) == Some(&'*') {
                    i += 2;
                    while i < chars.len() && !(chars[i] == '*' && chars.get(i + 1) == Some(&'/')) {
                        i += 1;
                    }
                    i += 2;
                    continue;
                }
                if c == '"' {
                    i += 1;
                    while i < chars.len() && chars[i] != '"' {
                        i += if chars[i] == '\\' { 2 } else { 1 };
                    }
                    i += 1;
                    continue;
                }
                if c == '(' {
                    depth += 1;
                } else if c == ')' {
                    depth -= 1;
                } else if c.is_ascii_alphabetic() || c == '_' {
                    let begin = i;
                    while i < chars.len() && (chars[i].is_ascii_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    let ident: String = chars[begin..i].iter().collect();
                    let mut j = i;
                    while j < chars.len() && chars[j].is_whitespace() {
                        j += 1;
                    }
                    if depth == 1 && chars.get(j) == Some(&'(') {
                        if HTTP_METHODS.contains(&ident.as_str()) {
                            ops.push((ident.to_ascii_uppercase(), path.clone()));
                            found += 1;
                        } else {
                            assert!(
                                ROUTE_MODIFIERS.contains(&ident.as_str()),
                                "route {path}: unsupported top-level call `{ident}(..)`; the \
                                 gate cannot tell which methods it serves"
                            );
                        }
                    }
                    continue;
                }
                i += 1;
            }
            assert!(
                found > 0,
                "route {path} has no readable get/post/... handler"
            );
        }
        ops
    }

    /// `"METHOD path"` for every operation under `paths` in the OpenAPI document.
    fn openapi_ops(openapi_source: &str) -> std::collections::BTreeSet<(String, String)> {
        let document: serde_yaml::Value =
            serde_yaml::from_str(openapi_source).expect("protocol/openapi.yaml must parse as YAML");
        let paths = document
            .get("paths")
            .and_then(serde_yaml::Value::as_mapping)
            .expect("OpenAPI document must contain a paths mapping");
        let mut ops = std::collections::BTreeSet::new();
        for (path, item) in paths {
            let path = path.as_str().expect("path keys are strings");
            for method in HTTP_METHODS {
                if item.get(method).is_some() {
                    ops.insert((method.to_ascii_uppercase(), path.to_owned()));
                }
            }
        }
        ops
    }

    fn route_openapi_mismatches(
        server_source: &str,
        openapi_source: &str,
    ) -> (Vec<String>, Vec<String>) {
        use std::collections::BTreeSet;

        let mut routes: BTreeSet<(String, String)> = BTreeSet::new();
        for (method, path) in route_ops(server_source, "pub fn build_router_with_modules") {
            if let Some(path) = path.strip_prefix("/api/v1") {
                routes.insert((method, path.to_owned()));
            }
        }
        // CORE-5f-2: the docked router is a second router, scanned the same way.
        for (method, path) in route_ops(include_str!("docked_router.rs"), "pub fn docked_router") {
            if let Some(path) = path.strip_prefix("/api/v1") {
                routes.insert((method, path.to_owned()));
            }
        }
        assert!(
            routes.len() >= 20,
            "route scan found only {} operations; build_router_with_modules may have changed shape",
            routes.len()
        );

        let openapi = openapi_ops(openapi_source);

        let undocumented: BTreeSet<&str> = UNDOCUMENTED_KERNEL_ROUTES.iter().copied().collect();
        let module_paths: BTreeSet<&str> = MODULE_SURFACE_PATHS.iter().copied().collect();
        let mut comm = 0;
        for (source, anchor) in COMM_ROUTE_SOURCES {
            for (method, path) in route_ops(source, anchor) {
                routes.insert((method, format!("/comm{path}")));
                comm += 1;
            }
        }
        assert!(comm >= 20, "comm route scan found only {comm} operations");
        let expected_openapi: BTreeSet<(String, String)> = routes
            .iter()
            .filter(|(_, path)| !undocumented.contains(path.as_str()))
            .cloned()
            .collect();
        let actual_openapi: BTreeSet<(String, String)> = openapi
            .iter()
            .filter(|(_, path)| !module_paths.contains(path.as_str()))
            .cloned()
            .collect();

        let missing_from_openapi = expected_openapi
            .difference(&actual_openapi)
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        let extra_in_openapi = actual_openapi
            .difference(&expected_openapi)
            .map(|(method, path)| format!("{method} {path}"))
            .collect();
        (missing_from_openapi, extra_in_openapi)
    }

    fn assert_route_openapi_covered(server_source: &str, openapi_source: &str) {
        let (missing_from_openapi, extra_in_openapi) =
            route_openapi_mismatches(server_source, openapi_source);
        assert!(
            missing_from_openapi.is_empty(),
            "kernel operations (METHOD path) missing from OpenAPI (except UNDOCUMENTED_KERNEL_ROUTES): {missing_from_openapi:?}"
        );
        assert!(
            extra_in_openapi.is_empty(),
            "OpenAPI operations (METHOD path) without a kernel route (except MODULE_SURFACE_PATHS): {extra_in_openapi:?}"
        );
    }

    #[test]
    fn kernel_routes_and_openapi_paths_have_no_unlisted_drift() {
        let routes = include_str!("server.rs");
        let openapi = include_str!("../../../../protocol/openapi.yaml");
        assert_route_openapi_covered(routes, openapi);
    }

    #[test]
    fn route_openapi_gate_rejects_an_undocumented_kernel_route() {
        let routes = include_str!("server.rs").replace(
            ".route(\"/api/v1/health\"",
            ".route(\"/api/v1/not-in-openapi\", get(health))\n        .route(\"/api/v1/health\"",
        );
        let openapi = include_str!("../../../../protocol/openapi.yaml");
        let (missing_from_openapi, extra_in_openapi) = route_openapi_mismatches(&routes, openapi);
        assert_eq!(missing_from_openapi, ["GET /not-in-openapi"]);
        assert!(extra_in_openapi.is_empty());
    }

    /// The comm scan reads `.route(` literals out of two functions. A route
    /// added through `.merge(`/`.nest(` of another module (or a second
    /// `routes()`) would be invisible to it AND absent from OpenAPI, i.e. the
    /// gate would pass an unregistered route. Pin the composition instead.
    fn comm_composition_is_scanned(router_source: &str) -> Result<(), String> {
        let start = router_source
            .find("pub fn router(")
            .ok_or("pub fn router( must exist")?;
        let body = &router_source[start..];
        let body = &body[..body
            .find("\n}\n")
            .ok_or("router must be brace-terminated")?];
        let composed: Vec<&str> = body
            .match_indices(".merge(")
            .chain(body.match_indices(".nest("))
            .chain(body.match_indices(".fallback"))
            .chain(body.match_indices(".route_service("))
            .map(|(i, _)| body[i..].lines().next().unwrap_or("").trim())
            .collect();
        if composed == [".merge(crate::groupchat::routes())"] {
            Ok(())
        } else {
            Err(format!(
                "comm router composes unscanned routes; extend COMM_ROUTE_SOURCES: {composed:?}"
            ))
        }
    }

    #[test]
    fn comm_route_scan_covers_every_composed_module() {
        let real = COMM_ROUTE_SOURCES[0].0;
        comm_composition_is_scanned(real).unwrap();
        let sneaky = real.replace(
            ".merge(crate::groupchat::routes())",
            ".merge(crate::groupchat::routes())\n        .merge(crate::other::routes())",
        );
        assert!(comm_composition_is_scanned(&sneaky).is_err());
        let nested = real.replace(
            ".merge(crate::groupchat::routes())",
            ".merge(crate::groupchat::routes())\n        .nest(\"/x\", y())",
        );
        assert!(comm_composition_is_scanned(&nested).is_err());
    }

    #[test]
    fn route_openapi_gate_covers_comm_routes() {
        let routes = include_str!("server.rs");
        let openapi = include_str!("../../../../protocol/openapi.yaml")
            .replace("  /comm/groupchat:\n", "  /comm/groupchat-gone:\n");
        let (missing_from_openapi, extra_in_openapi) = route_openapi_mismatches(routes, &openapi);
        assert_eq!(
            missing_from_openapi,
            ["GET /comm/groupchat", "POST /comm/groupchat"]
        );
        assert_eq!(
            extra_in_openapi,
            ["GET /comm/groupchat-gone", "POST /comm/groupchat-gone"]
        );
    }

    #[test]
    fn route_openapi_gate_rejects_an_unimplemented_openapi_path() {
        let routes = include_str!("server.rs");
        let openapi = include_str!("../../../../protocol/openapi.yaml").replace(
            "paths:\n",
            "paths:\n  /not-in-router:\n    get:\n      responses: {}\n",
        );
        let (missing_from_openapi, extra_in_openapi) = route_openapi_mismatches(routes, &openapi);
        assert!(missing_from_openapi.is_empty());
        assert_eq!(extra_in_openapi, ["GET /not-in-router"]);
    }

    /// Positive control for the method-level gate: dropping ONE method of a path that
    /// still has another method must turn the gate red. A path-only comparison cannot
    /// see this, because `/approvals/{id}` stays present either way.
    #[test]
    fn route_openapi_gate_rejects_a_missing_method_on_a_present_path() {
        let routes = include_str!("server.rs");
        let openapi = include_str!("../../../../protocol/openapi.yaml");
        assert_route_openapi_covered(routes, openapi);

        let without_get = openapi.replace(
            "    get:\n      operationId: getApproval\n",
            "    x-dropped:\n      operationId: getApproval\n",
        );
        assert_ne!(without_get, openapi, "the getApproval operation must exist");
        let (missing, extra) = route_openapi_mismatches(routes, &without_get);
        assert_eq!(missing, ["GET /approvals/{id}"]);
        assert!(extra.is_empty());
    }

    /// The reverse direction: an OpenAPI method the router does not serve.
    #[test]
    fn route_openapi_gate_rejects_an_unserved_method_on_a_present_path() {
        let original = include_str!("server.rs");
        let routes = original.replace(
            "get(crate::approvals::get_approval).post(crate::approvals::decide_approval)",
            "post(crate::approvals::decide_approval)",
        );
        assert_ne!(routes, original, "the approvals route literal must exist");
        let openapi = include_str!("../../../../protocol/openapi.yaml");
        let (missing, extra) = route_openapi_mismatches(&routes, openapi);
        assert!(missing.is_empty());
        assert_eq!(extra, ["GET /approvals/{id}"]);
    }

    /// B1: the registered `GET /api/v1/approvals/{id}` is a documented operation.
    #[test]
    fn get_approval_by_id_is_documented_and_registered() {
        let openapi = openapi_ops(include_str!("../../../../protocol/openapi.yaml"));
        assert!(openapi.contains(&("GET".to_owned(), "/approvals/{id}".to_owned())));
        let routes = route_ops(
            include_str!("server.rs"),
            "pub fn build_router_with_modules",
        );
        assert!(routes.contains(&("GET".to_owned(), "/api/v1/approvals/{id}".to_owned())));
    }

    /// The method reader sees chained handlers, ignores nested arguments, and refuses a
    /// route it cannot read instead of dropping it from the gate.
    #[test]
    fn route_ops_reads_chained_methods_and_rejects_unreadable_routes() {
        let src = "pub fn r() {\n    x\n        .route(\"/a/{id}\", get(h).post(g))\n        .route(\"/b\", put(h).layer(Limit::max(1)))\n        .route(\"/c\", axum::routing::delete(h))\n}\n";
        assert_eq!(
            route_ops(src, "pub fn r("),
            [
                ("GET".to_owned(), "/a/{id}".to_owned()),
                ("POST".to_owned(), "/a/{id}".to_owned()),
                ("PUT".to_owned(), "/b".to_owned()),
                ("DELETE".to_owned(), "/c".to_owned()),
            ]
        );
        for bad in [
            ".route(\"/d\", any(h))",
            ".route(\"/d\", any(h).post(g))",
            ".route(\"/d\", get(h).on(MethodFilter::DELETE, g))",
            ".route(\"/d\", get(h).merge(other()))",
            ".route(\"/d\", get(h).trace(g))",
            ".route(PATH, get(h))\n        .route(\"/b\", get(h))",
        ] {
            let src = format!("pub fn r() {{\n    x\n        {bad}\n}}\n");
            assert!(
                std::panic::catch_unwind(|| route_ops(&src, "pub fn r(")).is_err(),
                "{bad} must be refused"
            );
        }
        // comments and string literals are not code
        let noisy = "pub fn r() {\n    x.route(\"/e\", get(h) /* any(x) */ // on(y)\n        .layer(L::new(\"on(\")))\n}\n";
        assert_eq!(
            route_ops(noisy, "pub fn r("),
            [("GET".to_owned(), "/e".to_owned())]
        );
    }

    /// B3 / ADR-045 D6: the `_a24/*` routes of `router()` in the documents module are listed
    /// as module-internal channels, in both directions (a new one fails, a stale entry
    /// fails). Separately, the count of `"/_a24` literals in the whole non-test source of
    /// `agent24-documents/src/lib.rs` must equal the table size, so an `_a24` route added
    /// outside `router()` is caught too. They never appear in OpenAPI.
    /// (Other files of the documents crate are not scanned; `_a24` routes live in `lib.rs`.)
    #[test]
    fn module_internal_channel_routes_match_documents_source() {
        let scan = |source: &str| -> Vec<(String, String)> {
            let mut ops: Vec<_> = route_ops(source, DOCUMENTS_ROUTE_SOURCE.1)
                .into_iter()
                .filter(|(_, path)| path.split('/').any(|seg| seg.eq_ignore_ascii_case("_a24")))
                .collect();
            ops.sort();
            ops
        };
        let mut listed: Vec<(String, String)> = MODULE_INTERNAL_CHANNEL_ROUTES
            .iter()
            .map(|(m, p)| ((*m).to_owned(), (*p).to_owned()))
            .collect();
        listed.sort();
        assert_eq!(scan(DOCUMENTS_ROUTE_SOURCE.0), listed);

        // positive controls: an unlisted `_a24` route, and a listed route that is gone
        let extra = DOCUMENTS_ROUTE_SOURCE.0.replace(
            ".route(\"/_a24/authorization/revoked\"",
            ".route(\"/_a24/new/thing\", post(authorization::revoked))\n        .route(\"/_a24/authorization/revoked\"",
        );
        assert_ne!(extra, DOCUMENTS_ROUTE_SOURCE.0);
        assert_ne!(scan(&extra), listed);
        let gone = DOCUMENTS_ROUTE_SOURCE
            .0
            .replace("\"/_a24/authorization/revoked\"", "\"/gone\"");
        assert_ne!(gone, DOCUMENTS_ROUTE_SOURCE.0);
        assert_ne!(scan(&gone), listed);

        let non_test = DOCUMENTS_ROUTE_SOURCE
            .0
            .split("#[cfg(test)]")
            .next()
            .expect("split yields at least one part");
        assert_eq!(
            non_test.matches("\"/_a24").count(),
            MODULE_INTERNAL_CHANNEL_ROUTES.len(),
            "an `_a24` literal outside the table (or outside router()) in agent24-documents/src/lib.rs"
        );

        // every `_a24` route is kept out of the client contract
        let openapi = include_str!("../../../../protocol/openapi.yaml");
        assert!(
            openapi_ops(openapi)
                .iter()
                .all(|(_, path)| !path.contains("_a24")),
            "OpenAPI must not contain _a24 paths"
        );
        // the scan sees the module's ordinary routes too
        assert!(
            route_ops(DOCUMENTS_ROUTE_SOURCE.0, DOCUMENTS_ROUTE_SOURCE.1)
                .iter()
                .any(|(_, p)| p == "/documents/{document_id}"),
        );
    }

    /// D5 / Q4 (iii): every `/sin90/*` operation is marked as an external contract, so the
    /// exemption in MODULE_SURFACE_PATHS is visible in the contract itself.
    #[test]
    fn sin90_operations_are_marked_external_contract() {
        let doc: serde_yaml::Value =
            serde_yaml::from_str(include_str!("../../../../protocol/openapi.yaml")).unwrap();
        let paths = doc
            .get("paths")
            .and_then(serde_yaml::Value::as_mapping)
            .unwrap();
        let mut seen = 0;
        for (path, item) in paths {
            let path = path.as_str().unwrap();
            if !path.starts_with("/sin90/") {
                continue;
            }
            for method in HTTP_METHODS {
                if let Some(op) = item.get(method) {
                    seen += 1;
                    assert_eq!(
                        op.get("x-external-contract")
                            .and_then(serde_yaml::Value::as_str),
                        Some("iDoris-ai/Sin90"),
                        "{method} {path}"
                    );
                }
            }
        }
        assert_eq!(seen, 10, "ADR-045 D5 counts 10 sin90 operations");
    }

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

    /// In-flight model calls (and the usage recorder) are cut off at the drain plus the 2s
    /// call-cancel grace, however long the stop grace is: attached modules get no SIGTERM,
    /// so the 15s a package gets to flush must not keep their calls alive (ADR-038 D5/D6).
    #[tokio::test(start_paused = true)]
    async fn model_calls_are_cut_off_by_the_cancel_grace_not_the_stop_grace() {
        let shutdown = Shutdown::new(CancellationToken::new());
        let began = tokio::time::Instant::now();
        let _ = shutdown.deadlines();
        shutdown.token().cancel();
        shutdown.calls_cut_off().await;
        let p = crate::lifecycle::Params::default();
        assert_eq!(
            began.elapsed(),
            p.drain + crate::lifecycle::CALL_CANCEL_GRACE
        );
        assert!(
            began.elapsed() < p.drain + p.stop_grace,
            "not the stop grace"
        );
        // a stop grace shorter than the cancel grace still ends it first
        let short = crate::lifecycle::Params {
            drain: Duration::from_millis(100),
            stop_grace: Duration::from_millis(500),
        };
        assert_eq!(
            short.deadlines(began).calls - began,
            Duration::from_millis(600)
        );
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

    pub(crate) async fn state_with_router(router: Arc<ModelRouter>) -> AppState {
        state_with_guardian_and_router(None, router).await
    }

    async fn state_with_guardian(
        guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    ) -> AppState {
        state_with_guardian_and_router(guardian, Arc::new(ModelRouter::with_defaults(vec![]))).await
    }

    async fn state_with_guardian_and_router(
        guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
        router: Arc<ModelRouter>,
    ) -> AppState {
        AppState::new(AppDeps {
            token: "testtoken".to_owned(),
            router,
            tools: agent24_tools::ToolRegistry::new(),
            store: Store::open_memory().await.unwrap(),
            shutdown: Shutdown::new(CancellationToken::new()),
            guardian,
            memory: None,
            workspace_service: None,
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

    #[test]
    fn idoris_shutdown_budget_includes_drain_and_frozen_term_grace() {
        let shutdown = Shutdown::new(CancellationToken::new());
        shutdown
            .idoris_budget
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let deadlines = shutdown.deadlines();
        assert!(deadlines.persist - deadlines.began >= std::time::Duration::from_secs(50));
        assert!(deadlines.watchdog > deadlines.persist);
        assert!(deadlines.modules < deadlines.persist);
    }

    pub(crate) async fn capability_state() -> (AppState, String, String) {
        let mut state = state().await;
        let now = crate::capabilities::unix_now();
        let store = crate::capabilities::CapabilityStore::new("daemon-test");
        let host = store
            .mint_product_host("host-test", Duration::from_secs(3_600), now)
            .unwrap();
        let (host_bearer, _, _) = host.into_bearer_parts();
        let host_authorization = store
            .validate_bearer(
                &host_bearer,
                crate::capabilities::Operation::ModelsRead,
                &crate::capabilities::Resource::global(),
                now,
            )
            .unwrap();
        let creative = store
            .mint_creative_authorized(
                &host_authorization,
                crate::capabilities::CreativeMintRequest::new(
                    "workspace-test",
                    "attachment-test",
                    "principal-test",
                    "sidecar-test",
                    Duration::from_secs(300),
                    now,
                ),
            )
            .unwrap();
        let (creative_bearer, _, _) = creative.into_bearer_parts();
        state.enable_capability_auth(store);
        state.enable_desktop_presence();
        (state, host_bearer, creative_bearer)
    }

    #[tokio::test]
    async fn component_install_requires_host_authority_before_dispatch() {
        let (state, host, creative) = capability_state().await;
        let modules = Router::new().route(
            "/api/v1/comm/install",
            axum::routing::post(|| async { StatusCode::NO_CONTENT }),
        );
        let app = build_router_with_modules(state, modules);
        for (bearer, expected) in [
            (None, StatusCode::UNAUTHORIZED),
            (Some(creative), StatusCode::FORBIDDEN),
            (Some(host), StatusCode::NO_CONTENT),
        ] {
            let mut req = Request::builder()
                .method(Method::POST)
                .uri("/api/v1/comm/install");
            if let Some(bearer) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), expected);
        }
    }

    #[tokio::test]
    async fn key_backup_is_product_host_only_in_capabilities_mode() {
        let (state, host, creative) = capability_state().await;
        let modules = Router::new()
            .route(
                "/api/v1/comm/identity/export",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/v1/comm/identity/import",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            );
        let app = build_router_with_modules(state, modules);
        for (uri, bearer, expected) in [
            (
                "/api/v1/comm/identity/export",
                None,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/v1/comm/identity/export",
                Some(creative.clone()),
                StatusCode::FORBIDDEN,
            ),
            (
                "/api/v1/comm/identity/export",
                Some(host.clone()),
                StatusCode::NO_CONTENT,
            ),
            (
                "/api/v1/comm/identity/import",
                None,
                StatusCode::UNAUTHORIZED,
            ),
            (
                "/api/v1/comm/identity/import",
                Some(creative),
                StatusCode::FORBIDDEN,
            ),
            (
                "/api/v1/comm/identity/import",
                Some(host),
                StatusCode::NO_CONTENT,
            ),
        ] {
            let mut req = Request::builder().method(Method::POST).uri(uri);
            if let Some(bearer) = bearer {
                req = req.header(header::AUTHORIZATION, format!("Bearer {bearer}"));
            }
            let res = app
                .clone()
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), expected);
        }
    }

    #[tokio::test]
    async fn key_backup_is_refused_in_legacy_mode_even_with_the_right_token() {
        let modules = Router::new()
            .route(
                "/api/v1/comm/identity/export",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/v1/comm/identity/import",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/api/v1/comm/contact",
                axum::routing::post(|| async { StatusCode::NO_CONTENT }),
            );
        let app = build_router_with_modules(state().await, modules);
        for (uri, expected) in [
            ("/api/v1/comm/identity/export", StatusCode::FORBIDDEN),
            ("/api/v1/comm/identity/import", StatusCode::FORBIDDEN),
            ("/api/v1/comm/contact", StatusCode::NO_CONTENT), // other routes unchanged
        ] {
            let res = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri(uri)
                        .header(header::AUTHORIZATION, "Bearer testtoken")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(res.status(), expected, "{uri}");
        }
    }

    /// Through the REAL comm router (nested exactly as production nests it),
    /// not a stub handler: the restriction must hold on the actual route.
    #[tokio::test]
    async fn key_backup_restriction_holds_on_the_real_comm_router() {
        // Export names a path; restore carries the backup text itself (the
        // desktop reads the file, the daemon never opens a caller-named path).
        let body = |uri: &str| {
            Body::from(if uri.ends_with("/export") {
                r#"{"nickname":"a","path":"/tmp/x.ncryptsec","backup_password":"long-enough-1"}"#
            } else {
                r#"{"nickname":"a","ncryptsec":"ncryptsec1qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq","backup_password":"long-enough-1"}"#
            })
        };
        for uri in [
            "/api/v1/comm/identity/export",
            "/api/v1/comm/identity/import",
        ] {
            let post = |bearer: &str| {
                Request::builder()
                    .method(Method::POST)
                    .uri(uri)
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
                    .body(body(uri))
                    .unwrap()
            };
            let dir = tempfile::tempdir().unwrap();
            let slot = Arc::new(crate::comm_routes::CommDaemonSlot::new());

            // legacy mode, correct token: refused
            let comm =
                crate::comm_routes::build(dir.path(), Duration::from_millis(500), &slot).await;
            let legacy = build_router_with_modules(state().await, comm);
            let res = legacy.oneshot(post("testtoken")).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);

            // capabilities mode: Creative refused, ProductHost reaches the handler
            // (no Hyphae binary here, so it answers its own not_configured)
            let (state, host, creative) = capability_state().await;
            let comm =
                crate::comm_routes::build(dir.path(), Duration::from_millis(500), &slot).await;
            let app = build_router_with_modules(state, comm);
            let res = app.clone().oneshot(post(&creative)).await.unwrap();
            assert_eq!(res.status(), StatusCode::FORBIDDEN);
            let res = app.oneshot(post(&host)).await.unwrap();
            assert_ne!(res.status(), StatusCode::FORBIDDEN);
            assert_eq!(res.status(), StatusCode::CONFLICT);
        }
    }

    #[tokio::test]
    async fn hot_comm_router_preserves_core2_matched_templates() {
        let (state, host, _) = capability_state().await;
        let dir = tempfile::tempdir().unwrap();
        let slot = Arc::new(crate::comm_routes::CommDaemonSlot::new());
        let comm = crate::comm_routes::build(dir.path(), Duration::from_millis(500), &slot).await;
        let app = build_router_with_modules(state, comm);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/v1/comm/identity")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(response).await["error"], "not_configured");
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
    async fn health_reports_protocol_version_and_applied_migrations() {
        let mut app = state().await;
        let memory = agent24_memory::KvStore::open_memory().await.unwrap();
        let store_applied: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(agent24_store::test_hooks::pool(&app.store))
                .await
                .unwrap();
        let memory_applied: Option<i64> =
            sqlx::query_scalar("SELECT MAX(version) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(agent24_memory::test_hooks::pool(&memory))
                .await
                .unwrap();
        let store_applied = store_applied.unwrap();
        let memory_applied = memory_applied.unwrap();
        assert!(store_applied > 0, "run database has no applied migration");
        assert!(
            memory_applied > 0,
            "memory database has no applied migration"
        );
        app.memory_kv = Some(memory);

        let res = build_router(app)
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
        assert_eq!(
            json["protocol_version"].as_u64(),
            Some(u64::from(agent24_protocol::PROTOCOL_VERSION))
        );
        assert_eq!(json["migrations"]["store"].as_i64(), Some(store_applied));
        assert_eq!(json["migrations"]["memory"].as_i64(), Some(memory_applied));
    }

    #[tokio::test]
    async fn module_consent_listing_route_is_available_to_the_host() {
        let router = build_router(state().await);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert!(body["authorizations"].is_array());
    }

    #[tokio::test]
    async fn module_consent_export_contains_metadata_only_and_revoke_broadcasts() {
        let state = state().await;
        let summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "read_document",
            "1.2.3",
            agent24_store::ConsentSource::FirstParty,
            Some("local documents".to_owned()),
            None,
            Some("https://example.invalid".to_owned()),
            agent24_store::HostRiskLevel::Medium,
        );
        state
            .store
            .grant_module_consent(&summary, "2026-10-08T00:00:00Z", "2099-01-01T00:00:00Z")
            .await
            .unwrap();
        let second_summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "write_document",
            "1.2.3",
            agent24_store::ConsentSource::FirstParty,
            None,
            Some("local documents".to_owned()),
            None,
            agent24_store::HostRiskLevel::Medium,
        );
        state
            .store
            .grant_module_consent(
                &second_summary,
                "2026-10-08T00:00:00Z",
                "2099-01-01T00:00:00Z",
            )
            .await
            .unwrap();
        let mut notifications = state.store.subscribe_module_consent_revocations();
        let router = build_router(state);

        let exported = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents/export")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(exported.status(), StatusCode::OK);
        assert_eq!(
            exported.headers()[header::CONTENT_DISPOSITION],
            "attachment; filename=module-authorizations.json"
        );
        let bytes = exported.into_body().collect().await.unwrap().to_bytes();
        let serialized = String::from_utf8(bytes.to_vec()).unwrap();
        let export: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        let authorization = &export["authorizations"][0];
        assert_eq!(authorization["module"], "doc-module");
        assert_eq!(authorization["tool"], "read_document");
        assert_eq!(
            authorization["scope_summary"]["readable"],
            "local documents"
        );
        // No module is mounted here and the grant names no package: the gate
        // would refuse it, so the export says stale too (#1290).
        assert_eq!(authorization["status"], "stale");
        for forbidden in ["body", "content", "credential", "token", "password"] {
            assert!(!serialized.to_lowercase().contains(forbidden));
        }

        let revoked = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v1/module-consents/modules/doc-module/tools/read_document")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);
        let notice = notifications.recv().await.unwrap();
        assert_eq!(notice.module, "doc-module");
        assert_eq!(notice.op.as_deref(), Some("read_document"));

        let revoked = router
            .oneshot(
                Request::builder()
                    .method(Method::DELETE)
                    .uri("/api/v1/module-consents/modules/doc-module")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(revoked.status(), StatusCode::OK);
        let notice = notifications.recv().await.unwrap();
        assert_eq!(notice.module, "doc-module");
        assert_eq!(notice.op, None);
    }

    /// K1-6a-G §2.1: a card shows the summary the gate compares a grant
    /// with — its fingerprint, version and the running package's identity —
    /// and there is none for a tool that does not exist or a module with no
    /// running package.
    #[tokio::test]
    async fn a_module_tool_card_shows_what_a_grant_would_be_checked_against() {
        let none_assembled = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents/modules/doc-module/tools/read_document/card")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(none_assembled.status(), StatusCode::NOT_FOUND);
        let mut state = state().await;
        let mut summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "read_document",
            "1.2.3",
            agent24_store::ConsentSource::ManualInstall,
            Some("local documents".to_owned()),
            None,
            None,
            agent24_store::HostRiskLevel::High,
        );
        summary.reversible_draft = true;
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let source = |takes_grants| crate::consent_routes::ToolCardSource {
            summary: summary.clone(),
            resource_kinds: vec!["doc".to_owned()],
            takes_grants,
        };
        let gate = source(true).gate_request();
        let mut old = summary.clone();
        old.module = "old-module".to_owned();
        state.module_tool_cards = Some(crate::consent_routes::ModuleToolCards {
            notice: answering(crate::module_tool_runtime::RevocationStop::NotRunning),
            tools: StdArc::new(std::collections::HashMap::from([
                (
                    ("doc-module".to_owned(), "read_document".to_owned()),
                    source(true),
                ),
                (
                    ("old-module".to_owned(), "read_document".to_owned()),
                    crate::consent_routes::ToolCardSource {
                        summary: old,
                        ..source(false)
                    },
                ),
            ])),
            artifact: {
                let running = StdArc::clone(&running);
                StdArc::new(move |_: &str| {
                    running
                        .load(std::sync::atomic::Ordering::SeqCst)
                        .then(|| "sha256:abc".to_owned())
                })
            },
        });
        let router = build_router(state);
        let get = |uri: &'static str| {
            let router = router.clone();
            async move {
                router
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
        };
        let card = "/api/v1/module-consents/modules/doc-module/tools/read_document/card";
        let response = get(card).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["fingerprint"], gate.current_scope_fingerprint);
        assert_eq!(body["module_version"], gate.module_version);
        assert_eq!(body["module_version"], "1.2.3");
        assert_eq!(body["artifact"], "sha256:abc");
        assert_eq!(body["resource_kinds"], serde_json::json!(["doc"]));
        assert_eq!(body["host_risk"], "high");
        assert_eq!(body["source"], "manual_install");
        assert_eq!(body["scope_summary"]["readable"], "local documents");
        // What the manifest declares, shown as declared; the gate still
        // grants no draft exception on this path (`gate_request`).
        assert_eq!(body["reversible_draft"], true);
        assert!(!gate.reversible_draft_declared);
        let response =
            get("/api/v1/module-consents/modules/doc-module/tools/write_document/card").await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let response =
            get("/api/v1/module-consents/modules/old-module/tools/read_document/card").await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "protocol_too_old"
        );
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        let response = get(card).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(
            body_json(response).await["error"]["code"],
            "no_package_identity"
        );
    }

    /// A revocation notice that answers `stop` and keeps what it was told.
    fn answering_into(
        stop: crate::module_tool_runtime::RevocationStop,
        told: StdArc<std::sync::Mutex<Vec<crate::module_tool_runtime::GrantEnd>>>,
    ) -> crate::consent_routes::RevocationNotice {
        StdArc::new(move |end| {
            told.lock().unwrap().push(end);
            Box::pin(async move { stop }) as futures::future::BoxFuture<'static, _>
        })
    }

    fn answering(
        stop: crate::module_tool_runtime::RevocationStop,
    ) -> crate::consent_routes::RevocationNotice {
        answering_into(stop, StdArc::default())
    }

    /// K1-6a-G §2.5: a revocation tells the module and answers with what it
    /// said; the list shows it for each revoked grant, and a revocation no
    /// answer was recorded for — or one replaced since — reads unconfirmed,
    /// as does one the module did not confirm, which is audited too. It
    /// tells the grants it ended (§2.5.1), and answers its outbox row.
    #[tokio::test]
    async fn a_revocation_answers_whether_the_module_stopped() {
        use crate::module_tool_runtime::RevocationStop::{Confirmed, NotRunning, Unconfirmed};
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &dir.path().join("os.json")).await;
        let store = state.store.clone();
        let told = StdArc::new(std::sync::Mutex::new(Vec::new()));
        let with = |stop| {
            let mut state = state.clone();
            let cards = state.module_tool_cards.as_mut().unwrap();
            cards.notice = answering_into(stop, StdArc::clone(&told));
            build_router(state)
        };
        let delete = |router: Router, uri: &'static str| async move {
            let response = router
                .oneshot(
                    Request::builder()
                        .method("DELETE")
                        .uri(uri)
                        .header("Authorization", "Bearer testtoken")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            body_json(response).await["stop"].clone()
        };
        let stop_listed = |store: agent24_store::Store| async move {
            let views = authorization_list(store).await;
            views["authorizations"][0]["stop"].clone()
        };
        let grant =
            || store.grant_module_consent(&summary, "2026-10-24T00:00:00Z", "2099-01-01T00:00:00Z");
        let granted = grant().await.unwrap();
        assert_eq!(delete(with(Confirmed), DECIDE).await, "confirmed");
        let unconfirmed = |store: agent24_store::Store| async move {
            store
                .list_audit()
                .await
                .unwrap()
                .iter()
                .filter(|e| e.action == "module_consent.stop_unconfirmed")
                .count()
        };
        assert_eq!(
            unconfirmed(store.clone()).await,
            0,
            "a confirmed stop is not audited"
        );
        let told_end = |operation: &str, refs: Vec<String>| {
            Some(crate::module_tool_runtime::GrantEnd {
                module: "doc-module".to_owned(),
                operation: operation.to_owned(),
                ended_at: String::new(),
                reason: "revoked",
                authorization_refs: Some(refs),
            })
        };
        let pop = || {
            told.lock()
                .unwrap()
                .pop()
                .map(|end| crate::module_tool_runtime::GrantEnd {
                    ended_at: String::new(),
                    ..end
                })
        };
        assert_eq!(
            pop(),
            told_end("read_document", vec![granted.authorization_ref()])
        );
        assert_eq!(stop_listed(store.clone()).await, "confirmed");
        let module = "/api/v1/module-consents/modules/doc-module";
        assert_eq!(delete(with(Unconfirmed), module).await, "unconfirmed");
        // The grant was told already: the module's revocation ends none.
        assert_eq!(pop(), told_end("*", Vec::new()));
        assert_eq!(stop_listed(store.clone()).await, "unconfirmed");
        let (pending, _) = store
            .pending_grant_end_notices("2099-01-01T00:00:00Z")
            .await
            .unwrap();
        assert!(
            pending.is_empty(),
            "each route answers its own: {pending:?}"
        );
        assert_eq!(unconfirmed(store.clone()).await, 1);
        // A later revocation of the key while the module answers: the answer
        // is not this revocation's, so it is not confirmed.
        let mut racing = state.clone();
        racing.module_tool_cards.as_mut().unwrap().notice = {
            let store = store.clone();
            StdArc::new(move |end: crate::module_tool_runtime::GrantEnd| {
                let store = store.clone();
                Box::pin(async move {
                    store
                        .revoke_module_consent_tracked(&end.module, None, "2026-10-24T00:30:00Z")
                        .await
                        .unwrap();
                    Confirmed
                }) as futures::future::BoxFuture<'static, _>
            })
        };
        assert_eq!(delete(build_router(racing), module).await, "unconfirmed");
        assert_eq!(unconfirmed(store.clone()).await, 2);
        assert_eq!(delete(with(NotRunning), module).await, "not_running");
        assert_eq!(stop_listed(store.clone()).await, "not_running");
        assert_eq!(
            unconfirmed(store.clone()).await,
            2,
            "a module not running is not audited"
        );
        // Revoked outside this route (the CLI's lifecycle revocation):
        // nothing recorded, so unconfirmed.
        store
            .revoke_module_consent("doc-module", None, "2026-10-24T01:00:00Z")
            .await
            .unwrap();
        assert_eq!(stop_listed(store.clone()).await, "unconfirmed");
        let no_cards = build_router(AppState {
            module_tool_cards: None,
            ..state.clone()
        });
        assert_eq!(delete(no_cards, module).await, "not_running");
        assert_eq!(unconfirmed(store.clone()).await, 2);
        // The notifier answered the row first (§2.5.1): its answer stands,
        // on the row and on the revocation, and the route claims none.
        let mut late = state.clone();
        late.module_tool_cards.as_mut().unwrap().notice = {
            let store = store.clone();
            StdArc::new(move |end: crate::module_tool_runtime::GrantEnd| {
                let store = store.clone();
                Box::pin(async move {
                    let (pending, _) = store
                        .pending_grant_end_notices("2099-01-01T00:00:00Z")
                        .await
                        .unwrap();
                    let row = pending.last().unwrap();
                    assert!(
                        store
                            .record_grant_end_stop(row.id, agent24_store::GrantEndStop::NotRunning)
                            .await
                            .unwrap()
                    );
                    store
                        .record_revocation_stop(&end.module, None, row.generation, "not_running")
                        .await
                        .unwrap();
                    Confirmed
                }) as futures::future::BoxFuture<'static, _>
            })
        };
        assert_eq!(delete(build_router(late), module).await, "unconfirmed");
        assert_eq!(stop_listed(store.clone()).await, "not_running");
        assert_eq!(unconfirmed(store.clone()).await, 3);
        let fresh = grant().await.unwrap();
        let views = authorization_list(store.clone()).await;
        assert!(views["authorizations"][0].get("stop").is_none(), "{views}");
        // A route's confirmed answer settles what its revocation carried
        // (#1202 G7, #1222 gate 3): an end left unconfirmed is carried by the
        // next revocation, and once that is confirmed, by none after it.
        let carries = |fresh: &agent24_store::ModuleConsentRecord| {
            let told = pop().unwrap().authorization_refs.unwrap();
            told.contains(&fresh.authorization_ref())
        };
        assert_eq!(delete(with(Unconfirmed), DECIDE).await, "unconfirmed");
        assert!(carries(&fresh));
        assert_eq!(delete(with(Confirmed), DECIDE).await, "confirmed");
        assert!(carries(&fresh), "carried while unconfirmed");
        assert_eq!(delete(with(Confirmed), DECIDE).await, "confirmed");
        assert!(!carries(&fresh), "settled by the route's confirmed answer");
    }

    /// Deletes `uri` through `state`, and gives the stop it answered.
    async fn revoked_stop(state: AppState, uri: &str) -> serde_json::Value {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(uri)
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        body_json(response).await["stop"].clone()
    }

    /// K1-6a-G §2.5.1 (A5c-2a review round 2): a revocation whose route
    /// cannot audit an unconfirmed answer, or cannot read its row, leaves
    /// both answers to the notifier: the row stays unanswered and the
    /// revocation records nothing; the route claims no stop. Unread, the
    /// module is told no references, and stops by operation.
    #[tokio::test]
    async fn a_revocation_the_route_cannot_finish_is_left_to_the_notifier() {
        use crate::module_tool_runtime::RevocationStop::{Confirmed, Unconfirmed};
        let pool = |store: &agent24_store::Store| agent24_store::test_hooks::pool(store).clone();
        let left = |store: agent24_store::Store| async move {
            let (pending, _) = store
                .pending_grant_end_notices("2099-01-01T00:00:00Z")
                .await
                .unwrap();
            let recorded = store
                .revocation_stop_state("doc-module", "read_document")
                .await
                .unwrap();
            (pending.len(), recorded)
        };
        // The audit fails.
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (mut state, _) = decision_state(&running, &dir.path().join("os.json")).await;
        let store = state.store.clone();
        state.module_tool_cards.as_mut().unwrap().notice = answering(Unconfirmed);
        sqlx::query("CREATE TRIGGER reject_audit BEFORE INSERT ON audit_log BEGIN SELECT RAISE(FAIL, 'offline'); END")
            .execute(&pool(&store))
            .await
            .unwrap();
        assert_eq!(revoked_stop(state, DECIDE).await, "unconfirmed");
        assert_eq!(left(store).await, (1, None));
        // The row cannot be read.
        let dir = tempfile::tempdir().unwrap();
        let (mut state, _) = decision_state(&running, &dir.path().join("os.json")).await;
        let store = state.store.clone();
        let told = StdArc::new(std::sync::Mutex::new(Vec::new()));
        state.module_tool_cards.as_mut().unwrap().notice =
            answering_into(Confirmed, StdArc::clone(&told));
        sqlx::query("CREATE TRIGGER garble AFTER INSERT ON module_grant_end_notices BEGIN UPDATE module_grant_end_notices SET authorization_refs = 'x' WHERE id = NEW.id; END")
            .execute(&pool(&store))
            .await
            .unwrap();
        assert_eq!(revoked_stop(state, DECIDE).await, "unconfirmed");
        assert_eq!(told.lock().unwrap()[0].authorization_refs, None);
        let recorded = store
            .revocation_stop_state("doc-module", "read_document")
            .await
            .unwrap();
        assert_eq!(recorded, None);
    }

    /// K1-6a-G §2.5 (A5b): an expired grant lists what its module answered
    /// when told it ended; one not told yet reads unconfirmed. An expired
    /// refusal started nothing to stop.
    #[tokio::test]
    async fn an_expired_grant_lists_whether_the_module_stopped() {
        let store = agent24_store::Store::open_memory().await.unwrap();
        let summary = |op: &str| {
            agent24_store::ToolPermissionSummary::new(
                "doc-module",
                op,
                "1.2.3",
                agent24_store::ConsentSource::FirstParty,
                Some("local documents".to_owned()),
                None,
                None,
                agent24_store::HostRiskLevel::Low,
            )
        };
        let (decided, expired) = ("2026-01-01T00:00:00Z", "2026-01-02T00:00:00Z");
        let granted = store
            .grant_module_consent(&summary("read_document"), decided, expired)
            .await
            .unwrap();
        store
            .deny_module_consent(&summary("write_document"), decided, expired)
            .await
            .unwrap();
        let stops = |list: serde_json::Value| {
            list["authorizations"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| (a["tool"].clone(), a["status"].clone(), a["stop"].clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            stops(authorization_list(store.clone()).await),
            [
                (
                    "read_document".into(),
                    "expired".into(),
                    "unconfirmed".into()
                ),
                (
                    "write_document".into(),
                    "expired".into(),
                    serde_json::Value::Null
                ),
            ]
        );
        assert!(
            store
                .record_expiry_stop(
                    "doc-module",
                    "read_document",
                    granted.generation,
                    "confirmed"
                )
                .await
                .unwrap()
        );
        assert_eq!(
            stops(authorization_list(store).await)[0].2,
            serde_json::json!("confirmed")
        );
    }

    /// #1290: the list calls a grant what the gate would make of a call —
    /// for the tool and the package as they are now. Only the grant for the
    /// running package reads granted; one with no package identity, one for
    /// another package (replaced in place), one whose package is not
    /// running, one whose tool is not mounted, one of a module not on the
    /// grants protocol (never dispatched, §2.6) and one given for an earlier
    /// version of the tool all read stale.
    #[tokio::test]
    async fn the_list_calls_stale_what_the_gate_would_refuse() {
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &dir.path().join("os.json")).await;
        let (decided, expires) = ("2026-10-24T00:00:00Z", "2099-01-01T00:00:00Z");
        let statuses = |state: AppState| async move {
            let list = listed(state).await;
            let views = list["authorizations"].as_array().unwrap();
            let status = |module: &str| {
                let view = views.iter().find(|view| view["module"] == module).unwrap();
                view["status"].as_str().unwrap().to_owned()
            };
            (
                status("doc-module"),
                status("gone-module"),
                status("old-module"),
            )
        };
        let grant = |summary: agent24_store::ToolPermissionSummary, artifact: Option<&str>| {
            let (store, artifact) = (state.store.clone(), artifact.map(str::to_owned));
            async move {
                let scope = agent24_store::GrantScope {
                    resources: Vec::new(),
                    artifact,
                };
                store
                    .grant_module_consent_scoped(&summary, &scope, decided, expires)
                    .await
                    .unwrap();
            }
        };
        // The tool is gone from the running package: never dispatched.
        let gone = agent24_store::ToolPermissionSummary {
            module: "gone-module".to_owned(),
            ..summary.clone()
        };
        grant(gone, Some("sha256:abc")).await;
        // Version, scope and package all match, but the module is not on the
        // grants protocol: the gate never dispatches it.
        let old_protocol = agent24_store::ToolPermissionSummary {
            module: "old-module".to_owned(),
            ..summary.clone()
        };
        grant(old_protocol, Some("sha256:abc")).await;

        grant(summary.clone(), Some("sha256:abc")).await;
        assert_eq!(
            statuses(state.clone()).await,
            ("granted".to_owned(), "stale".to_owned(), "stale".to_owned())
        );
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(statuses(state.clone()).await.0, "stale");
        running.store(true, std::sync::atomic::Ordering::SeqCst);
        // Given for 1.0.0 of a tool that is 1.2.3 now: judged by what it is now.
        let earlier = agent24_store::ToolPermissionSummary {
            module_version: "1.0.0".to_owned(),
            ..summary.clone()
        };
        grant(earlier, Some("sha256:abc")).await;
        assert_eq!(statuses(state.clone()).await.0, "stale");
        grant(summary.clone(), Some("sha256:replaced-in-place")).await;
        assert_eq!(statuses(state.clone()).await.0, "stale");
        grant(summary, None).await;
        assert_eq!(statuses(state.clone()).await.0, "stale");
        // No identity on either side is no match.
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(statuses(state).await.0, "stale");
    }

    async fn authorization_list(store: agent24_store::Store) -> serde_json::Value {
        let mut state = state().await;
        state.store = store;
        listed(state).await
    }

    async fn listed(state: AppState) -> serde_json::Value {
        let response = build_router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        body_json(response).await
    }

    /// A state with one grants-protocol tool (`doc-module.read_document`,
    /// kinds `doc`) and one older one, running while `running` holds, and
    /// decisions open against `os_json`.
    async fn decision_state(
        running: &StdArc<std::sync::atomic::AtomicBool>,
        os_json: &std::path::Path,
    ) -> (AppState, agent24_store::ToolPermissionSummary) {
        let mut state = state().await;
        let summary = agent24_store::ToolPermissionSummary::new(
            "doc-module",
            "read_document",
            "1.2.3",
            agent24_store::ConsentSource::ManualInstall,
            None,
            None,
            None,
            agent24_store::HostRiskLevel::High,
        );
        let source = |module: &str, takes_grants| crate::consent_routes::ToolCardSource {
            summary: agent24_store::ToolPermissionSummary {
                module: module.to_owned(),
                ..summary.clone()
            },
            resource_kinds: vec!["doc".to_owned()],
            takes_grants,
        };
        let running = StdArc::clone(running);
        state.module_tool_cards = Some(crate::consent_routes::ModuleToolCards {
            notice: answering(crate::module_tool_runtime::RevocationStop::NotRunning),
            tools: StdArc::new(std::collections::HashMap::from([
                (
                    ("doc-module".to_owned(), "read_document".to_owned()),
                    source("doc-module", true),
                ),
                (
                    ("old-module".to_owned(), "read_document".to_owned()),
                    source("old-module", false),
                ),
            ])),
            artifact: StdArc::new(move |_: &str| {
                running
                    .load(std::sync::atomic::Ordering::SeqCst)
                    .then(|| "sha256:abc".to_owned())
            }),
        });
        state.module_grants = Some(crate::consent_routes::GrantSettings {
            max_days: 30,
            os_config: os_json.to_owned(),
        });
        (state, summary)
    }

    const DECIDE: &str = "/api/v1/module-consents/modules/doc-module/tools/read_document";

    fn decision(
        summary: &agent24_store::ToolPermissionSummary,
        patch: serde_json::Value,
    ) -> String {
        let mut body = serde_json::json!({
            "decision": "granted",
            "fingerprint": summary.scope_fingerprint(),
            "module_version": "1.2.3",
            "artifact": "sha256:abc",
            "resources": ["doc:a1"],
            "expires_at": (chrono::Utc::now() + chrono::Duration::days(7)).to_rfc3339(),
        });
        for (key, value) in patch.as_object().unwrap() {
            body[key] = value.clone();
        }
        body.to_string()
    }

    async fn post(router: &Router, uri: &str, body: String) -> (StatusCode, serde_json::Value) {
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("Authorization", "Bearer testtoken")
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        (status, body_json(response).await)
    }

    /// K1-6a-G §2.1, criteria 6, 7 and 14: a decision is written only on the
    /// card as the tool is checked now, with the switch on, the module
    /// enabled and running, a bounded expiry and resources of declared
    /// kinds; anything else writes nothing.
    #[tokio::test]
    async fn a_module_tool_decision_is_written_only_on_the_card_as_it_is() {
        let dir = tempfile::tempdir().unwrap();
        let os_json = dir.path().join("os.json");
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &os_json).await;
        let store = state.store.clone();
        let closed = build_router(AppState {
            module_grants: None,
            ..state.clone()
        });
        let router = build_router(state);
        let code = |(status, body): (StatusCode, serde_json::Value)| {
            (
                status,
                body["error"]["code"]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned(),
            )
        };
        let expect = |status: StatusCode, code: &str| (status, code.to_owned());
        let ok = decision(&summary, serde_json::json!({}));
        assert_eq!(
            code(post(&closed, DECIDE, ok.clone()).await),
            expect(StatusCode::FORBIDDEN, "grants_closed")
        );
        let (status, _) = post(&router, &format!("{DECIDE}x"), ok.clone()).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            code(
                post(
                    &router,
                    "/api/v1/module-consents/modules/old-module/tools/read_document",
                    ok.clone()
                )
                .await
            ),
            expect(StatusCode::CONFLICT, "protocol_too_old")
        );
        let (status, _) = post(&router, DECIDE, "{".to_owned()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let not_text = decision(&summary, serde_json::json!({ "resources": ["doc:a1", 5] }));
        let (status, _) = post(&router, DECIDE, not_text).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        let in_days = |days| (chrono::Utc::now() + chrono::Duration::days(days)).to_rfc3339();
        let many: Vec<String> = (0..257).map(|i| format!("doc:d{i}")).collect();
        for patch in [
            serde_json::json!({ "expires_at": null }),
            serde_json::json!({ "expires_at": "next week" }),
            serde_json::json!({ "expires_at": in_days(31) }),
            serde_json::json!({ "expires_at": in_days(-1) }),
            serde_json::json!({ "resources": ["doc:a b"] }),
            serde_json::json!({ "resources": ["page:a1"] }),
            serde_json::json!({ "resources": many }),
            serde_json::json!({ "decision": "denied" }),
        ] {
            assert_eq!(
                code(post(&router, DECIDE, decision(&summary, patch.clone())).await),
                expect(StatusCode::UNPROCESSABLE_ENTITY, "invalid_decision"),
                "{patch}"
            );
        }
        for patch in [
            serde_json::json!({ "fingerprint": "sha256:other" }),
            serde_json::json!({ "module_version": "1.2.4" }),
            serde_json::json!({ "artifact": "sha256:other" }),
        ] {
            assert_eq!(
                code(post(&router, DECIDE, decision(&summary, patch.clone())).await),
                expect(StatusCode::CONFLICT, "card_changed"),
                "{patch}"
            );
        }
        running.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(
            code(post(&router, DECIDE, ok.clone()).await),
            expect(StatusCode::CONFLICT, "no_package_identity")
        );
        running.store(true, std::sync::atomic::Ordering::SeqCst);
        std::fs::write(&os_json, r#"{"domainOs":{"doc-module":{"enabled":false}}}"#).unwrap();
        assert_eq!(
            code(post(&router, DECIDE, ok.clone()).await),
            expect(StatusCode::CONFLICT, "module_disabled")
        );
        std::fs::write(&os_json, "not json").unwrap();
        assert_eq!(
            code(post(&router, DECIDE, ok.clone()).await),
            expect(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        );
        assert!(store.list_module_consents().await.unwrap().is_empty());

        std::fs::remove_file(&os_json).unwrap();
        let both = decision(
            &summary,
            serde_json::json!({ "resources": ["doc:b2", "doc:a1", "doc:b2"] }),
        );
        let (status, body) = post(&router, DECIDE, both).await;
        assert_eq!(
            (status, &body["status"]),
            (StatusCode::OK, &serde_json::json!("granted"))
        );
        let records = store.list_module_consents().await.unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].resources, ["doc:a1", "doc:b2"]);
        assert_eq!(records[0].artifact.as_deref(), Some("sha256:abc"));
        let now = chrono::Utc::now().to_rfc3339();
        let lookup = |store: agent24_store::Store| {
            let (fingerprint, now) = (summary.scope_fingerprint(), now.clone());
            async move {
                store
                    .lookup_module_consent(
                        "doc-module",
                        "read_document",
                        "1.2.3",
                        &fingerprint,
                        &now,
                    )
                    .await
                    .unwrap()
            }
        };
        assert!(matches!(
            lookup(store.clone()).await,
            agent24_store::ConsentLookup::Granted(_)
        ));
        let denial = decision(
            &summary,
            serde_json::json!({ "decision": "denied", "resources": [] }),
        );
        let (status, body) = post(&router, DECIDE, denial).await;
        assert_eq!(
            (status, &body["status"]),
            (StatusCode::OK, &serde_json::json!("denied"))
        );
        assert!(matches!(
            lookup(store).await,
            agent24_store::ConsentLookup::Denied(_)
        ));
    }

    /// K1-6a-G §2.1 F7 and #1157's S2: a decision waits for `os_control`,
    /// so no enable, disable or stop lands between its checks and its write.
    #[tokio::test]
    async fn a_module_tool_decision_waits_for_os_control() {
        let dir = tempfile::tempdir().unwrap();
        let running = StdArc::new(std::sync::atomic::AtomicBool::new(true));
        let (state, summary) = decision_state(&running, &dir.path().join("os.json")).await;
        let (store, control) = (state.store.clone(), state.os_control.clone());
        let router = build_router(state);
        let held = control.lock_owned().await;
        let body = decision(&summary, serde_json::json!({}));
        let task = tokio::spawn(async move { post(&router, DECIDE, body).await });
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(!task.is_finished());
        assert!(store.list_module_consents().await.unwrap().is_empty());
        drop(held);
        assert_eq!(task.await.unwrap().0, StatusCode::OK);
    }

    /// The host's switch and bound (K1-6a-G §2.1): off by default and on an
    /// ephemeral daemon; a bound past 90 days, or none, stops the start.
    #[test]
    fn module_tool_decisions_open_only_on_the_host_switch() {
        use crate::consent_routes::{GrantSettings, grant_settings};
        let path = || Some(std::path::PathBuf::from("/x/os.json"));
        let open = |days| {
            Ok(Some(GrantSettings {
                max_days: days,
                os_config: "/x/os.json".into(),
            }))
        };
        assert_eq!(grant_settings(None, None, false, path()), Ok(None));
        assert_eq!(grant_settings(Some("0"), None, false, path()), Ok(None));
        assert_eq!(grant_settings(Some("1"), None, false, path()), open(30));
        assert_eq!(
            grant_settings(Some("1"), Some("90"), false, path()),
            open(90)
        );
        assert_eq!(grant_settings(Some("1"), Some("1"), false, path()), open(1));
        assert_eq!(grant_settings(Some("1"), None, true, path()), Ok(None));
        assert_eq!(grant_settings(Some("1"), None, false, None), Ok(None));
        for bad in ["0", "91", "-1", "30d", ""] {
            assert!(
                grant_settings(None, Some(bad), false, path()).is_err(),
                "{bad}"
            );
        }
        assert!(grant_settings(Some("yes"), None, false, path()).is_err());
    }

    #[tokio::test]
    async fn module_consent_routes_reject_creative_identity() {
        let (state, _host, creative) = capability_state().await;
        let state_again = state.clone();
        // Decisions open, so only the host-authority check can answer 403.
        let state_post = AppState {
            module_grants: Some(crate::consent_routes::GrantSettings {
                max_days: 30,
                os_config: "/nonexistent/os.json".into(),
            }),
            ..state.clone()
        };
        let router = build_router(state);
        let response = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = build_router(state_again)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/module-consents/modules/doc-module/tools/read_document/card")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let response = build_router(state_post)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(DECIDE)
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn capability_startup_requires_the_trusted_host_bootstrap_transport() {
        assert_eq!(
            validate_auth_startup(AuthMode::Capabilities, true, false, false).unwrap(),
            StartupMode::HostChild
        );
        let err = validate_auth_startup(AuthMode::Capabilities, false, false, false)
            .expect_err("capability startup without private host pipes");
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn only_the_host_child_mints_a_product_host() {
        assert!(mints_product_host(StartupMode::HostChild));
        assert!(!mints_product_host(StartupMode::Service));
        assert!(!mints_product_host(StartupMode::Legacy));
    }

    /// CORE-2e T4: every row of the D8 matrix (+ Q5, + ephemeral service).
    #[test]
    fn startup_matrix_matches_d8() {
        use AuthMode::{Capabilities as C, LegacySingleToken as L};
        let m = |a, b, e, eph| validate_auth_startup(a, b, e, eph).ok();
        assert_eq!(m(L, false, false, false), Some(StartupMode::Legacy));
        assert_eq!(m(L, true, false, false), None);
        assert_eq!(m(L, true, true, false), None);
        assert_eq!(m(L, false, true, false), None);
        assert_eq!(m(C, true, false, false), Some(StartupMode::HostChild));
        assert_eq!(m(C, true, false, true), Some(StartupMode::HostChild));
        assert_eq!(m(C, false, true, false), Some(StartupMode::Service));
        assert_eq!(m(C, false, false, false), None);
        assert_eq!(m(C, true, true, false), None);
        assert_eq!(m(C, false, true, true), None);
    }

    #[tokio::test]
    async fn capability_state_does_not_accept_the_empty_legacy_sentinel() {
        let mut capability_state = state().await;
        capability_state.enable_capability_auth(crate::capabilities::CapabilityStore::new(
            "capability-generation".to_owned(),
        ));

        let res = build_router(capability_state)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, "Bearer ")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(res).await["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn creative_bearer_can_read_models_but_not_the_closed_default_route() {
        let store = crate::capabilities::CapabilityStore::new("capability-generation");
        let host = store
            .mint_product_host(
                "host-generation",
                Duration::from_secs(60),
                crate::capabilities::unix_now(),
            )
            .unwrap();
        let creative = store
            .mint_creative(
                host.token(),
                crate::capabilities::CreativeMintRequest::new(
                    "workspace",
                    "attachment",
                    "principal",
                    "sidecar-generation",
                    Duration::from_secs(60),
                    crate::capabilities::unix_now(),
                ),
            )
            .unwrap();
        let (creative_bearer, _, _) = creative.into_bearer_parts();
        let mut capability_state = state().await;
        capability_state.enable_capability_auth(store);
        let router = build_router(capability_state);

        let models = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {creative_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);

        let unknown = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .header(header::AUTHORIZATION, format!("Bearer {creative_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(unknown).await["error"]["code"], "not_found");

        let closed = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/usage")
                    .header(header::AUTHORIZATION, format!("Bearer {creative_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(closed.status(), StatusCode::FORBIDDEN);
        assert_eq!(body_json(closed).await["error"]["code"], "forbidden");
    }

    #[tokio::test]
    async fn legacy_state_still_accepts_its_bearer_token() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .header(header::AUTHORIZATION, "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(res.status(), StatusCode::NOT_FOUND);
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

    // ---- B1: Host / Origin guard --------------------------------------

    async fn guarded_status(host: Option<&str>, origin: Option<&str>) -> (StatusCode, String) {
        let router = guard_host_origin(build_router(state().await), 4242);
        let mut b = Request::builder().uri("/api/v1/health");
        if let Some(h) = host {
            b = b.header(header::HOST, h);
        }
        if let Some(o) = origin {
            b = b.header(header::ORIGIN, o);
        }
        let res = router
            .oneshot(b.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = res.status();
        let code = if status == StatusCode::OK {
            String::new()
        } else {
            body_json(res).await["error"]["code"]
                .as_str()
                .unwrap_or("")
                .to_owned()
        };
        (status, code)
    }

    #[tokio::test]
    async fn guard_accepts_the_three_loopback_hosts_case_insensitively() {
        for h in [
            "127.0.0.1:4242",
            "localhost:4242",
            "[::1]:4242",
            "LocalHost:4242",
        ] {
            assert_eq!(guarded_status(Some(h), None).await.0, StatusCode::OK, "{h}");
        }
    }

    #[tokio::test]
    async fn guard_rejects_wrong_host_even_on_health() {
        for h in [
            "evil.example:4242",
            "127.0.0.1:4243",
            "127.0.0.1",
            "localhost.evil.com:4242",
            "127.0.0.1:4242.evil.com",
        ] {
            let (s, c) = guarded_status(Some(h), None).await;
            assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "bad_host"), "{h}");
        }
    }

    #[tokio::test]
    async fn guard_rejects_a_request_with_no_host_as_400() {
        let (s, c) = guarded_status(None, None).await;
        assert_eq!((s, c.as_str()), (StatusCode::BAD_REQUEST, "bad_host"));
    }

    #[tokio::test]
    async fn guard_rejects_a_browser_origin_but_allows_null_and_none() {
        let (s, c) = guarded_status(Some("127.0.0.1:4242"), Some("http://evil.example")).await;
        assert_eq!((s, c.as_str()), (StatusCode::FORBIDDEN, "forbidden_origin"));
        let (s, _) = guarded_status(Some("127.0.0.1:4242"), Some("null")).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = guarded_status(Some("127.0.0.1:4242"), None).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn guard_runs_before_auth_and_in_front_of_module_routes() {
        // Wrong Host on an authenticated route is the guard's answer, not 401.
        let router = guard_host_origin(build_router(state().await), 4242);
        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header(header::HOST, "evil.example")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    #[test]
    fn legacy_ready_line_has_token_unless_told_to_omit_it() {
        let with = legacy_ready_line(4242, "sekret", false, "g1");
        assert_eq!(with["token"], "sekret");
        assert_eq!(with["port"], 4242);
        let without = legacy_ready_line(4242, "sekret", true, "g1");
        assert!(without.get("token").is_none());
        assert!(!without.to_string().contains("sekret"));
        assert_eq!(without["type"], "ready");
        assert_eq!(without["port"], 4242);
        assert_eq!(without["auth_mode"], "legacy_single_token");
    }

    #[test]
    fn serve_layers_the_host_guard_on_the_router_it_serves() {
        let src = include_str!("server.rs");
        let start = src.find("pub async fn serve(").expect("serve must exist");
        assert!(src[start..].contains("guard_host_origin(router, local.port())"));
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

    #[tokio::test]
    async fn creative_is_default_denied_except_for_models() {
        let (state, _, creative) = capability_state().await;
        let router = build_router(state);
        let models = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::OK);

        for (method, path) in [
            ("POST", "/api/v1/chat"),
            ("GET", "/api/v1/sessions"),
            ("POST", "/api/v1/runs"),
            ("GET", "/api/v1/peer/runs"),
            ("GET", "/api/v1/events"),
            ("GET", "/api/v1/approvals"),
            ("GET", "/api/v1/tools"),
            ("POST", "/api/v1/shutdown"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(path)
                        .header("Authorization", format!("Bearer {creative}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
        }

        let unknown = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/future-unregistered-route")
                    .header("Authorization", format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    }

    async fn attached_probe() -> StatusCode {
        StatusCode::NO_CONTENT
    }

    fn concrete_path(template: &str) -> String {
        let mut path = String::new();
        let mut rest = template;
        while let Some(start) = rest.find('{') {
            path.push_str(&rest[..start]);
            let end = rest[start..].find('}').expect("template brace");
            path.push('x');
            rest = &rest[start + end + 1..];
        }
        path.push_str(rest);
        path
    }

    #[tokio::test]
    async fn creative_is_refused_on_every_mapped_route_except_models() {
        let (state, _host, creative) = capability_state().await;
        let router = router_with_comm(state);
        for policy in crate::capabilities::policies() {
            // Docked-only routes are not on the business router; the docked
            // router's own tests cover Creative there.
            if policy.operation == crate::capabilities::Operation::DiagnosticRead {
                continue;
            }
            // X11-2g: this POST is on the catalog so the route scan matches,
            // and capabilities mode answers 404 instead of the creative 403.
            if policy.method == "POST" && policy.template == "/api/v1/inbound/events" {
                continue;
            }
            // The bridge mint route is dark unless A24_DEV_TOOL=1. A live
            // token then gets 404, same as the wechat POST above.
            if policy.method == "POST" && policy.template == "/api/v1/capabilities/bridge" {
                continue;
            }
            let path = concrete_path(policy.template);
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(policy.method)
                        .uri(&path)
                        .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let expect = if policy.template == "/api/v1/models"
                || policy.level == crate::capabilities::AssuranceLevel::Public
            {
                StatusCode::OK
            } else {
                StatusCode::FORBIDDEN
            };
            assert_eq!(response.status(), expect, "{} {path}", policy.method);
        }
    }

    #[tokio::test]
    async fn capability_head_follows_get_and_does_not_skip_auth() {
        let (state, _host, creative) = capability_state().await;
        let router = build_router(state);

        let health = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(health.status(), StatusCode::OK);

        let models = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(models.status(), StatusCode::UNAUTHORIZED);

        let creative_models = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(creative_models.status(), StatusCode::FORBIDDEN);

        let usage = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/usage")
                    .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(usage.status(), StatusCode::FORBIDDEN);

        let post_only = router
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/chat")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_only.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn capability_missing_invalid_and_revoked_tokens_cannot_enumerate_routes() {
        let mut app = state().await;
        let now = crate::capabilities::unix_now();
        let store = crate::capabilities::CapabilityStore::new("daemon-test");
        let live = store
            .mint_product_host("live-host", Duration::from_secs(3_600), now)
            .unwrap();
        let (live_bearer, _, _) = live.into_bearer_parts();
        let revoked = store
            .mint_product_host("revoked-host", Duration::from_secs(3_600), now)
            .unwrap();
        let (revoked_bearer, revoked_id, _) = revoked.into_bearer_parts();
        assert!(store.revoke_by_id(&revoked_id));
        app.enable_capability_auth(store);
        let router = build_router(app);
        let invalid = "00".repeat(32);
        let probes = [
            (Method::POST, "/api/v1/models"),
            (Method::GET, "/api/v1/not-a-route"),
        ];
        let tokens: [(&str, Option<&str>); 3] = [
            ("missing", None),
            ("invalid", Some(invalid.as_str())),
            ("revoked", Some(revoked_bearer.as_str())),
        ];
        for (label, token) in tokens {
            for (method, path) in &probes {
                let mut request = Request::builder().method(method.clone()).uri(*path);
                if let Some(token) = token {
                    request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
                }
                let response = router
                    .clone()
                    .oneshot(request.body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::UNAUTHORIZED,
                    "{label} {method} {path}"
                );
                assert_eq!(
                    body_json(response).await["error"]["code"],
                    "unauthorized",
                    "{label} {method} {path}"
                );
            }
        }

        for method in [Method::GET, Method::HEAD] {
            let health = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri("/api/v1/health")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(health.status(), StatusCode::OK, "{method} /api/v1/health");
        }

        let denied_method = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/models")
                    .header(header::AUTHORIZATION, format!("Bearer {live_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(denied_method.status(), StatusCode::METHOD_NOT_ALLOWED);

        let unknown = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/not-a-route")
                    .header(header::AUTHORIZATION, format!("Bearer {live_bearer}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
        assert_eq!(body_json(unknown).await["error"]["code"], "not_found");
    }

    #[tokio::test]
    async fn capability_unknown_method_is_405_and_unknown_path_does_not_run_fallback() {
        let (mut state, host, creative) = capability_state().await;
        let post_health = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/v1/health")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(post_health.status(), StatusCode::METHOD_NOT_ALLOWED);

        state.module_namespaces = Arc::new(BTreeSet::from(["/api/v1/probe".to_owned()]));
        let router = Router::new()
            .route("/api/v1/mounted/secret", get(attached_probe))
            .fallback(attached_probe)
            .layer(middleware::from_fn_with_state(state, auth));

        let mounted = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/mounted/secret")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(mounted.status(), StatusCode::NO_CONTENT);
        let creative_mounted = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/mounted/secret")
                    .header(header::AUTHORIZATION, format!("Bearer {creative}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(creative_mounted.status(), StatusCode::FORBIDDEN);

        let namespace = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/probe/secret")
                    .header(header::AUTHORIZATION, format!("Bearer {host}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(namespace.status(), StatusCode::NO_CONTENT);

        for path in ["/api/v1/probe-x", "/api/v1/missing"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .header(header::AUTHORIZATION, format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(body_json(response).await["error"]["code"], "not_found");
        }
    }

    #[tokio::test]
    async fn legacy_head_health_stays_unauthorized() {
        let res = build_router(state().await)
            .oneshot(
                Request::builder()
                    .method(Method::HEAD)
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    fn router_with_comm(state: AppState) -> Router {
        let modules = Router::new().nest(
            "/api/v1/comm",
            agent24_comm::router(agent24_comm::CommState::unconfigured("coverage"))
                .route("/install", axum::routing::post(attached_probe)),
        );
        build_router_with_modules(state, modules)
    }

    fn function_span(src: &str, signature: &str) -> std::ops::Range<usize> {
        let start = src
            .find(signature)
            .unwrap_or_else(|| panic!("missing {signature}"));
        let end = src[start..]
            .find("\n}\n")
            .unwrap_or_else(|| panic!("unclosed {signature}"));
        start..start + end
    }

    fn ident_boundary_before(src: &str, at: usize) -> bool {
        match src[..at].chars().next_back() {
            Some(ch) => !ch.is_ascii_alphanumeric() && ch != '_',
            None => true,
        }
    }

    fn line_comment_end(src: &str, at: usize) -> usize {
        match src[at..].find('\n') {
            Some(rel) => at + rel + 1,
            None => src.len(),
        }
    }

    fn block_comment_end(src: &str, at: usize) -> usize {
        let mut i = at + 2;
        let mut depth = 1;
        while i < src.len() && depth > 0 {
            if src[i..].starts_with("/*") {
                depth += 1;
                i += 2;
            } else if src[i..].starts_with("*/") {
                depth -= 1;
                i += 2;
            } else {
                i += src[i..].chars().next().unwrap().len_utf8();
            }
        }
        i
    }

    /// `r"..."`, `r#"..."#`, and the `b`/`c` prefixes. `at` may sit on the prefix.
    fn raw_string_end(src: &str, at: usize) -> Option<usize> {
        let rest = &src[at..];
        let mut rel = 0;
        if rest.starts_with('b') || rest.starts_with('c') {
            if !ident_boundary_before(src, at) {
                return None;
            }
            rel = 1;
        } else if !rest.starts_with('r') {
            return None;
        }
        if !rest[rel..].starts_with('r') {
            return None;
        }
        if rel == 0 && !ident_boundary_before(src, at) {
            return None;
        }
        let bytes = src.as_bytes();
        let mut i = at + rel + 1;
        let mut hashes = 0;
        while bytes.get(i) == Some(&b'#') {
            hashes += 1;
            i += 1;
        }
        if bytes.get(i) != Some(&b'"') {
            return None;
        }
        i += 1;
        let mut close = String::from("\"");
        close.extend(std::iter::repeat_n('#', hashes));
        while i < src.len() {
            if src[i..].starts_with(&close) {
                return Some(i + close.len());
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        Some(src.len())
    }

    fn cooked_string_end(src: &str, at: usize) -> usize {
        let mut i = at + 1;
        let mut escaped = false;
        while i < src.len() {
            let ch = src[i..].chars().next().unwrap();
            i += ch.len_utf8();
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => return i,
                _ => {}
            }
        }
        src.len()
    }

    fn char_literal_end(src: &str, at: usize) -> Option<usize> {
        let rest = &src[at..];
        if !rest.starts_with('\'') {
            return None;
        }
        let mut i = at + 1;
        if i >= src.len() {
            return None;
        }
        if src[i..].starts_with('\\') {
            i += 1;
            if src[i..].starts_with("u{") {
                let close = src[i..].find('}')?;
                i += close + 1;
            } else {
                i += src[i..].chars().next()?.len_utf8();
            }
        } else {
            let ch = src[i..].chars().next()?;
            if ch.is_ascii_alphabetic() || ch == '_' {
                return None;
            }
            i += ch.len_utf8();
        }
        if src[i..].starts_with('\'') {
            Some(i + 1)
        } else {
            None
        }
    }

    /// End index when `src[at..]` opens a comment, string, or char literal.
    fn non_code_end(src: &str, at: usize) -> Option<usize> {
        let rest = &src[at..];
        if rest.starts_with("//") {
            return Some(line_comment_end(src, at));
        }
        if rest.starts_with("/*") {
            return Some(block_comment_end(src, at));
        }
        if (rest.starts_with('b') || rest.starts_with('c') || rest.starts_with('r'))
            && let Some(end) = raw_string_end(src, at)
        {
            return Some(end);
        }
        if rest.starts_with('"') || rest.starts_with("b\"") || rest.starts_with("c\"") {
            let quote = if rest.starts_with('"') { at } else { at + 1 };
            return Some(cooked_string_end(src, quote));
        }
        if rest.starts_with('\'') {
            return char_literal_end(src, at);
        }
        None
    }

    fn skip_trivia(src: &str) -> &str {
        let mut i = 0;
        loop {
            let rest = &src[i..];
            let trimmed = rest.trim_start();
            if trimmed.len() != rest.len() {
                i += rest.len() - trimmed.len();
                continue;
            }
            if (rest.starts_with("//") || rest.starts_with("/*"))
                && let Some(end) = non_code_end(src, i)
            {
                i = end;
                continue;
            }
            return &src[i..];
        }
    }

    fn route_interior(src: &str) -> &str {
        let mut depth = 1;
        let mut i = 0;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            match ch {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        return &src[..i];
                    }
                }
                _ => {}
            }
            i += ch.len_utf8();
        }
        panic!("unbalanced .route(");
    }

    fn rust_string(src: &str) -> Option<(&str, &str)> {
        let src = skip_trivia(src);
        let mut chars = src.char_indices();
        if chars.next()?.1 != '"' {
            return None;
        }
        let mut escaped = false;
        for (index, ch) in chars {
            if escaped {
                escaped = false;
                continue;
            }
            match ch {
                '\\' => escaped = true,
                '"' => return Some((&src[1..index], &src[index + 1..])),
                _ => {}
            }
        }
        None
    }

    fn method_names(expr: &str) -> Vec<&'static str> {
        // Every axum method registrar is recognised, so an extra registration
        // (`.options(..)`, `any(..)`, `on(..)`) shows up as a (method, path) pair
        // the catalog does not list instead of being skipped.
        const NAMES: &[&str] = &[
            "get", "post", "put", "patch", "delete", "head", "options", "trace", "connect", "any",
            "on",
        ];
        let mut found = Vec::new();
        let mut index = 0;
        let mut boundary = true;
        while index < expr.len() {
            if let Some(end) = non_code_end(expr, index) {
                index = end;
                boundary = true;
                continue;
            }
            let ch = expr[index..].chars().next().unwrap();
            if boundary {
                for name in NAMES {
                    if !expr[index..].starts_with(name) {
                        continue;
                    }
                    let end = index + name.len();
                    let next = expr[end..].chars().next();
                    let ended = next.is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_');
                    if ended && expr[end..].starts_with('(') {
                        found.push(*name);
                    }
                    break;
                }
            }
            boundary = !ch.is_ascii_alphanumeric() && ch != '_';
            index += ch.len_utf8();
        }
        found
    }

    /// Every `.route(` in `signature`. Comments are not calls. Unparseable calls,
    /// calls with no method, and duplicate method/template pairs fail closed.
    /// Other axum registrars are policed by [`registration_violations`].
    fn parsed_routes(src: &str, signature: &str, prefix: &str) -> BTreeSet<(String, String)> {
        let span = function_span(src, signature);
        let body = &src[span];
        let mut routes = BTreeSet::new();
        for call in code_calls(body) {
            if call.form != "route" {
                continue;
            }
            let interior = route_interior(&body[call.at + ".route(".len()..]);
            let (path, after) = rust_string(interior)
                .unwrap_or_else(|| panic!("unparseable .route( near {signature}"));
            let methods = method_names(after);
            assert!(!methods.is_empty(), "no method on {path}");
            for name in methods {
                let template = if path.starts_with("/api/v1") {
                    path.to_owned()
                } else {
                    format!("{prefix}{path}")
                };
                assert!(
                    routes.insert((name.to_ascii_uppercase(), template.clone())),
                    "duplicate {name} {template}"
                );
            }
        }
        assert!(!routes.is_empty(), "{signature} registered no routes");
        routes
    }

    struct Registration {
        form: &'static str,
        at: usize,
    }

    /// Longest form first, so `.route_service(` is not read as `.route(`.
    const REGISTRATION_FORMS: &[&str] = &[
        "fallback_service",
        "route_service",
        "nest_service",
        "route_layer",
        "fallback",
        "route",
        "nest",
        "merge",
    ];

    fn registration_at(src: &str, at: usize) -> Option<&'static str> {
        let rest = &src[at..];
        if !rest.starts_with('.') {
            return None;
        }
        let rest = &rest[1..];
        REGISTRATION_FORMS
            .iter()
            .copied()
            .find(|form| rest.starts_with(form) && rest[form.len()..].starts_with('('))
    }

    fn code_calls(src: &str) -> Vec<Registration> {
        let mut calls = Vec::new();
        let mut i = 0;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            if let Some(form) = registration_at(src, i) {
                calls.push(Registration { form, at: i });
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        calls
    }

    fn skip_ws_idx(src: &str, mut i: usize) -> usize {
        while let Some(ch) = src[i..].chars().next() {
            if !ch.is_whitespace() {
                break;
            }
            i += ch.len_utf8();
        }
        i
    }

    fn skip_bracket_group(src: &str, at: usize) -> usize {
        let start = if src[at..].starts_with('#') {
            at + 1
        } else {
            at
        };
        let mut i = start + 1;
        let mut depth = 1;
        while i < src.len() && depth > 0 {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            match src[i..].chars().next().unwrap() {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        i
    }

    fn skip_paren_group(src: &str, at: usize) -> usize {
        let mut i = at + 1;
        let mut depth = 1;
        while i < src.len() && depth > 0 {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            match src[i..].chars().next().unwrap() {
                '(' => depth += 1,
                ')' => depth -= 1,
                _ => {}
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        i
    }

    fn skip_visibility(src: &str, i: usize) -> usize {
        let i = skip_ws_idx(src, i);
        if !src[i..].starts_with("pub") {
            return i;
        }
        let after = i + 3;
        let next = src[after..].chars().next();
        if next.is_some_and(|ch| ch.is_ascii_alphanumeric() || ch == '_') {
            return i;
        }
        let i = skip_ws_idx(src, after);
        if src[i..].starts_with('(') {
            skip_paren_group(src, i)
        } else {
            i
        }
    }

    /// One Rust item: through the matching `}` of a body, or through `;`.
    fn skip_item_body(src: &str, mut i: usize) -> usize {
        let mut brace = 0i32;
        let mut paren = 0i32;
        let mut bracket = 0i32;
        let mut seen_brace = false;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                i = end;
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            match ch {
                '{' => {
                    brace += 1;
                    seen_brace = true;
                }
                '}' => {
                    brace -= 1;
                    if seen_brace && brace == 0 && paren == 0 && bracket == 0 {
                        return i + ch.len_utf8();
                    }
                }
                '(' => paren += 1,
                ')' => paren -= 1,
                '[' => bracket += 1,
                ']' => bracket -= 1,
                ';' if brace == 0 && paren == 0 && bracket == 0 => return i + ch.len_utf8(),
                _ => {}
            }
            i += ch.len_utf8();
        }
        i
    }

    fn skip_cfg_test_item(src: &str, i: usize) -> usize {
        let mut i = i + "#[cfg(test)]".len();
        loop {
            i = skip_ws_idx(src, i);
            if (src[i..].starts_with("//") || src[i..].starts_with("/*"))
                && let Some(end) = non_code_end(src, i)
            {
                i = end;
                continue;
            }
            if src[i..].starts_with("#[") {
                i = skip_bracket_group(src, i);
                continue;
            }
            break;
        }
        skip_item_body(src, skip_visibility(src, i))
    }

    fn strip_cfg_test(src: &str) -> String {
        let mut out = String::with_capacity(src.len());
        let mut i = 0;
        while i < src.len() {
            if let Some(end) = non_code_end(src, i) {
                out.push_str(&src[i..end]);
                i = end;
                continue;
            }
            if src[i..].starts_with("#[cfg(test)]") {
                i = skip_cfg_test_item(src, i);
                continue;
            }
            let ch = src[i..].chars().next().unwrap();
            out.push(ch);
            i += ch.len_utf8();
        }
        out
    }

    /// `at` is a code byte, not inside a comment or string.
    fn is_code_index(src: &str, at: usize) -> bool {
        let mut i = 0;
        while i < at {
            if let Some(end) = non_code_end(src, i) {
                if at < end {
                    return false;
                }
                i = end;
                continue;
            }
            i += src[i..].chars().next().unwrap().len_utf8();
        }
        i == at && non_code_end(src, at).is_none()
    }

    struct AllowedRegistration {
        file: &'static str,
        form: &'static str,
        /// Exact call text in production code. Must occur once and cover one call.
        call: &'static str,
        reason: &'static str,
    }

    /// Axum registrars that are not `.route(` inside the two catalog functions.
    /// `build_router_with_modules` and `agent24_comm::router` are parsed into the
    /// operation catalog; every other registration in agent24d / agent24-comm
    /// production source must be listed here.
    const ALLOWED_REGISTRATIONS: &[AllowedRegistration] = &[
        AllowedRegistration {
            file: "apps/agent24d/src/server.rs",
            form: "fallback",
            call: ".fallback(fallback)",
            reason: "v1 fallback：未登记路径的 kernel 404，不进 Operation 目录",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/server.rs",
            form: "merge",
            call: ".merge(modules)",
            reason: "模块路由 merge 入口：auth layer 之前折入 domain OS",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/server.rs",
            form: "merge",
            call: "module_routes.merge(comm_router)",
            reason: "comm 路由 merge 入口：并入 module_routes，使 /api/v1/comm 走 kernel auth",
        },
        AllowedRegistration {
            file: "crates/agent24-comm/src/router.rs",
            form: "merge",
            call: ".merge(crate::groupchat::routes())",
            reason: "群聊路由并入 comm router；方法清单在 groupchat::routes，由目录扫描覆盖",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/comm_routes.rs",
            form: "nest",
            call: ".nest(\"/api/v1/comm\", agent24_comm::router(state))",
            reason: "comm 前缀挂载点；方法清单在 agent24-comm::router，由目录扫描覆盖",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/comm_routes.rs",
            form: "route_layer",
            call: ".route_layer(middleware::from_fn_with_state(",
            reason: "保留固定路由模板，仅将请求转发到热切换后的通信实例",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/comm_routes.rs",
            form: "nest",
            call: ".nest(\"/api/v1/comm\", routes)",
            reason: "热切换 comm 前缀挂载；runtime_router 的方法清单由目录扫描覆盖",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/docked_router.rs",
            form: "fallback",
            call: ".fallback(not_found)",
            reason: "停靠 router 的 404：未登记路径一律 not_found，不进 Operation 目录",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "nest",
            call: "app.nest(namespace, Router::new().fallback(handler.clone()))",
            reason: "模块命名空间 503 的 nest，路径来自 namespace，不是固定 Operation",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "fallback",
            call: "Router::new().fallback(handler.clone())",
            reason: "模块命名空间 503 的 fallback，应答该命名空间下全部方法",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "route",
            call: ".route(&format!(\"{namespace}/\"), axum::routing::any(handler))",
            reason: "命名空间尾斜杠 503：matchit 不匹配空 segment",
        },
        AllowedRegistration {
            file: "apps/agent24d/src/domain.rs",
            form: "nest",
            call: "app.nest(&namespace, module.routes(ctx))",
            reason: "已挂载模块的动态路由入口，路径来自 manifest",
        },
    ];

    fn production_sources() -> Vec<(String, String)> {
        let rust_root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut files = Vec::new();
        for root in ["apps/agent24d/src", "crates/agent24-comm/src"] {
            collect_rs(&rust_root.join(root), root, &mut files);
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        files
    }

    fn collect_rs(dir: &std::path::Path, rel: &str, out: &mut Vec<(String, String)>) {
        let entries =
            std::fs::read_dir(dir).unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
        for entry in entries {
            let entry = entry.unwrap_or_else(|err| panic!("read {}: {err}", dir.display()));
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == "tests" {
                continue;
            }
            let path = entry.path();
            let rel_path = format!("{rel}/{name}");
            if path.is_dir() {
                collect_rs(&path, &rel_path, out);
                continue;
            }
            if !name.ends_with(".rs") {
                continue;
            }
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
            out.push((rel_path, strip_cfg_test(&text)));
        }
    }

    fn source_of<'a>(files: &'a [(String, String)], path: &str) -> &'a str {
        files
            .iter()
            .find(|(name, _)| name == path)
            .unwrap_or_else(|| panic!("missing {path}"))
            .1
            .as_str()
    }

    fn preview_at(src: &str, at: usize) -> String {
        let end = src[at..]
            .find('\n')
            .map_or(src.len(), |rel| at + rel)
            .min(at + 160);
        src[at..end].trim().to_owned()
    }

    fn registrar_span(path: &str, src: &str) -> Option<std::ops::Range<usize>> {
        let signature = match path {
            "apps/agent24d/src/server.rs" => "pub fn build_router_with_modules",
            "crates/agent24-comm/src/router.rs" => "pub fn router(",
            "crates/agent24-comm/src/groupchat.rs" => "pub(crate) fn routes(",
            "apps/agent24d/src/comm_routes.rs" => "fn runtime_router(",
            "apps/agent24d/src/docked_router.rs" => "pub fn docked_router",
            _ => return None,
        };
        Some(function_span(src, signature))
    }

    /// Empty means every production axum registration is either a catalog
    /// `.route(` or a live whitelist entry. A missing whitelist call is a
    /// violation, so deleting the real call fails the gate.
    fn registration_violations(files: &[(String, String)]) -> Vec<String> {
        let mut violations = Vec::new();
        let mut ranges: Vec<(&str, std::ops::Range<usize>, &str)> = Vec::new();
        for entry in ALLOWED_REGISTRATIONS {
            let Some(src) = files
                .iter()
                .find(|(path, _)| path == entry.file)
                .map(|(_, text)| text.as_str())
            else {
                violations.push(format!("whitelist file missing: {}", entry.file));
                continue;
            };
            let mut starts = Vec::new();
            let mut search = 0;
            while let Some(rel) = src[search..].find(entry.call) {
                let at = search + rel;
                if is_code_index(src, at) {
                    starts.push(at);
                }
                search = at + entry.call.len();
            }
            if starts.len() != 1 {
                violations.push(format!(
                    "whitelist entry must occur once, found {} in {}: `{}` ({})",
                    starts.len(),
                    entry.file,
                    entry.call,
                    entry.reason
                ));
                continue;
            }
            let start = starts[0];
            let range = start..start + entry.call.len();
            let covered = code_calls(src)
                .iter()
                .filter(|call| call.form == entry.form && range.contains(&call.at))
                .count();
            if covered != 1 {
                violations.push(format!(
                    "whitelist entry does not cover exactly one {} call (covered {covered}) in {}: `{}` ({})",
                    entry.form, entry.file, entry.call, entry.reason
                ));
                continue;
            }
            ranges.push((entry.file, range, entry.form));
        }
        for (path, src) in files {
            let span = registrar_span(path, src);
            for call in code_calls(src) {
                let in_catalog =
                    call.form == "route" && span.as_ref().is_some_and(|s| s.contains(&call.at));
                let listed = ranges.iter().any(|(file, range, form)| {
                    *file == path && *form == call.form && range.contains(&call.at)
                });
                if !in_catalog && !listed {
                    violations.push(format!(
                        "unlisted {} in {path}: {}",
                        call.form,
                        preview_at(src, call.at)
                    ));
                }
            }
        }
        violations
    }

    fn replace_server(files: &[(String, String)], from: &str, to: &str) -> Vec<(String, String)> {
        let mut files = files.to_vec();
        let server = files
            .iter_mut()
            .find(|(path, _)| path == "apps/agent24d/src/server.rs")
            .expect("server.rs");
        let count = server.1.matches(from).count();
        assert_eq!(count, 1, "expected one occurrence of {from}, found {count}");
        server.1 = server.1.replacen(from, to, 1);
        files
    }

    #[test]
    fn a_route_with_a_chinese_comment_parses() {
        let src = "\
pub fn build_router_with_modules() {
    .route(
        /* 中文 */
        \"/api/v1/health\",
        get(/* 中文) */ health).post(health), // 行注释 post(
    )
}
";
        let routes = parsed_routes(src, "pub fn build_router_with_modules", "");
        assert_eq!(
            routes,
            BTreeSet::from([
                ("GET".to_owned(), "/api/v1/health".to_owned()),
                ("POST".to_owned(), "/api/v1/health".to_owned()),
            ])
        );
    }

    #[test]
    fn an_unlisted_route_service_fails_the_registration_gate() {
        let files = replace_server(
            &production_sources(),
            ".route(\"/api/v1/health\", get(health))",
            ".route(\"/api/v1/health\", get(health))\n        .route_service(\"/api/v1/x\", get(health))",
        );
        let violations = registration_violations(&files);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("route_service") && v.contains("/api/v1/x")),
            "{violations:?}"
        );
    }

    #[test]
    fn an_unlisted_nest_fails_the_registration_gate() {
        let files = replace_server(
            &production_sources(),
            ".route(\"/api/v1/health\", get(health))",
            ".nest(\"/api/v1/y\", Router::new())\n        .route(\"/api/v1/health\", get(health))",
        );
        let violations = registration_violations(&files);
        assert!(
            violations
                .iter()
                .any(|v| v.contains("unlisted nest") && v.contains("/api/v1/y")),
            "{violations:?}"
        );
    }

    #[test]
    fn removing_a_whitelisted_merge_fails_the_registration_gate() {
        let files = replace_server(&production_sources(), ".merge(modules)", "");
        let violations = registration_violations(&files);
        assert!(
            violations.iter().any(|v| {
                v.contains(".merge(modules)") && v.contains("模块路由 merge 入口")
            }),
            "{violations:?}"
        );
    }

    #[tokio::test]
    async fn registered_routes_match_the_operation_catalog() {
        let files = production_sources();
        let kernel_src = source_of(&files, "apps/agent24d/src/server.rs");
        let comm_src = source_of(&files, "crates/agent24-comm/src/router.rs");
        assert!(
            !kernel_src.contains("fn registered_routes_match_the_operation_catalog"),
            "cfg(test) module was scanned as production"
        );
        assert_eq!(kernel_src.matches(".merge(modules)").count(), 1);
        let kernel = parsed_routes(kernel_src, "pub fn build_router_with_modules", "");
        let mut comm = parsed_routes(comm_src, "pub fn router(", "/api/v1/comm");
        // `router` merges `groupchat::routes()`; both register under the comm prefix.
        let groupchat_src = source_of(&files, "crates/agent24-comm/src/groupchat.rs");
        comm.extend(parsed_routes(
            groupchat_src,
            "pub(crate) fn routes(",
            "/api/v1/comm",
        ));
        comm.extend(parsed_routes(
            source_of(&files, "apps/agent24d/src/comm_routes.rs"),
            "fn runtime_router(",
            "/api/v1/comm",
        ));
        let violations = registration_violations(&files);
        assert!(
            violations.is_empty(),
            "registration gate:\n{}",
            violations.join("\n")
        );
        assert!(kernel.len() >= 50, "kernel scan saw only {}", kernel.len());
        let comm_paths = comm
            .iter()
            .map(|(_, path)| path.as_str())
            .collect::<BTreeSet<_>>();
        assert!(
            comm_paths.len() >= 16,
            "comm scan saw only {}",
            comm_paths.len()
        );

        let mut registered = kernel.clone();
        registered.extend(comm.iter().cloned());
        // CORE-5f-2: docked-only routes are catalogued but not mounted on the
        // business router, so they join the catalog comparison, not the 405 sweep.
        let docked = parsed_routes(
            source_of(&files, "apps/agent24d/src/docked_router.rs"),
            "pub fn docked_router",
            "",
        );
        let business = registered.clone();
        registered.extend(docked.iter().cloned());
        let catalog = crate::capabilities::policies()
            .iter()
            .map(|policy| (policy.method.to_owned(), policy.template.to_owned()))
            .collect::<BTreeSet<_>>();
        assert_eq!(registered, catalog);

        let empty = BTreeSet::new();
        for (method_name, template) in &registered {
            let decision = crate::capabilities::decide(
                &Method::from_bytes(method_name.as_bytes()).unwrap(),
                Some(template),
                template,
                &empty,
            );
            assert!(
                matches!(decision, crate::capabilities::RouteDecision::Mapped(_)),
                "{method_name} {template} fell into the module fallback"
            );
        }

        let (state, host, _) = capability_state().await;
        let router = router_with_comm(state);
        let trace = Method::from_bytes(b"TRACE").unwrap();
        let mut templates = BTreeSet::new();
        for (_, template) in &business {
            templates.insert(template.clone());
        }
        for template in templates {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(trace.clone())
                        .uri(concrete_path(&template))
                        .header(header::AUTHORIZATION, format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::METHOD_NOT_ALLOWED,
                "{template}"
            );
            let allow = response
                .headers()
                .get(header::ALLOW)
                .unwrap_or_else(|| panic!("no Allow on {template}"))
                .to_str()
                .unwrap();
            let got = allow
                .split(',')
                .map(|part| part.trim().to_owned())
                .collect::<BTreeSet<_>>();
            let mut expect = business
                .iter()
                .filter(|(_, path)| path == &template)
                .map(|(method_name, _)| method_name.clone())
                .collect::<BTreeSet<_>>();
            if expect.contains("GET") {
                expect.insert("HEAD".to_owned());
            }
            assert_eq!(got, expect, "{template} Allow: {allow}");
        }
    }

    #[tokio::test]
    async fn attached_routes_are_host_only_in_capability_mode() {
        let (state, host, creative) = capability_state().await;
        let router = Router::new()
            .route("/api/v1/attached", get(attached_probe).post(attached_probe))
            .route(
                "/api/v1/attached/{name}",
                axum::routing::patch(attached_probe).delete(attached_probe),
            )
            .layer(middleware::from_fn_with_state(state, auth));

        for (method, path) in [
            (Method::GET, "/api/v1/attached"),
            (Method::POST, "/api/v1/attached"),
            (Method::PATCH, "/api/v1/attached/agentear"),
            (Method::DELETE, "/api/v1/attached/agentear"),
        ] {
            let unauthenticated = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                unauthenticated.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path}"
            );
            assert_eq!(
                body_json(unauthenticated).await["error"]["code"],
                "unauthorized"
            );

            let creative_denied = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .header("Authorization", format!("Bearer {creative}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                creative_denied.status(),
                StatusCode::FORBIDDEN,
                "{method} {path}"
            );
            assert_eq!(
                body_json(creative_denied).await["error"]["code"],
                "forbidden"
            );

            let host_allowed = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method.clone())
                        .uri(path)
                        .header("Authorization", format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                host_allowed.status(),
                StatusCode::NO_CONTENT,
                "{method} {path}"
            );
        }
    }

    #[tokio::test]
    async fn host_can_mint_and_idempotently_revoke_creative_authority() {
        let (state, host, existing_creative) = capability_state().await;
        let router = build_router(state);

        let creative_mint_attempt = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/capabilities/creative")
                    .header("Authorization", format!("Bearer {existing_creative}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(creative_mint_attempt.status(), StatusCode::FORBIDDEN);

        let minted = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/capabilities/creative")
                    .header("Authorization", format!("Bearer {host}"))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        serde_json::json!({
                            "workspace_id": "workspace-new",
                            "creative_attachment_id": "attachment-new",
                            "creative_principal_id": "principal-new",
                            "sidecar_generation": "sidecar-new",
                            "ttl_seconds": 60,
                            "allowed_operations": ["models.read"]
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(minted.status(), StatusCode::CREATED);
        let minted = body_json(minted).await;
        let id = minted["capability_id"].as_str().unwrap();
        let token = minted["token"].as_str().unwrap();

        for _ in 0..2 {
            let revoked = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri(format!("/api/v1/capabilities/{id}/revoke"))
                        .header("Authorization", format!("Bearer {host}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(revoked.status(), StatusCode::OK);
            assert_eq!(body_json(revoked).await["state"], "revoked");
        }

        let rejected = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
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

    #[tokio::test]
    async fn approval_recovery_scan_expires_tokenless_run_and_stops_with_shutdown() {
        let state = state().await;
        seed_run(&state.store, "run_expired").await;
        sqlx::query("UPDATE runs SET status='awaiting_approval' WHERE id='run_expired'")
            .execute(agent24_store::test_hooks::pool(&state.store))
            .await
            .unwrap();
        state
            .store
            .insert_approval(&agent24_protocol::Approval {
                id: "apr_expired".to_owned(),
                run_id: "run_expired".to_owned(),
                tool_call_id: "tc_expired".to_owned(),
                kind: "exec".to_owned(),
                summary: "expired".to_owned(),
                payload: serde_json::Map::new(),
                available_decisions: vec!["approve".to_owned()],
                standing_target: None,
                status: agent24_protocol::ApprovalStatus::Pending,
                decision: None,
                expires_at: "2020-01-01T00:00:00Z".to_owned(),
                created_at: "2019-12-31T23:59:00Z".to_owned(),
                decided_at: None,
            })
            .await
            .unwrap();

        let cancel = CancellationToken::new();
        let task = spawn_approval_recovery_scan_every(
            Arc::clone(&state.broker),
            Arc::clone(&state.runs),
            Duration::from_millis(10),
            cancel.clone(),
        );
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if state
                    .store
                    .get_run("run_expired")
                    .await
                    .unwrap()
                    .unwrap()
                    .status
                    == agent24_protocol::RunStatus::Cancelled
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("recovery scan should cancel the parked run");

        assert_eq!(
            state
                .store
                .get_approval("apr_expired")
                .await
                .unwrap()
                .unwrap()
                .status,
            agent24_protocol::ApprovalStatus::TimedOut
        );
        assert_eq!(
            state
                .store
                .get_run("run_expired")
                .await
                .unwrap()
                .unwrap()
                .status,
            agent24_protocol::RunStatus::Cancelled
        );

        cancel.cancel();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .expect("shutdown should stop the recovery scan")
            .unwrap();
    }

    #[test]
    fn startup_recovers_timed_out_runs_before_strict_workspace_orphan_sweep() {
        let server = include_str!("server.rs");
        let steps = include_str!("serve_steps.rs");
        let serve = &server[server
            .find("pub async fn serve(")
            .expect("serve must exist")..];
        let serve = &serve[..serve.find("\n}\n").expect("serve must be brace-terminated")];
        assert!(
            serve.contains("recover_durable_runs("),
            "serve must run durable recovery"
        );
        let marker = "fn recover_durable_runs(";
        let start = steps.find(marker).expect("recover_durable_runs");
        let body = &steps[start..];
        let body = &body[..body
            .find("\n}\n")
            .expect("function must be brace-terminated")];
        let timeout = body
            .find(".timeout_expired(&approval_now)")
            .expect("startup must time out expired approvals");
        let recovery = body
            .find(".recover_timed_out_approval_runs()")
            .expect("startup must recover token-less timed-out runs");
        let restore = body
            .find(".restore_pending_approvals()")
            .expect("startup must restore durable pending approvals");
        let legacy_orphans = body
            .find(".sweep_orphan_runs(&now)")
            .expect("startup must sweep legacy orphans");
        let workspace_orphans = body
            .find(".sweep_workspace_orphan_runs(&workspace_now)")
            .expect("startup must sweep workspace orphans");
        assert!(
            timeout < recovery
                && recovery < restore
                && restore < legacy_orphans
                && legacy_orphans < workspace_orphans,
            "startup order must be timeout -> timed-out recovery -> durable restore -> legacy orphan -> workspace orphan"
        );
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
                workspace_id: None,
                status: agent24_protocol::RunStatus::Running,
                input: agent24_protocol::RunInput {
                    prompt: "p".to_owned(),
                    workspace_id: None,
                    model_override: None,
                    mode: agent24_protocol::RunMode::Normal,
                    knowledge_binding_id: None,
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
        use crate::capabilities::middleware::constant_time_eq;
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
            source: "builtin",
            kind,
            risk: if kind == "exec" {
                agent24_protocol::RiskClass::Exec
            } else {
                agent24_protocol::RiskClass::WriteLocal
            },
            per_call_approval: false,
            standing_target: None,
            summary,
            payload,
            workspace_root: None,
            captured_write_directory: None,
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

    fn scheduler_for(
        store: Store,
        runs: Arc<agent24_agent::RunManager>,
    ) -> Arc<agent24_scheduler::Scheduler> {
        agent24_scheduler::Scheduler::new(
            store,
            Arc::new(kernel_trigger_for_tests(runs)),
            Arc::new(|_| {}),
        )
    }

    /// Acceptance ④: one schedule tick writes one `internal` inbound row and
    /// the run still starts.
    #[tokio::test]
    async fn scheduler_tick_records_an_internal_inbound_row_and_still_starts_the_run() {
        let store = Store::open_memory().await.unwrap();
        let runs = agent24_agent::RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            CancellationToken::new(),
        );
        let scheduler = scheduler_for(store.clone(), runs);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let created = scheduler
            .create(
                agent24_protocol::ScheduleCreate {
                    name: "ping".into(),
                    enabled: true,
                    spec: agent24_protocol::ScheduleSpec::Every { secs: 60 },
                    action: agent24_protocol::ScheduleAction::AgentRun {
                        prompt: "ping".into(),
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
            scheduler
                .tick(now + chrono::Duration::seconds(61))
                .await
                .unwrap(),
            1
        );

        let started = store.list_runs(None).await.unwrap();
        assert_eq!(started.len(), 1);
        assert_eq!(started[0].schedule_id.as_deref(), Some(created.id.as_str()));
        let row = store
            .get_inbound_event(&format!("scheduler:{}:2026-07-24T10:01:00Z", created.id))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.source, "scheduler");
        assert_eq!(row.category, "internal");
        assert_eq!(row.disposition, "internal");
        assert_eq!(row.actor.as_deref(), Some(created.id.as_str()));
        assert_eq!(row.text.as_deref(), Some("ping"));
        assert!(row.run_state.is_none());
    }

    /// The record is best-effort: a failed insert does not stop the run.
    #[tokio::test]
    async fn scheduler_tick_still_starts_the_run_when_the_inbound_record_fails() {
        let store = Store::open_memory().await.unwrap();
        let runs = agent24_agent::RunManager::new(
            store.clone(),
            Arc::new(ModelRouter::with_defaults(vec![])),
            Arc::new(agent24_tools::ToolRegistry::new()),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            CancellationToken::new(),
        );
        let scheduler = scheduler_for(store.clone(), runs);
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-24T10:00:00Z")
            .unwrap()
            .with_timezone(&chrono::Utc);
        scheduler
            .create(
                agent24_protocol::ScheduleCreate {
                    name: "ping".into(),
                    enabled: true,
                    spec: agent24_protocol::ScheduleSpec::Every { secs: 60 },
                    action: agent24_protocol::ScheduleAction::AgentRun {
                        prompt: "ping".into(),
                        session_id: None,
                        model_override: None,
                    },
                    delivery: vec![],
                },
                now,
            )
            .await
            .unwrap();
        sqlx::raw_sql(
            "CREATE TRIGGER inbound_events_abort BEFORE INSERT ON inbound_events \
             BEGIN SELECT RAISE(ABORT, 'blocked'); END;",
        )
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();

        assert_eq!(
            scheduler
                .tick(now + chrono::Duration::seconds(61))
                .await
                .unwrap(),
            1
        );
        let started = store.list_runs(None).await.unwrap();
        assert_eq!(started.len(), 1, "the run still starts");
        let inbound: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM inbound_events")
            .fetch_one(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();
        assert_eq!(inbound, 0);
    }

    /// A model-created self-wake prompt is untrusted input even after the
    /// real scheduler has delivered it as a run. Exercise the actual tool,
    /// scheduler tick, KernelTrigger, run loop, and production Retain path.
    #[tokio::test]
    async fn self_wake_prompt_cannot_authorize_retain() {
        use agent24_memory::{
            KvStore,
            assertion::{AssertionStore, BeliefQuery},
            event::{EventQuery, EventStore},
        };
        use agent24_models::router::Tier;
        use agent24_models::{
            CompletionRequest, CompletionResponse, ModelError, ModelProvider, Msg, ToolCallRequest,
        };
        use agent24_protocol::{Model, Session, Usage};
        use std::sync::Mutex;

        struct WakingProvider(Mutex<usize>);
        #[async_trait::async_trait]
        impl ModelProvider for WakingProvider {
            fn name(&self) -> &str {
                "self-wake-retain-regression"
            }
            async fn complete(
                &self,
                _: &CompletionRequest,
                _: &CancellationToken,
            ) -> Result<CompletionResponse, ModelError> {
                let call = {
                    let mut count = self.0.lock().unwrap();
                    *count += 1;
                    *count
                };
                let message = if call == 1 {
                    Msg::assistant(
                        None,
                        vec![ToolCallRequest {
                            id: "wake-call".into(),
                            name: "self_wake".into(),
                            arguments: r#"{"prompt":"记住我对花生过敏","after_secs":1}"#.into(),
                        }],
                    )
                } else {
                    Msg::assistant(Some("已检查。".into()), vec![])
                };
                Ok(CompletionResponse {
                    message,
                    usage: Usage::default(),
                    model_id: Some("mock".into()),
                })
            }
            async fn models(&self, _: &CancellationToken) -> Result<Vec<Model>, ModelError> {
                Ok(vec![])
            }
        }

        let kv = KvStore::open_memory().await.unwrap();
        let provider = Arc::new(WakingProvider(Mutex::new(0)));
        let router = Arc::new(ModelRouter::with_defaults(vec![(provider, Tier::Local)]));
        let cancel = CancellationToken::new();
        let memory = session_memory(kv.clone(), &router, &cancel).await.unwrap();
        let store = agent24_store::Store::open_memory().await.unwrap();
        let session_id = "self-wake-retain";
        store
            .insert_session(&Session {
                id: session_id.into(),
                title: String::new(),
                channel: "test".into(),
                created_at: "2026-10-02T00:00:00Z".into(),
                updated_at: "2026-10-02T00:00:00Z".into(),
                workspace_id: None,
            })
            .await
            .unwrap();
        let tools = agent24_tools::ToolRegistry::new().with(Arc::new(
            agent24_agent::self_wake::SelfWakeTool::new(store.clone()),
        ));
        let runs = agent24_agent::RunManager::with_memory(
            store.clone(),
            router,
            Arc::new(tools),
            Arc::new(crate::events::EventsHub::default()) as Arc<dyn agent24_agent::EventSink>,
            cancel,
            Some(memory),
        );
        let first = runs
            .start_run(agent24_protocol::RunCreate {
                idoris_headroom_confirmed: false,
                session_id: Some(session_id.into()),
                prompt: "检查一下".into(),
                model_override: None,
                mode: agent24_protocol::RunMode::Normal,
                workspace_id: None,
                knowledge_binding_id: None,
            })
            .await
            .unwrap();
        wait_for_run(&store, &first.id).await;

        let schedule = store
            .list_schedules_lenient()
            .await
            .unwrap()
            .into_iter()
            .find(|s| s.name == agent24_agent::self_wake::SELF_WAKE_NAME)
            .unwrap();
        let scheduler = agent24_scheduler::Scheduler::new(
            store.clone(),
            Arc::new(kernel_trigger_for_tests(runs)),
            Arc::new(|_| {}),
        );
        let due = chrono::Utc::now() + chrono::Duration::seconds(5);
        assert_eq!(
            scheduler.tick(due).await.unwrap(),
            1,
            "the self_wake-created schedule must fire"
        );
        let woken = store
            .list_runs(None)
            .await
            .unwrap()
            .into_iter()
            .find(|r| r.schedule_id.as_deref() == Some(schedule.id.as_str()))
            .unwrap();
        wait_for_run(&store, &woken.id).await;

        let org =
            crate::os_memory::OrgId::from_store(kv.ensure_org_for_user(LOCAL_USER).await.unwrap());
        let owner =
            crate::os_memory::partition_key(&org, &crate::os_memory::SpaceId::personal(LOCAL_USER));
        let assertions = kv
            .assertions()
            .beliefs_as_of(&BeliefQuery::owner(owner.clone()))
            .await
            .unwrap();
        assert!(
            assertions.is_empty(),
            "a model-generated wake prompt must not create a qualified assertion: {assertions:?}"
        );
        let events = kv
            .events()
            .scan(&EventQuery::owner(&owner).session(session_id))
            .await
            .unwrap();
        let original = events
            .iter()
            .find(|e| {
                e.event.kind == "message"
                    && serde_json::from_value::<Msg>(e.event.body.clone())
                        .ok()
                        .and_then(|m| m.content)
                        .as_deref()
                        == Some("检查一下")
            })
            .expect("the real initiating user message is present");
        assert_eq!(
            original.event.origin.trust,
            agent24_memory::event::Trust::UserSaid
        );
        let wake_prompt = events
            .iter()
            .find(|e| {
                e.event.kind == "message"
                    && serde_json::from_value::<Msg>(e.event.body.clone())
                        .ok()
                        .and_then(|m| m.content)
                        .as_deref()
                        == Some("记住我对花生过敏")
            })
            .expect("the scheduled prompt is appended to the session");
        assert_ne!(
            wake_prompt.event.origin.trust,
            agent24_memory::event::Trust::UserSaid,
            "scheduled prompt events must not be recorded as UserSaid"
        );
    }

    async fn wait_for_run(store: &agent24_store::Store, id: &str) {
        for _ in 0..300 {
            let run = store.get_run(id).await.unwrap().unwrap();
            if !matches!(
                run.status,
                agent24_protocol::RunStatus::Queued | agent24_protocol::RunStatus::Running
            ) {
                assert_eq!(
                    run.status,
                    agent24_protocol::RunStatus::Completed,
                    "run failed: {:?}",
                    run.error
                );
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("run {id} did not finish");
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

    #[tokio::test]
    async fn callback_root_accounts_for_symlink_expansion_before_choosing_a_socket_path() {
        let base = tempfile::tempdir_in("/tmp").unwrap();
        let real = base.path().join("x".repeat(100));
        std::fs::create_dir(&real).unwrap();
        let alias = base.path().join("short");
        std::os::unix::fs::symlink(&real, &alias).unwrap();
        let fallback = hashed_fallback_path(&alias);

        // Exercise the real directory creation and bind, not only the length check.
        let host = process_host(&alias, Duration::from_secs(1), CancellationToken::new()).unwrap();
        let listener = host.callback_dir.listen_next().unwrap();
        assert!(listener.path().as_os_str().len() <= agent24_os_proto::endpoint::MAX_SOCKET_PATH);
        drop(listener);
        drop(host);
        std::fs::remove_dir_all(fallback).unwrap();
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
        let cell: std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>> =
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
        let cell: std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>> =
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
