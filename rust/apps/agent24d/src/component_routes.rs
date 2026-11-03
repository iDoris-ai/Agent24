//! `GET /api/v1/components` and `POST /api/v1/components/{id}/report` (ADR-038 D5, C3a).
//!
//! One status model for every component. The daemon reads the sources it owns (iDoris, the
//! Hyphae daemon, attached modules, the Documents package, Knowledge); Open Design is the
//! desktop's, so the desktop reports it and the daemon only remembers the report for a while.
//! The old status routes stay as they are and are not called from here; for iDoris, Hyphae,
//! attached modules and Documents this route reads the same objects (`IdorisHost`, the comm
//! daemon supervisor, `AttachRegistry`, `os_reports` + `module_status`) at request time.

use std::collections::HashMap;
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

/// The Hyphae daemon, from the supervisor's process state (is the child alive, did it exit;
/// ADR-038 §7.4: no `health` before C2, so never
/// `degraded`). A process that died and will be restarted, gave up, or was refused its
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
    if let Some(rejection) = state.comm_slot.as_ref().and_then(|slot| slot.rejection()) {
        return item.with(|i| {
            i.state = ComponentState::Failed;
            i.reason = Some(rejection.as_str().to_owned());
        });
    }
    let Some(daemon) = state.comm_slot.as_ref().and_then(|slot| slot.current()) else {
        return item;
    };
    let process = daemon.status();
    let (mapped, reason) = from_hyphae(process.state);
    item.with(|i| {
        i.state = mapped;
        i.version = Some(lock.version.clone());
        i.reason = reason.or(process.reason);
        if let Some(Value::Object(d)) = i.detail.as_mut() {
            d.insert("generation".into(), json!(process.generation));
            d.insert(
                "consecutive_failures".into(),
                json!(process.consecutive_failures),
            );
        }
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
    }
}

/// Fill provides and version from the catalog. A missing or broken catalog
/// leaves the state alone and the provides empty: listing components must not
/// panic because the JSON failed to parse.
fn apply_documents_catalog(
    item: &mut Item,
    catalog: Result<documents_catalog::DocumentsCatalog, documents_catalog::CatalogError>,
) {
    match catalog {
        Ok(catalog) => {
            item.version = Some(catalog.version.clone());
            item.provides = catalog.into_provides();
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
    apply_documents_catalog(&mut item, catalog);
    item
}

/// The Documents package. State comes from the mount report and its supervisor.
/// Not mounted at all is `absent`. Provides and version come from the D9 catalog.
fn documents_item(state: &AppState) -> Item {
    let report = state.os_reports.iter().find(|r| r.name == "documents");
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

/// Knowledge, remote subset: no binding active is `stopped` ("not configured"). There is no
/// probe yet (C5), so an active binding is `ready` with `probed: false` rather than `degraded`.
fn knowledge_item(state: &AppState) -> Item {
    let active = state.knowledge_host.any_active();
    Item::new(
        "knowledge",
        Form::Remote,
        if active {
            ComponentState::Ready
        } else {
            ComponentState::Stopped
        },
    )
    .with(|i| {
        if active {
            i.detail = Some(json!({"probed": false}));
        } else {
            i.reason = Some("not_configured".into());
        }
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
    items.push(knowledge_item(state));
    items.push(open_design_item(state, now));
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
