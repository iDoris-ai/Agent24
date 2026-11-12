//! Health, shutdown, creative-capability mint, and revoke.
//!
//! Named by `build_router_with_modules` in `server.rs`. The bodies
//! moved here unchanged.

use std::time::Duration;

use agent24_protocol::Health;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};

use super::{AppState, error_response};

pub(super) async fn health(State(state): State<AppState>) -> Json<Health> {
    let store = match state.store.latest_applied_migration().await {
        Ok(version) => version,
        Err(err) => {
            tracing::warn!("health: run database migration watermark: {err}");
            0
        }
    };
    let memory = match &state.memory_kv {
        Some(kv) => match kv.latest_applied_migration().await {
            Ok(version) => version,
            Err(err) => {
                tracing::warn!("health: memory database migration watermark: {err}");
                0
            }
        },
        None => 0,
    };
    Json(Health {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        backend: "rust".to_owned(),
        protocol_version: agent24_protocol::PROTOCOL_VERSION,
        migrations: agent24_protocol::HealthMigrations { store, memory },
    })
}

/// Authenticated shutdown (bearer token proves the caller owns this daemon —
/// unlike a pid from a possibly-stale state file, this can never kill an
/// unrelated reused-pid process). Used by `agent24 daemon stop`.
pub(super) async fn shutdown_handler(State(state): State<AppState>) -> Response {
    state.shutdown.request();
    tracing::info!("shutdown requested via /api/v1/shutdown");
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

/// `GET /api/v1/shutdown` (SHUT-1c): the shutdown budgets in effect, what
/// was warned about them, and the daemon before this one.
pub(super) async fn shutdown_report(
    State(state): State<AppState>,
) -> Json<agent24_protocol::ShutdownReport> {
    Json((*state.shutdown_report).clone())
}

#[derive(serde::Deserialize)]
pub(super) struct MintCreativeBody {
    workspace_id: String,
    #[serde(alias = "attachment_id")]
    creative_attachment_id: String,
    #[serde(alias = "principal_id")]
    creative_principal_id: String,
    #[serde(alias = "sidecar_instance_id")]
    sidecar_generation: String,
    #[serde(default = "default_creative_ttl")]
    ttl_seconds: u64,
    #[serde(default)]
    allowed_operations: Option<Vec<String>>,
}

const fn default_creative_ttl() -> u64 {
    300
}

fn valid_scope_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn operation_name(operation: crate::capabilities::Operation) -> &'static str {
    operation.action()
}

pub(super) async fn mint_creative_capability(
    State(state): State<AppState>,
    authorization: Option<Extension<crate::capabilities::Authorization>>,
    Json(body): Json<MintCreativeBody>,
) -> Response {
    let Some(store) = state.capabilities.as_ref() else {
        return error_response(
            StatusCode::CONFLICT,
            "capability_mode_required",
            "Creative capabilities require capability auth mode",
        );
    };
    let Some(Extension(authorization)) = authorization else {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Host authority required",
        );
    };
    if body.ttl_seconds == 0
        || body.ttl_seconds > crate::capabilities::MAX_CREATIVE_TTL_SECONDS
        || !valid_scope_id(&body.workspace_id)
        || !valid_scope_id(&body.creative_attachment_id)
        || !valid_scope_id(&body.creative_principal_id)
        || !valid_scope_id(&body.sidecar_generation)
    {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_capability_scope",
            "Invalid capability scope or TTL",
        );
    }

    let mut request = crate::capabilities::CreativeMintRequest::new(
        body.workspace_id,
        body.creative_attachment_id,
        body.creative_principal_id,
        body.sidecar_generation,
        Duration::from_secs(body.ttl_seconds),
        crate::capabilities::unix_now(),
    );
    if let Some(actions) = body.allowed_operations {
        let allowlist = crate::capabilities::Operation::creative_allowlist();
        let mut operations = Vec::with_capacity(actions.len());
        for action in actions {
            let Some(operation) = crate::capabilities::Operation::from_action(&action) else {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_capability_operation",
                    "Unknown capability operation",
                );
            };
            if !allowlist.contains(&operation) {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    "invalid_capability_operation",
                    "Operation is not available to Creative runtimes",
                );
            }
            operations.push(operation);
        }
        request = request.with_allowed_operations(operations);
    }

    let minted = match store.mint_creative_authorized(&authorization, request) {
        Ok(minted) => minted,
        Err(_) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Host authority required",
            );
        }
    };
    let (token, capability_id, claims) = minted.into_bearer_parts();
    let allowed_operations = claims
        .allowed_operations
        .iter()
        .copied()
        .map(operation_name)
        .collect::<Vec<_>>();
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "capability_id": capability_id,
            "token": token,
            "audience": "creative_runtime",
            "workspace_id": claims.workspace_id,
            "creative_attachment_id": claims.attachment_id,
            "creative_principal_id": claims.principal_id,
            "daemon_generation": claims.daemon_generation,
            "host_generation": claims.host_generation,
            "sidecar_generation": claims.sidecar_generation,
            "created_at": claims.created_at,
            "expires_at": claims.expires_at,
            "allowed_operations": allowed_operations,
        })),
    )
        .into_response()
}

