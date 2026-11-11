// Each step borrows what `serve` still owns. `too_many_arguments` and
// `type_complexity` stay file-wide: splitting the signatures would change the
// step boundaries. `needless_borrow` / `ptr_arg` are allowed per function.
#![allow(clippy::too_many_arguments, clippy::type_complexity)]

use std::sync::Arc;
use std::sync::Arc as StdArc;
use std::time::Duration;

use agent24_models::router::ModelRouter;
use agent24_protocol::state_file::AuthMode;
use agent24_store::Store;
use tokio_util::sync::CancellationToken;

use super::{
    AlreadyRunning, AppDeps, AppState, LOCAL_USER, ModelCatalog, READY_SENT, Shutdown, StartupMode,
    build_guardian, catalogue_from_join, fill_attach_registry_and_recheck, generate_token,
    legacy_ready_line, mints_product_host, module_tools_if_live, open_memory_base,
    os_signature_policy, package_dirs_for_startup, process_host, register_discovered_module_tools,
    session_memory, spawn_approval_recovery_scan, stop_supervisors, validate_auth_startup,
    with_discovered_policy, workspace_recovery_instant_at,
};

pub(super) struct StillUp {
    pub(super) stopping: tokio::task::JoinHandle<()>,
    pub(super) usage_recorder: std::sync::Arc<crate::usage_recorder::UsageRecorder>,
    pub(super) usage_writer: tokio::task::JoinHandle<()>,
    pub(super) enroll: Option<crate::enroll_endpoint::EnrollEndpoint>,
    pub(super) daemon_pid: u32,
}

pub(super) fn install_shutdown_controller(
    auth_mode: AuthMode,
    host_bootstrap_stdio: bool,
    enroll_endpoint: bool,
    ephemeral: bool,
    cancel: &CancellationToken,
) -> Result<
    (
        StartupMode,
        Option<crate::consent_routes::GrantSettings>,
        Option<crate::host_bootstrap::ReadyWriter>,
        crate::lifecycle::Params,
        Vec<String>,
        Shutdown,
        std::path::PathBuf,
        std::path::PathBuf,
    ),
    std::io::Error,
> {
    let mode = validate_auth_startup(auth_mode, host_bootstrap_stdio, enroll_endpoint, ephemeral)?;
    let module_grants = crate::consent_routes::grant_settings(
        std::env::var("A24_MODULE_TOOL_GRANTS").ok().as_deref(),
        std::env::var("A24_MODULE_GRANT_MAX_DAYS").ok().as_deref(),
        ephemeral,
        crate::os_config::config_path(),
    )
    .map_err(|why| std::io::Error::new(std::io::ErrorKind::InvalidInput, why))?;
    // Only the host-child mode owns the trusted pipes (and so the parent-EOF
    // lease). Service mode has neither: it is not a child of anything.
    let (host_ready, parent_liveness) = if mode == StartupMode::HostChild {
        let (writer, liveness) = crate::host_bootstrap::open_stdio()?;
        (Some(writer), Some(liveness))
    } else {
        (None, None)
    };
    // The shutdown controller, and the signals that request it, before
    // anything else: a SIGTERM during startup — which can take seconds (the
    // store, MCP servers, model probing) — runs this bounded shutdown rather
    // than the default action, which ends the daemon as signal-killed, a crash
    // to a supervisor such as launchd (review of SUP-4, round 6).
    // The modules' budgets, read first — no I/O, and a bad value is only a
    // warning (SHUT-1b): the shutdown controller's deadlines follow them.
    let (params, config_warnings) = crate::lifecycle::Params::from_process_env();
    let shutdown = Shutdown::with_params(cancel.clone(), params);
    let idoris_root =
        agent24_idoris::install::default_install_root().map_err(std::io::Error::other)?;
    // ADR-038 D1: releases live under <data dir>/components/idoris; the legacy
    // root above keeps only state and any not-yet-migrated release.
    let idoris_install_root = agent24_idoris::install::managed_root(
        &agent24_protocol::state_file::state_dir()
            .ok_or_else(|| std::io::Error::other("data directory is unavailable"))?,
    );
    if !ephemeral {
        shutdown
            .idoris_budget
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }
    if let Some(parent_liveness) = parent_liveness {
        let parent_shutdown = shutdown.clone();
        tokio::spawn(async move {
            parent_liveness.closed().await;
            parent_shutdown.request();
            tracing::info!("trusted host bootstrap closed; shutting down capability daemon");
        });
    }
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
    Ok((
        mode,
        module_grants,
        host_ready,
        params,
        config_warnings,
        shutdown,
        idoris_root,
        idoris_install_root,
    ))
}

pub(super) fn acquire_singleton(ephemeral: bool) -> Result<Option<std::fs::File>, std::io::Error> {
    // Non-ephemeral daemons are singletons: hold an exclusive lifetime lock so
    // a concurrently-started second daemon fails fast instead of leaking as an
    // untracked process (review B6). Ephemeral instances skip both the lock
    // and the discovery file — they are private to one CLI invocation.
    let _singleton = if ephemeral {
        None
    } else {
        match agent24_protocol::state_file::try_acquire_singleton()? {
            Some(guard) => Some(guard),
            None => return Err(std::io::Error::other(AlreadyRunning)),
        }
    };
    Ok(_singleton)
}

pub(super) fn install_run_marker(
    ephemeral: bool,
    config_warnings: &mut Vec<String>,
) -> (
    Option<(
        std::path::PathBuf,
        crate::lifecycle::Previous,
        Option<crate::lifecycle::Summary>,
    )>,
    Option<crate::lifecycle::MarkerGuard>,
) {
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
    (evidence, marker)
}

pub(super) fn mint_startup_credentials(
    auth_mode: AuthMode,
    mode: StartupMode,
) -> Result<
    (
        String,
        String,
        Option<crate::capabilities::CapabilityStore>,
        Option<String>,
    ),
    std::io::Error,
> {
    let daemon_generation = generate_token();
    let (token, capability_store, product_host_token) = match auth_mode {
        AuthMode::LegacySingleToken => (generate_token(), None, None),
        AuthMode::Capabilities => {
            let store = crate::capabilities::CapabilityStore::new(daemon_generation.clone());
            // CORE-2 R5: a service has no trusted parent pipe to hand a host
            // credential to, so it mints none (and nothing secret is ever
            // published for it).
            let bearer = if !mints_product_host(mode) {
                None
            } else {
                let minted = store
                    .mint_product_host(
                        generate_token(),
                        Duration::from_secs(u64::MAX),
                        crate::capabilities::unix_now(),
                    )
                    .map_err(std::io::Error::other)?;
                Some(minted.into_bearer_parts().0)
            };
            (String::new(), Some(store), bearer)
        }
    };
    Ok((
        daemon_generation,
        token,
        capability_store,
        product_host_token,
    ))
}

#[allow(clippy::needless_borrow)]
pub(super) async fn open_store_workspace_and_idoris(
    ephemeral: bool,
    startup: &std::time::Instant,
    idoris_root: std::path::PathBuf,
    idoris_install_root: std::path::PathBuf,
    idoris_dependencies: &Option<agent24_idoris::managed_idoris::ManagedDependencies>,
    cancel: &CancellationToken,
) -> Result<
    (
        Store,
        std::path::PathBuf,
        Arc<ModelRouter>,
        Option<Arc<agent24_idoris::managed_idoris::ManagedIdoris>>,
        bool,
        Option<Arc<agent24_idoris::idoris_host::IdorisHost>>,
        Option<tokio::task::JoinHandle<Result<(), agent24_idoris::idoris_host::Code>>>,
    ),
    std::io::Error,
