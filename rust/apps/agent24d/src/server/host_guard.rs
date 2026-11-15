use std::sync::Arc as StdArc;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use rand::RngCore;

use super::error_response;

/// The legacy-mode `ready` line. With `omit_token` (B3 / S-04: the LaunchAgent
/// persists stdout to a log file) the bearer is left out — it is still in the
/// 0600 `daemon.json`, which is where the CLI of a service-run daemon reads it.
pub(super) fn legacy_ready_line(
    port: u16,
    token: &str,
    omit_token: bool,
    generation: &str,
) -> serde_json::Value {
    let mut ready = serde_json::json!({
        "type": "ready",
        "port": port,
        "auth_mode": "legacy_single_token",
        "generation": generation,
        "version": env!("CARGO_PKG_VERSION"),
    });
    if !omit_token {
        ready["token"] = serde_json::json!(token);
    }
    ready
}

/// B1 (REVIEW-TODO PR-B, R4 / C-03): DNS-rebinding and browser-CSRF defence for
/// the REST surface. Layered OUTSIDE [`build_router`] by `serve` (the only
/// place the real bound port is known), so it runs before `auth` and covers
/// `/api/v1/health` and the module reverse-proxy routes alike. It only inspects
/// headers — the request reaching a module is untouched, and the proxy builds
/// its own fixed upstream `Host` (`agent24-os-proto::proxy`).
///
/// - `Host` must be `127.0.0.1:{port}`, `localhost:{port}` or `[::1]:{port}`
///   (ASCII case-insensitive); any other value is 403 `bad_host`, and no host
///   at all (an HTTP/1.0 request) is 400 `bad_host`. (HTTP/2 carries the authority in the URI, which is used when there is no
///   `Host` header.)
/// - An `Origin` header other than the literal `null` is 403 `forbidden_origin`
///   (browsers always send it on cross-origin requests; the CLI/desktop main
///   process/bridges never do). `Origin: null` stays allowed here; the WS
///   upgrade rejects any `Origin` itself.
pub fn guard_host_origin(router: Router, port: u16) -> Router {
    let allowed: StdArc<[String; 3]> = StdArc::new([
        format!("127.0.0.1:{port}"),
        format!("localhost:{port}"),
        format!("[::1]:{port}"),
    ]);
    router.layer(middleware::from_fn_with_state(allowed, host_origin_guard))
}

async fn host_origin_guard(
    State(allowed): State<StdArc<[String; 3]>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
        .or_else(|| req.uri().authority().map(|a| a.as_str().to_owned()));
    match host.as_deref() {
        None => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "bad_host",
                "A Host header is required",
            );
        }
        Some(h) if !allowed.iter().any(|a| a.eq_ignore_ascii_case(h)) => {
            return error_response(
                StatusCode::FORBIDDEN,
                "bad_host",
                "Host header is not this daemon's loopback address",
            );
        }
        Some(_) => {}
    }
    if req
        .headers()
        .get(header::ORIGIN)
        .is_some_and(|o| o.as_bytes() != b"null")
    {
        return error_response(
            StatusCode::FORBIDDEN,
            "forbidden_origin",
            "Browser-originated requests are not allowed",
        );
    }
    next.run(req).await
}

pub fn generate_token() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Set once the ready line has gone out (SPEC-002 §4): from then on stdout
/// carries nothing, not even a `failed` line.
pub static READY_SENT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
