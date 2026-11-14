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
    /// K-B3 #1715: service mode's presence queue, fed by the desktop's
    /// `role:"presence"` connection on the enrolment socket.
    pub presence_service: Option<Arc<crate::presence_service::ServicePresence>>,
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
                    skill_id: None,
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
            presence_service: None,
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

    /// Service mode: the desktop is not our parent, so presence comes over
    /// its own connection on the enrolment socket (K-B3 #1715).
    fn enable_service_presence(&mut self) {
        let presence = Arc::new(crate::presence_service::ServicePresence::new());
        self.presence_provider = Some(presence.clone());
        self.presence_service = Some(presence);
    }

    /// Where a capability daemon's presence confirmations go, by start mode:
    /// the parent desktop over HTTP, or the desktop's presence connection.
    fn enable_presence_for(&mut self, mode: StartupMode) {
        match mode {
            StartupMode::HostChild => self.enable_desktop_presence(),
            StartupMode::Service => self.enable_service_presence(),
            StartupMode::Legacy => {}
        }
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

#[cfg(test)]
#[path = "tests/elevation_summaries_http.rs"]
mod elevation_summaries_http_tests;

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
        authority.set_presence_connection(state.presence_service.clone());
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
    // Captured before `memory` moves into the run loop. The background task
    // installs into this same slot, so the breaker stays on the one retriever.
    let embed_backfill = memory
        .as_ref()
        .map(agent24_agent::SessionMemory::embed_backfill);
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
    let model_router = embed_backfill.as_ref().map(|_| Arc::clone(&state.router));
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
    // After the ready line. Awaiting the probe here would hold startup on the
    // embedding server (cold load, or a connection that never answers).
    if let (Some(backfill), Some(model_router)) = (embed_backfill, model_router) {
        let backfill_cancel = cancel.clone();
        tokio::spawn(async move {
            crate::embed_backfill::drive(backfill, model_router, backfill_cancel).await;
        });
    }
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
pub(crate) mod tests;