> {
    // Store: file-backed under ~/.agent24 (ephemeral instances get :memory:)
    let store = if ephemeral {
        Store::open_memory().await.map_err(std::io::Error::other)?
    } else {
        let dir = agent24_protocol::state_file::state_dir()
            .ok_or_else(|| std::io::Error::other("HOME not set"))?;
        // HW-1: one hardware probe per start, persisted to hardware.json.
        // Best-effort and off the startup path.
        crate::hardware::spawn_startup_snapshot(dir.clone());
        Store::open(&dir.join("agent24.db"))
            .await
            .map_err(std::io::Error::other)?
    };
    tracing::debug!(elapsed_ms = startup.elapsed().as_millis(), "startup: store");
    // NOTE: the startup sweeps (durable-resume restore + orphan cancel) run
    // AFTER the state is built, below — the restore sweep needs the run manager.
    // Tool workspace: the fs whitelist root + shell cwd. Created up front so
    // the canonicalized whitelist is non-empty from the first request.
    let workspace = agent24_protocol::state_file::state_dir()
        .ok_or_else(|| std::io::Error::other("HOME not set"))?
        .join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let router = Arc::new(ModelRouter::from_env());
    let managed_idoris = if ephemeral {
        None
    } else {
        let path = agent24_protocol::state_file::state_dir()
            .ok_or_else(|| std::io::Error::other("HOME not set"))?
            .join("idoris-connection.json");
        let managed = Arc::new(agent24_idoris::managed_idoris::ManagedIdoris::new(
            router.clone(),
            path,
        )?);
        if managed.is_configured()?
            && let Some(dependencies) = idoris_dependencies.as_ref()
        {
            managed
                .restore(dependencies, &cancel)
                .await
                .map_err(std::io::Error::other)?;
        }
        Some(managed)
    };
    let mut idoris_unavailable = false;
    let mut idoris_host = if let Some(managed) = &managed_idoris {
        match agent24_idoris::idoris_host::IdorisHost::system(
            idoris_root,
            idoris_install_root,
            managed.clone(),
            router.clone(),
        ) {
            Ok(host) => Some(host),
            Err(error) => {
                tracing::warn!(%error, "iDoris lifecycle unavailable; daemon startup continues");
                idoris_unavailable = true;
                router.block_idoris("iDoris lifecycle unavailable");
                None
            }
        }
    } else {
        None
    };
    // Registered before restore can spawn a process, including startup cancellation.
    let idoris_stopping = idoris_host
        .as_ref()
        .map(|host| host.stop_on_shutdown(cancel.clone()));
    if let Some(host) = &idoris_host
        && let Err(code) = host.restore().await
    {
        tracing::warn!(
            ?code,
            "iDoris restore unavailable; daemon startup continues"
        );
        idoris_unavailable = true;
        router.block_idoris("iDoris restore unavailable");
        idoris_host = None;
    }
    Ok((
        store,
        workspace,
        router,
        managed_idoris,
        idoris_unavailable,
        idoris_host,
        idoris_stopping,
    ))
}

#[allow(clippy::needless_borrow)]
pub(super) async fn open_guardian_and_memory(
    ephemeral: bool,
    router: &Arc<ModelRouter>,
    cancel: &CancellationToken,
    startup: &std::time::Instant,
) -> (
    Option<StdArc<agent24_policy::guardian::Guardian>>,
    Option<agent24_memory::KvStore>,
    Option<agent24_agent::SessionMemory>,
) {
    let guardian = build_guardian(&router);
    // D1 session memory: a KV file next to the main store (ephemeral daemons get
    // an in-memory one). A failure here degrades to no memory rather than
    // refusing to start — sessions simply don't remember, as before.
    let memory_base = open_memory_base(ephemeral).await;
    // P1-02c-c5: bound memory.db's WAL — idle-time wal_checkpoint(TRUNCATE),
    // tracing-only results, stops with the daemon-wide token. In-memory
    // (ephemeral) bases have no WAL and spawn nothing. Detached on purpose:
    // it owns its own connection and ends on `cancel`.
    if let Some(kv) = &memory_base {
        let _ = crate::memory_maintenance::spawn(
            kv,
            agent24_memory::wal_maintenance::WalMaintenanceConfig::default(),
            cancel.clone(),
        )
        .await;
    }
    tracing::debug!(
        elapsed_ms = startup.elapsed().as_millis(),
        "startup: memory_base"
    );
    let memory = match memory_base.clone() {
        Some(kv) => match session_memory(kv, &router, &cancel).await {
            Ok(memory) => Some(memory),
            Err(err) => {
                tracing::warn!(
                    error = %err,
                    "could not initialize the personal session memory partition; sessions will not remember"
                );
                None
            }
        },
        None => None,
    };

    tracing::debug!(
        elapsed_ms = startup.elapsed().as_millis(),
        "startup: session_memory"
    );
    (guardian, memory_base, memory)
}

pub(super) async fn discover_packages(
    ephemeral: bool,
    cancel: &CancellationToken,
) -> Result<
    (
        std::path::PathBuf,
        Arc<std::path::PathBuf>,
        Vec<crate::domain::Installed>,
        Vec<agent24_os_packages::discovery::Refused>,
    ),
    std::io::Error,
> {
    let state_dir = agent24_protocol::state_file::state_dir()
        .ok_or_else(|| std::io::Error::other("HOME not set"))?;
    let packages_root = Arc::new(agent24_os_packages::packages_root(&state_dir, ephemeral));
    // Hashing a package is blocking filesystem I/O (up to 512 MiB). The
    // shutdown token stops it; a cancelled package is left out of the
    // catalogue, so it is not registered and not spawned.
    let discovered = catalogue_from_join(
        tokio::task::spawn_blocking({
            let packages_root = StdArc::clone(&packages_root);
            let cancel = cancel.clone();
            move || {
                let policy = os_signature_policy();
                let mut pass =
                    with_discovered_policy(Vec::new(), packages_root.as_ref(), &cancel, &policy);
                // Same check `mount_package` repeats before routes. Doing it
                // here is what keeps a package that changed after scan off
                // the tool registry. The policy is the one the scan just used.
                let dropped =
                    crate::domain::retain_rechecked(&mut pass.catalogue, &cancel, &policy);
                pass.integrity_refusals.extend(dropped);
                pass
            }
        })
        .await,
    )?;
    let catalogue = discovered.catalogue;
    let integrity_refusals = discovered.integrity_refusals;
    Ok((state_dir, packages_root, catalogue, integrity_refusals))
}

