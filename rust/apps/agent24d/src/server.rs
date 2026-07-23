//! HTTP server: router, auth middleware, ready line, graceful shutdown.

use std::sync::Arc;
use std::time::Duration;

use agent24_models::ProviderRegistry;
use agent24_protocol::{ErrorBody, ErrorEnvelope, Health};
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, post};
use rand::RngCore;
use tokio_util::sync::CancellationToken;

/// Grace period for in-flight requests after a shutdown signal; the process
/// force-exits after this so `kill -TERM` always terminates within ~2s
/// (TASKS B2 acceptance).
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct AppState {
    pub token: Arc<String>,
    pub registry: Arc<ProviderRegistry>,
    pub usage: Arc<crate::routes::UsageCounters>,
    pub events: crate::events::EventsHub,
    /// Daemon-wide shutdown token; handlers derive request tokens from it so
    /// shutdown cancels in-flight provider calls (run-level cancel joins in C2)
    pub shutdown: CancellationToken,
}

impl AppState {
    pub fn new(token: String, registry: ProviderRegistry, shutdown: CancellationToken) -> Self {
        Self {
            token: Arc::new(token),
            registry: Arc::new(registry),
            usage: Arc::new(crate::routes::UsageCounters::default()),
            events: crate::events::EventsHub::default(),
            shutdown,
        }
    }
}

pub fn error_response(status: StatusCode, code: &str, message: &str) -> Response {
    let body = ErrorEnvelope {
        error: ErrorBody {
            code: code.to_owned(),
            message: message.to_owned(),
            details: None,
        },
    };
    (status, Json(body)).into_response()
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        backend: "rust".to_owned(),
    })
}

/// Authenticated shutdown (bearer token proves the caller owns this daemon —
/// unlike a pid from a possibly-stale state file, this can never kill an
/// unrelated reused-pid process). Used by `agent24 daemon stop`.
async fn shutdown_handler(State(state): State<AppState>) -> Response {
    tracing::info!("shutdown requested via /api/v1/shutdown");
    state.shutdown.cancel();
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "ok": true })),
    )
        .into_response()
}

async fn fallback() -> Response {
    error_response(StatusCode::NOT_FOUND, "not_found", "No v1 route")
}

/// Bearer-token gate for everything except `GET /api/v1/health`
/// (SPEC-002 §4: health is the only unauthenticated endpoint — method
/// included, so a future POST on the same path never silently bypasses auth).
async fn auth(State(state): State<AppState>, req: Request<Body>, next: Next) -> Response {
    if req.method() == Method::GET && req.uri().path() == "/api/v1/health" {
        return next.run(req).await;
    }
    let authorized = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|presented| constant_time_eq(presented.as_bytes(), state.token.as_bytes()));
    if authorized {
        next.run(req).await
    } else {
        error_response(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "Missing or invalid bearer token",
        )
    }
}

