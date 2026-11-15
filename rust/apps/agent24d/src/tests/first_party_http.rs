#![allow(clippy::unwrap_used)]
use super::*;
use crate::capabilities::{
    first_party::{body_digest, test_peer},
    presence::*,
};
use crate::enroll_api::{ElevateRequest, EnrollAuthority, IssuedToken};
use http_body_util::BodyExt;
use std::sync::atomic::{AtomicUsize, Ordering};
use tower::ServiceExt;

struct Allow;
#[async_trait::async_trait]
impl PresenceProvider for Allow {
    async fn confirm(&self, _req: PresenceRequest, _cancel: CancellationToken) -> PresenceOutcome {
        PresenceOutcome::Allowed
    }
}
async fn setup() -> (AppState, IssuedToken) {
    let (mut state, _, _) = tests::capability_state().await;
    state.presence_provider = Some(Arc::new(Allow));
    let token = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap();
    (state, token)
}
fn request(
    token: &str,
    method: &str,
    path: &str,
    body: &str,
    grant: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(grant) = grant {
        request = request.header("X-Agent24-Elevation", grant);
    }
    request.body(Body::from(body.to_owned())).unwrap()
}
async fn error_code(response: Response) -> String {
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()["error"]["code"]
        .as_str()
        .unwrap()
        .into()
}
async fn grant(
    state: &AppState,
    token: &IssuedToken,
    method: &str,
    path: &str,
    body: &str,
) -> zeroize::Zeroizing<String> {
    let (tx, _rx) = tokio::sync::oneshot::channel();
    state
        .first_party
        .as_ref()
        .unwrap()
        .elevate(
            &test_peer("daemon-test"),
            &token.token_id,
            ElevateRequest {
                method: method.into(),
                path: path.into(),
                body_sha256: body_digest(body.as_bytes()),
                client_summary: "ignored".into(),
            },
            tx,
            CancellationToken::new(),
        )
        .await
        .unwrap()
        .secret
}
#[tokio::test]
async fn bridge_mint_uses_the_host_summary_and_writes_the_audit_before_the_token() {
    let (mut state, token) = setup().await;
    state.dev_wechat_events = true;
    state
        .first_party
        .as_ref()
        .unwrap()
        .set_provider(Some(Arc::new(Allow)));
    let blocked = build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(blocked).await, "presence_required");

    let secret = grant(&state, &token, "POST", "/api/v1/capabilities/bridge", "").await;
    let response = build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["audience"], "component_runtime");
    assert_eq!(body["component_id"], "wechat-bridge");
    assert_eq!(body["ttl_seconds"], 28_800);
    assert_eq!(body["allowed_operations"][0], "inbound.ingest");
    let actions: Vec<_> = state
        .store
        .list_audit()
        .await
        .unwrap()
        .into_iter()
        .map(|entry| entry.action)
        .collect();
    assert!(
        actions
            .iter()
            .any(|action| action == "capability.token.issued")
    );
    assert!(!body.to_string().contains("客户端"));
    let issued = state
        .store
        .list_audit()
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.action == "capability.token.issued")
        .unwrap();
    assert_eq!(
        issued.detail["issued"]["issuer_capability_id"].as_str(),
        Some(token.token_id.as_str())
    );
}

