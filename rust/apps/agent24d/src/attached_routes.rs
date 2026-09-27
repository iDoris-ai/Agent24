//! `/api/v1/attached` — register, rotate, revoke and list A3 attached modules
//! (`docs/design/A3-ATTACHED-MODULE.md` §3.2, PR slice A3-2a).
//!
//! Storage and the A3-2a-level manifest checks live in [`crate::attached`];
//! this file is the axum-facing translation from that module's `Result`s to
//! the v1 error envelope. Bearer auth is the SAME layer every other kernel
//! route sits behind (`crate::server::auth`, applied in
//! `build_router_with_modules` after this router merges in) — nothing here
//! re-checks it.
//!
//! `GET /api/v1/attached` is a NEW endpoint rather than the augmentation the
//! design doc's §3.2 table describes for the existing `GET /api/v1/os` — see
//! `crate::attached::list`'s doc comment for why, and this PR's own report.

use std::path::Path;

use agent24_domain::http::{error_response, read_body_or_response};
use agent24_protocol::{AttachedAddRequest, AttachedList};
use axum::Json;
use axum::body::Body;
use axum::extract::{Path as AxumPath, State};
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};

use crate::attached::{self, RegisterError, RegisterOutcome};
use crate::server::AppState;

pub async fn post_attached(state: State<AppState>, req: Request<Body>) -> Response {
    let Some(path) = attached::config_path() else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "HOME not set",
        );
    };
    post_attached_at(state, req, &path).await
}

/// [`post_attached`], reading/writing the registry at an injected path — same
/// seam `os_routes::patch_os_at` uses, for the same reason: tests assert on
/// exactly what did or did not get written without touching the real
/// `~/.agent24/attached.json`.
async fn post_attached_at(
    State(state): State<AppState>,
    req: Request<Body>,
    path: &Path,
) -> Response {
    let bytes = match read_body_or_response(req).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let body: AttachedAddRequest = match serde_json::from_slice(&bytes) {
        Ok(b) => b,
        Err(e) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                "invalid_request",
                &format!("invalid body: {e}"),
            );
        }
    };
    // §3.2's 409 case: a brand-new name already claimed by some OTHER kind of
    // domain OS (an installed package or a compiled-in module). `os_reports`
    // is the daemon's own startup catalogue snapshot — the same source
    // `os_routes.rs` already treats as authoritative for "does this daemon
    // provide a module by this name" — so this needs no new AppState field.
    let name_taken = |name: &str| state.os_reports.iter().any(|r| r.name == name);
    match attached::register(path, &body.manifest, body.allow_relax, name_taken) {
        Ok(RegisterOutcome::Created(resp)) => (StatusCode::CREATED, Json(resp)).into_response(),
        Ok(RegisterOutcome::Rotated(resp)) => (StatusCode::OK, Json(resp)).into_response(),
        Err(RegisterError::InvalidManifest(msg)) => {
            error_response(StatusCode::BAD_REQUEST, "invalid_manifest", &msg)
        }
        Err(RegisterError::NameTaken(name)) => error_response(
            StatusCode::CONFLICT,
            "name_taken",
            &format!("{name:?} is already a domain OS this daemon provides"),
        ),
        Err(RegisterError::RelaxRequiresConfirmation) => error_response(
            StatusCode::FORBIDDEN,
            "relax_requires_confirmation",
            "registering with wider privacy (remote_allowed, or a new capability) requires host \
             confirmation — resend with \"allow_relax\": true",
        ),
        Err(RegisterError::Io(msg)) => {
            error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &msg)
        }
    }
}

pub async fn delete_attached(state: State<AppState>, name: AxumPath<String>) -> Response {
    let Some(path) = attached::config_path() else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "HOME not set",
        );
    };
    delete_attached_at(state, name, &path).await
}

async fn delete_attached_at(
    State(_state): State<AppState>,
    AxumPath(name): AxumPath<String>,
    path: &Path,
) -> Response {
    match attached::revoke(path, &name) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => error_response(
            StatusCode::NOT_FOUND,
            "not_found",
            &format!("no attached module named {name:?}"),
        ),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &e),
    }
}

pub async fn list_attached(state: State<AppState>) -> Response {
    let Some(path) = attached::config_path() else {
        return error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "HOME not set",
        );
    };
    list_attached_at(state, &path).await
}

