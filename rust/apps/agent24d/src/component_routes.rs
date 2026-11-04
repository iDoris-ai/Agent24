//! `GET /api/v1/components` and `POST /api/v1/components/{id}/report` (ADR-038 D5, C3a).
//!
//! One status model for every component. The daemon reads the sources it owns (iDoris, the
//! Hyphae daemon, attached modules, the Documents package, other domain OS modules, Knowledge,
//! configured MCP servers); Open Design is the
//! desktop's, so the desktop reports it and the daemon only remembers the report for a while.
//! The old status routes stay as they are and are not called from here; for iDoris, Hyphae,
//! attached modules and Documents this route reads the same objects (`IdorisHost`, the comm
//! daemon supervisor, `AttachRegistry`, `os_reports` + `module_status`) at request time.

use std::collections::{BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use agent24_domain::http::{error_response, read_body_or_response};
use agent24_os_proto::drain::DrainState;
use agent24_os_proto::supervisor::Status as Supervised;
use agent24d::idoris_host::{Action, State as IdorisState};
use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::capabilities::{Audience, Authorization};
use crate::comm_routes::BuildMark;
use crate::domain::{MountOutcome, MountReport};
use crate::server::AppState;
use agent24_components::{ComponentState, Form};
use agent24_protocol::state_file::AuthMode;

mod documents_catalog;

/// A desktop report is believed for this long. The desktop re-sends on a timer (C3b), so a
/// silent desktop (crashed, quit) reads as "not reporting" rather than as a stale `ready`.
/// There is no connection to watch: the report is a plain HTTP POST.
pub const REPORT_TTL: Duration = Duration::from_secs(90);
/// The ids of the components this daemon lists itself. No attached module may take one:
/// `attached::register` refuses them for new registrations, and [`attach_items`] drops same-named entries
/// from registrations that predate the rule. Pinned by a test.
pub(crate) const BUILTIN_COMPONENT_IDS: &[&str] =
    &["idoris", "hyphae", "documents", "knowledge", "open-design"];
/// Components whose state the desktop owns. Anything else is the daemon's to say.
const DESKTOP_OWNED: &[&str] = &["open-design"];

#[derive(Serialize, Clone)]
pub struct Provide {
    id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<Value>,
    /// One-line description in zh/en/th. Locks that omit it stay as C3a shipped them.
    #[serde(skip_serializing_if = "Option::is_none")]
    summary: Option<Value>,
    /// Tool names this capability exposes (D9). Empty for a lock that lists none.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<String>,
    /// `unavailable` when the component's own report omits a capability this
    /// build expects (ADR-038 D9). Absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    availability: Option<&'static str>,
}

/// The host view of one component.
#[derive(Serialize, Clone)]
pub struct Item {
    id: String,
    version: Option<String>,
    state: ComponentState,
    form: Form,
    provides: Vec<Provide>,
    /// A short machine-readable cause. Host-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    /// Ports, contract expectations, counters. Host-only.
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<Value>,
}

/// What a non-host audience sees in capability mode (see [`sees_everything`]): the fields are
/// only id, version, state and provides, so no paths, pids, ports, messages, team ids or commits.
#[derive(Serialize)]
struct Redacted<'a> {
    id: &'a str,
    version: Option<&'a str>,
    state: ComponentState,
    provides: &'a [Provide],
}

impl Item {
    fn new(id: &str, form: Form, state: ComponentState) -> Self {
        Self {
            id: id.to_owned(),
            version: None,
            state,
            form,
            provides: Vec::new(),
            reason: None,
            detail: None,
        }
    }
    fn with(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }
    /// D9 provides, straight from the component's lock (names only where it has them).
    fn provides_of(lock: &agent24_components::ComponentLock) -> Vec<Provide> {
        lock.provides
            .iter()
            .map(|p| Provide {
                id: p.id.clone(),
                name: p
                    .name
                    .as_ref()
                    .map(|n| json!({"zh": n.zh, "en": n.en, "th": n.th})),
                summary: None,
                tools: p.tools.clone(),
                availability: None,
            })
            .collect()
    }
    fn redacted(&self) -> Redacted<'_> {
        Redacted {
            id: &self.id,
            version: self.version.as_deref(),
            state: self.state,
            provides: &self.provides,
        }
    }
}

