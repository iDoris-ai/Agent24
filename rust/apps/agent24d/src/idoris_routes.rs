//! Host-only managed iDoris lifecycle endpoints.

use crate::server::AppState;
use agent24_idoris::idoris_host::{
    Action, Code, ConnectRequest, InstallRequest, State as LifecycleState, Status,
};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};

async fn query_idoris<H, HFut, C, CFut>(
    http: H,
    cli: C,
) -> Option<(&'static str, serde_json::Value)>
where
    H: FnOnce() -> HFut,
    HFut: std::future::Future<Output = Option<serde_json::Value>>,
    C: FnOnce() -> CFut,
    CFut: std::future::Future<Output = Option<serde_json::Value>>,
{
    if let Some(value) = http().await {
        return Some(("idoris_http", value));
    }
    cli().await.map(|value| ("idoris_cli", value))
}

/// Machine-local chat model recommendation. HTTP is preferred while the
/// managed daemon is ready; the verified executable is the offline fallback.
#[derive(serde::Deserialize)]
pub struct RecommendationQuery {
    role: String,
}

pub async fn model_recommendation(
    State(state): State<AppState>,
    Query(query): Query<RecommendationQuery>,
) -> Response {
    if query.role != "chat" {
        return crate::server::error_response(
            axum::http::StatusCode::BAD_REQUEST,
            "unsupported_role",
            "only chat is supported",
        );
    }
    if let Some(host) = state.idoris_host
        && let Some((source, recommendation)) = query_idoris(
            || host.read_data_json("/v1/recommendation?role=chat"),
            || host.read_model_cli(&["recommend", "--role", "chat", "--json"]),
        )
        .await
    {
        return Json(serde_json::json!({"source":source, "recommendation":recommendation}))
            .into_response();
    }
    let model = "Qwen3-8B-4bit";
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    let installed = home
        .map(|p| p.join(".omlx/models").join(model).is_dir())
        .unwrap_or(false);
    Json(serde_json::json!({"source":"fallback", "recommendation":{
        "role":"chat", "model_id":model, "installed":installed, "size_bytes":null,
        "est_memory_bytes":null, "fits":null, "reason":"idoris_unavailable", "host":null,
        "alternatives":[]
    }}))
    .into_response()
}

pub async fn local_models(State(state): State<AppState>) -> Response {
    if let Some(host) = state.idoris_host
        && let Some((source, value)) = query_idoris(
            || host.read_data_json("/v1/models/local"),
            || host.read_model_cli(&["models", "local", "--json"]),
        )
        .await
    {
        return Json(serde_json::json!({"source":source, "models":value["models"]}))
            .into_response();
    }
    Json(serde_json::json!({"source":"fallback", "models":null})).into_response()
}

#[cfg(test)]
mod model_query_tests {
    use super::query_idoris;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[tokio::test]
    async fn http_success_does_not_call_the_cli() {
        let cli_calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&cli_calls);
        let result = query_idoris(
            || async { Some(json!({"model_id":"http-model"})) },
            || async move {
                observed.fetch_add(1, Ordering::SeqCst);
                Some(json!({}))
            },
        )
        .await;
        assert_eq!(
            result,
            Some(("idoris_http", json!({"model_id":"http-model"})))
        );
        assert_eq!(cli_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn unavailable_http_falls_back_to_cli_and_then_to_fallback() {
        let result = query_idoris(|| async { None }, || async { Some(json!({"models":[]})) }).await;
        assert_eq!(result, Some(("idoris_cli", json!({"models":[]}))));

        let result = query_idoris(|| async { None }, || async { None }).await;
        assert_eq!(result, None);
    }
}

pub(crate) fn failure(code: Code) -> Response {
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
            state: if state.idoris_unavailable {
                LifecycleState::Failed
            } else {
                LifecycleState::NotInstalled
            },
            version: None,
            data_port: None,
            admin_port: None,
            roles: Vec::new(),
            last_error: state.idoris_unavailable.then_some(Code::PersistenceFailed),
            current_operation: None,
            verification: None,
            budget_suggestion: None,
            budget_unavailable_reason: Some("host_unavailable".into()),
            migration_pending: false,
            last_migration_error: None,
        },
    };
    Json(status).into_response()
}
async fn submit(state: AppState, headers: HeaderMap, action: Action) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(if state.idoris_unavailable {
            Code::PersistenceFailed
        } else {
            Code::NotInstalled
        });
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
        Ok(op) => (axum::http::StatusCode::ACCEPTED, Json(op.view())).into_response(),
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
        Ok(op) => (axum::http::StatusCode::ACCEPTED, Json(op.view())).into_response(),
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
        Ok(op) => (axum::http::StatusCode::ACCEPTED, Json(op.view())).into_response(),
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
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpstreamQuery {
    endpoint: String,
}
pub async fn upstream_models(Query(query): Query<UpstreamQuery>) -> Response {
    match agent24_idoris::idoris_host::upstream_models(&query.endpoint).await {
        Ok(models) => Json(serde_json::json!({"models": models})).into_response(),
        Err(code) => failure(code),
    }
}
pub async fn operation(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotFound);
    };
    match host.operation(&id).await {
        Ok(op) => Json(op.view()).into_response(),
        Err(code) => failure(code),
    }
}
pub async fn cancel(State(state): State<AppState>, Path(id): Path<String>) -> Response {
    let Some(host) = state.idoris_host else {
        return failure(Code::NotFound);
    };
    match host.cancel(&id).await {
        Ok(op) => Json(op.view()).into_response(),
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
            (
                Method::GET,
                "/api/v1/idoris/upstream-models?endpoint=http%3A%2F%2Fexample.com",
            ),
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
                crate::capabilities::required_operation(
                    &method,
                    &path
                        .split('?')
                        .next()
                        .unwrap()
                        .replace("operations/missing", "operations/{id}"),
                ),
                crate::capabilities::Operation::from_action(
                    if method == Method::GET && !path.ends_with("/candidates") {
                        "idoris.read"
                    } else {
                        "idoris.lifecycle"
                    }
                )
            );
        }
    }
    #[tokio::test]
    async fn upstream_models_returns_catalogue_only_to_host_and_rejects_remote_origin() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let upstream = tokio::spawn(async move {
            axum::serve(
                listener,
                axum::Router::new().route(
                    "/v1/models",
                    axum::routing::get(|| async {
                        axum::Json(serde_json::json!({"data":[{"id":"local-model"}]}))
                    }),
                ),
            )
            .await
            .unwrap();
        });
        let (state, host, creative) = crate::server::tests::capability_state().await;
        let app = crate::server::build_router(state);
        for (token, endpoint, expected) in [
            (&host, endpoint.as_str(), StatusCode::OK),
            (&creative, endpoint.as_str(), StatusCode::FORBIDDEN),
            (&host, "http://example.com", StatusCode::BAD_REQUEST),
        ] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(format!(
                            "/api/v1/idoris/upstream-models?endpoint={endpoint}"
                        ))
                        .header("authorization", format!("Bearer {token}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), expected);
            let body: serde_json::Value = serde_json::from_slice(
                &axum::body::to_bytes(response.into_body(), 8192)
                    .await
                    .unwrap(),
            )
            .unwrap();
            if expected == StatusCode::OK {
                assert_eq!(body, serde_json::json!({"models":["local-model"]}));
            }
        }
        upstream.abort();
    }
    #[tokio::test]
    async fn core_connect_does_not_require_consent_and_plaintext_credentials_are_rejected() {
        use agent24_idoris::{idoris_host::IdorisHost, managed_idoris::ManagedIdoris};
        use std::sync::Arc;
        let (mut state, host, creative) = crate::server::tests::capability_state().await;
        let temp = tempfile::tempdir().unwrap();
        let managed = Arc::new(
            ManagedIdoris::new(state.router.clone(), temp.path().join("connection.json")).unwrap(),
        );
        state.idoris_host = Some(
            IdorisHost::system(
                temp.path().to_path_buf(),
                temp.path().to_path_buf(),
                managed,
                state.router.clone(),
            )
            .unwrap(),
        );
        let app = crate::server::build_router(state);
        for (path, body, expected, code) in [
            (
                "connect",
                serde_json::json!({"endpoint":"http://127.0.0.1:8088", "daily_model":"real-model"}),
                StatusCode::UNPROCESSABLE_ENTITY,
                "not_installed",
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