#[allow(clippy::needless_borrow)]
pub(super) async fn register_startup_tools(
    workspace: std::path::PathBuf,
    store: &Store,
    router: &Arc<ModelRouter>,
    catalogue: &[crate::domain::Installed],
    cancel: &CancellationToken,
) -> (
    agent24_tools::ToolRegistry,
    agent24_domain::tool::ModuleToolRegistry,
    Arc<crate::module_tool_runtime::AgentModuleToolRuntime>,
    StdArc<dyn agent24_tools::ModuleToolRuntime>,
    Arc<std::sync::OnceLock<Arc<crate::domain::Supervisors>>>,
    std::collections::HashMap<(String, String), StdArc<dyn agent24_tools::ModuleToolAuthorization>>,
    crate::consent_routes::ModuleToolCards,
    StdArc<dyn agent24_domain::tool::ModuleToolAdvertView>,
    StdArc<agent24_knowledge::host::KnowledgeHostRegistry>,
    Vec<Arc<agent24_mcp::McpServer>>,
    Arc<crate::mcp::McpRoster>,
) {
    // M-E/E1b: mount external MCP servers from ~/.agent24/mcp.json and register
    // their tools. Registered with `with()` so they are dispatchable, while
    // McpTool sets requires_approval = true so EVERY call still goes through the
    // C4 gate — the whitelist decides "may be dispatched", the gate decides
    // "may run this time". A broken server is logged and skipped, never fatal.
    let mut tools = agent24_tools::ToolRegistry::builtin(workspace.clone());
    let mut module_registry = agent24_domain::tool::ModuleToolRegistry::new();
    let mut module_operations = std::collections::HashSet::new();
    register_discovered_module_tools(
        &catalogue,
        &cancel,
        &mut module_registry,
        &mut module_operations,
    );
    (module_registry, module_operations) =
        module_tools_if_live(&cancel, module_registry, module_operations);
    let module_tool_runtime =
        crate::module_tool_runtime::AgentModuleToolRuntime::new(module_operations);
    let module_tool_runtime_dyn: StdArc<dyn agent24_tools::ModuleToolRuntime> =
        module_tool_runtime.clone();
    let module_tool_supervisors = module_tool_runtime.supervisors_slot();
    let mut module_authorizations: std::collections::HashMap<
        (String, String),
        StdArc<dyn agent24_tools::ModuleToolAuthorization>,
    > = std::collections::HashMap::new();
    let mut card_sources = std::collections::HashMap::new();
    for (_, registered) in module_registry.iter() {
        let module = registered.module().to_owned();
        let declaration = registered.declaration();
        let operation = declaration.operation().to_owned();
        let entry = catalogue.iter().find(|entry| entry.name == module);
        // K1-6a-G §2.1: the card and the gate are made from one summary, so
        // a grant's fingerprint and the gate's agree byte for byte.
        let source = crate::consent_routes::ToolCardSource {
            summary: agent24_policy::tool_summary::module_tool_summary(
                &module,
                &entry.map_or_else(String::new, |entry| entry.version.clone()),
                declaration,
            ),
            resource_kinds: declaration.resource_kinds().to_vec(),
            takes_grants: entry.is_some_and(|entry| {
                matches!(&entry.build, crate::domain::Build::Package(p) if p.manifest.takes_grants())
            }),
        };
        let authorization = crate::module_tool_runtime::tool_authorization(
            &source,
            &store,
            module_tool_runtime.artifact_source(),
        );
        card_sources.insert((module.clone(), operation.clone()), source);
        if let Some(authorization) = authorization {
            module_authorizations.insert((module, operation), authorization);
        }
    }
    let module_tool_cards = crate::consent_routes::ModuleToolCards {
        tools: StdArc::new(card_sources),
        artifact: module_tool_runtime.artifact_source(),
        notice: {
            let runtime = module_tool_runtime.clone();
            StdArc::new(move |end: crate::module_tool_runtime::GrantEnd| {
                let runtime = runtime.clone();
                Box::pin(async move {
                    runtime
                        .notify_revoked(&end, crate::module_tool_runtime::REVOCATION_NOTICE_WITHIN)
                        .await
                }) as futures::future::BoxFuture<'static, _>
            })
        },
    };
    let advert_view = crate::module_tool_runtime::AgentModuleToolAdvertView::new(
        StdArc::clone(&module_tool_runtime),
        module_authorizations.clone(),
    );
    let advert_view_dyn: StdArc<dyn agent24_domain::tool::ModuleToolAdvertView> = advert_view;
    crate::module_tool_runtime::register_module_tools(
        &mut tools,
        &module_registry,
        &module_authorizations,
        &advert_view_dyn,
        &module_tool_runtime_dyn,
        &store,
    );
    // H9: register the read-only explorer subagent. It runs against a registry
    // that holds ONLY read builtins and NOT itself, so the sub-run cannot write,
    // execute, or recurse — the guarantees are structural, not policy-checked.
    let explorer_tools = StdArc::new(agent24_tools::ToolRegistry::read_only(workspace));
    tools = tools.with(StdArc::new(
        agent24_agent::subagent::ExplorerSubagent::new_with_store(
            Arc::clone(&router),
            explorer_tools,
            store.clone(),
        ),
    ));
    // H5: register self-wake. It creates one-shot schedules in the same store the
    // scheduler reads, so an agent can schedule its own follow-ups; the woken run
    // is gated like any other (see self_wake module docs).
    tools = tools.with(StdArc::new(agent24_agent::self_wake::SelfWakeTool::new(
        store.clone(),
    )));
    // #1087 b: the Knowledge tools, offered only to a run that names a
    // binding. They read the private host interface's registry, which the
    // state takes over below, so both see the same activations and C.
    let knowledge_host = StdArc::new(agent24_knowledge::host::KnowledgeHostRegistry::default());
    let knowledge_runs: StdArc<dyn agent24_knowledge::backend::RunSelector> =
        StdArc::new(crate::knowledge_backend::StoreRuns(store.clone()));
    for tool in crate::knowledge_tools::tools(StdArc::clone(&knowledge_host), knowledge_runs) {
        tools = tools.with(tool);
    }
    let (mcp_servers, mcp_roster) = match crate::mcp::config_path() {
        Some(path) => match crate::mcp::load_config(&path) {
            Ok(cfg) => {
                let targets = cfg.targets();
                if targets.is_empty() {
                    (Vec::new(), Arc::new(crate::mcp::McpRoster::default()))
                } else {
                    let (servers, mcp_tools, roster) = crate::mcp::mount(&targets, &cancel).await;
                    for tool in mcp_tools {
                        tools = tools.with(tool);
                    }
                    (servers, roster)
                }
            }
            Err(err) => {
                tracing::error!("ignoring {}: {err}", path.display());
                (Vec::new(), Arc::new(crate::mcp::McpRoster::default()))
            }
        },
        None => (Vec::new(), Arc::new(crate::mcp::McpRoster::default())),
    };
    (
        tools,
        module_registry,
        module_tool_runtime,
        module_tool_runtime_dyn,
        module_tool_supervisors,
        module_authorizations,
        module_tool_cards,
        advert_view_dyn,
        knowledge_host,
        mcp_servers,
        mcp_roster,
    )
}

