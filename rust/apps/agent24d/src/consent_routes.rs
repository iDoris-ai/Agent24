//! REST routes for module tool authorization management.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;

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
        views.push(view(record, status));
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

async fn revoke(state: AppState, module: String, tool: Option<String>) -> Response {
    let now = Utc::now().to_rfc3339();
    match state
        .store
        .revoke_module_consent(&module, tool.as_deref(), &now)
        .await
    {
        Ok(()) => Json(serde_json::json!({ "module": module, "tool": tool, "status": "revoked" }))
            .into_response(),
        Err(err) => storage_error("revoke", err),
    }
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
        return error_response(
            StatusCode::CONFLICT,
            "protocol_too_old",
            &format!(
                "{module:?} does not require kernel protocol {}, so it cannot be told \
                 its grants were revoked",
                agent24_domain::RESOURCE_KINDS_PROTOCOL
            ),
        );
    }
    let Some(artifact) = (cards.artifact)(&module) else {
        return error_response(
            StatusCode::CONFLICT,
            "no_package_identity",
            &format!(
                "{module:?} has no package identity this daemon can grant against \
                 (not running, not yet observed since it started, or an ephemeral daemon)"
            ),
        );
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
