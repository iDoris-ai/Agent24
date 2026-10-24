//! REST routes for module tool authorization management.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::module_tool_runtime::RevocationStop;
use crate::server::{AppState, error_response};

fn storage_error(action: &'static str, err: impl std::fmt::Display) -> Response {
    tracing::error!(action, error = %err, "module authorization storage operation failed");
    error_response(
        StatusCode::SERVICE_UNAVAILABLE,
        "storage_unavailable",
        "Module authorizations are temporarily unavailable",
    )
}

#[derive(Serialize)]
struct AuthorizationView {
    module: String,
    tool: String,
    module_version: String,
    scope_summary: ScopeSummary,
    host_risk: &'static str,
    source: &'static str,
    decided_at: String,
    expires_at: String,
    status: &'static str,
    /// For a revoked one: whether the module said it stopped what the
    /// grant started (K1-6a-G §2.5); none recorded reads `unconfirmed`.
    #[serde(skip_serializing_if = "Option::is_none")]
    stop: Option<String>,
}

#[derive(Serialize)]
struct ScopeSummary {
    readable: Option<String>,
    writable: Option<String>,
    external: Option<String>,
}

fn risk(value: agent24_store::HostRiskLevel) -> &'static str {
    match value {
        agent24_store::HostRiskLevel::Low => "low",
        agent24_store::HostRiskLevel::Medium => "medium",
        agent24_store::HostRiskLevel::High => "high",
    }
}

fn source(value: agent24_store::ConsentSource) -> &'static str {
    match value {
        agent24_store::ConsentSource::FirstParty => "first_party",
        agent24_store::ConsentSource::ManualInstall => "manual_install",
    }
}

fn view(record: agent24_store::ModuleConsentRecord, status: &'static str) -> AuthorizationView {
    AuthorizationView {
        module: record.module,
        tool: record.op,
        module_version: record.module_version,
        scope_summary: ScopeSummary {
            readable: record.readable,
            writable: record.writable,
            external: record.external,
        },
        host_risk: risk(record.risk),
        source: source(record.source),
        decided_at: record.decided_at,
        expires_at: record.expires_at,
        status,
        stop: None,
    }
}

async fn authorization_views(state: &AppState) -> Result<Vec<AuthorizationView>, Box<Response>> {
    let records = state
        .store
        .list_module_consents()
        .await
        .map_err(|err| Box::new(storage_error("list", err)))?;
    let now = Utc::now().to_rfc3339();
    let mut views = Vec::with_capacity(records.len());
    for record in records {
        let lookup = state
            .store
            .lookup_module_consent(
                &record.module,
                &record.op,
                &record.module_version,
                &record.scope_fingerprint,
                &now,
            )
            .await
            .map_err(|err| Box::new(storage_error("resolve_status", err)))?;
        let status = match lookup {
            agent24_store::ConsentLookup::Granted(_) => "granted",
            agent24_store::ConsentLookup::Denied(_) => "denied",
            agent24_store::ConsentLookup::Revoked(_) => "revoked",
            agent24_store::ConsentLookup::Expired(_) => "expired",
            agent24_store::ConsentLookup::Stale(_) => "stale",
            agent24_store::ConsentLookup::NotGranted => "not_granted",
        };
        let granted = record.decision == agent24_store::ConsentDecision::Granted;
        let mut view = view(record, status);
        if status == "expired" && granted {
            let stop = state
                .store
                .expiry_stop_state(&view.module, &view.tool)
                .await
                .map_err(|err| Box::new(storage_error("resolve_stop", err)))?;
            view.stop = Some(stop.unwrap_or_else(|| "unconfirmed".to_owned()));
        }
        if status == "revoked" {
            let stop = state
                .store
                .revocation_stop_state(&view.module, &view.tool)
                .await
                .map_err(|err| Box::new(storage_error("resolve_stop", err)))?;
            view.stop = Some(stop.unwrap_or_else(|| "unconfirmed".to_owned()));
        }
        views.push(view);
    }
    Ok(views)
}

#[derive(Serialize)]
struct AuthorizationList {
    authorizations: Vec<AuthorizationView>,
}

pub async fn list(State(state): State<AppState>) -> Response {
    match authorization_views(&state).await {
        Ok(authorizations) => Json(AuthorizationList { authorizations }).into_response(),
        Err(response) => *response,
    }
}

pub async fn export(State(state): State<AppState>) -> Response {
    match authorization_views(&state).await {
        Ok(authorizations) => {
            let mut response = Json(AuthorizationList { authorizations }).into_response();
            response.headers_mut().insert(
                header::CONTENT_DISPOSITION,
                HeaderValue::from_static("attachment; filename=module-authorizations.json"),
            );
            response
        }
        Err(response) => *response,
    }
}

