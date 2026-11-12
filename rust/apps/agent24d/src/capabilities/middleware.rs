//! Bearer gate layered onto the kernel router by `build_router_with_modules`.

use agent24_protocol::state_file::AuthMode;
use axum::body::Body;
use axum::extract::{MatchedPath, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::server::{AppState, error_response};

/// Bearer-token gate for everything except `GET /api/v1/health`
/// (SPEC-002 §4: health is the only unauthenticated endpoint — method
/// included, so a future POST on the same path never silently bypasses auth).
///
/// # If you are adding a kernel-private header, name it `X-A24-*`
///
/// An out-of-process module is reached through
/// [`agent24_os_proto::proxy`](../../../crates/agent24-os-proto/src/proxy.rs),
/// which strips every `X-A24-*` header in both directions by PREFIX, and strips
/// `Authorization` / `Cookie` from a hand-written list. The prefix rule keeps up
/// on its own; the list does not. A kernel-private header introduced here under
/// any other name reaches modules verbatim, and nothing will report it.
///
/// This note lives beside the auth middleware rather than beside the proxy
/// because the person adding such a header is reading this file
/// (SPEC-ME3-OUT-OF-PROCESS §2.1).
pub(crate) async fn auth(
    State(state): State<AppState>,
    req: Request<Body>,
    next: Next,
) -> Response {
    tracing::debug!(path = %req.uri().path(), "request received");
    if req.method() == Method::GET && req.uri().path() == "/api/v1/health" {
        return next.run(req).await;
    }
    match state.auth_mode {
        AuthMode::LegacySingleToken => legacy_auth(state, req, next).await,
        AuthMode::Capabilities => capability_auth(state, req, next).await,
    }
}

/// Routes that only the desktop's ProductHost may call, in any auth mode.
/// Private-key backup and restore, and the inbound allowlist / accept-mode
/// writes (X11-2f). Export lets the caller choose the password that encrypts
/// the key, and restore plants a key of the caller's choosing as one of the
/// user's identities, so a bearer that merely proves "same user, same
/// machine" (the legacy shared token, any FirstPartyShell or Creative token)
/// must not reach it: it would let any local process produce a backup it can
/// decrypt. The inbound writes are the same kind of host action: a shell
/// token, even one that could elevate, must not change who is admitted.
fn host_only_route(method: &Method, path: &str) -> bool {
    (method == Method::POST
        && (matches!(
            path,
            "/api/v1/comm/identity/export" | "/api/v1/comm/identity/import"
        ) || is_component_report(path)))
        || (method == Method::PUT
            && matches!(path, "/api/v1/inbound/allowlist" | "/api/v1/inbound/policy"))
}

/// `POST /api/v1/skills/{id}/activate` is `skills.activate`, Elevated. The
/// legacy shared token cannot be elevated, so it cannot reach it at all.
fn skill_activate_route(method: &Method, path: &str) -> bool {
    method == Method::POST
        && path
            .strip_prefix("/api/v1/skills/")
            .and_then(|rest| rest.strip_suffix("/activate"))
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// `/api/v1/components/{id}/report`: the desktop's status push (ADR-038 D5).
fn is_component_report(path: &str) -> bool {
    path.strip_prefix("/api/v1/components/")
        .and_then(|rest| rest.strip_suffix("/report"))
        .is_some_and(|id| !id.is_empty() && !id.contains('/'))
}

/// Single shared token. Does not consult the route catalog or assurance levels,
/// except for [`host_only_route`]s, which are refused outright.
async fn legacy_auth(state: AppState, req: Request<Body>, next: Next) -> Response {
    let bearer = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if bearer
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), state.token.as_bytes()))
    {
        if host_only_route(req.method(), req.uri().path())
            || skill_activate_route(req.method(), req.uri().path())
        {
            return error_response(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Operation not allowed in this auth mode",
            );
        }
        next.run(req).await
    } else {
        error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        )
    }
}