/// `{components: [...]}`, full or redacted as [`sees_everything`] decided for this request.
fn render(items: &[Item], full: bool) -> Value {
    let components = if full {
        serde_json::to_value(items)
    } else {
        serde_json::to_value(items.iter().map(Item::redacted).collect::<Vec<_>>())
    };
    json!({ "components": components.unwrap_or(Value::Null) })
}

// ---- mappings from the states that existed before D5 ----------------------------------

/// iDoris's lifecycle state. D5's table does not list it; the same definitions apply. An
/// install in flight is `installing` whatever the stored state says.
fn from_idoris(state: IdorisState, installing: bool) -> (ComponentState, Option<&'static str>) {
    use ComponentState as S;
    if installing {
        return (S::Installing, None);
    }
    match state {
        IdorisState::NotInstalled => (S::Absent, None),
        IdorisState::NotConfigured => (S::Installed, Some("not_configured")),
        IdorisState::Stopped => (S::Stopped, None),
        IdorisState::Starting => (S::Starting, None),
        IdorisState::Running => (S::Ready, None),
        IdorisState::Failed => (S::Failed, None),
        IdorisState::InstanceConflict => (S::Failed, Some("instance_conflict")),
    }
}

/// D5's `DrainState` rows, exactly.
fn from_drain(state: DrainState) -> ComponentState {
    match state {
        DrainState::Starting => ComponentState::Starting,
        DrainState::Running => ComponentState::Ready,
        DrainState::Draining | DrainState::Revoked => ComponentState::Stopping,
    }
}

/// A supervised package's live status. Not in D5's table either; a failed run is `failed`
/// (it will retry or has given up), a requested stop is `stopped`.
fn from_supervised(status: &Supervised) -> (ComponentState, Option<&'static str>) {
    use ComponentState as S;
    match status {
        Supervised::Starting { .. } => (S::Starting, None),
        Supervised::Running => (S::Ready, None),
        Supervised::Stopping => (S::Stopping, None),
        Supervised::Stopped => (S::Stopped, None),
        Supervised::Backoff { .. } => (S::Failed, Some("restarting")),
        Supervised::GaveUp { .. } => (S::Failed, Some("gave_up")),
        Supervised::PackageChanged { .. } => (S::Failed, Some("package_changed")),
        Supervised::StopFailed { .. } => (S::Failed, Some("stop_failed")),
        Supervised::Panicked => (S::Failed, Some("panicked")),
        Supervised::Killed => (S::Failed, Some("killed")),
    }
}

/// The Hyphae daemon, from the supervisor's process state alone (is the child
/// alive, did it exit). This mapping never returns `degraded`: that comes from
/// [`overlay_hyphae`] after N consecutive `hyphae health` failures (ADR-038 D5).
/// A process that died and will be restarted, gave up, or was refused its
/// password is `failed`; one that is not configured or was stopped is `stopped`.
fn from_hyphae(process: &str) -> (ComponentState, Option<String>) {
    use ComponentState as S;
    match process {
        "running" => (S::Ready, None),
        "stopped" => (S::Stopped, None),
        "not_configured" => (S::Stopped, Some("not_configured".to_owned())),
        other => (S::Failed, Some(other.to_owned())),
    }
}

/// Process state first, then the contract verdict and the health book (ADR-038
/// D3/D5). A contract mismatch or a handshake that never produced a report wins
/// over the process string (`gave_up` would otherwise read as `failed`). Health
/// moves `running` to `degraded`, and holds it in `starting` until the first passing sample; a stopped process keeps its process
/// mapping so a stale book cannot flap the component.
fn overlay_hyphae(
    process_state: &str,
    probe: &agent24_comm::ProbeSnapshot,
) -> (ComponentState, Option<String>) {
    use agent24_comm::ContractView;
    match probe.contract {
        ContractView::Mismatch => {
            return (
                ComponentState::Incompatible,
                Some("contract_mismatch".into()),
            );
        }
        ContractView::CheckFailed => {
            return (ComponentState::Failed, Some("contract_check_failed".into()));
        }
        ContractView::Unknown | ContractView::Compatible => {}
    }
    let (mapped, reason) = from_hyphae(process_state);
    if process_state == "running" && probe.health.as_ref().is_some_and(|health| health.degraded) {
        return (ComponentState::Degraded, Some(degraded_reason(probe)));
    }
    // ADR-038 D5: `starting -> ready` is "健康探测首次通过". With a probe armed, a
    // running process that has no passing sample yet is still `starting`; the
    // first N failures stay `starting` too, then the rule above says `degraded`.
    // Without an arm (fixtures, old path) nothing probes, so keep `ready`.
    if process_state == "running"
        && probe.health_armed
        && !probe.health.as_ref().is_some_and(|health| health.confirmed)
    {
        return (ComponentState::Starting, None);
    }
    (mapped, reason)
}

