use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::Arc as StdArc;
use std::time::Duration;

use agent24_models::router::ModelRouter;
use agent24_protocol::state_file::AuthMode;
use agent24_store::Store;

use super::{KernelTrigger, Shutdown, StartupMode, os_signature_policy};

pub(super) fn workspace_recovery_instant_at(
    time: std::time::SystemTime,
) -> std::io::Result<agent24_store::WorkspaceInstant> {
    let text = agent24_core::util::iso8601_millis_at(time).map_err(std::io::Error::other)?;
    agent24_store::WorkspaceInstant::parse(&text).map_err(std::io::Error::other)
}

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
    /// Signing team read from each package executable at discovery.
    /// `None` means the team was not read (not macOS, or unreadable).
    /// Tests set this directly. Not a grant.
    pub os_signing_teams: Arc<std::sync::RwLock<std::collections::HashMap<String, Option<String>>>>,
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
    /// `memory_owner` is `None` (memory unavailable; see [`super::open_memory_base`]).
    /// A SEPARATE handle from the one the run loop got (both are clones of the
    /// same pool via [`agent24_memory::KvStore`]'s `Clone`), because the
    /// `SessionMemory` that owns the run loop's handle is moved into
    /// [`agent24_agent::RunManager`] and no longer reachable from `AppState`.
    pub memory_kv: Option<agent24_memory::KvStore>,
    /// M1-T10: the personal partition key `memory_kv`'s handlers read and
    /// write — the SAME owner [`super::session_memory`] computed for the run loop.
    /// Never accepted from a request: the daemon injects it from session
    /// identity, per `docs/research/MEMORY-STRATEGY.md` §4.1's T10 row.
    pub memory_owner: Option<String>,
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
            os_signing_teams: Arc::new(std::sync::RwLock::new(std::collections::HashMap::new())),
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
    pub(super) fn enable_desktop_presence(&mut self) {
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
    pub(super) fn enable_presence_for(&mut self, mode: StartupMode) {
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