/// Method + matched template → operation. Levels are metadata: they are not a
/// deny input (2b). Unknown paths are answered here so a fallback cannot run
/// unauthenticated. HEAD uses the GET row inside [`crate::capabilities::decide`].
/// 404 and 405 come after the bearer is live, so a missing, unknown, or revoked
/// token cannot enumerate routes. Explicit public routes skip that check.
async fn capability_auth(state: AppState, req: Request<Body>, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_owned();
    // X11-2g: the wechat POST is not a product route. GET on the same path
    // would make `decide` answer 405. A live token gets 404; a missing token
    // still gets 401, so this does not enumerate the route.
    if method == Method::POST && path == "/api/v1/inbound/events" {
        if let Some(denied) = capability_token_rejected(&state, &req) {
            return finish_rejection(&state, denied).await;
        }
        // The dev gate opens the route in both auth modes. The handler, not
        // the catalog operation, accepts only a wechat-bridge component token.
        if !state.dev_wechat_events {
            return error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route");
        }
        return admit_inbound_ingest(state, req, next).await;
    }
    // Same darkness as the wechat POST. A live token sees 404 and nothing is
    // issued. 409 stays the legacy-mode answer when the gate is on.
    if method == Method::POST && path == "/api/v1/capabilities/bridge" && !state.dev_wechat_events {
        if let Some(denied) = capability_token_rejected(&state, &req) {
            return finish_rejection(&state, denied).await;
        }
        return error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route");
    }
    let matched = req
        .extensions()
        .get::<MatchedPath>()
        .map(|matched| matched.as_str().to_owned());
    match crate::capabilities::decide(&method, matched.as_deref(), &path, &state.module_namespaces)
    {
        crate::capabilities::RouteDecision::UnknownPath => {
            if let Some(denied) = capability_token_rejected(&state, &req) {
                return finish_rejection(&state, denied).await;
            }
            error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route")
        }
        crate::capabilities::RouteDecision::MethodNotAllowed => {
            if let Some(denied) = capability_token_rejected(&state, &req) {
                return finish_rejection(&state, denied).await;
            }
            next.run(req).await
        }
        crate::capabilities::RouteDecision::Mapped(policy)
            if policy.level == crate::capabilities::AssuranceLevel::Public =>
        {
            next.run(req).await
        }
        crate::capabilities::RouteDecision::Mapped(policy) => {
            authorize(
                &state,
                req,
                next,
                policy.operation,
                policy.template,
                &method,
            )
            .await
        }
        crate::capabilities::RouteDecision::Module { operation, .. } => {
            let template = matched.as_deref().unwrap_or("");
            authorize(&state, req, next, operation, template, &method).await
        }
    }
}

/// Live bearer, no operation check. `post_wechat_event` checks audience,
/// component id, and `InboundIngest` itself.
async fn admit_inbound_ingest(state: AppState, mut req: Request<Body>, next: Next) -> Response {
    let Some(store) = state.capabilities.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "host_authority_unavailable",
            "Capability authority unavailable",
        );
    };
    let Some(bearer) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        );
    };
    match store.authenticate_bearer(bearer, crate::capabilities::unix_now()) {
        Ok(authorization) => {
            req.extensions_mut().insert(authorization);
            next.run(req).await
        }
        Err(err) => {
            finish_rejection(
                &state,
                TokenRejection {
                    response: error_response(
                        StatusCode::UNAUTHORIZED,
                        "unauthorized",
                        "Missing or invalid bearer token",
                    ),
                    denial: component_liveness_denial(store, bearer, err),
                },
            )
            .await
        }
    }
}

struct TokenRejection {
    response: Response,
    /// Known component-runtime token that failed liveness. Written after the
    /// borrow of the request ends, so the middleware future stays `Send`.
    denial: Option<(
        crate::capabilities::Authorization,
        agent24_store::CapabilityDenyReason,
    )>,
}

/// Missing, unparseable, unknown, revoked, expired, or stale bearers are 401.
/// A live token returns `None` so the caller can answer 404 or 405.
/// A known component-runtime token that fails liveness also carries a denial
/// to audit. An unknown bearer does not.
fn capability_token_rejected(state: &AppState, req: &Request<Body>) -> Option<TokenRejection> {
    let Some(store) = state.capabilities.as_ref() else {
        return Some(TokenRejection {
            response: error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "host_authority_unavailable",
                "Capability authority unavailable",
            ),
            denial: None,
        });
    };
    let Some(bearer) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
    else {
        return Some(TokenRejection {
            response: error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing or invalid bearer token",
            ),
            denial: None,
        });
    };
    match store.authenticate_bearer(bearer, crate::capabilities::unix_now()) {
        Ok(_) => None,
        Err(err) => Some(TokenRejection {
            response: error_response(
                StatusCode::UNAUTHORIZED,
                "unauthorized",
                "Missing or invalid bearer token",
            ),
            denial: component_liveness_denial(store, bearer, err),
        }),
    }
}

/// Revoked, expired, or stale component-runtime rows are audited. An unknown
/// bearer is not: `known_component_bearer` returns `None`.
fn component_liveness_denial(
    store: &crate::capabilities::CapabilityStore,
    bearer: &str,
    err: crate::capabilities::CapabilityError,
) -> Option<(
    crate::capabilities::Authorization,
    agent24_store::CapabilityDenyReason,
)> {
    crate::capabilities::bridge::liveness_deny(err).and_then(|reason| {
        store
            .known_component_bearer(bearer)
            .map(|auth| (auth, reason))
    })
}