#[allow(clippy::needless_borrow, clippy::ptr_arg)]
pub(super) async fn assemble_app_state(
    token: &String,
    router: Arc<ModelRouter>,
    tools: agent24_tools::ToolRegistry,
    store: Store,
    shutdown: &Shutdown,
    guardian: Option<StdArc<agent24_policy::guardian::Guardian>>,
    memory: Option<agent24_agent::SessionMemory>,
    mcp_servers: Vec<Arc<agent24_mcp::McpServer>>,
    packages_root: &Arc<std::path::PathBuf>,
    params: &crate::lifecycle::Params,
    config_warnings: &[String],
    evidence: &Option<(
        std::path::PathBuf,
        crate::lifecycle::Previous,
        Option<crate::lifecycle::Summary>,
    )>,
    state_dir: &std::path::PathBuf,
    knowledge_host: StdArc<agent24_knowledge::host::KnowledgeHostRegistry>,
    managed_idoris: Option<Arc<agent24_idoris::managed_idoris::ManagedIdoris>>,
    idoris_host: Option<Arc<agent24_idoris::idoris_host::IdorisHost>>,
    idoris_unavailable: bool,
    capability_store: Option<crate::capabilities::CapabilityStore>,
    mode: StartupMode,
    mcp_roster: Arc<crate::mcp::McpRoster>,
    ephemeral: bool,
) -> Result<(agent24_protocol::ShutdownReport, AppState), std::io::Error> {
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
    #[cfg(unix)]
    let workspace_service = if ephemeral {
        None
    } else {
        Some(StdArc::new(
            agent24_workspace::WorkspaceService::compose(store.clone(), &state_dir)
                .map_err(std::io::Error::other)?,
        ))
    };
    #[cfg(not(unix))]
    let workspace_service = None;
    let has_memory = memory.is_some();
    let mut state = AppState::new(AppDeps {
        token: token.clone(),
        router,
        tools,
        store,
        risk_overrides,
        shutdown: shutdown.clone(),
        guardian,
        memory,
        workspace_service,
        mcp_servers,
        packages_root: Arc::clone(&packages_root),
    });
    // X11-2d: expire inbound message text (and its run/memory copies) after
    // 180 days. The purge goes through the run manager (it owns the session
    // memory and its locks). Detached on purpose: it ends on the shutdown token.
    drop(crate::inbound::spawn_text_expiry(
        &state.store,
        has_memory.then(|| state.runs.clone()),
        ephemeral,
        shutdown.child_token(),
    ));
    // Before any clone into the router. `AppState::new` starts this empty so
    // tests that never mount MCP keep an empty roster.
    state.mcp_roster = mcp_roster;
    state.knowledge_host = knowledge_host;
    state.managed_idoris = managed_idoris;
    state.idoris_host = idoris_host;
    state.idoris_unavailable = idoris_unavailable;
    if let Some(capability_store) = capability_store {
        state.enable_capability_auth(capability_store);
        if mode == StartupMode::HostChild {
            state.enable_desktop_presence();
        }
        state.delegations = Arc::new(crate::capabilities::delegation::Delegations::open(
            &state_dir,
        )?);
    }
    // Last, so it wins over the desktop provider that HostChild mode installs
    // just above (the e2e daemon runs as a host child). Debug test builds only.
    #[cfg(feature = "test-enrollment")]
    if crate::enroll_test_support::enabled() {
        state.presence_provider = Some(Arc::new(crate::enroll_test_support::TestPresence));
    }
    Ok((report, state))
}

#[allow(clippy::needless_borrow)]
pub(super) async fn recover_durable_runs(
    state: &AppState,
    ephemeral: bool,
    cancel: &CancellationToken,
    module_tool_runtime: &Arc<crate::module_tool_runtime::AgentModuleToolRuntime>,
    module_registry: &agent24_domain::tool::ModuleToolRegistry,
) -> Result<Result<crate::os_config::OsConfig, String>, std::io::Error> {
    // M7 durable expiry recovery runs before durable-resume: a stale approval
    // must become timed_out before anything can re-broadcast it or preflight its
    // authority. The orphan sweeps below then own cancellation of its parked run.
    let approval_now = agent24_core::util::now_iso8601();
    let expired = state
        .broker
        .timeout_expired(&approval_now)
        .await
        .map_err(std::io::Error::other)?;
    if expired > 0 {
        tracing::warn!("timed out {expired} expired approval(s) during startup recovery");
    }
    let recovered_timeouts = state
        .runs
        .recover_timed_out_approval_runs()
        .await
        .map_err(std::io::Error::other)?;
    if recovered_timeouts > 0 {
        tracing::warn!(
            "cancelled {recovered_timeouts} token-less run(s) after startup approval timeout"
        );
    }

    // H3 durable-resume startup, BEFORE accepting any request and BEFORE the
    // orphan sweep: restore restorable parked approvals (re-broadcast + keep
    // pending) so their runs survive to be resumed when answered, and abort the
    // rest fail-closed. The orphan sweep then cancels every still-non-terminal
    // run whose approval did NOT survive — so the restore MUST come first.
    let (restored, aborted) = state
        .runs
        .restore_pending_approvals()
        .await
        .map_err(std::io::Error::other)?;
    if restored > 0 || aborted > 0 {
        tracing::info!(
            "durable resume: {restored} approval(s) restored, {aborted} aborted from a previous process"
        );
    }
    let now = agent24_core::util::now_iso8601();
    let orphans = state
        .store
        .sweep_orphan_runs(&now)
        .await
        .map_err(std::io::Error::other)?;
    if orphans > 0 {
        tracing::warn!("cancelled {orphans} orphan non-terminal runs from a previous process");
    }
    let workspace_now = workspace_recovery_instant_at(std::time::SystemTime::now())?;
    let workspace_orphans = state
        .store
        .sweep_workspace_orphan_runs(&workspace_now)
        .await
        .map_err(std::io::Error::other)?;
    if workspace_orphans.released_leases > 0 {
        tracing::warn!(
            "cancelled {} workspace-bound orphan run(s) from a previous process",
            workspace_orphans.released_leases
        );
    }
    // After both orphan sweeps, so a cancelled inbound run is already terminal
    // and its row can leave the in-flight index (`started` → `done`).
    let inbound = crate::inbound::reconcile_inbound_runs(&state.store)
        .await
        .map_err(std::io::Error::other)?;
    if inbound.started > 0 || inbound.done > 0 || inbound.interrupted > 0 {
        tracing::warn!(
            started = inbound.started,
            done = inbound.done,
            interrupted = inbound.interrupted,
            "reconciled inbound runs from a previous process"
        );
    }

    // M7: approvals can outlive the task that originally armed their in-memory
    // timeout. Keep durable expiry/recovery active for the daemon lifetime; a
    // shutdown child token stops the loop cleanly, and each interval is also the
    // retry mechanism for transient store or recovery-cancel failures.
    spawn_approval_recovery_scan(
        Arc::clone(&state.broker),
        Arc::clone(&state.runs),
        cancel.child_token(),
    );

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

    let os_config_path =
        crate::os_config::config_path().ok_or_else(|| std::io::Error::other("HOME not set"))?;
    let os_config = crate::os_config::OsConfig::load(&os_config_path);
    if let Err(why) = &os_config {
        tracing::error!("os.json could not be read ({why}); every admissible domain OS will 503");
    }
    // K1-6a-G §2.1 F3: what this start sees of each module with tools,
    // before a grant of it can hold. A module this fails for keeps none;
    // an ephemeral daemon sees none, so honours no grant (F6).
    if !ephemeral && let Ok(config) = &os_config {
        crate::module_tool_runtime::observe_enable_epochs(
            &state.store,
            &module_tool_runtime,
            config,
            &os_config_path,
            module_registry.iter().map(|(_, r)| r.module()),
        )
        .await;
    }
    Ok(os_config)
}

#[allow(clippy::needless_borrow, clippy::ptr_arg)]
pub(super) async fn prepare_os_mount(
    ephemeral: bool,
    state_dir: &std::path::PathBuf,
    catalogue: &[crate::domain::Installed],
    os_config: &Result<crate::os_config::OsConfig, String>,
    state: &AppState,
    cancel: &CancellationToken,
    memory_base: Option<agent24_memory::KvStore>,
) -> (
    std::path::PathBuf,
    ModelCatalog,
    Option<crate::domain::MemoryLease>,
) {
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
    // A model declaration lives in a manifest. Packages carry that manifest
    // already (it was read at discovery), so a package that declares a model can
    // be known without constructing anything: probe if any does. Compiled-in
    // modules still cannot say without being built, and none in this build
    // declares one.
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
    (os_root, inventory, lease)
}