/// Tells a module a revocation ended its grants of an operation (`"*"`:
/// every tool) at an instant, and gives what it answered (K1-6a-G §2.5).
pub type RevocationNotice = std::sync::Arc<
    dyn Fn(String, String, String) -> futures::future::BoxFuture<'static, RevocationStop>
        + Send
        + Sync,
>;

/// The revocation is written first and stands whatever the module answers;
/// the answer says whether the work the grants started has stopped, and
/// only a recorded `confirmed` claims it has.
async fn revoke(state: AppState, module: String, tool: Option<String>) -> Response {
    let now = Utc::now().to_rfc3339();
    let generation = match state
        .store
        .revoke_module_consent_tracked(&module, tool.as_deref(), &now)
        .await
    {
        Ok(generation) => generation,
        Err(err) => return storage_error("revoke", err),
    };
    let operation = tool.clone().unwrap_or_else(|| "*".to_owned());
    let stop = match &state.module_tool_cards {
        Some(cards) => (cards.notice)(module.clone(), operation, now.clone()).await,
        None => RevocationStop::NotRunning,
    };
    let recorded = state
        .store
        .record_revocation_stop(&module, tool.as_deref(), generation, stop.word())
        .await;
    let stop = match recorded {
        Ok(true) => stop,
        // Not recorded, or a later revocation took its place: unconfirmed,
        // as the list reads it.
        Ok(false) => RevocationStop::Unconfirmed,
        Err(err) => {
            tracing::error!(module, error = %err, "a revocation's stop could not be recorded");
            RevocationStop::Unconfirmed
        }
    };
    if stop == RevocationStop::Unconfirmed {
        let detail = serde_json::json!({ "module": module, "tool": tool });
        if let Err(err) = state
            .store
            .append_audit(&now, "host", "module_consent.stop_unconfirmed", &detail)
            .await
        {
            tracing::error!(module, error = %err, "an unconfirmed stop could not be audited");
        }
    }
    Json(serde_json::json!({
        "module": module,
        "tool": tool,
        "status": "revoked",
        "stop": stop.word(),
    }))
    .into_response()
}

pub async fn revoke_module(Path(module): Path<String>, State(state): State<AppState>) -> Response {
    revoke(state, module, None).await
}

