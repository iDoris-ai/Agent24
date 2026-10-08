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

async fn authorization_views(state: &AppState) -> Result<Vec<AuthorizationView>, Response> {
    let records = state
        .store
        .list_module_consents()
        .await
        .map_err(|err| storage_error("list", err))?;
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
            .map_err(|err| storage_error("resolve_status", err))?;
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
        Err(response) => response,
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
        Err(response) => response,
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