pub(super) async fn bind_loopback(
    startup: &std::time::Instant,
    port: u16,
) -> Result<(tokio::net::TcpListener, std::net::SocketAddr), std::io::Error> {
    // SUP-4: what out-of-process modules are started with. Its callback
    // sockets live under the state directory — or, for an ephemeral daemon,
    // under its throwaway root. Directories left by daemons that are gone are
    // cleared first (FU-56). A daemon that cannot set this up still runs; its
    // packages degrade, with the reason.
    // 127.0.0.1 only — never a public bind (SPEC-001 §9). Bound BEFORE any
    // module is started: a port that is taken fails startup here, rather than
    // after packages are running, on a path that would leave them to the
    // runtime's teardown instead of the ordered stop (review of SUP-4, round 4).
    tracing::debug!(
        elapsed_ms = startup.elapsed().as_millis(),
        "startup: before_bind"
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    tracing::debug!(elapsed_ms = startup.elapsed().as_millis(), "startup: bound");
    let local = listener.local_addr()?;
    Ok((listener, local))
}

#[allow(clippy::needless_borrow)]
pub(super) fn start_process_host(
    ephemeral: bool,
    os_root: &std::path::PathBuf,
    state_dir: &std::path::PathBuf,
    params: crate::lifecycle::Params,
    shutdown: &Shutdown,
    module_tool_supervisors: &Arc<std::sync::OnceLock<Arc<crate::domain::Supervisors>>>,
) -> Result<crate::domain::ProcessHost, String> {
    let host = process_host(
        if ephemeral { &os_root } else { &state_dir },
        params.stop_grace,
        shutdown.token().clone(),
    );
    if let Ok(host) = &host {
        let _ = module_tool_supervisors.set(StdArc::clone(&host.supervisors));
    }
    if let Err(why) = &host {
        tracing::error!("out-of-process domain OS modules cannot be started: {why}");
    }
    host
}

pub(super) fn spawn_ordered_shutdown(
    shutdown: &Shutdown,
    host: &Result<crate::domain::ProcessHost, String>,
    ephemeral: bool,
    idoris_stopping: Option<tokio::task::JoinHandle<Result<(), agent24_idoris::idoris_host::Code>>>,
    marker: Option<crate::lifecycle::MarkerGuard>,
) -> (
    Arc<std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>>>,
    Arc<crate::comm_routes::CommDaemonSlot>,
    tokio::task::JoinHandle<()>,
) {
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
        std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>>,
    > = Arc::new(std::sync::OnceLock::new());
    let stopping_attach_registry = Arc::clone(&attach_registry_cell);
    // COMM-4a: the Hyphae daemon supervisor (if comm_routes manages to
    // build one) is, like the attach registry above, only known once
    // `comm_routes::build` runs — AFTER this task is spawned. PR #626
    // review, High #1: unlike the attach registry's plain `OnceLock`, this
    // is a `CommDaemonSlot` — a lock `comm_routes::build_ready_state`
    // registers into BEFORE any autostart attempt, synchronized with this
    // same task's own `close()` below, so a shutdown that begins mid-`build`
    // can never let an autostart spawn a Hyphae daemon nothing will ever be
    // told to stop. See `comm_routes::CommDaemonSlot`'s own doc comment.
    let comm_daemon_slot: Arc<crate::comm_routes::CommDaemonSlot> =
        Arc::new(crate::comm_routes::CommDaemonSlot::new());
    let stopping_comm_daemon = Arc::clone(&comm_daemon_slot);
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
        // COMM-4a §6.2 / PR #626 review, High #2: comm's stop shares the
        // SAME absolute deadline every other out-of-process module's stop
        // does, and runs CONCURRENTLY with them — it used to run
        // sequentially, AFTER this `timeout_at` had already elapsed, under
        // its own unvalidated ~5s-default grace (vs. SHUT-1b's validated
        // 500ms default): a Hyphae daemon that ignored SIGTERM could then
        // have `agent24d` hit the watchdog and exit before Hyphae was ever
        // sent SIGKILL or a shutdown record was written for it.
        // `HyphaeDaemonSupervisor::shutdown` queues its `Cmd::Shutdown`
        // (channel `send`) before this `.await` ever starts waiting on the
        // reply, so if the shared deadline is hit below, the actor task
        // keeps running the stop (TERM, grace, KILL, reap) to completion on
        // its own schedule regardless of whether anything here is still
        // waiting on its result.
        let comm_shutdown = async {
            // PR #626 review, High #1: `close()`, not a plain read — see
            // `CommDaemonSlot`'s doc comment; this is the one call that may
            // ever close this slot. High #2: `deadlines.modules` is the
            // SAME absolute instant the `timeout_at` below races this
            // against, handed straight into the actor (not just this
            // await) so an in-flight config-change restart's own stop of
            // an OLD generation picks it up on its very next grace-loop
            // poll, and nothing may spawn past it from here on.
            match stopping_comm_daemon.close() {
                Some(daemon) => Some(daemon.shutdown(deadlines.modules).await),
                None => None,
            }
        };
        let (supervisors_result, comm_result, idoris_result) = tokio::join!(
            tokio::time::timeout_at(deadlines.modules, stop_supervisors(closed, params.drain)),
            tokio::time::timeout_at(deadlines.modules, comm_shutdown),
            tokio::time::timeout_at(deadlines.persist - crate::lifecycle::PERSIST, async move {
                match idoris_stopping {
                    Some(task) => task.await,
                    None => Ok(Ok(())),
                }
            }),
        );
        if !matches!(idoris_result, Ok(Ok(Ok(())))) {
            tracing::warn!("managed iDoris shutdown failed or exceeded the host budget");
        }
        if supervisors_result.is_err() {
            tracing::warn!(
                "out-of-process modules were still stopping at the deadline; their supervisors \
                 were dropped (SIGKILL attempted, exit unconfirmed)"
            );
        }
        let mut records: Vec<_> = tracked
            .iter()
            .map(|(name, reason, record)| (name.clone(), *reason, record.snapshot()))
            .collect();
        // COMM-4a §6.2: comm's stop is folded into this same module-stop
        // phase, recorded as `comm.hyphae` in `last-shutdown.json`'s
        // `records[]`. `agent24-comm` never depends on
        // `agent24-os-proto` (COMM-HYPHAE.md §7's zero-run boundary), so the
        // translation from its own neutral `DaemonShutdownOutcome` into this
        // crate's `StopRecord` happens here, not there.
        match comm_result {
            Ok(None) => {}
            Ok(Some(outcome)) => {
                let leader = outcome.leader.map(|l| match l {
                    agent24_comm::ShutdownLeader::GoneBeforeTerm => {
                        agent24_os_proto::stop_record::Leader::GoneBeforeTerm
                    }
                    agent24_comm::ShutdownLeader::ExitedInGrace => {
                        agent24_os_proto::stop_record::Leader::ExitedInGrace
                    }
                    agent24_comm::ShutdownLeader::KilledAfterGrace => {
                        agent24_os_proto::stop_record::Leader::KilledAfterGrace
                    }
                });
                // `StopRecord` has two private fields (`drain_began`,
                // `abandoning`) not meant for a caller outside
                // `agent24-os-proto` to set, so `..Default::default()` is
                // unavailable here — plain field assignment on a
                // `Default::default()` value only touches the public ones
                // this comm translation actually has an opinion about.
                let mut record = agent24_os_proto::stop_record::StopRecord::default();
                record.process = Some(if outcome.had_process {
                    agent24_os_proto::stop_record::ProcessAtStop::Running
                } else {
                    agent24_os_proto::stop_record::ProcessAtStop::None
                });
                record.leader = leader;
                // PR #642 review round 3, Medium #1: `group` now comes
                // straight from `kill_group_gracefully`'s own final
                // post-KILL probe (`group_confirmed_gone`), not from
                // inferring it off how the LEADER alone ended. The previous
                // version recorded `Gone` for ANY leader outcome other than
                // `KilledAfterGrace` — including `ExitedInGrace`, where the
                // leader exiting on TERM says nothing about whether a
                // helper the leader left behind (still `Ok`/`EPERM` after
                // its own SIGKILL and the bounded post-KILL probe) was ever
                // actually confirmed gone. `Failed` (not `KillAttempted`,
                // which `stop_record.rs` reserves for a stop that was
                // dropped/cut off before ever reaching this probe) is the
                // correct fact here: this call always ran to completion and
                // always sent the KILL — it just could not confirm the
                // group empty afterwards.
                record.group = outcome.group_confirmed_gone.map(|confirmed| {
                    if confirmed {
                        agent24_os_proto::stop_record::GroupEnd::Gone
                    } else {
                        agent24_os_proto::stop_record::GroupEnd::Failed
                    }
                });
                record.supervisor = Some(agent24_os_proto::stop_record::SupervisorEnd::Stopped);
                records.push((
                    "comm.hyphae".to_owned(),
                    crate::lifecycle::Reason::Shutdown,
                    record,
                ));
            }
            Err(_) => {
                // The shared deadline was hit before comm's own stop
                // replied. Its process group is still being cleaned up on
                // the actor's own task either way (see the comment above) —
                // this only means the OUTCOME could not be confirmed in
                // time to record accurately, so the record says exactly
                // that rather than guessing `Gone`.
                tracing::warn!(
                    "comm: the hyphae daemon's stop did not complete by the shared modules \
                     deadline; its process group is still being cleaned up on the \
                     supervisor's own task, but the outcome could not be confirmed in time \
                     to record"
                );
                let mut record = agent24_os_proto::stop_record::StopRecord::default();
                record.process = Some(agent24_os_proto::stop_record::ProcessAtStop::Running);
                record.supervisor = Some(agent24_os_proto::stop_record::SupervisorEnd::CutOff);
                records.push((
                    "comm.hyphae".to_owned(),
                    crate::lifecycle::Reason::Shutdown,
                    record,
                ));
            }
        }
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
    (attach_registry_cell, comm_daemon_slot, stopping)
}

pub(super) fn start_usage_and_model_deps(
    shutdown: &Shutdown,
    state: &AppState,
) -> (
    std::sync::Arc<crate::usage_recorder::UsageRecorder>,
    tokio::task::JoinHandle<()>,
    crate::model_callback::ModelCallbackDeps,
) {
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
            tokio::time::sleep_until(shutdown.deadlines().calls + crate::lifecycle::CONFIRM).await;
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
        cancel_root: crate::model_callback::spawn_cancel_root(shutdown.calls_cut_off()),
        admission: crate::model_callback::ModelAdmission::new(
            crate::model_callback::MODEL_MAX_IN_FLIGHT_GLOBAL,
            crate::model_callback::MODEL_MAX_IN_FLIGHT_PER_MODULE,
        ),
        events: state.events.clone(),
        timings: state.timings.clone(),
    };
    (usage_recorder, usage_writer, model_deps)
}

