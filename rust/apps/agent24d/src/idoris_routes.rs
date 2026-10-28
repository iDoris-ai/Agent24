//! Host-only managed iDoris lifecycle endpoints.

use crate::server::AppState;
use agent24d::idoris_host::{Action, Code, State as LifecycleState, Status};
use axum::{
    Json,
    extract::{Path, State},
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
            if path.ends_with("/status") {
                assert_eq!(response.status(), StatusCode::OK);
            }
            assert_eq!(
                crate::server::required_operation(&method, path),
                crate::capabilities::Operation::from_action("idoris.lifecycle").unwrap()
            );
        }
    }
}
