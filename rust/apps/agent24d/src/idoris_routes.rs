//! Host-only managed iDoris lifecycle endpoints.

use crate::server::AppState;
use agent24d::idoris_host::{
    Action, Code, ConnectRequest, InstallRequest, State as LifecycleState, Status,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

fn failure(code: Code) -> Response {
    (code.http_status(), Json(serde_json::json!({"error": {
        "code": code, "message": code.message(),
        "retryable": matches!(code, Code::Busy | Code::KeychainLocked | Code::StartFailed),
        "next_action": if code == Code::KeychainLocked { "unlock_keychain" } else { "check_status" }
    }}))).into_response()
}
pub async fn status(State(state): State<AppState>) -> Response {
    let status = match state.idoris_host {
        Some(host) => host.status().await,
        None => Status {
            state: LifecycleState::NotInstalled,
            version: None,
            data_port: None,
            admin_port: None,
            roles: Vec::new(),
            last_error: None,
            current_operation: None,
        },
    };
    Json(status).into_response()
}
async fn submit(state: AppState, headers: HeaderMap, action: Action) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotInstalled);
    };
    let key = match headers
        .get("Idempotency-Key")
        .map(|h| h.to_str())
        .transpose()
    {
        Ok(key) => key,
        Err(_) => return failure(Code::InvalidRequest),
    };
    match host.submit(action, key).await {
        Ok(op) => (axum::http::StatusCode::ACCEPTED, Json(op)).into_response(),
        Err(code) => failure(code),
    }
}
pub async fn start(State(state): State<AppState>, headers: HeaderMap) -> Response {
    submit(state, headers, Action::Start).await
}
pub async fn stop(State(state): State<AppState>, headers: HeaderMap) -> Response {
    submit(state, headers, Action::Stop).await
}
pub async fn install(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<InstallRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotInstalled);
    };
    let Ok(Json(request)) = request else {
        return failure(Code::InvalidRequest);
    };
    let key = headers.get("Idempotency-Key").and_then(|k| k.to_str().ok());
    match host.install(request, key).await {
        Ok(op) => (axum::http::StatusCode::ACCEPTED, Json(op)).into_response(),
        Err(code) => failure(code),
    }
}
pub async fn connect(
    State(state): State<AppState>,
    headers: HeaderMap,
    request: Result<Json<ConnectRequest>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotInstalled);
    };
    let Ok(Json(request)) = request else {
        return failure(Code::InvalidRequest);
    };
    let key = headers.get("Idempotency-Key").and_then(|k| k.to_str().ok());
    match host.connect(request, key).await {
        Ok(op) => (axum::http::StatusCode::ACCEPTED, Json(op)).into_response(),
        Err(code) => failure(code),
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateQuery {
    endpoint: Option<String>,
}
pub async fn candidates(
    State(state): State<AppState>,
    Query(query): Query<CandidateQuery>,
) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotInstalled);
    };
    match host.candidates(query.endpoint.as_deref()).await {
        Ok(candidates) => Json(candidates).into_response(),
        Err(code) => failure(code),
    }
}
pub async fn operation(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotFound);
    };
    match host.operation(&id).await {
        Ok(op) => Json(op).into_response(),
        Err(code) => failure(code),
    }
}
pub async fn cancel(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotFound);
    };
    match host.cancel(&id).await {
        Ok(op) => Json(op).into_response(),
        Err(code) => failure(code),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use axum::{
        body::Body,
        http::{Method, Request, StatusCode},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn lifecycle_routes_exist_and_creative_is_forbidden() {
        let (state, host, creative) = crate::server::tests::capability_state().await;
        let app = crate::server::build_router(state);
        for (method, path) in [
            (Method::GET, "/api/v1/idoris/status"),
            (Method::POST, "/api/v1/idoris/install"),
            (Method::GET, "/api/v1/idoris/omlx/candidates"),
            (Method::POST, "/api/v1/idoris/connect"),
            (Method::POST, "/api/v1/idoris/start"),
            (Method::POST, "/api/v1/idoris/stop"),
            (Method::GET, "/api/v1/idoris/operations/missing"),
            (Method::POST, "/api/v1/idoris/operations/missing/cancel"),
        ] {
            let request = |token: &str| {
                Request::builder()
                    .method(method.clone())
                    .uri(path)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap()
            };
            assert_eq!(
                app.clone()
                    .oneshot(request(&creative))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::FORBIDDEN
            );
            let response = app.clone().oneshot(request(&host)).await.unwrap();
            if !path.contains("/operations/") {
                assert_ne!(response.status(), StatusCode::NOT_FOUND, "{path}");
            }
            if path.ends_with("/status") {
                assert_eq!(response.status(), StatusCode::OK);
            }
            assert_eq!(
                crate::server::required_operation(&method, path),
                crate::capabilities::Operation::from_action("idoris.lifecycle").unwrap()
            );
        }
    }
    #[tokio::test]
    async fn connect_and_install_reject_missing_consent_and_plaintext_credential_fields() {
        use agent24d::{idoris_host::IdorisHost, managed_idoris::ManagedIdoris};
        use std::sync::Arc;
        let (mut state, host, creative) = crate::server::tests::capability_state().await;
        let temp = tempfile::tempdir().unwrap();
        let managed = Arc::new(
            ManagedIdoris::new(state.router.clone(), temp.path().join("connection.json")).unwrap(),
        );
        state.idoris_host = Some(
            IdorisHost::system(temp.path().to_path_buf(), managed, state.router.clone()).unwrap(),
        );
        let app = crate::server::build_router(state);
        for (path, body, expected, code) in [
            (
                "connect",
                serde_json::json!({"endpoint":"http://127.0.0.1:8088", "daily_model":"real-model"}),
                StatusCode::UNPROCESSABLE_ENTITY,
                "consent_required",
            ),
            (
                "install",
                serde_json::json!({"consent":false}),
                StatusCode::UNPROCESSABLE_ENTITY,
                "consent_required",
            ),
            (
                "connect",
                serde_json::json!({"endpoint":"http://127.0.0.1:8088", "daily_model":"real-model", "omlx_key":"sk-request-sentinel"}),
                StatusCode::BAD_REQUEST,
                "invalid_request",
            ),
        ] {
            for (token, expected_status) in [(&host, expected), (&creative, StatusCode::FORBIDDEN)]
            {
                let response = app
                    .clone()
                    .oneshot(
                        Request::builder()
                            .method(Method::POST)
                            .uri(format!("/api/v1/idoris/{path}"))
                            .header("authorization", format!("Bearer {token}"))
                            .header("content-type", "application/json")
                            .header("Idempotency-Key", "contract-test")
                            .body(Body::from(body.to_string()))
                            .unwrap(),
                    )
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected_status);
                let bytes = axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if expected_status != StatusCode::FORBIDDEN {
                    assert_eq!(value["error"]["code"], code);
                }
                assert!(!String::from_utf8_lossy(&bytes).contains("sk-request-sentinel"));
            }
        }
    }
}