/// The shell that minted the bridge token revokes it. The next use is 401.
/// A second revoke does not write another `capability.token.revoked` row.
/// Removing `CapabilityRevoke` from `resolve_target` makes `grant` fail.
#[tokio::test]
async fn shell_revokes_the_bridge_token_it_issued() {
    let (mut state, token) = setup().await;
    state.dev_wechat_events = true;
    state
        .first_party
        .as_ref()
        .unwrap()
        .set_provider(Some(Arc::new(Allow)));
    let secret = grant(&state, &token, "POST", "/api/v1/capabilities/bridge", "").await;
    let response = build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = body["capability_id"].as_str().unwrap();
    let bearer = body["token"].as_str().unwrap().to_owned();
    let path = format!("/api/v1/capabilities/{id}/revoke");
    let blocked = build_router(state.clone())
        .oneshot(request(&token.secret, "POST", &path, "", None))
        .await
        .unwrap();
    assert_eq!(blocked.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(blocked).await, "presence_required");
    for _ in 0..2 {
        let secret = grant(&state, &token, "POST", &path, "").await;
        let response = build_router(state.clone())
            .oneshot(request(
                &token.secret,
                "POST",
                &path,
                "",
                Some(secret.as_str()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
    let revoked = state
        .store
        .list_audit()
        .await
        .unwrap()
        .into_iter()
        .filter(|entry| entry.action == "capability.token.revoked")
        .count();
    assert_eq!(revoked, 1);
    let rejected = build_router(state)
        .oneshot(request(&bearer, "GET", "/api/v1/models", "", None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
}

/// After a restart the id is gone. The shell hears 404
/// `bridge_token_unknown` and is not asked to elevate. Answering
/// `presence_required` or `not_elevated_operation` turns this red.
#[tokio::test]
async fn shell_revoke_of_an_unknown_id_is_not_an_elevation_failure() {
    let (state, token) = setup().await;
    let response = build_router(state)
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/capabilities/deadbeef/revoke",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(response).await, "bridge_token_unknown");
}

/// A shell grant is refused for another shell token, a creative token, and
/// an id that is not in the store. Those bearers stay valid. ProductHost
/// still revokes the creative token. Dropping the bridge check in
/// `target_summary` lets `elevate` return a grant. Dropping the shell check
/// in `revoke_capability` turns the direct 403 into 200 and revokes the target.
#[tokio::test]
async fn shell_revoke_elevation_is_only_the_bridge_token() {
    use crate::capabilities::{Operation, Resource, unix_now};
    use crate::enroll_api::EnrollErrorCode;

    let (state, host, creative) = tests::capability_state().await;
    state
        .first_party
        .as_ref()
        .unwrap()
        .set_provider(Some(Arc::new(Allow)));
    let shell = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap();
    let caps = state.capabilities.clone().unwrap();
    let creative_id = caps
        .validate_bearer(
            &creative,
            Operation::ModelsRead,
            &Resource::global(),
            unix_now(),
        )
        .unwrap()
        .claims
        .capability_id;
    let shell_auth = caps
        .validate_bearer(
            &shell.secret,
            Operation::CapabilityRevoke,
            &Resource::global(),
            unix_now(),
        )
        .unwrap();
    for id in [
        creative_id.as_str(),
        shell.token_id.as_str(),
        "deadbeefdeadbeefdeadbeefdeadbeef",
    ] {
        let path = format!("/api/v1/capabilities/{id}/revoke");
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let err = state
            .first_party
            .as_ref()
            .unwrap()
            .elevate(
                &test_peer("daemon-test"),
                &shell.token_id,
                ElevateRequest {
                    method: "POST".into(),
                    path: path.clone(),
                    body_sha256: body_digest(b""),
                    client_summary: "ignored".into(),
                },
                tx,
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err, EnrollErrorCode::NotElevatedOperation, "{id}");
        let response = super::revoke_capability(
            axum::extract::State(state.clone()),
            Some(axum::extract::Extension(shell_auth.clone())),
            axum::extract::Path(id.to_owned()),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{id}");
        assert_eq!(error_code(response).await, "forbidden");
    }
    let creative_ok = build_router(state.clone())
        .oneshot(request(&creative, "GET", "/api/v1/models", "", None))
        .await
        .unwrap();
    assert_eq!(creative_ok.status(), StatusCode::OK);
    let shell_ok = build_router(state.clone())
        .oneshot(request(&shell.secret, "GET", "/api/v1/models", "", None))
        .await
        .unwrap();
    assert_eq!(shell_ok.status(), StatusCode::OK);

    let revoked = build_router(state.clone())
        .oneshot(request(
            &host,
            "POST",
            &format!("/api/v1/capabilities/{creative_id}/revoke"),
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let rejected = build_router(state)
        .oneshot(request(&creative, "GET", "/api/v1/models", "", None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
}

/// The first revoke audit write fails with 503 and the bearer is dead.
/// The retry writes one `Revoked` row. A further retry does not write again.
/// Dropping the audit-pending flag turns the second count into 0.
#[tokio::test]
async fn bridge_revoke_audit_failure_is_retried_once() {
    let (mut state, token) = setup().await;
    state.dev_wechat_events = true;
    state
        .first_party
        .as_ref()
        .unwrap()
        .set_provider(Some(Arc::new(Allow)));
    let secret = grant(&state, &token, "POST", "/api/v1/capabilities/bridge", "").await;
    let response = build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let id = body["capability_id"].as_str().unwrap().to_owned();
    let bearer = body["token"].as_str().unwrap().to_owned();
    let pool = agent24_store::test_hooks::pool(&state.store);
    sqlx::query(
        "CREATE TRIGGER c9c_block_audit BEFORE INSERT ON audit_log \
         BEGIN SELECT RAISE(ABORT, 'blocked'); END",
    )
    .execute(pool)
    .await
    .unwrap();
    let path = format!("/api/v1/capabilities/{id}/revoke");
    let secret = grant(&state, &token, "POST", &path, "").await;
    let failed = build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            &path,
            "",
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(failed.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(error_code(failed).await, "audit_unavailable");
    let rejected = build_router(state.clone())
        .oneshot(request(&bearer, "GET", "/api/v1/models", "", None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    sqlx::query("DROP TRIGGER c9c_block_audit")
        .execute(pool)
        .await
        .unwrap();
    for expected in [1_usize, 1] {
        let secret = grant(&state, &token, "POST", &path, "").await;
        let response = build_router(state.clone())
            .oneshot(request(
                &token.secret,
                "POST",
                &path,
                "",
                Some(secret.as_str()),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let revoked = state
            .store
            .list_audit()
            .await
            .unwrap()
            .into_iter()
            .filter(|entry| entry.action == "capability.token.revoked")
            .count();
        assert_eq!(revoked, expected);
    }
}

/// The dev gate off is the same 404 as the wechat POST. Nothing is issued
/// and no `capability.token.issued` row is written. A missing bearer stays
/// 401. Legacy mode with the gate on is still 409.
#[tokio::test]
async fn bridge_mint_is_404_when_the_dev_gate_is_off() {
    use crate::capabilities::Audience;

    let (state, token) = setup().await;
    assert!(!state.dev_wechat_events);
    let missing = build_router(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/capabilities/bridge")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

    let closed = build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(closed.status(), StatusCode::NOT_FOUND);
    assert_eq!(error_code(closed).await, "not_found");
    assert!(
        state
            .store
            .list_audit()
            .await
            .unwrap()
            .iter()
            .all(|entry| entry.action != "capability.token.issued")
    );
    let caps = state.capabilities.clone().unwrap();
    assert!(
        caps.shared
            .state
            .lock()
            .unwrap()
            .records
            .values()
            .all(|record| record.claims.audience != Audience::ComponentRuntime)
    );

    let mut legacy = tests::state().await;
    assert!(!legacy.dev_wechat_events);
    let dark = build_router(legacy.clone())
        .oneshot(request(
            "testtoken",
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(dark.status(), StatusCode::NOT_FOUND);
    legacy.dev_wechat_events = true;
    let open = build_router(legacy.clone())
        .oneshot(request(
            "testtoken",
            "POST",
            "/api/v1/capabilities/bridge",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(open.status(), StatusCode::CONFLICT);
    assert_eq!(error_code(open).await, "capability_mode_required");
    assert!(
        legacy
            .store
            .list_audit()
            .await
            .unwrap()
            .iter()
            .all(|entry| entry.action != "capability.token.issued")
    );
}

#[tokio::test]
async fn daily_http_and_unknown_routes_and_no_http_renewal() {
    let (state, token) = setup().await;
    let router = build_router(state);
    assert_eq!(
        router
            .clone()
            .oneshot(request(&token.secret, "GET", "/api/v1/models", "", None))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        router
            .clone()
            .oneshot(request(&token.secret, "POST", "/api/v1/renew", "", None))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        router
            .oneshot(request(&token.secret, "GET", "/api/v1/unknown", "", None))
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}
#[tokio::test]
async fn elevation_blocks_handler_and_binds_path_body_method_and_token() {
    let (state, token) = setup().await;
    let body = serde_json::json!({"manifest":"name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\n"}).to_string();
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    // Use the production auth middleware and its real fixed route mapping;
    // the counter makes handler execution independently observable.
    let router = Router::new()
        .route(
            "/api/v1/attached",
            axum::routing::post(move || {
                let c = counter.clone();
                async move {
                    c.fetch_add(1, Ordering::SeqCst);
                    StatusCode::CREATED
                }
            }),
        )
        .route(
            "/api/v1/attached/{name}",
            axum::routing::delete(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth));
    state
        .first_party
        .as_ref()
        .unwrap()
        .set_provider(Some(Arc::new(Allow)));
    let response = router
        .clone()
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/attached",
            &body,
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(response).await, "presence_required");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let secret = grant(&state, &token, "POST", "/api/v1/attached", &body).await;
    for (method, path, body) in [
        ("POST", "/api/v1/attached", "changed"),
        ("DELETE", "/api/v1/attached/other", &body),
    ] {
        let response = router
            .clone()
            .oneshot(request(&token.secret, method, path, body, Some(&secret)))
            .await
            .unwrap();
        assert_eq!(error_code(response).await, "elevation_invalid");
    }
    let other = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap();
    assert_eq!(
        error_code(
            router
                .clone()
                .oneshot(request(
                    &other.secret,
                    "POST",
                    "/api/v1/attached",
                    &body,
                    Some(&secret)
                ))
                .await
                .unwrap()
        )
        .await,
        "elevation_invalid"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let oversized = "x".repeat(agent24_domain::http::MAX_BODY_BYTES + 1);
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                &token.secret,
                "POST",
                "/api/v1/attached",
                &oversized,
                Some(&secret)
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (a, b) = tokio::join!(
        router.clone().oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/attached",
            &body,
            Some(&secret)
        )),
        router.clone().oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/attached",
            &body,
            Some(&secret)
        ))
    );
    let statuses = [a.unwrap().status(), b.unwrap().status()];
    assert_eq!(
        statuses
            .iter()
            .filter(|s| **s == StatusCode::CREATED)
            .count(),
        1
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let failed_router = Router::new()
        .route(
            "/api/v1/attached",
            axum::routing::post(|| async { StatusCode::INTERNAL_SERVER_ERROR }),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth));
    let failed = grant(&state, &token, "POST", "/api/v1/attached", &body).await;
    assert_eq!(
        failed_router
            .clone()
            .oneshot(request(
                &token.secret,
                "POST",
                "/api/v1/attached",
                &body,
                Some(&failed)
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::INTERNAL_SERVER_ERROR
    );
    assert_eq!(
        error_code(
            failed_router
                .oneshot(request(
                    &token.secret,
                    "POST",
                    "/api/v1/attached",
                    &body,
                    Some(&failed)
                ))
                .await
                .unwrap()
        )
        .await,
        "elevation_invalid"
    );
    // Renewal revokes both old token access and grants tied to that token id.
    let old_grant = grant(&state, &token, "POST", "/api/v1/attached", &body).await;
    let renewed = state
        .first_party
        .as_ref()
        .unwrap()
        .renew(&test_peer("daemon-test"), &token.token_id)
        .unwrap();
    assert_eq!(
        router
            .clone()
            .oneshot(request(
                &token.secret,
                "POST",
                "/api/v1/attached",
                &body,
                Some(&old_grant)
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        error_code(
            router
                .oneshot(request(
                    &renewed.secret,
                    "POST",
                    "/api/v1/attached",
                    &body,
                    Some(&old_grant)
                ))
                .await
                .unwrap()
        )
        .await,
        "elevation_invalid"
    );
}
#[tokio::test]
async fn module_writes_require_explicit_delegation_and_surface_changes_reject() {
    use crate::capabilities::delegation::{Delegations, ModuleSurface};
    let (mut state, token) = setup().await;
    let dir = tempfile::tempdir().unwrap();
    state.delegations = Arc::new(Delegations::open(dir.path()).unwrap());
    let surface = ModuleSurface {
        name: "probe".into(),
        version: "1".into(),
        namespace: "/api/v1/probe".into(),
        routes_sha256: body_digest(b"POST /write"),
    };
    state.module_surfaces = Arc::new(vec![surface.clone()]);
    state.module_namespaces = Arc::new(BTreeSet::from([surface.namespace.clone()]));
    let modules = Router::new().route(
        "/api/v1/probe/write",
        axum::routing::post(|| async { StatusCode::CREATED }),
    );
    let router = build_router_with_modules(state.clone(), modules.clone());
    assert_eq!(
        error_code(
            router
                .clone()
                .oneshot(request(
                    &token.secret,
                    "POST",
                    "/api/v1/probe/write",
                    "",
                    None
                ))
                .await
                .unwrap()
        )
        .await,
        "module_delegation_required"
    );
    // Legacy enabled flags and an on-disk state created by an older daemon do not authorize.
    std::fs::write(dir.path().join("os.json"), b"{}").unwrap();
    assert!(!Delegations::open(dir.path()).unwrap().permits(&surface));
    let path = "/api/v1/os/probe/delegation";
    assert_eq!(
        error_code(
            router
                .clone()
                .oneshot(request(&token.secret, "POST", path, "", None))
                .await
                .unwrap()
        )
        .await,
        "presence_required"
    );
    let secret = grant(&state, &token, "POST", path, "").await;
    assert_eq!(
        router
            .clone()
            .oneshot(request(&token.secret, "POST", path, "", Some(&secret)))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        router
            .oneshot(request(
                &token.secret,
                "POST",
                "/api/v1/probe/write",
                "",
                None
            ))
            .await
            .unwrap()
            .status(),
        StatusCode::CREATED
    );
    for changed_version in [false, true] {
        let mut changed = state.clone();
        let mut surface = surface.clone();
        if changed_version {
            surface.version = "2".into();
        } else {
            surface.routes_sha256 = body_digest(b"POST /new");
        }
        changed.module_surfaces = Arc::new(vec![surface]);
        let router = build_router_with_modules(changed, modules.clone());
        assert_eq!(
            error_code(
                router
                    .oneshot(request(
                        &token.secret,
                        "POST",
                        "/api/v1/probe/write",
                        "",
                        None
                    ))
                    .await
                    .unwrap()
            )
            .await,
            "module_delegation_required"
        );
    }
}

#[tokio::test]
async fn first_party_permissions_are_checked_at_http_boundary() {
    let (state, token) = setup().await;
    let store = state.capabilities.as_ref().unwrap();
    {
        let mut records = store.shared.state.lock().unwrap();
        let key = records.ids[&token.token_id];
        records
            .records
            .get_mut(&key)
            .unwrap()
            .claims
            .allowed_operations
            .remove(&crate::capabilities::Operation::ModelsRead);
    }
    let response = build_router(state)
        .oneshot(request(&token.secret, "GET", "/api/v1/models", "", None))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn attached_summary_uses_daemon_parsed_body_bound_to_token_and_hash() {
    struct Capture(std::sync::Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl PresenceProvider for Capture {
        async fn confirm(
            &self,
            req: PresenceRequest,
            _cancel: CancellationToken,
        ) -> PresenceOutcome {
            self.0.lock().unwrap().push(req.summary);
            PresenceOutcome::Allowed
        }
    }
    let (mut state, token) = setup().await;
    let capture = Arc::new(Capture(std::sync::Mutex::new(Vec::new())));
    state.presence_provider = Some(capture.clone());
    let router = build_router(state.clone());
    let body = serde_json::json!({"manifest":"name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\n"}).to_string();
    assert_eq!(
        error_code(
            router
                .oneshot(request(
                    &token.secret,
                    "POST",
                    "/api/v1/attached",
                    &body,
                    None
                ))
                .await
                .unwrap()
        )
        .await,
        "presence_required"
    );
    grant(&state, &token, "POST", "/api/v1/attached", &body).await;
    let other = state
        .first_party
        .as_ref()
        .unwrap()
        .issue(&test_peer("daemon-test"))
        .unwrap();
    for (id, method, path, bytes) in [
        (&token.token_id, "POST", "/api/v1/attached", "changed"),
        (&other.token_id, "POST", "/api/v1/attached", body.as_str()),
        (&token.token_id, "PUT", "/api/v1/memory/settings", ""),
        (&token.token_id, "POST", "/api/v1/os/missing/delegation", ""),
    ] {
        let (tx, _rx) = tokio::sync::oneshot::channel();
        let result = state
            .first_party
            .as_ref()
            .unwrap()
            .elevate(
                &test_peer("daemon-test"),
                id,
                ElevateRequest {
                    method: method.into(),
                    path: path.into(),
                    body_sha256: body_digest(bytes.as_bytes()),
                    client_summary: "fake".into(),
                },
                tx,
                CancellationToken::new(),
            )
            .await;
        assert_eq!(
            result.err(),
            Some(crate::enroll_api::EnrollErrorCode::NotElevatedOperation)
        );
    }
    assert_eq!(capture.0.lock().unwrap().len(), 1);
    for invalid in ["{}", r#"{"manifest":"invalid"}"#] {
        let router = build_router(state.clone());
        router
            .oneshot(request(
                &token.secret,
                "POST",
                "/api/v1/attached",
                invalid,
                None,
            ))
            .await
            .unwrap();
        let (tx, _) = tokio::sync::oneshot::channel();
        assert!(
            state
                .first_party
                .as_ref()
                .unwrap()
                .elevate(
                    &test_peer("daemon-test"),
                    &token.token_id,
                    ElevateRequest {
                        method: "POST".into(),
                        path: "/api/v1/attached".into(),
                        body_sha256: body_digest(invalid.as_bytes()),
                        client_summary: "fake".into()
                    },
                    tx,
                    CancellationToken::new()
                )
                .await
                .is_err()
        );
    }
    let mut remote: serde_json::Value = serde_json::from_str(&body).unwrap();
    remote["manifest"] = serde_json::Value::String(format!(
        "{}model_access: remote_allowed\nkernel_capabilities: [models, events]\nhost_commands: [open]\n",
        serde_json::from_str::<serde_json::Value>(&body).unwrap()["manifest"]
            .as_str()
            .unwrap()
    ));
    let remote = remote.to_string();
    build_router(state.clone())
        .oneshot(request(
            &token.secret,
            "POST",
            "/api/v1/attached",
            &remote,
            None,
        ))
        .await
        .unwrap();
    grant(&state, &token, "POST", "/api/v1/attached", &remote).await;
    let summaries = capture.0.lock().unwrap();
    assert_eq!(summaries.len(), 2);
    assert!(summaries[0].contains("'agentear'"));
    assert!(summaries[0].contains("model_access: local_only"));
    assert!(summaries[0].contains("kernel_capabilities: []"));
    assert!(summaries[0].contains("host_commands: []"));
    assert!(summaries[1].contains("model_access: remote_allowed"));
    assert!(summaries[1].contains("models") && summaries[1].contains("events"));
    assert!(summaries[1].contains("host_commands: [open]"));
    assert_ne!(summaries[0], summaries[1]);
}

#[tokio::test]
async fn product_host_module_writes_do_not_need_delegation() {
    use crate::capabilities::delegation::{Delegations, ModuleSurface};
    let (mut state, host_bearer, _) = tests::capability_state().await;
    let dir = tempfile::tempdir().unwrap();
    state.delegations = Arc::new(Delegations::open(dir.path()).unwrap());
    let surface = ModuleSurface {
        name: "probe".into(),
        version: "1".into(),
        namespace: "/api/v1/probe".into(),
        routes_sha256: body_digest(b"POST /write"),
    };
    assert!(!state.delegations.permits(&surface));
    state.module_surfaces = Arc::new(vec![surface.clone()]);
    state.module_namespaces = Arc::new(BTreeSet::from([surface.namespace.clone()]));
    let modules = Router::new().route(
        "/api/v1/probe/write",
        axum::routing::post(|| async { StatusCode::CREATED }),
    );
    let router = build_router_with_modules(state, modules);
    let response = router
        .oneshot(request(
            &host_bearer,
            "POST",
            "/api/v1/probe/write",
            "",
            None,
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CREATED);
}

/// #1447 F1, first layer: an enrolled first-party token is never granted the
/// presence operations, so the middleware refuses it before any handler.
#[tokio::test]
async fn first_party_token_is_not_granted_presence_operations() {
    let (state, token) = setup().await;
    let store = state.capabilities.as_ref().unwrap();
    for operation in [
        crate::capabilities::Operation::PresenceRead,
        crate::capabilities::Operation::PresenceDecide,
    ] {
        assert!(
            store
                .validate_bearer(
                    &token.secret,
                    operation,
                    &crate::capabilities::Resource::global(),
                    crate::capabilities::unix_now(),
                )
                .is_err(),
            "{operation:?}"
        );
    }
    // Control: the same token validates for an everyday operation.
    assert!(
        store
            .validate_bearer(
                &token.secret,
                crate::capabilities::Operation::ConfigRead,
                &crate::capabilities::Resource::global(),
                crate::capabilities::unix_now(),
            )
            .is_ok()
    );
}

/// #1447 F1, end to end: an enrolled CLI cannot read or answer challenges,
/// so it cannot approve its own elevation.
#[tokio::test]
async fn first_party_token_cannot_reach_presence_routes() {
    let (state, token) = setup().await;
    assert!(state.presence_desktop.is_some());
    let router = build_router(state);
    for (method, path, body) in [
        ("GET", "/api/v1/presence/challenges/next?wait=0", ""),
        (
            "POST",
            "/api/v1/presence/challenges/abc",
            r#"{"decision":"allow"}"#,
        ),
    ] {
        let response = router
            .clone()
            .oneshot(request(&token.secret, method, path, body, None))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{method} {path}");
    }
}

// Keep HTTP in-process and use a socket pair for the real enrolment handshake.
// Only the registry path is injected; no test writes the user's attached.json.
// macOS only: off macOS the daemon has no per-writer peer token and rejects
// every enrolment frame by design (slice §3.1), so the handshake cannot succeed.
#[cfg(target_os = "macos")]
async fn desktop_presence_flow(decision: &str) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let (state, host, _) = tests::capability_state().await;
    let dir = tempfile::tempdir().unwrap();
    let registry = dir.path().join("attached.json");
    let target = registry.clone();
    let _production = build_router(state.clone());
    let router = Router::new()
        .route(
            "/api/v1/attached",
            axum::routing::post(move |state, req| {
                let path = target.clone();
                async move { crate::attached_routes::post_attached_at(state, req, &path).await }
            }),
        )
        .route(
            "/api/v1/presence/challenges/next",
            axum::routing::get(crate::presence_desktop::get_next),
        )
        .route(
            "/api/v1/presence/challenges/{id}",
            axum::routing::post(crate::presence_desktop::post_decision),
        )
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), auth))
        .with_state(state.clone());
    let (mut client, stop) = crate::enroll_endpoint::handshake_tests::desktop_enrollment(
        state.first_party.as_ref().unwrap().clone(),
    )
    .await;
    let mut line = String::new();
    client.read_line(&mut line).await.unwrap();
    let enrolled: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(enrolled["op"], "enrolled");
    let token = enrolled["token"].as_str().unwrap();
    let body = serde_json::json!({"manifest":"name: agentear\nversion: '1'\nroute_namespace: /api/v1/agentear\nevent_module: agentear\ndata_dir: ~/.agent24/os/agentear/\nimpl_kind: attached_process\n"}).to_string();
    assert_eq!(
        error_code(
            router
                .clone()
                .oneshot(request(token, "POST", "/api/v1/attached", &body, None))
                .await
                .unwrap()
        )
        .await,
        "presence_required"
    );
    assert!(!registry.exists());
    if decision != "unavailable" {
        assert_eq!(
            router
                .clone()
                .oneshot(request(
                    &host,
                    "GET",
                    "/api/v1/presence/challenges/next?wait=0",
                    "",
                    None
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::NO_CONTENT
        );
    }
    let frame = serde_json::json!({"v":1,"op":"elevate","method":"POST","path":"/api/v1/attached","body_sha256":body_digest(body.as_bytes()),"summary":"forged client summary"});
    client
        .get_mut()
        .write_all(format!("{frame}\n").as_bytes())
        .await
        .unwrap();
    line.clear();
    client.read_line(&mut line).await.unwrap();
    let pending: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(pending["op"], "elevation_pending");
    if decision != "unavailable" {
        let poll_router = router.clone();
        let poll_host = host.clone();
        let challenge = tokio::spawn(async move {
            let response = poll_router
                .oneshot(request(
                    &poll_host,
                    "GET",
                    "/api/v1/presence/challenges/next?wait=1",
                    "",
                    None,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()
        })
        .await
        .unwrap();
        assert_eq!(challenge["challenge_id"], pending["challenge_id"]);
        assert_eq!(challenge["operation"], "module.admin");
        assert_eq!(challenge["requester"]["identifier"], "agent24");
        assert_eq!(challenge["requester"]["pid"], std::process::id());
        assert!(challenge["summary"].as_str().unwrap().contains("agentear"));
        assert!(!challenge["summary"].as_str().unwrap().contains("forged"));
        if decision == "timeout" {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(61)).await;
        } else {
            let path = format!(
                "/api/v1/presence/challenges/{}",
                challenge["challenge_id"].as_str().unwrap()
            );
            assert_eq!(
                router
                    .clone()
                    .oneshot(request(
                        &host,
                        "POST",
                        &path,
                        &serde_json::json!({"decision":decision}).to_string(),
                        None
                    ))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
    }
    line.clear();
    client.read_line(&mut line).await.unwrap();
    let result: serde_json::Value = serde_json::from_str(&line).unwrap();
    if decision == "timeout" {
        tokio::time::resume();
    }
    if decision == "allow" {
        assert_eq!(result["op"], "elevated");
        assert_eq!(result["challenge_id"], pending["challenge_id"]);
        assert_eq!(
            router
                .oneshot(request(
                    token,
                    "POST",
                    "/api/v1/attached",
                    &body,
                    Some(result["grant"].as_str().unwrap())
                ))
                .await
                .unwrap()
                .status(),
            StatusCode::CREATED
        );
        assert!(registry.exists());
    } else {
        let code = match decision {
            "deny" => "presence_denied",
            "timeout" => "presence_timeout",
            _ => "presence_unavailable",
        };
        assert_eq!(result["code"], code);
        assert!(!registry.exists());
    }
    stop.cancel();
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn desktop_enroll_elevate_attach_allowed() {
    desktop_presence_flow("allow").await;
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn desktop_enroll_elevate_attach_denied() {
    desktop_presence_flow("deny").await;
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn desktop_enroll_elevate_without_poller() {
    desktop_presence_flow("unavailable").await;
}
#[cfg(target_os = "macos")]
#[tokio::test]
async fn desktop_enroll_elevate_presence_timeout() {
    desktop_presence_flow("timeout").await;
}

#[tokio::test]
async fn legacy_and_service_state_have_no_desktop_presence() {
    let mut state = tests::state().await;
    assert!(state.presence_provider.is_none());
    assert!(state.presence_desktop.is_none());
    state.enable_capability_auth(crate::capabilities::CapabilityStore::new("service-test"));
    assert!(state.presence_provider.is_none());
    assert!(state.presence_desktop.is_none());
}

/// K-B3 #1715: service mode confirms on the presence connection and has no
/// HTTP challenge routes; a host child keeps HTTP; legacy has neither.
#[tokio::test]
async fn each_start_mode_gets_its_own_presence_channel() {
    use crate::server::StartupMode;
    let provider_is = |state: &AppState, other: *const ()| {
        state
            .presence_provider
            .as_ref()
            .is_some_and(|p| std::ptr::eq(Arc::as_ptr(p).cast::<()>(), other))
    };
    let mut service = tests::state().await;
    service.enable_capability_auth(crate::capabilities::CapabilityStore::new("service-test"));
    service.enable_presence_for(StartupMode::Service);
    let queue = service.presence_service.clone().unwrap();
    assert!(provider_is(&service, Arc::as_ptr(&queue).cast()));
    assert!(service.presence_desktop.is_none());
    // Building the router hands the same queue to the enrolment endpoint's
    // authority, which is where `role:"presence"` connections attach.
    let authority = service.first_party.clone().unwrap();
    assert!(authority.presence_connection().is_none());
    let _router = build_router(service);
    let attached = authority.presence_connection().unwrap();
    assert!(Arc::ptr_eq(&attached, &queue));

    let mut child = tests::state().await;
    child.enable_capability_auth(crate::capabilities::CapabilityStore::new("child-test"));
    child.enable_presence_for(StartupMode::HostChild);
    let desktop = child.presence_desktop.clone().unwrap();
    assert!(provider_is(&child, Arc::as_ptr(&desktop).cast()));
    assert!(child.presence_service.is_none());
    let authority = child.first_party.clone().unwrap();
    let _router = build_router(child);
    assert!(authority.presence_connection().is_none());

    let mut legacy = tests::state().await;
    legacy.enable_presence_for(StartupMode::Legacy);
    assert!(legacy.presence_provider.is_none());
    assert!(legacy.presence_desktop.is_none());
    assert!(legacy.presence_service.is_none());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn http_rejects_and_deletes_token_after_peer_exec() {
    use tokio::io::AsyncWriteExt;
    let dir = tempfile::tempdir_in("/tmp").unwrap();
    let socket = dir.path().join("peer.sock");
    let listener = tokio::net::UnixListener::bind(&socket).unwrap();
    let mut child = tokio::process::Command::new("python3")
        .args(["-c", include_str!("fixtures/exec_peer.py")])
        .arg(&socket)
        .arg("plain")
        .stdin(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let (state, _) = setup().await;
    let mut peer = test_peer("daemon-test");
    peer.pid = child.id().unwrap() as i32;
    let start = agent24_os_proc::start_time(peer.pid).unwrap();
    peer.start = crate::enroll_api::ProcStart {
        sec: start.sec,
        usec: start.usec,
    };
    let audit_token = agent24_os_proc::peer_audit_token(&stream).unwrap();
    peer.uid = audit_token.euid();
    peer.identity.audit_token = Some(audit_token);
    let token = state.first_party.as_ref().unwrap().issue(&peer).unwrap();
    let store = state.capabilities.as_ref().unwrap().clone();
    let router = build_router(state);
    assert_eq!(
        router
            .clone()
            .oneshot(request(&token.secret, "GET", "/api/v1/models", "", None))
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    child.stdin.take().unwrap().write_all(b"e").await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while agent24_os_proc::executable_path(peer.pid)
            .unwrap()
            .canonicalize()
            .unwrap()
            != std::path::Path::new("/bin/sleep").canonicalize().unwrap()
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(agent24_os_proc::start_time(peer.pid).unwrap(), start);
    assert_eq!(
        router
            .oneshot(request(&token.secret, "GET", "/api/v1/models", "", None))
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert!(
        !store
            .shared
            .state
            .lock()
            .unwrap()
            .ids
            .contains_key(&token.token_id)
    );
    drop(stream);
}

/// I-1 L2: the HTTP rejection of an elevated iDoris install or connect is
/// what records the daemon's summary; without it nothing can be approved.
#[tokio::test]
async fn idoris_install_and_connect_record_their_summary_at_the_http_boundary() {
    struct Capture(std::sync::Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl PresenceProvider for Capture {
        async fn confirm(
            &self,
            req: PresenceRequest,
            _cancel: CancellationToken,
        ) -> PresenceOutcome {
            self.0.lock().unwrap().push(req.summary);
            PresenceOutcome::Allowed
        }
    }
    let connect = serde_json::json!({
        "endpoint": "http://127.0.0.1:8088", "daily_model": "daily-real",
        "consent": {"share_omlx_key": true, "daily_model": "daily-real"}
    })
    .to_string();
    for (path, body, expected) in [
        (
            "/api/v1/idoris/install",
            r#"{"consent":true}"#.to_owned(),
            "install the locked iDoris release",
        ),
        (
            "/api/v1/idoris/connect",
            connect,
            "daily model: \"daily-real\"",
        ),
    ] {
        let (mut state, token) = setup().await;
        let capture = Arc::new(Capture(std::sync::Mutex::new(Vec::new())));
        state.presence_provider = Some(capture.clone());
        let router = build_router(state.clone());
        let rejected = router
            .clone()
            .oneshot(request(&token.secret, "POST", path, &body, None))
            .await
            .unwrap();
        assert_eq!(rejected.status(), StatusCode::FORBIDDEN, "{path}");
        assert_eq!(error_code(rejected).await, "presence_required", "{path}");
        let secret = grant(&state, &token, "POST", path, &body).await;
        let summaries = capture.0.lock().unwrap().clone();
        assert_eq!(summaries.len(), 1, "{path}");
        assert!(summaries[0].contains(expected), "{path}: {}", summaries[0]);
        // The approved request passes the gate (the handler answers).
        let approved = router
            .oneshot(request(&token.secret, "POST", path, &body, Some(&secret)))
            .await
            .unwrap();
        assert_ne!(approved.status(), StatusCode::FORBIDDEN, "{path}");
    }
}

/// I-1 B1: a grant binds method, path and body. A query string changes what
/// `/candidates` does (`endpoint=` probes another port with the oMLX key),
/// so a granted request carrying one is refused and the grant survives.
#[tokio::test]
async fn elevation_grant_refuses_a_query_string() {
    let (state, token) = setup().await;
    let router = build_router(state.clone());
    let path = "/api/v1/idoris/omlx/candidates";
    let secret = grant(&state, &token, "GET", path, "").await;
    let get = |uri: &str| {
        Request::builder()
            .method("GET")
            .uri(uri)
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", token.secret.as_str()),
            )
            .header("X-Agent24-Elevation", secret.as_str())
            .body(Body::empty())
            .unwrap()
    };
    let swapped = router
        .clone()
        .oneshot(get(&format!("{path}?endpoint=http://127.0.0.1:9")))
        .await
        .unwrap();
    assert_eq!(swapped.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(swapped).await, "elevation_invalid");
    let bare = router.oneshot(get(path)).await.unwrap();
    assert_ne!(bare.status(), StatusCode::FORBIDDEN);
}

async fn seed_skill_pair(state: &AppState) -> (String, String) {
    use agent24_store::{NewSkillRevision, SkillOrigin, SkillRevisionInsert, SkillRevisionKind};
    let body = "---\nname: demo\nversion: 1.0.0\ndescription: d\nrequires: []\nevals: evals/c.jsonl\n---\nhello world\n";
    let evals = "{\"input\":\"i\",\"expect\":\"hello world\",\"judge\":\"contains\"}\n";
    let mut ids = Vec::new();
    for (kind, text) in [
        (SkillRevisionKind::Body, body),
        (SkillRevisionKind::Evals, evals),
    ] {
        let inserted = state
            .store
            .insert_skill_revision(&NewSkillRevision {
                revision_id: format!("rev-{}", kind.as_str()),
                skill_id: "demo".to_owned(),
                origin: SkillOrigin::Imported,
                semver: "1.0.0".to_owned(),
                kind,
                content_digest: agent24_skill::sha256_hex(text),
                content: text.as_bytes().to_vec(),
                created_at: "2026-11-12T00:00:00Z".to_owned(),
            })
            .await
            .unwrap();
        let (SkillRevisionInsert::Inserted { revision_id }
        | SkillRevisionInsert::Existing { revision_id }) = inserted;
        ids.push(revision_id);
    }
    (ids[0].clone(), ids[1].clone())
}

fn activate_body(body: &str, evals: &str) -> String {
    serde_json::json!({
        "expected_generation": 0, "body_revision_id": body, "evals_revision_id": evals
    })
    .to_string()
}

/// SL-4: a daily CLI token cannot activate without a grant, the dialog text
/// is built from the stored revisions, and the grant binds the exact body.
/// Dropping `activate` from `resolve_target`'s supported set turns the
/// `grant` call below into `NotElevatedOperation`.
#[tokio::test]
async fn skill_activation_needs_elevation_and_shows_the_stored_summary() {
    struct Capture(std::sync::Mutex<Vec<String>>);
    #[async_trait::async_trait]
    impl PresenceProvider for Capture {
        async fn confirm(
            &self,
            req: PresenceRequest,
            _cancel: CancellationToken,
        ) -> PresenceOutcome {
            self.0.lock().unwrap().push(req.summary);
            PresenceOutcome::Allowed
        }
    }
    let (mut state, token) = setup().await;
    let capture = Arc::new(Capture(std::sync::Mutex::new(Vec::new())));
    state.presence_provider = Some(capture.clone());
    let (body, evals) = seed_skill_pair(&state).await;
    let path = "/api/v1/skills/demo/activate";
    let request_body = activate_body(&body, &evals);
    let router = build_router(state.clone());
    let rejected = router
        .clone()
        .oneshot(request(&token.secret, "POST", path, &request_body, None))
        .await
        .unwrap();
    assert_eq!(rejected.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(rejected).await, "presence_required");
    assert!(
        state
            .store
            .get_skill_pointer("demo")
            .await
            .unwrap()
            .is_none(),
        "no grant, no move"
    );

    let secret = grant(&state, &token, "POST", path, &request_body).await;
    let summaries = capture.0.lock().unwrap().clone();
    assert_eq!(summaries.len(), 1);
    let digest = agent24_skill::sha256_hex(
        "---\nname: demo\nversion: 1.0.0\ndescription: d\nrequires: []\nevals: evals/c.jsonl\n---\nhello world\n",
    );
    assert!(summaries[0].contains("\"demo\""), "{}", summaries[0]);
    assert!(summaries[0].contains("\"1.0.0\""), "{}", summaries[0]);
    assert!(summaries[0].contains(&digest[..8]), "{}", summaries[0]);

    // The grant is for these bytes only.
    let other = activate_body(&body, &body);
    let swapped = router
        .clone()
        .oneshot(request(
            &token.secret,
            "POST",
            path,
            &other,
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(swapped.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(swapped).await, "elevation_invalid");

    let approved = router
        .oneshot(request(
            &token.secret,
            "POST",
            path,
            &request_body,
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);
    assert_eq!(
        state
            .store
            .get_skill_pointer("demo")
            .await
            .unwrap()
            .unwrap()
            .generation,
        1
    );
}

async fn seed_rev(
    state: &AppState,
    origin: agent24_store::SkillOrigin,
    kind: agent24_store::SkillRevisionKind,
    semver: &str,
    text: &str,
) -> String {
    use agent24_store::{NewSkillRevision, SkillRevisionInsert};
    let inserted = state
        .store
        .insert_skill_revision(&NewSkillRevision {
            revision_id: uuid::Uuid::new_v4().to_string(),
            skill_id: "demo".to_owned(),
            origin,
            semver: semver.to_owned(),
            kind,
            content_digest: agent24_skill::sha256_hex(text),
            content: text.as_bytes().to_vec(),
            created_at: "2026-11-12T00:00:00Z".to_owned(),
        })
        .await
        .unwrap();
    let (SkillRevisionInsert::Inserted { revision_id }
    | SkillRevisionInsert::Existing { revision_id }) = inserted;
    revision_id
}

/// Round 2: a first-party shell gets the plan's business errors straight
/// away, with no elevation, and only a request that would really move the
/// pointer is told `presence_required`. Mutation: make the middleware answer
/// `None` for failures again (`.ok()?`) and the first assertions go red.
#[tokio::test]
async fn skill_activation_business_failures_answer_before_elevation() {
    use agent24_store::{SkillOrigin as O, SkillRevisionKind as K};
    let (state, token) = setup().await;
    let md = |v: &str, t: &str| {
        format!(
            "---\nname: demo\nversion: {v}\ndescription: d\nrequires: []\nevals: evals/c.jsonl\n---\n{t}\n"
        )
    };
    let ev = |e: &str| format!("{{\"input\":\"i\",\"expect\":\"{e}\",\"judge\":\"contains\"}}\n");
    let b1 = seed_rev(
        &state,
        O::Imported,
        K::Body,
        "1.0.0",
        &md("1.0.0", "hello world"),
    )
    .await;
    let e1 = seed_rev(&state, O::Imported, K::Evals, "1.0.0", &ev("hello world")).await;
    let b2 = seed_rev(
        &state,
        O::Imported,
        K::Body,
        "2.0.0",
        &md("2.0.0", "hello world two"),
    )
    .await;
    let e2 = seed_rev(
        &state,
        O::Imported,
        K::Evals,
        "2.0.0",
        &ev("hello world two"),
    )
    .await;
    let bad = seed_rev(&state, O::Imported, K::Evals, "1.5.0", &ev("absent")).await;
    let ue = seed_rev(&state, O::User, K::Evals, "1.0.0", &ev("hello world")).await;
    let path = "/api/v1/skills/demo/activate";
    let router = build_router(state.clone());
    let send = |body: serde_json::Value| {
        let router = router.clone();
        let secret = token.secret.clone();
        async move {
            let response = router
                .oneshot(request(&secret, "POST", path, &body.to_string(), None))
                .await
                .unwrap();
            let status = response.status();
            let bytes = response.into_body().collect().await.unwrap().to_bytes();
            (
                status,
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            )
        }
    };
    let pair = |generation: i64, b: &str, e: &str| serde_json::json!({"expected_generation": generation, "body_revision_id": b, "evals_revision_id": e});

    // Nothing is pinned yet.
    let (status, v) = send(pair(0, "no-such", &e1)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::NOT_FOUND, Some("skill_revision_not_found")),
        "{v}"
    );
    let (status, v) = send(pair(0, &b1, &bad)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("skill_eval_failed")),
        "{v}"
    );
    assert_eq!(
        v["error"]["details"]["cases"].as_array().unwrap().len(),
        1,
        "case summary"
    );
    let (status, v) = send(serde_json::json!({"expected_generation": 0})).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("invalid_request")),
        "{v}"
    );
    // First activation of a clean pair moves the pointer, so it needs presence.
    let (status, v) = send(pair(0, &b1, &e1)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("presence_required")),
        "{v}"
    );
    // Pin it as the host would, then retry through the shell.
    let secret = grant(&state, &token, "POST", path, &pair(0, &b1, &e1).to_string()).await;
    let approved = router
        .clone()
        .oneshot(request(
            &token.secret,
            "POST",
            path,
            &pair(0, &b1, &e1).to_string(),
            Some(secret.as_str()),
        ))
        .await
        .unwrap();
    assert_eq!(approved.status(), StatusCode::OK);

    let (status, v) = send(pair(0, &b2, &e1)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::CONFLICT, Some("skill_pointer_conflict")),
        "{v}"
    );
    let (status, v) = send(pair(1, &b2, &e2)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("skill_activation_mixed")),
        "{v}"
    );
    let (status, v) = send(pair(1, &b1, &ue)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::BAD_REQUEST, Some("skill_origin_mismatch")),
        "{v}"
    );
    // The idempotent retry succeeds with no elevation, even on a stale generation.
    for generation in [1, 0] {
        let (status, v) = send(pair(generation, &b1, &e1)).await;
        assert_eq!(status, StatusCode::OK, "{v}");
        assert_eq!(v["unchanged"], true);
        assert_eq!(v["generation"], 1);
    }
    let history = state.store.list_skill_activations("demo").await.unwrap();
    assert_eq!(history.len(), 1, "no history from the retries");
    assert_eq!(
        state
            .store
            .get_skill_pointer("demo")
            .await
            .unwrap()
            .unwrap()
            .generation,
        1
    );
    // A request that would really move the pointer is the one that asks.
    let (status, v) = send(pair(1, &b2, &e1)).await;
    assert_eq!(
        (status, v["error"]["code"].as_str()),
        (StatusCode::FORBIDDEN, Some("presence_required")),
        "{v}"
    );
}

/// The legacy shared token cannot be elevated, so it is refused outright. The
/// host token (the desktop main process) is the one that may call directly,
/// and a Creative token never can.
#[tokio::test]
async fn skill_activation_refuses_legacy_and_creative_tokens_and_admits_the_host() {
    let path = "/api/v1/skills/demo/activate";
    let legacy = tests::state().await;
    let (body, evals) = seed_skill_pair(&legacy).await;
    let request_body = activate_body(&body, &evals);
    let refused = build_router(legacy.clone())
        .oneshot(request("testtoken", "POST", path, &request_body, None))
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
    assert_eq!(error_code(refused).await, "forbidden");
    assert!(
        legacy
            .store
            .get_skill_pointer("demo")
            .await
            .unwrap()
            .is_none()
    );

    let (state, host, creative) = tests::capability_state().await;
    let (body, evals) = seed_skill_pair(&state).await;
    let request_body = activate_body(&body, &evals);
    let router = build_router(state.clone());
    let denied = router
        .clone()
        .oneshot(request(&creative, "POST", path, &request_body, None))
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::FORBIDDEN);
    assert!(
        state
            .store
            .get_skill_pointer("demo")
            .await
            .unwrap()
            .is_none()
    );
    let admitted = router
        .oneshot(request(&host, "POST", path, &request_body, None))
        .await
        .unwrap();
    assert_eq!(admitted.status(), StatusCode::OK);
    assert_eq!(
        state
            .store
            .get_skill_pointer("demo")
            .await
            .unwrap()
            .unwrap()
            .generation,
        1
    );
}

/// Round 3. The token is checked again after the body is read: a slow upload
/// must not get pointer or eval state once the token has been revoked.
/// Mutation: remove the `validate_bearer` re-check in the activate pre-check
/// branch and this goes red (the answer becomes a business response).
#[tokio::test]
async fn skill_activation_precheck_rechecks_the_token_after_a_slow_body() {
    let (state, token) = setup().await;
    let (body, evals) = seed_skill_pair(&state).await;
    let path = "/api/v1/skills/demo/activate";
    let full = activate_body(&body, &evals).into_bytes();
    let (first, rest) = full.split_at(10);
    let (tx, rx) = tokio::sync::mpsc::channel::<axum::body::Bytes>(4);
    let stream = futures::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|chunk| (Ok::<_, std::io::Error>(chunk), rx))
    });
    let req = Request::builder()
        .method("POST")
        .uri(path)
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", token.secret.as_str()),
        )
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stream))
        .unwrap();
    let router = build_router(state.clone());
    let pending = tokio::spawn(async move { router.oneshot(req).await.unwrap() });
    tx.send(axum::body::Bytes::copy_from_slice(first))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    state
        .first_party
        .as_ref()
        .unwrap()
        .revoke(&test_peer("daemon-test"), &token.token_id);
    tx.send(axum::body::Bytes::copy_from_slice(rest))
        .await
        .unwrap();
    drop(tx);
    let response = pending.await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["error"]["code"], "unauthorized");
    assert!(
        value["error"].get("details").is_none(),
        "no state in the answer: {value}"
    );
    assert!(!value.to_string().contains(&body));
}

/// Round 3. An over-limit body is `400 invalid_request` on both paths: the
/// first-party pre-check in the middleware and the handler itself (host).
#[tokio::test]
async fn skill_activation_oversized_body_is_invalid_request_on_both_paths() {
    let path = "/api/v1/skills/demo/activate";
    let huge = "x".repeat(2 * 1024 * 1024);
    let (state, token) = setup().await;
    let via_middleware = build_router(state.clone())
        .oneshot(request(&token.secret, "POST", path, &huge, None))
        .await
        .unwrap();
    assert_eq!(via_middleware.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(via_middleware).await, "invalid_request");
    let (state, host, _) = tests::capability_state().await;
    let via_handler = build_router(state)
        .oneshot(request(&host, "POST", path, &huge, None))
        .await
        .unwrap();
    assert_eq!(via_handler.status(), StatusCode::BAD_REQUEST);
    assert_eq!(error_code(via_handler).await, "invalid_request");
}