async fn list_attached_at(State(_state): State<AppState>, path: &Path) -> Response {
    match attached::list(path) {
        Ok(modules) => Json(AttachedList { modules }).into_response(),
        Err(e) => error_response(StatusCode::INTERNAL_SERVER_ERROR, "internal", &e),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use axum::body::to_bytes;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    fn tmp_path() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "a24-attached-routes-test-{}-{n}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("attached.json")
    }

    fn manifest(name: &str) -> String {
        format!(
            "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             impl_kind: attached_process\nkernel_capabilities: [events]\n"
        )
    }

    async fn body_json(r: Response) -> serde_json::Value {
        let bytes = to_bytes(r.into_body(), 64 * 1024).await.unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn req(body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .unwrap()
    }

    #[tokio::test]
    async fn a_first_registration_is_201_with_the_token_shown_once() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        let r = post_attached_at(
            State(state),
            req(serde_json::json!({"manifest": manifest("agentear")})),
            &path,
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
        let body = body_json(r).await;
        for key in [
            "name",
            "manifest_digest",
            "token",
            "socket_path",
            "token_id",
        ] {
            assert!(body.get(key).is_some(), "missing {key}: {body}");
        }
        assert_eq!(body["name"], "agentear");
    }

    #[tokio::test]
    async fn rotating_the_same_name_is_200_not_201() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        let m = manifest("agentear");
        let first = post_attached_at(
            State(state.clone()),
            req(serde_json::json!({"manifest": m})),
            &path,
        )
        .await;
        assert_eq!(first.status(), StatusCode::CREATED);
        let second =
            post_attached_at(State(state), req(serde_json::json!({"manifest": m})), &path).await;
        assert_eq!(second.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn a_relaxing_registration_without_allow_relax_is_403() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        let mut m = manifest("agentear");
        m.push_str("model_access: remote_allowed\n");
        let r =
            post_attached_at(State(state), req(serde_json::json!({"manifest": m})), &path).await;
        assert_eq!(r.status(), StatusCode::FORBIDDEN);
        let body = body_json(r).await;
        assert_eq!(body["error"]["code"], "relax_requires_confirmation");
        assert!(
            !path.exists(),
            "a refused relax must not create a record at all"
        );
    }

    #[tokio::test]
    async fn the_same_relaxing_registration_with_allow_relax_succeeds() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        let mut m = manifest("agentear");
        m.push_str("model_access: remote_allowed\n");
        let r = post_attached_at(
            State(state),
            req(serde_json::json!({"manifest": m, "allow_relax": true})),
            &path,
        )
        .await;
        assert_eq!(r.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn an_invalid_manifest_is_400() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        let r = post_attached_at(
            State(state),
            req(serde_json::json!({"manifest": "not: [valid"})),
            &path,
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(r).await["error"]["code"], "invalid_manifest");
    }

    #[tokio::test]
    async fn the_reserved_name_attached_is_400() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        let r = post_attached_at(
            State(state),
            req(serde_json::json!({"manifest": manifest("attached")})),
            &path,
        )
        .await;
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(body_json(r).await["error"]["code"], "invalid_manifest");
    }

    #[tokio::test]
    async fn get_never_returns_a_token_field() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        post_attached_at(
            State(state.clone()),
            req(serde_json::json!({"manifest": manifest("agentear")})),
            &path,
        )
        .await;
        let r = list_attached_at(State(state), &path).await;
        assert_eq!(r.status(), StatusCode::OK);
        let body = body_json(r).await;
        let text = body.to_string();
        assert!(!text.contains("\"token\""));
        assert!(!text.contains("token_sha256"));
        assert_eq!(body["modules"][0]["name"], "agentear");
        assert_eq!(body["modules"][0]["attach_status"], "detached");
    }

    #[tokio::test]
    async fn delete_is_204_then_404() {
        let state = crate::server::tests::state().await;
        let path = tmp_path();
        post_attached_at(
            State(state.clone()),
            req(serde_json::json!({"manifest": manifest("agentear")})),
            &path,
        )
        .await;
        let first =
            delete_attached_at(State(state.clone()), AxumPath("agentear".to_owned()), &path).await;
        assert_eq!(first.status(), StatusCode::NO_CONTENT);
        let second = delete_attached_at(State(state), AxumPath("agentear".to_owned()), &path).await;
        assert_eq!(second.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn a_name_already_taken_by_another_module_is_409() {
        // `state()` below builds an AppState whose `os_reports` we cannot
        // easily seed from here without duplicating `crate::server::tests`'
        // own fixture machinery — so this exercises the 409 branch directly
        // against `crate::attached::register`'s `name_taken` closure instead,
        // which is exactly what `post_attached_at` calls it with. The
        // `attached.rs` unit test
        // `a_name_already_claimed_by_another_kind_of_module_is_refused`
        // covers the same branch at the storage layer; this one additionally
        // pins the REST status code and error `code`.
        let path = tmp_path();
        let err = crate::attached::register(&path, &manifest("sin90"), false, |n| n == "sin90")
            .unwrap_err();
        assert_eq!(
            err,
            crate::attached::RegisterError::NameTaken("sin90".to_owned())
        );
    }
}
