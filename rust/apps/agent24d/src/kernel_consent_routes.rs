//! `GET|POST|DELETE /api/v1/os/{name}/kernel-consent`.
//!
//! The renderer may propose a checked subset. The record is written only
//! after the native presence dialog lists that subset and the user agrees,
//! or after a first-party shell has already elevated on the same summary.
//! A shell request that has not elevated never reaches [`grant`].

use agent24_domain::http::error_response;
use axum::Json;
use axum::body::Bytes;
use axum::extract::{Extension, Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::capabilities::presence::{PresenceOutcome, PresenceRequest, RequesterIdentity};
use crate::capabilities::{Audience, Authorization, Operation, unix_now};
use crate::component_routes::{OsKernelDeclaration, os_kernel_declaration};
use crate::kernel_consent::{self, KernelConsentClass};
use crate::server::AppState;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantBody {
    fingerprint: String,
    capabilities: Vec<String>,
}

struct Ready {
    caps: Vec<String>,
    fingerprint: String,
    version: String,
    team: Option<String>,
}

enum Prepared {
    Refuse(Response),
    Ready(Ready),
}

/// What the first-party shell middleware does before any dialog.
pub enum ShellPrecheck {
    Refuse(Response),
    Ask(String),
}

pub fn module_from_path(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/api/v1/os/")?;
    let name = rest.strip_suffix("/kernel-consent")?;
    if name.is_empty() || name.contains('/') {
        None
    } else {
        Some(name)
    }
}

/// The sentence the native dialog shows, one capability at a time.
pub fn consent_summary(name: &str, caps: &[String]) -> String {
    let lines: Vec<&str> = caps.iter().map(|cap| kernel_consent::phrase(cap)).collect();
    format!("同意 {name} 的内核能力：{}", lines.join("；"))
}

pub fn shell_summary(state: &AppState, path: &str, bytes: &[u8]) -> ShellPrecheck {
    let Some(name) = module_from_path(path) else {
        return ShellPrecheck::Refuse(error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "OS module not found",
        ));
    };
    match prepare(state, name, bytes) {
        Prepared::Refuse(response) => ShellPrecheck::Refuse(response),
        Prepared::Ready(ready) => ShellPrecheck::Ask(consent_summary(name, &ready.caps)),
    }
}

fn prepare(state: &AppState, name: &str, bytes: &[u8]) -> Prepared {
    let Some(declared) = os_kernel_declaration(state, name) else {
        return Prepared::Refuse(error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            "OS module not found",
        ));
    };
    if declared.exempt {
        return Prepared::Refuse(error_response(
            StatusCode::CONFLICT,
            "consent_exempt",
            "这个模块随桌面自带，不用同意卡",
        ));
    }
    let body: GrantBody = match serde_json::from_slice(bytes) {
        Ok(body) => body,
        Err(_) => {
            return Prepared::Refuse(error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                "kernel consent body must be {fingerprint, capabilities}",
            ));
        }
    };
    if body.capabilities.is_empty() {
        return Prepared::Refuse(error_response(
            StatusCode::CONFLICT,
            "consent_empty",
            "一项都没勾，等于拒绝",
        ));
    }
    let fingerprint = kernel_consent::fingerprint(&declared.kernel, &declared.operations);
    if body.fingerprint != fingerprint {
        return Prepared::Refuse(error_response(
            StatusCode::CONFLICT,
            "fingerprint_mismatch",
            "指纹和刚才看到的不一致",
        ));
    }
    let caps = kernel_consent::intersect(
        &kernel_consent::offered(&declared.kernel),
        &body.capabilities,
    );
    if caps.is_empty() {
        return Prepared::Refuse(error_response(
            StatusCode::CONFLICT,
            "consent_empty",
            "一项都没勾，等于拒绝",
        ));
    }
    Prepared::Ready(Ready {
        caps,
        fingerprint,
        version: declared.version,
        team: declared.team,
    })
}

