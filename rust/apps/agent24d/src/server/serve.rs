use std::sync::Arc;

use agent24_protocol::state_file::AuthMode;
use tokio_util::sync::CancellationToken;

use super::{StartupMode, build_router_with_modules, guard_host_origin, serve_steps};

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
    if let Some(kv) = &memory_base {
        crate::default_model::apply_saved(&router, kv).await;
    }
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
