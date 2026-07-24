//! `/api/v1/tool-overrides` — the user's local risk overrides (H2).
//!
//! These three handlers are the ONLY writers of the `risk_overrides` table, and
//! they are reachable only from a user-driven surface (desktop / CLI / TUI). No
//! module, persona, or MCP install path may call them: a package that could
//! ship its own exemption would make the conservative `external` default for
//! third-party code worthless. That is a rule about what may call this module,
//! not something the module can enforce about itself — so it is stated here,
//! in the migration, and on `agent24_tools::RiskOverrides`.

use agent24_core::util::now_iso8601;
use agent24_protocol::RiskClass;
use axum::Json;
use axum::extract::{Path, State};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::server::{AppState, error_response};

#[derive(Debug, Deserialize)]
pub struct OverrideBody {
    pub risk_class: RiskClass,
    /// Which surface the user acted from. Audit metadata, never authorization.
    #[serde(default)]
    pub source: Option<String>,
}

fn row_json(pattern: &str, class: RiskClass, source: &str, created_at: &str) -> serde_json::Value {
    json!({
        "pattern": pattern,
        "risk_class": class,
        "source": source,
        "created_at": created_at,
    })
}

/// `GET /api/v1/tool-overrides`
pub async fn list_overrides(State(state): State<AppState>) -> Response {
    match state.store.list_risk_overrides().await {
        Ok(rows) => Json(json!({
            "overrides": rows
                .iter()
                .map(|r| row_json(&r.pattern, r.risk_class, &r.source, &r.created_at))
                .collect::<Vec<_>>()
        }))
        .into_response(),
        Err(err) => {
            tracing::error!("listing risk overrides: {err}");
            error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not read risk overrides",
            )
        }
    }
}

/// `PUT /api/v1/tool-overrides/{pattern}` — upsert one rule.
///
/// The rule is recorded whatever it says; whether it actually takes effect is
/// the registry's call (`effective_risk`), which refuses to relax a builtin.
/// Keeping those separate means the user's stated intent is never silently
/// discarded — a rule that currently has no effect is still listed, and starts
/// working if the tool it names is later provided by an MCP server instead.
pub async fn put_override(
    State(state): State<AppState>,
    Path(pattern): Path<String>,
    Json(body): Json<OverrideBody>,
) -> Response {
    let pattern = pattern.trim().to_owned();
    if pattern.is_empty() {
        return error_response(
            axum::http::StatusCode::BAD_REQUEST,
            "invalid_request",
            "pattern must not be empty",
        );
    }
    let source = body.source.unwrap_or_else(|| "api".to_owned());
    let now = now_iso8601();
    match state
        .store
        .set_risk_override(&pattern, body.risk_class, &source, &now)
        .await
    {
        Ok(row) => {
            state
                .store
                .audit_override("set", &row.pattern, Some(row.risk_class))
                .await;
            state.reload_overrides().await;
            (
                axum::http::StatusCode::OK,
                Json(row_json(
                    &row.pattern,
                    row.risk_class,
                    &row.source,
                    &row.created_at,
                )),
            )
                .into_response()
        }
        Err(err) => {
            tracing::error!("setting risk override {pattern}: {err}");
            error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not store the risk override",
            )
        }
    }
}

/// `DELETE /api/v1/tool-overrides/{pattern}` — remove one rule.
pub async fn delete_override(
    State(state): State<AppState>,
    Path(pattern): Path<String>,
) -> Response {
    match state.store.delete_risk_override(pattern.trim()).await {
        Ok(true) => {
            state
                .store
                .audit_override("delete", pattern.trim(), None)
                .await;
            state.reload_overrides().await;
            axum::http::StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            "no such risk override",
        ),
        Err(err) => {
            tracing::error!("deleting risk override {pattern}: {err}");
            error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not delete the risk override",
            )
        }
    }
}

/// Every change to the override set goes in the hash-chained audit log: it
/// changes what may run without asking, which is exactly the kind of decision
/// that must still be reconstructible months later.
trait AuditOverride {
    async fn audit_override(&self, action: &str, pattern: &str, class: Option<RiskClass>);
}

impl AuditOverride for agent24_store::Store {
    async fn audit_override(&self, action: &str, pattern: &str, class: Option<RiskClass>) {
        let detail = json!({ "pattern": pattern, "risk_class": class });
        if let Err(err) = self
            .append_audit(
                &now_iso8601(),
                "user",
                &format!("risk_override.{action}"),
                &detail,
            )
            .await
        {
            tracing::error!("auditing risk override {action} {pattern}: {err}");
        }
    }
}

// ── H4: target-scoped standing grants ────────────────────────────────────────

/// `GET /api/v1/standing-grants` — every persistent pre-authorisation.
///
/// A grant is a permission that outlives the process and fires while nobody is
/// watching, so it must be trivially reviewable. Listing them is not a
/// convenience endpoint; it is the other half of being allowed to mint them.
pub async fn list_standing_grants(State(state): State<AppState>) -> Response {
    match state.store.list_standing_grants().await {
        Ok(grants) => Json(json!({
            "standing_grants": grants
                .iter()
                .map(|g| json!({
                    "id": g.id,
                    "scope_kind": g.scope_kind,
                    "scope_id": g.scope_id,
                    "tool": g.tool,
                    "target": g.target,
                    "created_at": g.created_at,
                }))
                .collect::<Vec<_>>()
        }))
        .into_response(),
        Err(err) => {
            tracing::error!("listing standing grants: {err}");
            error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not read standing grants",
            )
        }
    }
}

/// `DELETE /api/v1/standing-grants/{id}` — revoke one.
///
/// Takes effect on the next call: the broker reads the table per dispatch
/// rather than caching, precisely so a revocation is immediate. A permission
/// you cannot withdraw until restart is not really revocable.
pub async fn delete_standing_grant(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.store.delete_standing_grant(id.trim()).await {
        Ok(true) => {
            if let Err(err) = state
                .store
                .append_audit(
                    &now_iso8601(),
                    "user",
                    "standing_grant.revoked",
                    &json!({ "grant_id": id }),
                )
                .await
            {
                tracing::error!("auditing standing grant revocation {id}: {err}");
            }
            axum::http::StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => error_response(
            axum::http::StatusCode::NOT_FOUND,
            "not_found",
            "no such standing grant",
        ),
        Err(err) => {
            tracing::error!("deleting standing grant {id}: {err}");
            error_response(
                axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                "internal",
                "could not delete the standing grant",
            )
        }
    }
}
