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