#[allow(clippy::needless_borrow)]
pub(super) async fn mount_domain_os(
    catalogue: &[crate::domain::Installed],
    os_root: &std::path::Path,
    state: &AppState,
    os_config: &Result<crate::os_config::OsConfig, String>,
    inventory: &ModelCatalog,
    lease: &Option<crate::domain::MemoryLease>,
    host: &Result<crate::domain::ProcessHost, String>,
    integrity_refusals: &[agent24_os_packages::discovery::Refused],
    model_deps: &crate::model_callback::ModelCallbackDeps,
) -> (
    axum::Router,
    Vec<crate::domain::MountReport>,
    crate::os_memory::OsMemoryCatalog,
    std::collections::HashMap<String, std::path::PathBuf>,
) {
    let (module_routes, mut reports, partitions) = crate::domain::mount_all_with_refusals(
        &catalogue,
        &os_root,
        &state.events,
        os_config.as_ref().map_err(String::as_str),
        inventory,
        lease.as_ref(),
        host.as_ref().map_err(String::as_str),
        &state.module_approval_broker,
        &integrity_refusals,
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
    let (refusal_rows, package_dirs) =
        package_dirs_for_startup(&catalogue, &integrity_refusals, &reports);
    reports.extend(refusal_rows);
    (module_routes, reports, partitions, package_dirs)
}

pub(super) fn arm_scheduler_after_mount(
    state: &mut AppState,
    catalogue: &[crate::domain::Installed],
    host: &Result<crate::domain::ProcessHost, String>,
    cancel: &CancellationToken,
) {
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
}

#[allow(clippy::needless_borrow)]
pub(super) async fn publish_mount_and_attach(
    state: &mut AppState,
    partitions: &crate::os_memory::OsMemoryCatalog,
    lease: &Option<crate::domain::MemoryLease>,
    reports: Vec<crate::domain::MountReport>,
    package_dirs: std::collections::HashMap<String, std::path::PathBuf>,
    host: &Result<crate::domain::ProcessHost, String>,
    module_tool_cards: crate::consent_routes::ModuleToolCards,
    cancel: &CancellationToken,
    module_grants: Option<crate::consent_routes::GrantSettings>,
    report: agent24_protocol::ShutdownReport,
    model_deps: crate::model_callback::ModelCallbackDeps,
    ephemeral: bool,
    attach_registry_cell: &Arc<
        std::sync::OnceLock<Arc<agent24_attach::attach_registry::AttachRegistry>>,
    >,
    shutdown: &Shutdown,
) {
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
    state.module_namespaces = Arc::new(
        state
            .os_reports
            .iter()
            .filter(|report| {
                matches!(
                    report.outcome,
                    crate::domain::MountOutcome::Mounted
                        | crate::domain::MountOutcome::Disabled
                        | crate::domain::MountOutcome::Degraded(_)
                )
            })
            .map(|report| report.namespace.clone())
            .collect(),
    );
    // Includes integrity refusals, keyed by the component name, so enable can
    // rescan a package that never entered the catalogue.
    state.package_dirs = Arc::new(package_dirs);
    state.module_status = Arc::new(
        host.as_ref()
            .map(|h| h.supervisors.statuses())
            .unwrap_or_default(),
    );
    state.supervisors = host.as_ref().ok().map(|h| h.supervisors.clone());
    // K1-6a-G §2.5 (A5b), §2.5.1: told once the modules run, so a grant
    // that expired, or an end not told, while the daemon was down reaches
    // the run that is up now.
    tokio::spawn(crate::grant_expiry::run(
        state.store.clone(),
        module_tool_cards.notice.clone(),
        {
            let cards = module_tool_cards.clone();
            StdArc::new(move |module: &str| cards.on_grants_protocol(module))
        },
        cancel.child_token(),
    ));
    state.module_tool_cards = Some(module_tool_cards);
    state.module_grants = module_grants;
    state.shutdown_report = Arc::new(report);
    // A3-2b (design §4/§5): the real attach registry — same "start empty,
    // replace once real data is ready" pattern as `os_reports` above, and for
    // the same ordering reason (this runs before `state` is cloned into the
    // router). Hydrated from `attached.json` BEFORE the listener takes its
    // first connection (`AttachRegistry::hydrate`'s own doc: a handshake
    // racing an unloaded name would otherwise see a spurious `auth_failed`).
    state.attach_registry = Arc::new(agent24_attach::attach_registry::AttachRegistry::new(
        agent24_attach::attach_registry::AttachDeps {
            grants: Arc::new(crate::attach_grant::DaemonAttachGrantBuilder {
                scheduler: state.scheduler.clone(),
                models: Some(model_deps),
                approval_broker: state.module_approval_broker.clone(),
                events: state.events.clone(),
            }),
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
        if let Some(path) = agent24_attach::attached::config_path()
            && let Err(e) = state.attach_registry.hydrate(&path)
        {
            tracing::error!("could not hydrate the attach registry from {path:?}: {e}");
        }
        // Filled before the listener starts (see the cell's own comment
        // above): the `stopping` task's `revoke_all` can now find it. See
        // [`fill_attach_registry_and_recheck`]'s own doc comment for why the
        // fill is immediately followed by a re-check of `cancel`.
        fill_attach_registry_and_recheck(&attach_registry_cell, &state.attach_registry, &cancel);
        if let Some(path) = agent24_attach::attached::socket_path() {
            tokio::spawn(agent24_attach::attach_listener::run(
                Arc::clone(&state.attach_registry),
                path,
                shutdown.child_token(),
            ));
        } else {
            tracing::error!("attach listener not started: HOME is not set, no socket path to bind");
        }
    }
}

pub(super) fn fill_module_surfaces(state: &mut AppState, catalogue: &[crate::domain::Installed]) {
    if state.auth_mode == AuthMode::Capabilities {
        // Package modules register an ANY-method proxy for their namespace.
        // Bind that registered surface and the exact manifest, not enabled flags.
        // In-process modules have no route inventory API and remain undelegated.
        state.module_surfaces = Arc::new(
            catalogue
                .iter()
                .filter_map(|entry| {
                    let crate::domain::Build::Package(package) = &entry.build else {
                        return None;
                    };
                    let report = state.os_reports.iter().find(|r| {
                        r.name == package.manifest.name()
                            && matches!(r.outcome, crate::domain::MountOutcome::Mounted)
                    })?;
                    Some(crate::capabilities::delegation::ModuleSurface {
                        name: report.name.clone(),
                        version: report.version.clone(),
                        namespace: report.namespace.clone(),
                        routes_sha256: crate::capabilities::first_party::body_digest(
                            format!(
                                "ANY {}\nANY {}/{{*rest}}\n{}",
                                report.namespace, report.namespace, package.digest
                            )
                            .as_bytes(),
                        ),
                    })
                })
                .collect(),
        );
    }
}

#[allow(clippy::needless_borrow)]
pub(super) async fn advertise_readiness(
    cancel: &CancellationToken,
    stopping: tokio::task::JoinHandle<()>,
    usage_recorder: std::sync::Arc<crate::usage_recorder::UsageRecorder>,
    usage_writer: tokio::task::JoinHandle<()>,
    shutdown: &Shutdown,
    ephemeral: bool,
    mode: StartupMode,
    state_dir: &std::path::Path,
    auth_mode: AuthMode,
    local: std::net::SocketAddr,
    token: &String,
    daemon_generation: &String,
    enroll_authority: Option<Arc<crate::capabilities::first_party::FirstPartyAuthority>>,
    ready_omit_token: bool,
    startup: &std::time::Instant,
    product_host_token: Option<String>,
    host_ready: &mut Option<crate::host_bootstrap::ReadyWriter>,
) -> Result<Option<StillUp>, std::io::Error> {
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
        return Ok(None);
    }

    // SPEC-002 §4 ready line: legacy parsers scan stdout; capability mode first
    // proves stdin/stdout are parent-owned pipes and binds lifetime to stdin.
    // stdout carries nothing else in either mode (logs go to stderr), except
    // a `failed` line in place of the ready line when the start fails first.
    // Discovery state file BEFORE the ready line: a CLI that has seen the
    // ready line may immediately rely on attached-mode discovery.
    let daemon_pid = std::process::id();
    // CORE-2e (D5): lock -> migration -> bind the enrolment endpoint -> only
    // then publish daemon.json and the ready line. A bind or (service mode)
    // publish failure ends this start; nothing half-ready is advertised.
    let mut enroll = None;
    let mut publish_error = None;
    if !ephemeral {
        let bound = if mode == StartupMode::Legacy {
            Ok(None)
        } else {
            crate::enroll_endpoint::bind(&state_dir).map(Some)
        };
        match bound {
            Ok(endpoint) => enroll = endpoint,
            Err(err) => {
                publish_error = Some(std::io::Error::other(format!(
                    "cannot bind the enrolment endpoint: {err}"
                )))
            }
        }
    }
    if publish_error.is_none() && !ephemeral {
        let discovery = match auth_mode {
            AuthMode::LegacySingleToken => agent24_protocol::state_file::DaemonState::new_legacy(
                local.port(),
                token.clone(),
                daemon_pid,
                env!("CARGO_PKG_VERSION"),
                daemon_generation.clone(),
            ),
            AuthMode::Capabilities => agent24_protocol::state_file::DaemonState::new_capabilities(
                local.port(),
                daemon_pid,
                env!("CARGO_PKG_VERSION"),
                daemon_generation.clone(),
            ),
        }
        .map_err(std::io::Error::other)?;
        let discovery = match &enroll {
            Some(endpoint) => {
                discovery.with_enroll_endpoint(endpoint.path().to_string_lossy().into_owned())
            }
            None => discovery,
        };
        if let Err(err) = agent24_protocol::state_file::write(&discovery) {
            if mode == StartupMode::Service {
                // Revises facts §8.3 for the service path only: a service
                // nobody can discover is a failed start.
                publish_error = Some(err);
            } else {
                tracing::warn!("could not write daemon state file: {err}");
            }
        }
    }
    if let Some(err) = publish_error {
        tracing::error!("daemon start failed before ready: {err}");
        shutdown.request();
        tracing::info!("stopping modules after a failed start");
        let _ = stopping.await;
        crate::usage_recorder::stop_usage_writer(
            usage_recorder,
            usage_writer,
            shutdown.deadlines().modules,
        )
        .await;
        if !ephemeral {
            agent24_protocol::state_file::remove_if_owner(daemon_pid);
        }
        return Err(err);
    }
    if let Some(endpoint) = enroll.as_mut()
        && let Some(authority) = enroll_authority
    {
        endpoint.serve(
            cancel.clone(),
            daemon_generation.clone(),
            local.port(),
            authority,
        );
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
        return Ok(None);
    }
    tracing::debug!(elapsed_ms = startup.elapsed().as_millis(), "startup: ready");
    let ready = match auth_mode {
        AuthMode::LegacySingleToken => {
            legacy_ready_line(local.port(), &token, ready_omit_token, &daemon_generation)
        }
        AuthMode::Capabilities if mode == StartupMode::Service => serde_json::json!({
            "type": "ready",
            "port": local.port(),
            "auth_mode": "capabilities",
            "generation": daemon_generation,
            "version": env!("CARGO_PKG_VERSION"),
            "enroll_endpoint": enroll.as_ref().map(|e| e.path().to_string_lossy().into_owned()),
        }),
        AuthMode::Capabilities => {
            let product_host_token = product_host_token.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "capability startup lost its host credential",
                )
            })?;
            serde_json::json!({
                "type": "ready",
                "port": local.port(),
                // This secret crosses only the inherited ready pipe to the
                // spawning trusted host. It is never written to daemon.json.
                "product_host_token": product_host_token,
                "auth_mode": "capabilities",
                "generation": daemon_generation,
                "version": env!("CARGO_PKG_VERSION"),
            })
        }
    };
    match auth_mode {
        AuthMode::LegacySingleToken => println!("{ready}"),
        AuthMode::Capabilities if mode == StartupMode::Service => println!("{ready}"),
        AuthMode::Capabilities => {
            let host_ready = host_ready.as_mut().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "capability startup lost its private ready pipe",
                )
            })?;
            host_ready.send(&ready).await?;
        }
    }
    // SPEC-002 §4: nothing follows the ready line, so `main` writes no
    // `failed` line after this. Only review guards this store: no test can
    // fail a start between the line and here (review of #1332).
    READY_SENT.store(true, std::sync::atomic::Ordering::SeqCst);
    // In capability mode `ready` is the only temporary owner of the raw host
    // bearer after the private write completes. Do not retain it for daemon
    // lifetime; the authority store keeps only its digest.
    drop(ready);
    Ok(Some(StillUp {
        stopping,
        usage_recorder,
        usage_writer,
        enroll,
        daemon_pid,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::sync::Arc;

    use agent24_store::NewInboundEvent;
    use agent24_tools::HttpFetchTool;

    use super::*;

    fn event(id: &str, actor: &str, run_state: &str) -> NewInboundEvent {
        NewInboundEvent {
            id: id.to_owned(),
            source: "hyphae".to_owned(),
            category: "direct".to_owned(),
            recipient_identity: None,
            thread_key: None,
            actor: Some(actor.to_owned()),
            human: true,
            addressed: "direct".to_owned(),
            at: "2026-11-11T00:00:00Z".to_owned(),
            received_at: "2026-11-11T00:00:01Z".to_owned(),
            text: Some("t".to_owned()),
            meta: None,
            disposition: "explicit".to_owned(),
            disposition_reason: None,
            policy_version: Some("p".to_owned()),
            run_state: Some(run_state.to_owned()),
        }
    }

    async fn insert_run(store: &Store, id: &str, status: &str) {
        sqlx::query(
            "INSERT INTO runs (id, status, input, usage, created_at) \
             VALUES (?, ?, \
             '{\"prompt\":\"x\",\"workspace_id\":null,\"knowledge_binding_id\":null}', \
             '{\"prompt_tokens\":0,\"completion_tokens\":0,\"total_tokens\":0,\"cost_usd\":0.0}', \
             ?)",
        )
        .bind(id)
        .bind(status)
        .bind(agent24_core::util::now_iso8601())
        .execute(agent24_store::test_hooks::pool(store))
        .await
        .unwrap();
    }

    async fn run_state(store: &Store, id: &str) -> String {
        store
            .get_inbound_event(id)
            .await
            .unwrap()
            .unwrap()
            .run_state
            .unwrap()
    }

    /// Restart through `recover_durable_runs`, which is the production call
    /// after the orphan sweeps. Mutation: drop `reconcile_inbound_runs` from
    /// that function and every row below stays at its seeded `run_state`.
    #[tokio::test]
    async fn startup_reconcile_converges_the_four_restart_shapes() {
        let store = Store::open_memory().await.unwrap();
        for (id, actor, state) in [
            ("hyphae:live", "a-live", "starting"),
            ("hyphae:none", "a-none", "starting"),
            ("hyphae:ended", "a-ended", "started"),
            ("hyphae:gone", "a-gone", "started"),
        ] {
            assert_eq!(
                store
                    .insert_inbound_event(&event(id, actor, state))
                    .await
                    .unwrap(),
                agent24_store::InboundInsert::Inserted
            );
        }
        insert_run(&store, "run_live", "awaiting_approval").await;
        insert_run(&store, "run_ended", "completed").await;
        insert_run(&store, "run_gone", "completed").await;
        store
            .insert_inbound_run("run_live", "hyphae:live")
            .await
            .unwrap();
        store
            .insert_inbound_run("run_ended", "hyphae:ended")
            .await
            .unwrap();
        store
            .insert_inbound_run("run_gone", "hyphae:gone")
            .await
            .unwrap();
        let now = agent24_core::util::now_iso8601();
        sqlx::query(
            "INSERT INTO run_messages (run_id, seq, role, content, tool_calls, created_at) \
             VALUES ('run_live', 0, 'assistant', NULL, ?, ?)",
        )
        .bind(r#"[{"id":"call_1","name":"http_fetch","arguments":"{}"}]"#)
        .bind(&now)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO approvals (id, run_id, tool_call_id, kind, summary, payload, \
             available_decisions, status, expires_at, created_at) \
             VALUES ('apr_live', 'run_live', 'call_1', 'exec', 'fetch', '{}', \
             '[\"approve\",\"deny\"]', 'pending', '2099-01-01T00:00:00Z', ?)",
        )
        .bind(&now)
        .execute(agent24_store::test_hooks::pool(&store))
        .await
        .unwrap();
        sqlx::query("DELETE FROM runs WHERE id = 'run_gone'")
            .execute(agent24_store::test_hooks::pool(&store))
            .await
            .unwrap();

        let tools = agent24_tools::ToolRegistry::new().with(Arc::new(HttpFetchTool::new(false)));
        let state = AppState::new(AppDeps {
            token: "testtoken".to_owned(),
            router: Arc::new(ModelRouter::with_defaults(vec![])),
            tools,
            store: store.clone(),
            shutdown: Shutdown::new(CancellationToken::new()),
            guardian: None,
            memory: None,
            workspace_service: None,
            mcp_servers: Vec::new(),
            risk_overrides: StdArc::new(agent24_policy::overrides::RiskOverrideStore::from_rows(
                Vec::new(),
            )),
            packages_root: Arc::new(tempfile::tempdir().expect("tempdir").path().to_path_buf()),
        });
        let cancel = CancellationToken::new();
        let runtime = crate::module_tool_runtime::AgentModuleToolRuntime::new(
            std::collections::HashSet::new(),
        );
        let registry = agent24_domain::tool::ModuleToolRegistry::new();
        recover_durable_runs(&state, true, &cancel, &runtime, &registry)
            .await
            .unwrap()
            .expect("os config");
        cancel.cancel();

        assert_eq!(run_state(&store, "hyphae:live").await, "started");
        assert_eq!(
            store.get_run("run_live").await.unwrap().unwrap().status,
            agent24_protocol::RunStatus::AwaitingApproval
        );
        assert_eq!(run_state(&store, "hyphae:none").await, "interrupted");
        assert_eq!(run_state(&store, "hyphae:ended").await, "done");
        assert_eq!(run_state(&store, "hyphae:gone").await, "done");
        assert!(
            store
                .inbound_run_id_for_event("hyphae:gone")
                .await
                .unwrap()
                .is_none()
        );
    }
}