pub async fn revoke_tool(
    Path((module, tool)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    revoke(state, module, Some(tool)).await
}

/// What a tool's card is made from (K1-6a-G §2.1): the summary the startup
/// assembly gates the tool's dispatch with — the same value, so a grant's
/// fingerprint and the gate's agree byte for byte — and the resource kinds
/// a grant of it may name.
#[derive(Clone)]
pub struct ToolCardSource {
    pub summary: agent24_store::ToolPermissionSummary,
    pub resource_kinds: Vec<String>,
    /// The module speaks the grants protocol (§2.6); no card otherwise.
    pub takes_grants: bool,
}

impl ToolCardSource {
    /// What the tool's dispatch is gated with: this card's fingerprint and
    /// version, the running package's identity read per lookup (§2.2).
    pub fn gate_request(&self) -> agent24_policy::consent_gate::ConsentGateRequest {
        agent24_policy::consent_gate::ConsentGateRequest {
            module: self.summary.module.clone(),
            op: self.summary.op.clone(),
            module_version: self.summary.module_version.clone(),
            current_scope_fingerprint: self.summary.scope_fingerprint(),
            risk: agent24_protocol::RiskClass::External,
            org_restricted: false,
            source_restricted: false,
            // The host has no verified reversible-draft semantics for this
            // path yet, so the L-APPR-5 draft exception never applies here.
            reversible_draft_declared: false,
            draft_verification: None,
            current_artifact: None,
        }
    }
}

/// Every module tool's card source, and the identity of each module's
/// running package, read per request: what the gate compares a grant with.
#[derive(Clone)]
pub struct ModuleToolCards {
    pub tools: std::sync::Arc<std::collections::HashMap<(String, String), ToolCardSource>>,
    pub artifact: agent24_policy::consent_gate::ArtifactSource,
    pub notice: RevocationNotice,
}

#[derive(Serialize)]
struct CardView {
    module: String,
    tool: String,
    module_version: String,
    scope_summary: ScopeSummary,
    host_risk: &'static str,
    source: &'static str,
    reversible_draft: bool,
    resource_kinds: Vec<String>,
    fingerprint: String,
    artifact: String,
}

/// `GET …/tools/{tool}/card`: what a grant of `module.tool` would be a
/// grant of. No card for a module on an older protocol (§2.6), or whose
/// running package has no identity (not running, not yet observed by this
/// start, or an ephemeral daemon): nothing could be granted against it
/// (§2.1, fail closed).
pub async fn card(
    Path((module, tool)): Path<(String, String)>,
    State(state): State<AppState>,
) -> Response {
    let Some(cards) = &state.module_tool_cards else {
        return no_such_tool(&module, &tool);
    };
    let Some(found) = cards.tools.get(&(module.clone(), tool.clone())) else {
        return no_such_tool(&module, &tool);
    };
    if !found.takes_grants {
        return protocol_too_old(&module);
    }
    let Some(artifact) = (cards.artifact)(&module) else {
        return no_package_identity(&module);
    };
    let summary = &found.summary;
    Json(CardView {
        module,
        tool,
        module_version: summary.module_version.clone(),
        scope_summary: ScopeSummary {
            readable: summary.readable.clone(),
            writable: summary.writable.clone(),
            external: summary.external.clone(),
        },
        host_risk: risk(summary.risk),
        source: source(summary.source),
        reversible_draft: summary.reversible_draft,
        resource_kinds: found.resource_kinds.clone(),
        fingerprint: summary.scope_fingerprint(),
        artifact,
    })
    .into_response()
}

fn no_such_tool(module: &str, tool: &str) -> Response {
    error_response(
        StatusCode::NOT_FOUND,
        "not_found",
        &format!("no module tool {module:?}.{tool:?}"),
    )
}

fn protocol_too_old(module: &str) -> Response {
    error_response(
        StatusCode::CONFLICT,
        "protocol_too_old",
        &format!(
            "{module:?} does not require kernel protocol {}, so it cannot be told \
             its grants were revoked",
            agent24_domain::RESOURCE_KINDS_PROTOCOL
        ),
    )
}

fn no_package_identity(module: &str) -> Response {
    error_response(
        StatusCode::CONFLICT,
        "no_package_identity",
        &format!(
            "{module:?} has no package identity this daemon can grant against \
             (not running, not yet observed since it started, or an ephemeral daemon)"
        ),
    )
}

/// The longest bound a host may set on a grant (K1-6a-G §2.1).
pub const MAX_GRANT_DAYS_CAP: u32 = 90;

/// How this daemon takes module tool decisions, when it takes them at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GrantSettings {
    /// A decision expires no later than this many days after it is written.
    pub max_days: u32,
    /// `os.json`, read under `os_control` for whether the module is enabled.
    pub os_config: std::path::PathBuf,
}

/// The host's switch (K1-6a-G §2.1, default off): `A24_MODULE_TOOL_GRANTS`
/// unset or `0` keeps decisions closed, `1` opens them; an ephemeral daemon,
/// whose store does not outlive it, never takes them (F6). The bound is
/// checked whether or not the switch is on, so a host never starts with
/// one it believes is in force but is not.
pub fn grant_settings(
    enabled: Option<&str>,
    max_days: Option<&str>,
    ephemeral: bool,
    os_config: Option<std::path::PathBuf>,
) -> Result<Option<GrantSettings>, String> {
    let max_days = match max_days {
        None => 30,
        Some(raw) => match raw.parse::<u32>() {
            Ok(days) if (1..=MAX_GRANT_DAYS_CAP).contains(&days) => days,
            _ => {
                return Err(format!(
                    "A24_MODULE_GRANT_MAX_DAYS={raw:?} must be a number of days from 1 to \
                     {MAX_GRANT_DAYS_CAP}"
                ));
            }
        },
    };
    let open = match enabled {
        None | Some("0") => false,
        Some("1") => true,
        Some(raw) => return Err(format!("A24_MODULE_TOOL_GRANTS={raw:?} must be 0 or 1")),
    };
    Ok(os_config
        .filter(|_| open && !ephemeral)
        .map(|os_config| GrantSettings {
            max_days,
            os_config,
        }))
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum Decision {
    Granted,
    Denied,
}

/// What the user decided on a card: the card's own values, repeated.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecisionRequest {
    decision: Decision,
    fingerprint: String,
    module_version: String,
    artifact: String,
    #[serde(default)]
    resources: Vec<String>,
    expires_at: Option<String>,
}