pub async fn show(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some(declared) = os_kernel_declaration(&state, &name) else {
        return error_response(StatusCode::NOT_FOUND, "not_found", "OS module not found");
    };
    match view_json(&state, &name, &declared).await {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}

pub async fn grant(
    State(state): State<AppState>,
    authorization: Option<Extension<Authorization>>,
    Path(name): Path<String>,
    body: Bytes,
) -> Response {
    let ready = match prepare(&state, &name, &body) {
        Prepared::Refuse(response) => return response,
        Prepared::Ready(ready) => ready,
    };
    let audience = authorization.as_ref().map(|auth| auth.0.claims.audience);
    match audience {
        Some(Audience::ProductHost) => {
            let summary = consent_summary(&name, &ready.caps);
            match confirm_desktop(&state, &name, &summary).await {
                PresenceOutcome::Allowed => {}
                PresenceOutcome::Denied => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "presence_denied",
                        "用户拒绝了这次内核能力",
                    );
                }
                PresenceOutcome::Unavailable => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "presence_unavailable",
                        "没有在场连接，没有写入同意",
                    );
                }
                PresenceOutcome::TimedOut => {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "presence_timeout",
                        "在场确认超时，没有写入同意",
                    );
                }
            }
        }
        Some(Audience::FirstPartyShell) => {}
        _ => {
            return error_response(
                StatusCode::FORBIDDEN,
                "consent_host_required",
                "只有桌面或已经在场确认的终端可以写入内核同意",
            );
        }
    }
    if let Err(response) = store_grant(&state, &name, &ready).await {
        return *response;
    }
    let Some(declared) = os_kernel_declaration(&state, &name) else {
        return error_response(StatusCode::NOT_FOUND, "not_found", "OS module not found");
    };
    match view_json(&state, &name, &declared).await {
        Ok(body) => Json(body).into_response(),
        Err(response) => *response,
    }
}

pub async fn revoke(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    if state.store.delete_kernel_consent(&name).await.is_err() {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_unavailable",
            "could not delete the kernel consent",
        );
    }
    let now = Utc::now();
    kernel_consent::track(
        &name,
        std::iter::empty::<String>(),
        now + chrono::Duration::days(365),
    );
    StatusCode::NO_CONTENT.into_response()
}

async fn confirm_desktop(state: &AppState, name: &str, summary: &str) -> PresenceOutcome {
    let Some(provider) = state.presence_provider.clone() else {
        return PresenceOutcome::Unavailable;
    };
    let request = PresenceRequest {
        challenge_id: format!("kc-{}", unix_now()),
        operation: Operation::ModuleAdmin,
        method: "POST".to_owned(),
        path: format!("/api/v1/os/{name}/kernel-consent"),
        summary: summary.to_owned(),
        requester: RequesterIdentity {
            identifier: "desktop".to_owned(),
            pid: std::process::id() as i32,
        },
        expires_at: unix_now().saturating_add(60),
    };
    provider.confirm(request, CancellationToken::new()).await
}

async fn store_grant(state: &AppState, name: &str, ready: &Ready) -> Result<(), Box<Response>> {
    let now = Utc::now();
    let expires = kernel_consent::expires_at(now).map_err(|message| {
        Box::new(error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            &message,
        ))
    })?;
    let row = agent24_store::KernelConsentRow {
        module: name.to_owned(),
        fingerprint: ready.fingerprint.clone(),
        capabilities: ready.caps.clone(),
        team_id: ready.team.clone(),
        version: ready.version.clone(),
        decided_at: now.to_rfc3339(),
        expires_at: expires.to_rfc3339(),
    };
    state.store.upsert_kernel_consent(&row).await.map_err(|_| {
        Box::new(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "store_unavailable",
            "could not store the kernel consent",
        ))
    })?;
    kernel_consent::track(name, ready.caps.iter(), expires);
    Ok(())
}

async fn view_json(
    state: &AppState,
    name: &str,
    declared: &OsKernelDeclaration,
) -> Result<Value, Box<Response>> {
    let fingerprint = kernel_consent::fingerprint(&declared.kernel, &declared.operations);
    let capabilities = kernel_consent::offered(&declared.kernel);
    let (class, consented) = if declared.exempt {
        ("exempt", Value::Null)
    } else {
        let row = state.store.get_kernel_consent(name).await.map_err(|_| {
            Box::new(error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "store_unavailable",
                "could not read the kernel consent",
            ))
        })?;
        let now = Utc::now();
        let held = kernel_consent::hold(&fingerprint, &capabilities, row.as_ref(), now);
        if held.delete {
            let _ = state.store.delete_kernel_consent(name).await;
        }
        let class = match held.class {
            KernelConsentClass::Granted => "granted",
            KernelConsentClass::Exempt => "exempt",
            KernelConsentClass::Missing => "missing",
        };
        let consented = if held.class == KernelConsentClass::Granted {
            json!(row.map(|row| row.capabilities).unwrap_or_default())
        } else {
            Value::Null
        };
        (class, consented)
    };
    let origin = if kernel_consent::BUNDLED_OS_MODULES.contains(&name) {
        "bundled"
    } else {
        "local"
    };
    Ok(json!({
        "module": name,
        "class": class,
        "fingerprint": fingerprint,
        "version": declared.version,
        "team": declared.team,
        "origin": origin,
        "isolated": cfg!(target_os = "macos"),
        "capabilities": capabilities,
        "consented": consented,
    }))
}
