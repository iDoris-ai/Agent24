//! ME4-S3 §2.5/§3.6 — the `fired` registration point: the kernel's own
//! delivery of a schedule's fire, POSTed to a fixed path under the module's
//! `route_namespace`. See `agent24_os_proto::kernel_call::FiredBody` for the
//! wire body both sides now share (M7).
//!
//! **Kernel contract** (from `agent24-scheduler::deliveries`, mirrored here
//! so a module author does not have to go read that crate): the whole
//! attempt must finish within `DELIVERY_TIMEOUT` (10s); a non-2xx response
//! or a timeout counts as one failed attempt, retried after 5s then 15s;
//! `MAX_SENT_ATTEMPTS` (3) sent attempts before the fire is recorded
//! `failed`; the response body is capped at `MAX_FIRED_RESPONSE_BYTES`
//! (64 KiB). A handler should therefore de-duplicate by `fire_id` first,
//! answer 2xx as fast as possible, and do slow work after replying.

use agent24_os_proto::kernel_call::{FIRE_ID_HEADER, FiredBody, SCHEDULE_KEY_HEADER};
use axum::Json;
use axum::extract::{FromRequest, Request};
use axum::handler::Handler;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{Router, post};
use serde_json::json;

use crate::context::RequestContext;

/// Relative to the module's own `route_namespace` (ME4-S1 §5.3): the
/// kernel's full path is `/api/v1/<ns>/_a24/scheduler/fired`.
pub const FIRED_PATH: &str = "/_a24/scheduler/fired";

/// One `fired` delivery, extracted and validated.
#[derive(Debug, Clone)]
pub struct FiredDelivery {
    pub fire_id: String,
    pub schedule_key: Option<String>,
    pub body: FiredBody,
    pub ctx: RequestContext,
}

/// Why a `fired` POST was rejected. Rendered as 400 by default; a handler
/// that wants its own error shape can use `Result<FiredDelivery,
/// FiredRejection>` and render it itself instead of relying on this impl.
#[derive(Debug, Clone)]
pub enum FiredRejection {
    MissingFireId,
    BadBody(String),
    BadTimestamp,
}

impl IntoResponse for FiredRejection {
    fn into_response(self) -> Response {
        let message = match &self {
            Self::MissingFireId => format!("missing required header `{FIRE_ID_HEADER}`"),
            Self::BadBody(msg) => msg.clone(),
            Self::BadTimestamp => {
                "scheduled_for/fired_at must be a fixed-width ISO-8601 UTC timestamp \
                 (YYYY-MM-DDTHH:MM:SSZ)"
                    .to_owned()
            }
        };
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": {"code": "invalid_request", "message": message}})),
        )
            .into_response()
    }
}

impl<S: Send + Sync> FromRequest<S> for FiredDelivery {
    type Rejection = FiredRejection;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        let (parts, body) = req.into_parts();
        let fire_id = parts
            .headers
            .get(FIRE_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .ok_or(FiredRejection::MissingFireId)?;
        let schedule_key = parts
            .headers
            .get(SCHEDULE_KEY_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let ctx = RequestContext::from_headers(&parts.headers);

        let req = Request::from_parts(parts, body);
        let Json(body): Json<FiredBody> = Json::from_request(req, state)
            .await
            .map_err(|e| FiredRejection::BadBody(e.body_text()))?;
        if !is_fixed_width_iso8601(&body.scheduled_for) || !is_fixed_width_iso8601(&body.fired_at) {
            return Err(FiredRejection::BadTimestamp);
        }
        Ok(Self {
            fire_id,
            schedule_key,
            body,
            ctx,
        })
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ`, byte-exact (20 bytes) — a shape check, not a
/// calendar validation (the kernel already produced this timestamp; this
/// only guards against a differently-shaped one reaching a handler that
/// assumes fixed width, e.g. for string-prefix comparisons).
fn is_fixed_width_iso8601(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z'
        && b[0..4].iter().all(u8::is_ascii_digit)
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[11..13].iter().all(u8::is_ascii_digit)
        && b[14..16].iter().all(u8::is_ascii_digit)
        && b[17..19].iter().all(u8::is_ascii_digit)
}

/// Registers `handler` at [`FIRED_PATH`] on `router`. See the module docs
/// for the kernel's timing/retry contract.
pub fn with_fired<S, H, T>(router: Router<S>, handler: H) -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    H: Handler<T, S>,
    T: 'static,
{
    router.route(FIRED_PATH, post(handler))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        with_fired(Router::new(), |delivery: FiredDelivery| async move {
            Json(json!({"received": delivery.fire_id}))
        })
    }

    fn body_json() -> serde_json::Value {
        json!({
            "key": "k1",
            "trigger": "tick",
            "scheduled_for": "2026-01-01T00:00:00Z",
            "fired_at": "2026-01-01T00:00:05Z",
        })
    }

    #[tokio::test]
    async fn a_legal_delivery_is_accepted_and_reaches_the_handler() {
        let req = HttpRequest::post(FIRED_PATH)
            .header(FIRE_ID_HEADER, "fire-1")
            .header(SCHEDULE_KEY_HEADER, "k1")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body_json()).unwrap()))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn missing_fire_id_header_is_rejected_400() {
        let req = HttpRequest::post(FIRED_PATH)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body_json()).unwrap()))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_unknown_body_field_is_rejected_400() {
        let mut body = body_json();
        body["extra"] = json!("nope");
        let req = HttpRequest::post(FIRED_PATH)
            .header(FIRE_ID_HEADER, "fire-1")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_non_fixed_width_timestamp_is_rejected_400() {
        let mut body = body_json();
        body["scheduled_for"] = json!("2026-01-01T00:00:00.000Z");
        let req = HttpRequest::post(FIRED_PATH)
            .header(FIRE_ID_HEADER, "fire-1")
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap();
        let resp = app().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_handler_can_render_the_rejection_in_its_own_shape() {
        let router: Router = with_fired(
            Router::new(),
            |delivery: Result<FiredDelivery, FiredRejection>| async move {
                match delivery {
                    Ok(d) => (StatusCode::OK, Json(json!({"fire_id": d.fire_id}))).into_response(),
                    Err(_) => (StatusCode::UNPROCESSABLE_ENTITY, "custom shape").into_response(),
                }
            },
        );
        let req = HttpRequest::post(FIRED_PATH)
            .header("content-type", "application/json")
            .body(Body::from(serde_json::to_vec(&body_json()).unwrap()))
            .unwrap();
        let resp = router.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }
}