/// Why `request` cannot be written for `kinds`, whatever the module is now
/// running; otherwise its expiry, normalised. A decision lasts no longer
/// than `max_days` from `now`, and never forever.
fn checked_expiry(
    request: &DecisionRequest,
    kinds: &[String],
    max_days: u32,
    now: chrono::DateTime<Utc>,
) -> Result<String, String> {
    let raw = request
        .expires_at
        .as_deref()
        .ok_or("expires_at is required")?;
    let expires_at = chrono::DateTime::parse_from_rfc3339(raw)
        .map_err(|_| format!("expires_at {raw:?} is not an RFC 3339 instant"))?
        .with_timezone(&Utc);
    if expires_at <= now {
        return Err(format!("expires_at {raw:?} has already passed"));
    }
    if expires_at > now + chrono::Duration::days(i64::from(max_days)) {
        return Err(format!(
            "expires_at {raw:?} is past this host's {max_days}-day bound"
        ));
    }
    if matches!(request.decision, Decision::Denied) && !request.resources.is_empty() {
        return Err("a denial names no resources".to_owned());
    }
    if request.resources.len() > agent24_store::MAX_GRANT_RESOURCES {
        return Err(format!(
            "at most {} resources",
            agent24_store::MAX_GRANT_RESOURCES
        ));
    }
    for element in &request.resources {
        let declared = element
            .split_once(':')
            .is_some_and(|(kind, _)| kinds.iter().any(|k| k == kind));
        if !agent24_store::is_resource_element(element) || !declared {
            return Err(format!(
                "{element:?} is not a resource of a kind this tool declares ({kinds:?})"
            ));
        }
    }
    Ok(expires_at.to_rfc3339())
}

/// `POST …/tools/{tool}`: write the user's decision on a card (K1-6a-G
/// §2.1). Closed unless the host opened it (403). The card's fingerprint,
/// version and package identity must still be what the tool is checked
/// against, read at the write (409); the module must be enabled and
/// running (F7, 409). The check and the write hold `os_control`, so no
/// enable, disable or stop lands between them.
pub async fn decide(
    Path((module, tool)): Path<(String, String)>,
    State(state): State<AppState>,
    body: Result<Json<DecisionRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(settings) = &state.module_grants else {
        return error_response(
            StatusCode::FORBIDDEN,
            "grants_closed",
            "this daemon takes no module tool decisions (A24_MODULE_TOOL_GRANTS is not 1, \
             or the daemon is ephemeral)",
        );
    };
    let Some(cards) = &state.module_tool_cards else {
        return no_such_tool(&module, &tool);
    };
    let Some(found) = cards.tools.get(&(module.clone(), tool.clone())) else {
        return no_such_tool(&module, &tool);
    };
    if !found.takes_grants {
        return protocol_too_old(&module);
    }
    let request = match body {
        Ok(Json(request)) => request,
        Err(why) => {
            return error_response(why.status(), "invalid_request", &why.body_text());
        }
    };
    let now = Utc::now();
    let expires_at = match checked_expiry(&request, &found.resource_kinds, settings.max_days, now) {
        Ok(expires_at) => expires_at,
        Err(why) => {
            return error_response(StatusCode::UNPROCESSABLE_ENTITY, "invalid_decision", &why);
        }
    };
    let _control = state.os_control.lock().await;
    match crate::os_config::OsConfig::load(&settings.os_config) {
        Ok(config) if config.is_enabled(&module) => {}
        Ok(_) => {
            return error_response(
                StatusCode::CONFLICT,
                "module_disabled",
                &format!("{module:?} is disabled; enable it before deciding on its tools"),
            );
        }
        Err(why) => {
            tracing::error!(module, error = %why, "os.json unreadable for a module tool decision");
            return error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "whether the module is enabled could not be read",
            );
        }
    }
    let Some(artifact) = (cards.artifact)(&module) else {
        return no_package_identity(&module);
    };
    let summary = &found.summary;
    if request.fingerprint != summary.scope_fingerprint()
        || request.module_version != summary.module_version
        || request.artifact != artifact
    {
        return error_response(
            StatusCode::CONFLICT,
            "card_changed",
            &format!("{module:?}.{tool:?} is no longer what the card showed; fetch it again"),
        );
    }
    let decided_at = now.to_rfc3339();
    let written = match request.decision {
        Decision::Granted => {
            let scope = agent24_store::GrantScope {
                resources: request.resources,
                artifact: Some(artifact),
            };
            state
                .store
                .grant_module_consent_scoped(summary, &scope, &decided_at, &expires_at)
                .await
                .map(|record| view(record, "granted"))
        }
        Decision::Denied => state
            .store
            .deny_module_consent(summary, &decided_at, &expires_at)
            .await
            .map(|record| view(record, "denied")),
    };
    match written {
        Ok(view) => Json(view).into_response(),
        Err(err) => storage_error("decide", err),
    }
}
