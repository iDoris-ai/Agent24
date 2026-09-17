//! v1 module-approvals endpoints (T7b/ME-3e), `/api/v1/module-approvals`.
//! Manual body/query parsing throughout, following `crate::approvals`'s
//! existing style: axum extractor rejections would violate the v1 error
//! envelope (SPEC-002).

use agent24_protocol::ModuleApprovalDecision;
use agent24_store::StoreError;
use axum::Json;
use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use crate::routes::read_body_or_response;
use crate::server::{AppState, error_response};

pub async fn list_module_approvals(State(state): State<AppState>, req: Request<Body>) -> Response {
    let mut decision: Option<ModuleApprovalDecision> = None;
    if let Some(raw) = req.uri().query() {
        for pair in raw.split('&') {
            let mut it = pair.splitn(2, '=');
            if it.next() == Some("decision") {
                let value = it.next().unwrap_or("");
                match serde_json::from_value::<ModuleApprovalDecision>(serde_json::Value::String(
                    value.to_owned(),
                )) {
                    Ok(d) => decision = Some(d),
                    Err(_) => {
                        return error_response(
                            StatusCode::BAD_REQUEST,
                            "invalid_request",
                            &format!("invalid decision filter: {value}"),
                        );
                    }
                }
            }
        }
    }
    match state.module_approval_broker.list(decision).await {
        Ok(rows) => Json(serde_json::json!({ "module_approvals": rows })).into_response(),
        Err(err) => {
            tracing::error!("list module approvals failed: {err}");
            error_response(StatusCode::SERVICE_UNAVAILABLE, "internal", "storage error")
        }
    }
}

pub async fn get_module_approval(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Response {
    match state.module_approval_broker.get(&id).await {
        Ok(Some(approval)) => Json(approval).into_response(),
        Ok(None) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("module approval {id}"),
        ),
        Err(err) => {
            tracing::error!("get module approval failed: {err}");
            error_response(StatusCode::SERVICE_UNAVAILABLE, "internal", "storage error")
        }
    }
}

/// `decision` MUST be `approved` or `denied` — `pending`/`timed_out` are
/// states the SYSTEM assigns (submission, the periodic scan), never a value
/// a REST caller may request directly.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecideBody {
    decision: ModuleApprovalDecision,
}

pub async fn decide_module_approval(State(state): State<AppState>, req: Request<Body>) -> Response {
    // Path parsed manually off the URI, same as `crate::approvals::decide_approval`:
    // URI shape is /api/v1/module-approvals/{id}.
    let id = req
        .uri()
        .path()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_owned();
    let body = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(resp) => return resp,
    };
    let decide: DecideBody = match serde_json::from_slice(&body) {
        Ok(d) => d,
        Err(err) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("body must be {{decision: \"approved\"|\"denied\"}}: {err}"),
            );
        }
    };
    if !matches!(
        decide.decision,
        ModuleApprovalDecision::Approved | ModuleApprovalDecision::Denied
    ) {
        return error_response(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "decision must be \"approved\" or \"denied\"",
        );
    }
    match state
        .module_approval_broker
        .decide(&id, decide.decision)
        .await
    {
        Ok(approval) => Json(approval).into_response(),
        Err(StoreError::NotFound(_)) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("module approval {id}"),
        ),
        Err(StoreError::Conflict(_)) => error_response(
            StatusCode::CONFLICT,
            "module_approval_already_resolved",
            &format!("module approval {id} was already resolved or has expired"),
        ),
        Err(err) => {
            tracing::error!("decide module approval failed: {err}");
            error_response(StatusCode::SERVICE_UNAVAILABLE, "internal", "storage error")
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::extract::Path;

    fn req(uri: &str) -> Request<Body> {
        Request::builder().uri(uri).body(Body::empty()).unwrap()
    }

    fn post(uri: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    async fn body_json(r: Response) -> (StatusCode, serde_json::Value) {
        let status = r.status();
        let bytes = axum::body::to_bytes(r.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn get_and_list_agree_a_nonexistent_id_is_404() {
        let state = crate::server::tests::state().await;
        let r = get_module_approval(State(state.clone()), Path("nope".to_owned())).await;
        assert_eq!(r.status(), StatusCode::NOT_FOUND);

        let (status, body) =
            body_json(list_module_approvals(State(state), req("/api/v1/module-approvals")).await)
                .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["module_approvals"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn submit_then_list_get_and_decide_round_trip_over_rest() {
        let state = crate::server::tests::state().await;
        let answer = state
            .module_approval_broker
            .submit(
                "probe",
                "req-1",
                agent24_protocol::ModuleApprovalKind::Advise,
                "send_email".to_owned(),
                None,
                serde_json::json!({"x": 1}),
            )
            .await
            .unwrap();

        // list
        let (status, body) = body_json(
            list_module_approvals(State(state.clone()), req("/api/v1/module-approvals")).await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["module_approvals"].as_array().unwrap().len(), 1);

        // list filtered by decision
        let (status, body) = body_json(
            list_module_approvals(
                State(state.clone()),
                req("/api/v1/module-approvals?decision=denied"),
            )
            .await,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["module_approvals"], serde_json::json!([]));

        // get
        let r = get_module_approval(State(state.clone()), Path(answer.approval_id.clone())).await;
        assert_eq!(r.status(), StatusCode::OK);

        // decide: bad shape is 400
        let bad = decide_module_approval(
            State(state.clone()),
            post(
                &format!("/api/v1/module-approvals/{}", answer.approval_id),
                serde_json::json!({"decision": "pending"}),
            ),
        )
        .await;
        assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

        // decide: approve
        let ok = decide_module_approval(
            State(state.clone()),
            post(
                &format!("/api/v1/module-approvals/{}", answer.approval_id),
                serde_json::json!({"decision": "approved"}),
            ),
        )
        .await;
        assert_eq!(ok.status(), StatusCode::OK);
        let (_, decided) = body_json(ok).await;
        assert_eq!(decided["decision"], "approved");

        // decide again: conflict, not a silent overwrite
        let conflict = decide_module_approval(
            State(state),
            post(
                &format!("/api/v1/module-approvals/{}", answer.approval_id),
                serde_json::json!({"decision": "denied"}),
            ),
        )
        .await;
        assert_eq!(conflict.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn an_invalid_decision_filter_is_400() {
        let state = crate::server::tests::state().await;
        let r = list_module_approvals(
            State(state),
            req("/api/v1/module-approvals?decision=nonsense"),
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
    }
}