pub(super) async fn mint_bridge_capability(
    State(state): State<AppState>,
    authorization: Option<Extension<crate::capabilities::Authorization>>,
) -> Response {
    // Dark unless `A24_DEV_TOOL=1`, including legacy mode. 409 remains the
    // answer when the gate is on and this process has no capability store.
    // Checked before issue so a closed gate writes no `capability.token.issued`.
    if !state.dev_wechat_events {
        return error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route");
    }
    let Some(store) = state.capabilities.clone() else {
        return error_response(
            StatusCode::CONFLICT,
            "capability_mode_required",
            "Bridge capabilities require capability auth mode",
        );
    };
    let Some(Extension(authorization)) = authorization else {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden",
            "Shell authority required",
        );
    };
    let db = state.store.clone();
    let minted = match crate::capabilities::bridge::issue_wechat_bridge(
        &store,
        &authorization,
        crate::capabilities::unix_now(),
        |event| {
            let db = db.clone();
            async move {
                let ts = crate::capabilities::bridge::audit_now()?;
                db.append_capability_token_audit(&ts, &event)
                    .await
                    .map(|_| ())
                    .map_err(|_| ())
            }
        },
    )
    .await
    {
        Ok(minted) => minted,
        Err(crate::capabilities::bridge::BridgeIssueError::AuditFailed) => {
            return error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "audit_unavailable",
                "Capability audit could not be written",
            );
        }
        Err(crate::capabilities::bridge::BridgeIssueError::Rejected) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Shell authority required",
            );
        }
        Err(crate::capabilities::bridge::BridgeIssueError::CallerInvalid) => {
            return error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing or invalid bearer token",
            );
        }
    };
    let (token, capability_id, claims) = minted.into_bearer_parts();
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "capability_id": capability_id,
            "token": token,
            "audience": "component_runtime",
            "component_id": claims.component_id,
            "daemon_generation": claims.daemon_generation,
            "created_at": claims.created_at,
            "expires_at": claims.expires_at,
            "ttl_seconds": crate::capabilities::bridge::BRIDGE_TTL_SECONDS,
            "allowed_operations": claims.allowed_operations.iter().copied().map(operation_name).collect::<Vec<_>>(),
        })),
    )
        .into_response()
}

/// Revoke one capability by id.
///
/// A live dev-bridge token is revoked in memory first. The
/// `capability.token.revoked` row is written only when `revoke_by_id`
/// returns true (the record changed from live to revoked). If that write
/// fails the response is 503 `audit_unavailable` and the bearer stays
/// unusable. A later call sees the record already revoked, answers 200, and
/// does not append another row. It does not backfill the missing one.
///
/// 503 is only returned for a bridge id whose revoke transition just
/// happened and whose audit write failed. That tells the caller this id was
/// a live dev-bridge token. The body contains no bearer. Unknown ids,
/// already-revoked ids, and tokens outside the dev-bridge profile answer
/// 200 `revoked` and do not write that row, including while the audit store
/// is down, so this is not an existence oracle for arbitrary ids.
///
/// The shell that holds `capability.revoke` can revoke the bridge token
/// after desktop confirmation. Revoking that shell does not revoke the
/// bridge token. The token still dies at its 8-hour TTL, or when this
/// daemon process is gone and the generation no longer matches.
pub(super) async fn revoke_capability(
    State(state): State<AppState>,
    Path(capability_id): Path<String>,
) -> Response {
    let Some(store) = state.capabilities.as_ref() else {
        return error_response(
            StatusCode::CONFLICT,
            "capability_mode_required",
            "Capability revocation requires capability auth mode",
        );
    };
    if !valid_scope_id(&capability_id) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_capability_id",
            "Invalid capability id",
        );
    }
    // A dev-bridge token is revoked in memory before the audit row, and the
    // row is written only for that transition. A failed write leaves the
    // bearer unusable. The next call is 200 and does not write again.
    // Other audiences keep the previous idempotent revoke, with no audit.
    if store
        .claims_by_id(&capability_id)
        .is_some_and(|claims| claims.audience == crate::capabilities::Audience::ComponentRuntime)
    {
        let db = state.store.clone();
        match crate::capabilities::bridge::revoke_wechat_bridge(store, &capability_id, |event| {
            let db = db.clone();
            async move {
                let ts = crate::capabilities::bridge::audit_now()?;
                db.append_capability_token_audit(&ts, &event)
                    .await
                    .map(|_| ())
                    .map_err(|_| ())
            }
        })
        .await
        {
            Ok(()) => {}
            Err(crate::capabilities::bridge::BridgeRevokeError::AuditFailed) => {
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "audit_unavailable",
                    "Capability audit could not be written",
                );
            }
            // A component token outside the dev-bridge profile cannot carry
            // the closed audit row. It is still revoked in memory.
            Err(crate::capabilities::bridge::BridgeRevokeError::NotComponent) => {
                let _ = store.revoke_by_id(&capability_id);
            }
        }
    } else {
        let _ = store.revoke_by_id(&capability_id);
    }
    Json(serde_json::json!({
        "capability_id": capability_id,
        "state": "revoked",
    }))
    .into_response()
}