fn degraded_reason(probe: &agent24_comm::ProbeSnapshot) -> String {
    let Some(health) = probe.health.as_ref() else {
        return "health".to_owned();
    };
    if health.failed_probes.is_empty() {
        return "health".to_owned();
    }
    let mut names = health.failed_probes.clone();
    names.sort();
    names.dedup();
    names.join(",")
}

/// Host-only health summary. Probe names and counters only — never a relay URL.
fn health_json(health: &agent24_comm::HealthView) -> Value {
    let checks = health
        .checks
        .iter()
        .map(|(name, check)| {
            (
                name.clone(),
                json!({
                    "ok": check.ok,
                    "consecutive_failures": check.consecutive_failures,
                }),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    json!({
        "consecutive_failures": health.consecutive_failures,
        "degraded": health.degraded,
        "checks": checks,
    })
}

fn apply_hyphae_runtime(
    item: &mut Item,
    process_state: &str,
    process_reason: Option<String>,
    generation: u64,
    process_failures: u32,
    probe: &agent24_comm::ProbeSnapshot,
) {
    let (mapped, reason) = overlay_hyphae(process_state, probe);
    item.state = mapped;
    item.reason = reason.or(process_reason);
    if let Some(Value::Object(detail)) = item.detail.as_mut() {
        detail.insert("generation".into(), json!(generation));
        detail.insert("consecutive_failures".into(), json!(process_failures));
        if process_state == "running"
            && let Some(health) = probe.health.as_ref()
        {
            detail.insert("health".into(), health_json(health));
        }
    }
}

// ---- sources ---------------------------------------------------------------------------

async fn idoris_item(state: &AppState) -> Item {
    let provides = Item::provides_of(&agent24_idoris::IdorisLock::embedded().to_component());
    let item = Item::new("idoris", Form::Resident, ComponentState::Absent);
    let Some(host) = state.idoris_host.as_ref() else {
        return item.with(|i| {
            i.provides = provides;
            if state.idoris_unavailable {
                i.state = ComponentState::Failed;
                i.reason = Some("persistence_failed".into());
            }
        });
    };
    let status = host.status().await;
    let installing = match &status.current_operation {
        Some(id) => host
            .operation(id)
            .await
            .is_ok_and(|op| op.action == Action::Install),
        None => false,
    };
    let (mapped, reason) = from_idoris(status.state, installing);
    item.with(|i| {
        i.provides = provides;
        i.state = mapped;
        i.version = status.version.clone();
        i.reason = reason.map(str::to_owned).or_else(|| {
            status
                .last_error
                .and_then(|c| serde_json::to_value(c).ok()?.as_str().map(str::to_owned))
        });
        i.detail = Some(json!({
            "data_port": status.data_port, "admin_port": status.admin_port,
            "roles": status.roles, "current_operation": status.current_operation,
            "migration_pending": status.migration_pending,
        }));
    })
}

fn hyphae_item(state: &AppState) -> Item {
    // C1b: contract and provides come from the embedded schema-2 lock (ADR-038 §7.4).
    let lock = match crate::hyphae_install::HyphaeLock::embedded() {
        Ok(lock) => lock,
        Err(_) => {
            return Item::new("hyphae", Form::Resident, ComponentState::Failed)
                .with(|i| i.reason = Some("lock_invalid".into()));
        }
    };
    let lock = lock.component();
    let item = Item::new("hyphae", Form::Resident, ComponentState::Absent).with(|i| {
        i.provides = Item::provides_of(lock);
        i.detail = Some(json!({"contract": {
            "name": lock.contract.name, "major": lock.contract.major,
            "min_minor": lock.contract.min_minor,
        }}));
    });
    // A binary the last start (or install rebuild) refused is `failed` with the stable
    // code, ahead of any supervisor left registered from before. Host view only: the
    // redacted view drops `reason`. Not installed at all keeps the plain `absent`.
    // A start that failed without a binary verdict (missing file, I/O) is the same
    // `failed`, with `start_failed`: D5 reserves `failed` for a start that did not
    // come up, and `absent` only for nothing installed. The code carries no path.
    if let Some(slot) = state.comm_slot.as_ref() {
        // One `BuildNote` lock. Two reads can both miss if `note()` lands
        // between them, and this GET would fall through to the supervisor or `absent`.
        if let Some(mark) = slot.build_mark() {
            return item.with(|i| {
                i.state = ComponentState::Failed;
                i.reason = Some(match mark {
                    BuildMark::Rejected(rejection) => rejection.as_str().to_owned(),
                    BuildMark::StartFailed => "start_failed".into(),
                });
            });
        }
    }
    let Some(daemon) = state.comm_slot.as_ref().and_then(|slot| slot.current()) else {
        return item;
    };
    let process = daemon.status();
    let probe = daemon.probe_snapshot();
    item.with(|i| {
        i.version = Some(lock.version.clone());
        apply_hyphae_runtime(
            i,
            process.state,
            process.reason,
            process.generation,
            process.consecutive_failures,
            &probe,
        );
    })
}

/// An attached module: `ready` while a generation is running, `stopped` when not connected,
/// disabled, or after the generation is revoked (D5's attach subset: a dropped connection
/// returns to `stopped`, whereas D5's `DrainState` row would say `stopping` forever).
fn attach_state(
    disabled: bool,
    drain: Option<DrainState>,
) -> (ComponentState, Option<&'static str>) {
    match (disabled, drain) {
        (true, _) => (ComponentState::Stopped, Some("disabled")),
        (false, None | Some(DrainState::Revoked)) => (ComponentState::Stopped, None),
        (false, Some(d)) => (from_drain(d), None),
    }
}

fn attach_items(state: &AppState) -> Vec<Item> {
    attach_items_from(state.attach_registry.snapshot())
}

fn attach_items_from(mut modules: Vec<crate::attach_registry::AttachSnap>) -> Vec<Item> {
    // D9 has no catalog for an attached module: no component lock (D2 does
    // not apply), `kernel_capabilities` are `requires` (ADR-039 §4), and
    // `docs/capabilities/REGISTRY.md` is C7. `provides` stays empty.
    modules.sort_by(|a, b| a.name.cmp(&b.name));
    modules
        .into_iter()
        .filter(|module| !BUILTIN_COMPONENT_IDS.contains(&module.name.as_str()))
        .map(|module| {
            let (mapped, reason) = attach_state(module.disabled, module.drain);
            // A user-disabled module stays `stopped`. The mismatch is kept
            // and shows again once the module is re-enabled.
            let mismatch = module.mismatch.filter(|_| !module.disabled);
            Item::new(&module.name, Form::Attach, mapped).with(|item| {
                if !module.version.is_empty() {
                    item.version = Some(module.version);
                }
                if let Some(mismatch) = mismatch {
                    item.state = ComponentState::Incompatible;
                    item.reason = Some("component_contract_mismatch".to_owned());
                    let id = item.id.clone();
                    item.detail = Some(json!({
                        "id": id,
                        "expected": mismatch.expected,
                        "actual": mismatch.actual,
                    }));
                } else {
                    item.reason = reason.map(str::to_owned);
                }
            })
        })
        .collect()
}

/// Documents runs as a supervised out-of-process domain OS (`domain-os.yml`:
/// `impl_kind: out_of_process_provider`). D5's 常驻型 uses the whole state set,
/// so the form is [`Form::Resident`]. The supervisor's starting, ready,
/// stopping and stopped readings all belong to that set.
fn documents_form() -> Form {
    Form::Resident
}

/// Mount outcome plus the supervisor's live status. The state is D5's; the
/// catalog does not choose it.
fn documents_mapped(
    outcome: &MountOutcome,
    live: Option<&Supervised>,
) -> (ComponentState, Option<&'static str>, Option<String>) {
    match (outcome, live) {
        (MountOutcome::Mounted, Some(status)) => {
            let (state, reason) = from_supervised(status);
            (state, reason, None)
        }
        (MountOutcome::Mounted, None) => (ComponentState::Ready, None, None),
        (MountOutcome::Disabled, _) => (ComponentState::Stopped, Some("disabled"), None),
        (MountOutcome::Degraded(why), _) => {
            (ComponentState::Failed, Some("degraded"), Some(why.clone()))
        }
        (MountOutcome::Refused(why), _) => {
            (ComponentState::Failed, Some("refused"), Some(why.clone()))
        }
        (MountOutcome::RefusedFor(reason, why), _) => (
            ComponentState::Failed,
            Some(reason.code()),
            Some(why.clone()),
        ),
    }
}

/// Fill provides from the catalog and check it against what the mount report
/// says is running (ADR-038 D9: the compiled catalog is the expectation, the
/// report is the actual).
///
/// - `version` is the report's; the catalog's goes to `detail.expected_version`.
///   With no report there is no runtime, and the catalog's version is shown.
///   A different version does not change the state: the package is installed on
///   demand and can be ahead of or behind this daemon, so `detail` gets
///   `actual_version` and `update_available: true` and nothing else.
/// - A capability the manifest declares without a tool the catalog expects is
///   `component_contract_mismatch` and `incompatible`, but only over a live
///   `ready` or `degraded` reading: the process states that follow it
///   (starting, stopping, failed, stopped) say more about what is happening.
/// - A capability the catalog expects and a non-empty manifest list omits is
///   `unavailable`. Extra capabilities in the manifest are ignored.
///
/// A missing or broken catalog leaves the state alone and the provides empty:
/// listing components must not panic because the JSON failed to parse.
fn apply_documents_catalog(
    item: &mut Item,
    catalog: Result<documents_catalog::DocumentsCatalog, documents_catalog::CatalogError>,
    report: Option<&MountReport>,
) {
    match catalog {
        Ok(catalog) => {
            let runtime = report.map(|report| report.version.as_str());
            let actual = report
                .and_then(|report| report.d9.as_ref())
                .map(|d9| d9.provides.as_slice());
            let checked = catalog.check(runtime, actual);
            if runtime.is_none() {
                item.version = Some(catalog.version.clone());
            }
            let mut detail = match item.detail.take() {
                Some(Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            };
            detail.insert("expected_version".to_owned(), json!(catalog.version));
            if let Some((_, running)) = checked.version_drift {
                detail.insert("actual_version".to_owned(), json!(running));
                detail.insert("update_available".to_owned(), json!(true));
            }
            let live = matches!(item.state, ComponentState::Ready | ComponentState::Degraded);
            if let (Some(mismatch), true) = (checked.mismatch, live) {
                item.state = ComponentState::Incompatible;
                item.reason = Some("component_contract_mismatch".to_owned());
                detail.insert("id".to_owned(), json!(item.id));
                detail.extend(mismatch.into_detail());
            }
            item.detail = Some(Value::Object(detail));
            item.provides = catalog.into_provides();
            for provide in &mut item.provides {
                if checked.unavailable.contains(&provide.id) {
                    provide.availability = Some("unavailable");
                }
            }
        }
        Err(err) => {
            tracing::warn!(%err, "documents component catalog is unusable; provides left empty");
            if item.reason.is_none() {
                item.reason = Some("catalog_invalid".into());
            }
        }
    }
}

fn documents_from(
    report: Option<&MountReport>,
    live: Option<&Supervised>,
    catalog: Result<documents_catalog::DocumentsCatalog, documents_catalog::CatalogError>,
) -> Item {
    let mut item = Item::new("documents", documents_form(), ComponentState::Absent);
    if let Some(report) = report {
        let (mapped, reason, detail) = documents_mapped(&report.outcome, live);
        item.state = mapped;
        item.version = Some(report.version.clone());
        item.reason = reason.map(str::to_owned);
        item.detail = detail.map(|why| json!({"message": why}));
    }
    apply_documents_catalog(&mut item, catalog, report);
    item
}

/// The capability ids in the compiled-in Documents D9 catalog (`component.json`).
/// Empty when the catalog is unusable, which makes every declared id reserved.
pub(crate) fn documents_provide_ids() -> Vec<String> {
    documents_catalog::DocumentsCatalog::embedded()
        .map(|catalog| catalog.into_provides().into_iter().map(|p| p.id).collect())
        .unwrap_or_default()
}

/// The Documents package. State comes from the mount report and its supervisor.
/// Not mounted at all is `absent`. Provides and version come from the D9 catalog.
fn documents_item(state: &AppState) -> Item {
    // A package that only borrowed the name (refused as `reserved_component_name`)
    // is not Documents; it is listed on its own row by `os_module_items`.
    let report = documents_report(state);
    let live = report.and_then(|_| {
        state
            .module_status
            .get("documents")
            .map(|rx| rx.borrow().clone())
    });
    documents_from(
        report,
        live.as_ref(),
        documents_catalog::DocumentsCatalog::embedded(),
    )
}

/// Domain OS and in-process modules other than the builtins. Documents stays
/// on [`documents_item`]. The state map is the same one Documents uses.
fn os_mapped(
    outcome: &MountOutcome,
    live: Option<Supervised>,
) -> (ComponentState, Option<&'static str>, Option<String>) {
    match (outcome, live) {
        (MountOutcome::Mounted, Some(status)) => {
            let (state, reason) = from_supervised(&status);
            (state, reason, None)
        }
        (MountOutcome::Mounted, None) => (ComponentState::Ready, None, None),
        (MountOutcome::Disabled, _) => (ComponentState::Stopped, Some("disabled"), None),
        (MountOutcome::Degraded(why), _) => {
            (ComponentState::Failed, Some("degraded"), Some(why.clone()))
        }
        (MountOutcome::Refused(why), _) => {
            (ComponentState::Failed, Some("refused"), Some(why.clone()))
        }
        (MountOutcome::RefusedFor(reason, why), _) => (
            ComponentState::Failed,
            Some(reason.code()),
            Some(why.clone()),
        ),
    }
}

/// Refused because a user package took a builtin component's name.
fn squats_builtin_name(outcome: &MountOutcome) -> bool {
    matches!(
        outcome,
        MountOutcome::RefusedFor(crate::domain::RefusalReason::ReservedComponentName, _)
    )
}

fn is_refused(outcome: &MountOutcome) -> bool {
    matches!(
        outcome,
        MountOutcome::Refused(_) | MountOutcome::RefusedFor(..)
    )
}

/// The report [`documents_item`] describes: the first one named `documents`
/// that is not a name squatter.
fn documents_report(state: &AppState) -> Option<&MountReport> {
    state
        .os_reports
        .iter()
        .find(|r| r.name == "documents" && !squats_builtin_name(&r.outcome))
}

fn os_module_items(state: &AppState) -> Vec<Item> {
    let documents = documents_report(state).map(std::ptr::from_ref);
    let mut reports: Vec<_> = state
        .os_reports
        .iter()
        // A refused package keeps its row even when it carries a builtin name
        // (a squatter, or a second directory with the same name), so a refusal
        // is never invisible; `dedupe_component_ids` makes its id unique. The
        // builtin's own report is not listed twice.
        .filter(|r| {
            !BUILTIN_COMPONENT_IDS.contains(&r.name.as_str())
                || (is_refused(&r.outcome) && documents != Some(std::ptr::from_ref(*r)))
        })
        .collect();
    reports.sort_by(|a, b| a.name.cmp(&b.name));
    reports
        .into_iter()
        .map(|report| {
            let live = state
                .module_status
                .get(&report.name)
                .map(|rx| rx.borrow().clone());
            let (mapped, reason, message) = os_mapped(&report.outcome, live);
            let d9 = report.d9.as_ref();
            let provides = d9
                .map(|d| {
                    d.provides
                        .iter()
                        .map(|p| Provide {
                            id: p.id.clone(),
                            name: None,
                            summary: None,
                            tools: p.tools.clone(),
                            availability: None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let binding = d9.map(|d| {
                if d.in_process {
                    "in_process"
                } else {
                    "package"
                }
            });
            Item::new(&report.name, Form::Resident, mapped).with(|i| {
                i.version = Some(report.version.clone());
                i.provides = provides;
                i.reason = reason.map(str::to_owned);
                let mut detail = json!({
                    "requires": {
                        "kernel": d9.map(|d| d.requires_kernel.clone()).unwrap_or_default(),
                        "operations": [],
                    },
                });
                if let Some(binding) = binding {
                    detail["binding"] = json!(binding);
                }
                if let Some(message) = message {
                    detail["message"] = json!(message);
                }
                if BUILTIN_COMPONENT_IDS.contains(&report.name.as_str()) {
                    detail["package"] = json!(report.name);
                }
                i.detail = Some(detail);
            })
        })
        .collect()
}

/// Configured MCP servers. One provide per legal id, tools from `tools/list`.
/// D5's MCP subset is what the roster already stored. A server with no
/// capability id contributes tools to the agent and nothing to `provides`.
fn mcp_items(state: &AppState) -> Vec<Item> {
    let mut records = state.mcp_roster.records();
    records.sort_by(|a, b| a.id.cmp(&b.id));
    records
        .into_iter()
        .map(|record| {
            // `provide_ids` is the only gate: tools on the record stay off
            // `provides` when the server has no capability id of its own.
            let capability = record.provide_ids().into_iter().next();
            let crate::mcp::McpRecord {
                server,
                id,
                state,
                reason,
                tools,
                protocol_version,
                ..
            } = record;
            let provides = capability
                .map(|cap| Provide {
                    id: cap,
                    name: None,
                    summary: None,
                    tools,
                    availability: None,
                })
                .into_iter()
                .collect();
            Item::new(&id, Form::Mcp, state).with(|i| {
                i.provides = provides;
                i.reason = reason;
                i.detail = Some(json!({
                    "server": server,
                    "protocol_version": protocol_version,
                    "requires": { "kernel": [], "operations": [] },
                }));
            })
        })
        .collect()
}

/// Component ids in one list are unique. A later row that reuses an id already
/// taken (a domain package named `mcp-files` beside the MCP server `files`)
/// is shown as `mcp-bad-<hash>`. Its reason becomes `duplicate` when it had none.
fn dedupe_component_ids(items: &mut [Item]) {
    let mut seen = BTreeSet::new();
    for item in items.iter_mut() {
        if seen.insert(item.id.clone()) {
            continue;
        }
        let original = item.id.clone();
        item.id = crate::mcp::claim_fallback_id(&original, &mut seen);
        if item.reason.is_none() {
            item.reason = Some("duplicate".to_owned());
        }
    }
}

/// Knowledge, remote subset (ADR-038 D5, C5). No endpoint is `stopped`. A
/// configured binding is whatever the cached `/system/capabilities` probe said.
async fn knowledge_item(
    registry: &crate::knowledge_host::KnowledgeHostRegistry,
    now: Instant,
) -> Item {
    let view = registry.component_status(now).await;
    Item::new("knowledge", Form::Remote, view.state).with(|i| {
        i.version = view.version;
        i.reason = view.reason;
        i.detail = view.detail;
    })
}

fn open_design_item(state: &AppState, now: Instant) -> Item {
    let item = Item::new("open-design", Form::Sidecar, ComponentState::Absent);
    match state.component_reports.current("open-design", now) {
        Some(r) => item.with(|i| {
            i.state = r.state;
            i.version = r.version;
            i.reason = r.reason;
            i.detail = r
                .unexpected
                .map(|(from, to)| json!({"unexpected_transition": {"from": from, "to": to}}));
        }),
        None => item.with(|i| i.reason = Some("not_reported".into())),
    }
}

async fn collect(state: &AppState, now: Instant) -> Vec<Item> {
    let mut items = vec![idoris_item(state).await, hyphae_item(state)];
    items.extend(attach_items(state));
    items.push(documents_item(state));
    items.extend(os_module_items(state));
    items.push(knowledge_item(&state.knowledge_host, now).await);
    items.push(open_design_item(state, now));
    items.extend(mcp_items(state));
    dedupe_component_ids(&mut items);
    items
}

// ---- GET -------------------------------------------------------------------------------

/// Host sees everything. In capability mode anyone else, and a request that somehow reached
/// here without an authorization, gets the redacted view. Legacy mode has one shared token
/// that already reads every full status route, so it gets the full view.
fn sees_everything(mode: AuthMode, authorization: Option<&Authorization>) -> bool {
    match (mode, authorization) {
        (_, Some(a)) => a.claims.audience == Audience::ProductHost,
        (AuthMode::LegacySingleToken, None) => true,
        (AuthMode::Capabilities, None) => false,
    }
}

pub async fn list(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
) -> Response {
    let full = sees_everything(state.auth_mode, authorization.as_ref().map(|e| &e.0));
    Json(render(&collect(&state, Instant::now()).await, full)).into_response()
}

// ---- the desktop's report --------------------------------------------------------------

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReportBody {
    state: ComponentState,
    version: Option<String>,
    reason: Option<String>,
}

impl ReportBody {
    fn valid(&self) -> bool {
        let version = self.version.as_deref().is_none_or(|v| {
            (1..=64).contains(&v.len())
                && v.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
        });
        let reason = self
            .reason
            .as_deref()
            .is_none_or(|r| r.len() <= 256 && !r.chars().any(char::is_control));
        version && reason
    }
}

#[derive(Clone)]
pub struct Reported {
    state: ComponentState,
    version: Option<String>,
    reason: Option<String>,
    /// Set when this report did not follow the last live one by a D5 edge. Host-only.
    unexpected: Option<(ComponentState, ComponentState)>,
    at: Instant,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ReportError {
    UnknownComponent,
    /// Not a state this form ever shows (cannot happen for the sidecar form today).
    IllegalState(ComponentState),
}

/// What the desktop last said about the components it owns, in memory only. An entry older than
/// [`REPORT_TTL`] is ignored: reading it gives `None`, and the next report is a fresh baseline.
#[derive(Default)]
pub struct ComponentReports(Mutex<HashMap<String, Reported>>);

impl ComponentReports {
    pub fn current(&self, id: &str, now: Instant) -> Option<Reported> {
        let map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        map.get(id)
            .filter(|r| now.duration_since(r.at) < REPORT_TTL)
            .cloned()
    }

    /// Record a report. The daemon only aggregates (D5): any state legal for the
    /// form is
    /// stored and the TTL refreshed, or a restarted desktop would leave a stale state on
    /// show. A state that does not follow the last live one by a D5 edge (or the same state
    /// again) is still stored, logged, and marked in `detail.unexpected_transition`; with
    /// nothing live to compare to, the report is a baseline. Returns the unexpected edge.
    fn report(
        &self,
        id: &str,
        body: ReportBody,
        now: Instant,
    ) -> Result<Option<(ComponentState, ComponentState)>, ReportError> {
        if !DESKTOP_OWNED.contains(&id) {
            return Err(ReportError::UnknownComponent);
        }
        if !Form::Sidecar.allows(body.state) {
            return Err(ReportError::IllegalState(body.state));
        }
        let mut map = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let prev = map
            .get(id)
            .filter(|r| now.duration_since(r.at) < REPORT_TTL);
        let unexpected = prev
            .filter(|p| p.state != body.state && !Form::Sidecar.can_transition(p.state, body.state))
            .map(|p| (p.state, body.state));
        // A version describes an installation; absent or mid-install has none to inherit.
        let keeps_version = !matches!(
            body.state,
            ComponentState::Absent | ComponentState::Installing
        );
        let version = body.version.or_else(|| {
            prev.and_then(|p| p.version.clone())
                .filter(|_| keeps_version)
        });
        map.insert(
            id.to_owned(),
            Reported {
                state: body.state,
                version,
                reason: body.reason,
                unexpected,
                at: now,
            },
        );
        if let Some((from, to)) = unexpected {
            tracing::warn!(
                component = id,
                ?from,
                ?to,
                "desktop reported an unexpected transition"
            );
        }
        Ok(unexpected)
    }
}

/// Host audience only: [`crate::server`]'s `host_only_route` refuses everyone else (and
/// legacy mode) before this runs.
pub async fn report(
    State(state): State<AppState>,
    Path(id): Path<String>,
    req: Request<Body>,
) -> Response {
    // The id first: an unknown component is 404 whatever the body looks like.
    if !DESKTOP_OWNED.contains(&id.as_str()) {
        return error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "No component reported by the desktop has this id",
        );
    }
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: ReportBody = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid body: {e}"),
            );
        }
    };
    if !body.valid() {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "invalid version or reason",
        );
    }
    match state.component_reports.report(&id, body, Instant::now()) {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(ReportError::UnknownComponent) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "No component reported by the desktop has this id",
        ),
        Err(ReportError::IllegalState(state)) => error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &format!("{state:?} is not a state this component shows").to_lowercase(),
        ),
    }
}

#[cfg(test)]
mod tests;