async fn finish_rejection(state: &AppState, rejection: TokenRejection) -> Response {
    if let Some((authorization, reason)) = rejection.denial
        && let Some(store) = state.capabilities.as_ref()
    {
        crate::capabilities::bridge::write_component_denial(
            store,
            &state.store,
            &authorization,
            reason,
        )
        .await;
    }
    rejection.response
}

async fn authorize(
    state: &AppState,
    mut req: Request<Body>,
    next: Next,
    operation: crate::capabilities::Operation,
    template: &str,
    method: &Method,
) -> Response {
    let Some(store) = state.capabilities.as_ref() else {
        return error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "host_authority_unavailable",
            "Capability authority unavailable",
        );
    };
    let Some(bearer) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    else {
        return error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        );
    };
    match store.validate_bearer(
        bearer,
        operation,
        &crate::capabilities::Resource::global(),
        crate::capabilities::unix_now(),
    ) {
        Ok(authorization) => {
            if host_only_route(method, req.uri().path())
                && authorization.claims.audience != crate::capabilities::Audience::ProductHost
            {
                if authorization.claims.audience == crate::capabilities::Audience::ComponentRuntime
                {
                    crate::capabilities::bridge::write_component_denial(
                        store,
                        &state.store,
                        &authorization,
                        agent24_store::CapabilityDenyReason::Route,
                    )
                    .await;
                }
                return error_response(StatusCode::FORBIDDEN, "forbidden", "Operation not allowed");
            }
            // R5: a Creative token that validates still cannot call a route
            // this mapping newly exposed. Resource scope is not this gate.
            if authorization.claims.audience == crate::capabilities::Audience::CreativeRuntime
                && !crate::capabilities::creative_route_permitted(method, template)
            {
                return error_response(StatusCode::FORBIDDEN, "forbidden", "Operation not allowed");
            }
            // Same shape as the creative gate: operation equality is not
            // enough, and host-only / module routes are not on the list.
            if authorization.claims.audience == crate::capabilities::Audience::ComponentRuntime
                && (host_only_route(method, req.uri().path())
                    || operation == crate::capabilities::Operation::ModuleRoute
                    || !crate::capabilities::component_route_permitted(method, template))
            {
                crate::capabilities::bridge::write_component_denial(
                    store,
                    &state.store,
                    &authorization,
                    agent24_store::CapabilityDenyReason::Route,
                )
                .await;
                return error_response(StatusCode::FORBIDDEN, "forbidden", "Operation not allowed");
            }
            // §10 R1 guards daily first-party tokens. ProductHost is the
            // parent's full authority and must keep reaching module routes
            // (desktop documents/knowledge) without a delegation record.
            if authorization.claims.audience == crate::capabilities::Audience::FirstPartyShell
                && operation == crate::capabilities::Operation::ModuleRoute
                && method != Method::GET
                && method != Method::HEAD
            {
                let path = req.uri().path();
                let delegated = state
                    .module_surfaces
                    .iter()
                    .find(|surface| {
                        path.strip_prefix(&surface.namespace)
                            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
                    })
                    .is_some_and(|surface| state.delegations.permits(surface));
                if !delegated {
                    return error_response(
                        StatusCode::FORBIDDEN,
                        "module_delegation_required",
                        "Module surface requires delegation",
                    );
                }
            }
            if authorization.claims.audience == crate::capabilities::Audience::FirstPartyShell {
                let elevated = operation.assurance_level()
                    == Some(crate::capabilities::AssuranceLevel::Elevated);
                let grant = req
                    .headers()
                    .get("X-Agent24-Elevation")
                    .map(|v| v.to_str().unwrap_or("").to_owned());
                if elevated && grant.is_none() {
                    if method == Method::POST
                        && matches!(
                            template,
                            "/api/v1/idoris/install" | "/api/v1/idoris/connect"
                        )
                    {
                        // I-1: the presence summary comes from this body.
                        let Ok(bytes) = axum::body::to_bytes(
                            req.into_body(),
                            agent24_domain::http::MAX_BODY_BYTES,
                        )
                        .await
                        else {
                            return error_response(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "payload_too_large",
                                "Request body exceeds limit",
                            );
                        };
                        if let Some(authority) = &state.first_party {
                            authority.remember_idoris_request(
                                &authorization.claims.capability_id,
                                template,
                                &bytes,
                            );
                        }
                    } else if method == Method::POST && template == "/api/v1/skills/{id}/activate" {
                        // SL-4: the summary is built from stored revisions,
                        // never from anything the caller sent besides ids.
                        let path = req.uri().path().to_owned();
                        let Ok(bytes) = axum::body::to_bytes(
                            req.into_body(),
                            agent24_domain::http::MAX_BODY_BYTES,
                        )
                        .await
                        else {
                            return error_response(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "payload_too_large",
                                "Request body exceeds limit",
                            );
                        };
                        if let Some(authority) = &state.first_party
                            && let Some(skill_id) = path
                                .strip_prefix("/api/v1/skills/")
                                .and_then(|rest| rest.strip_suffix("/activate"))
                            && let Some(summary) = crate::skill_routes::activation_summary(
                                &state.store,
                                skill_id,
                                &bytes,
                            )
                            .await
                        {
                            authority.remember_skill_activation(
                                &authorization.claims.capability_id,
                                &path,
                                &bytes,
                                summary,
                            );
                        }
                    } else if method == Method::POST && template == "/api/v1/attached" {
                        let bytes = match axum::body::to_bytes(
                            req.into_body(),
                            agent24_domain::http::MAX_BODY_BYTES,
                        )
                        .await
                        {
                            Ok(bytes) => bytes,
                            Err(_) => {
                                return error_response(
                                    StatusCode::PAYLOAD_TOO_LARGE,
                                    "payload_too_large",
                                    "Request body exceeds limit",
                                );
                            }
                        };
                        if let Some(authority) = &state.first_party
                            && let Some(path) = agent24_attach::attached::config_path()
                            && let Ok(registrations) = agent24_attach::attached::list(&path)
                        {
                            authority.remember_attached_request(
                                &authorization.claims.capability_id,
                                &bytes,
                                &registrations,
                            );
                        }
                    }
                    return (StatusCode::FORBIDDEN, axum::Json(serde_json::json!({"error":{"code":"presence_required","message":"Presence confirmation required","operation":format!("{operation:?}")}}))).into_response();
                }
                if let Some(grant) = grant {
                    let (parts, body) = req.into_parts();
                    // A grant binds method, path and body; a query string is
                    // none of those, so the approved text could not describe
                    // it (`/candidates?endpoint=` would reach another port).
                    // No CLI request carries one: refuse before consuming.
                    if parts.uri.query().is_some() {
                        return error_response(
                            StatusCode::FORBIDDEN,
                            "elevation_invalid",
                            "Elevation grant is invalid",
                        );
                    }
                    let bytes = match axum::body::to_bytes(
                        body,
                        agent24_domain::http::MAX_BODY_BYTES,
                    )
                    .await
                    {
                        Ok(bytes) => bytes,
                        Err(_) => {
                            return error_response(
                                StatusCode::PAYLOAD_TOO_LARGE,
                                "payload_too_large",
                                "Request body exceeds limit",
                            );
                        }
                    };
                    let target = crate::capabilities::first_party::Target {
                        operation,
                        method: method.to_string(),
                        template: template.to_owned(),
                        path: parts.uri.path().to_owned(),
                        body_sha256: crate::capabilities::first_party::body_digest(&bytes),
                    };
                    // Revalidate after buffering, immediately before the atomic
                    // consume. A process may exit while supplying a slow body.
                    let bearer = parts
                        .headers
                        .get(header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .unwrap_or("");
                    if store
                        .validate_bearer(
                            bearer,
                            operation,
                            &crate::capabilities::Resource::global(),
                            crate::capabilities::unix_now(),
                        )
                        .is_err()
                        || !state.first_party.as_ref().is_some_and(|authority| {
                            authority.consume(
                                &grant,
                                &authorization.claims.capability_id,
                                &target,
                                crate::capabilities::unix_now(),
                            )
                        })
                    {
                        return error_response(
                            StatusCode::FORBIDDEN,
                            "elevation_invalid",
                            "Elevation grant is invalid",
                        );
                    }
                    req = Request::from_parts(parts, Body::from(bytes));
                }
            }
            req.extensions_mut().insert(authorization);
            next.run(req).await
        }
        Err(
            err @ (crate::capabilities::CapabilityError::OperationDenied
            | crate::capabilities::CapabilityError::ResourceDenied
            | crate::capabilities::CapabilityError::HostRequired),
        ) => {
            if matches!(err, crate::capabilities::CapabilityError::OperationDenied)
                && let Ok(auth) = store.authenticate_bearer(bearer, crate::capabilities::unix_now())
            {
                crate::capabilities::bridge::write_component_denial(
                    store,
                    &state.store,
                    &auth,
                    agent24_store::CapabilityDenyReason::Operation,
                )
                .await;
            }
            error_response(StatusCode::FORBIDDEN, "forbidden", "Operation not allowed")
        }
        Err(err) => {
            finish_rejection(
                state,
                TokenRejection {
                    response: error_response(
                        StatusCode::UNAUTHORIZED,
                        "unauthorized",
                        "Missing or invalid bearer token",
                    ),
                    denial: component_liveness_denial(store, bearer, err),
                },
            )
            .await
        }
    }
}

/// Constant-time comparison — a timing oracle on a localhost token is a small
/// risk, but the cost of doing it right is one function.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