/// Constant-time comparison — a timing oracle on a localhost token is a small
/// risk, but the cost of doing it right is one function.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/chat", post(crate::routes::post_chat))
        .route("/api/v1/models", get(crate::routes::get_models))
        .route("/api/v1/usage", get(crate::routes::get_usage))
        .route("/api/v1/events", get(crate::events::ws_events))
        .route("/api/v1/shutdown", axum::routing::post(shutdown_handler))
        .fallback(fallback)
        .layer(middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state)
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub async fn serve(port: u16, cancel: CancellationToken) -> Result<(), std::io::Error> {
    let token = generate_token();
    let state = AppState::new(token.clone(), ProviderRegistry::from_env(), cancel.clone());
    let router = build_router(state);

    // 127.0.0.1 only — never a public bind (SPEC-001 §9)
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    let local = listener.local_addr()?;

    // SPEC-002 §4 ready line: parsers scan stdout for the first type=="ready"
    // JSON line. stdout carries nothing else (logs go to stderr).
    // Discovery state file BEFORE the ready line: a CLI that has seen the
    // ready line may immediately rely on attached-mode discovery.
    let daemon_pid = std::process::id();
    if let Err(err) =
        agent24_protocol::state_file::write(&agent24_protocol::state_file::DaemonState {
            port: local.port(),
            token: token.clone(),
            pid: daemon_pid,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        })
    {
        tracing::warn!("could not write daemon state file: {err}");
    }

    println!(
        "{}",
        serde_json::json!({
            "type": "ready",
            "port": local.port(),
            "token": token,
            "version": env!("CARGO_PKG_VERSION"),
        })
    );

    // Signal handling: SIGTERM (process managers) + SIGINT (Ctrl+C in dev)
    let signal_cancel = cancel.clone();
    tokio::spawn(async move {
        let sigterm = async {
            #[cfg(unix)]
            {
                use tokio::signal::unix::{SignalKind, signal};
                match signal(SignalKind::terminate()) {
                    Ok(mut s) => {
                        s.recv().await;
                    }
                    Err(err) => {
                        // Never resolve on registration failure — resolving would
                        // be indistinguishable from a real signal and trigger an
                        // immediate graceful shutdown at startup.
                        tracing::error!("SIGTERM handler failed: {err}");
                        std::future::pending::<()>().await;
                    }
                }
            }
            #[cfg(not(unix))]
            std::future::pending::<()>().await;
        };
        let sigint = async {
            if let Err(err) = tokio::signal::ctrl_c().await {
                // Mirror the SIGTERM arm: a registration failure must never be
                // indistinguishable from a real signal — park forever instead
                // of resolving the select and triggering a spurious shutdown.
                tracing::error!("SIGINT handler failed: {err}");
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = sigterm => {},
            () = sigint => {},
        }
        tracing::info!("shutdown signal received");
        signal_cancel.cancel();
    });

    let graceful_cancel = cancel.clone();
    let server = axum::serve(listener, router)
        .with_graceful_shutdown(async move { graceful_cancel.cancelled().await });

    // Force-exit backstop: once cancelled, in-flight requests get
    // SHUTDOWN_GRACE to finish, then the process exits regardless.
    let result = tokio::select! {
        result = server => result,
        () = async {
            cancel.cancelled().await;
            tokio::time::sleep(SHUTDOWN_GRACE).await;
        } => {
            tracing::warn!("graceful shutdown exceeded {SHUTDOWN_GRACE:?}; forcing exit");
            Ok(())
        }
    };
    // Only remove our own state file — a newer daemon may have replaced it
    agent24_protocol::state_file::remove_if_owner(daemon_pid);
    result
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn state() -> AppState {
        AppState::new(
            "testtoken".to_owned(),
            ProviderRegistry::new(vec![]),
            CancellationToken::new(),
        )
    }

    async fn body_json(res: Response) -> serde_json::Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn health_needs_no_token() {
        let res = build_router(state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let json = body_json(res).await;
        assert_eq!(json["status"], "ok");
        assert_eq!(json["backend"], "rust");
        assert!(json["version"].as_str().is_some());
    }

    #[tokio::test]
    async fn post_to_health_path_requires_token() {
        // The auth exemption is GET-only — same path, other method: 401
        let res = build_router(state())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/v1/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn other_routes_401_without_token_with_v1_envelope() {
        let res = build_router(state())
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "unauthorized");
    }

    #[tokio::test]
    async fn wrong_token_401_correct_token_reaches_404_envelope() {
        let router = build_router(state());
        let res = router
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/api/v1/models")
                    .header("Authorization", "Bearer wrong")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        let res = router
            .oneshot(
                Request::builder()
                    .uri("/api/v1/definitely-not-a-route")
                    .header("Authorization", "Bearer testtoken")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        // authorized but unknown route → v1 404 envelope
        assert_eq!(res.status(), StatusCode::NOT_FOUND);
        let json = body_json(res).await;
        assert_eq!(json["error"]["code"], "not_found");
    }

    #[test]
    fn token_is_32_bytes_hex_and_unique() {
        let a = generate_token();
        let b = generate_token();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_eq_basics() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
    }
}
