//! HTTP server: router, ready line, graceful shutdown.
//! Bearer auth middleware lives in `capabilities/middleware.rs`.

use std::sync::Arc;

use agent24_protocol::state_file::AuthMode;
use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
// Child test modules resolve these through `use super::*`, the same names
// `server.rs` imported before the split.
#[cfg(test)]
use agent24_models::router::ModelRouter;
#[cfg(test)]
use agent24_store::Store;
#[cfg(test)]
use axum::body::Body;
#[cfg(test)]
use axum::http::{Request, header};
#[cfg(test)]
use std::collections::BTreeSet;
#[cfg(test)]
use std::sync::Arc as StdArc;
#[cfg(test)]
use std::time::Duration;
#[cfg(test)]
use tokio_util::sync::CancellationToken;

mod attach;
mod callback;
mod discovery;
mod host_guard;
mod memory;
mod model_catalog;
mod serve;
mod shutdown;
mod startup;
mod state;

pub(crate) use discovery::{module_tools_if_live, register_discovered_module_tools};
pub use host_guard::{READY_SENT, generate_token, guard_host_origin};
pub use serve::serve;
pub use shutdown::Shutdown;
pub use startup::{AlreadyRunning, StartupMode};
pub use state::{AppDeps, AppState};

#[cfg(test)]
use attach::spawn_approval_recovery_scan_every;
use attach::{fill_attach_registry_and_recheck, spawn_approval_recovery_scan};
use callback::process_host;
#[cfg(test)]
use callback::{callback_root, fallback_dir_hash, secure_fallback_dir};
#[cfg(test)]
use discovery::with_discovered;
use discovery::{
    catalogue_from_join, os_signature_policy, package_dirs_for_startup, with_discovered_policy,
};
use host_guard::legacy_ready_line;
use memory::{LOCAL_USER, build_guardian, open_memory_base, session_memory};
#[cfg(test)]
use memory::{guardian_enabled, parse_always_review};
use model_catalog::ModelCatalog;
#[cfg(test)]
use shutdown::STOPPING;
use shutdown::stop_supervisors;
use startup::{mints_product_host, validate_auth_startup};
use state::workspace_recovery_instant_at;

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

#[cfg(test)]
#[path = "../tests/session_memory/mod.rs"]
mod session_memory_tests;

#[cfg(test)]
#[path = "../tests/inbound_admission/positive.rs"]
mod inbound_admission_positive;

#[path = "serve_steps.rs"]
mod serve_steps;

#[cfg(test)]
pub(crate) mod tests;
