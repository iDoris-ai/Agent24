#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use agent24_knowledge::host::{
    KnowledgeRefusal, MAX_CREDENTIAL_BYTES, MAX_KNOWLEDGE_BASES, RunBinding,
};

use crate::capabilities::unix_now;

use super::*;
use crate::capabilities::first_party::{body_digest, test_peer_as};
use crate::capabilities::presence::{PresenceOutcome, PresenceProvider, PresenceRequest};
use crate::enroll_api::{
    ElevateRequest, EnrollAuthority, EnrollErrorCode, EnrolledPeer, IssuedToken,
};
use crate::server::build_router;

fn pin(host: &str, binding: &str, generation: u64) -> RunBinding {
    RunBinding {
        host: host.to_owned(),
        binding: binding.to_owned(),
        generation,
    }
}

#[test]
fn the_routes_map_to_a_host_only_operation() {
    use crate::capabilities::Operation;
    for method in [Method::PUT, Method::DELETE] {
        for path in [
            "/api/v1/host/knowledge/bindings/{binding_id}",
            "/api/v1/host/knowledge/bindings/{binding_id}/credential",
        ] {
            // The fixed route table keys on axum's matched template.
            let operation = crate::capabilities::required_operation(&method, path);
            assert_eq!(operation, Some(Operation::KnowledgeHost), "{method} {path}");
        }
    }
    assert!(!Operation::creative_allowlist().contains(&Operation::KnowledgeHost));
    assert_eq!(
        Operation::from_action("knowledge.host"),
        Some(Operation::KnowledgeHost)
    );
}

#[tokio::test]
async fn the_handler_itself_refuses_any_audience_but_the_product_host() {
    let (state, host, creative) = crate::server::tests::capability_state().await;
    let store = state.capabilities.clone().unwrap();
    let validate = |bearer: &str| {
        let operation = crate::capabilities::Operation::ModelsRead;
        let resource = crate::capabilities::Resource::global();
        let now = crate::capabilities::unix_now();
        Extension(
            store
                .validate_bearer(bearer, operation, &resource, now)
                .unwrap(),
        )
    };
    assert!(host_capability(&state, Some(validate(&host))).is_some());
    assert_eq!(host_capability(&state, Some(validate(&creative))), None);
    assert_eq!(host_capability(&state, None), None);
    // Legacy mode never attaches an authorization; if one were there, the
    // mode alone still refuses it.
    let legacy = crate::server::tests::state().await;
    assert_eq!(host_capability(&legacy, Some(validate(&host))), None);
}

fn bearer(value: &str) -> Credential {
    Credential::Bearer(value.to_owned())
}

// ---- over HTTP, through the kernel router and its auth layer ----

const BINDING: &str = "/api/v1/host/knowledge/bindings/b1";

fn request(method: Method, path: &str, bearer: &str, seq: Option<u64>) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"));
    let body = match seq {
        Some(seq) => {
            builder = builder.header(header::CONTENT_TYPE, "application/json");
            Body::from(serde_json::json!({ "seq": seq }).to_string())
        }
        None => Body::empty(),
    };
    builder.body(body).unwrap()
}

async fn send(router: &axum::Router, req: Request<Body>) -> (StatusCode, String) {
    let res = router.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(bytes.to_vec()).unwrap())
}

const CREDENTIAL: &str = "/api/v1/host/knowledge/bindings/b1/credential";

fn every_route() -> [(Method, &'static str); 4] {
    [
        (Method::PUT, BINDING),
        (Method::DELETE, BINDING),
        (Method::PUT, CREDENTIAL),
        (Method::DELETE, CREDENTIAL),
    ]
}

fn with_body(method: Method, path: &str, bearer: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn credential_body(value: &str, lifetime: u64) -> String {
    serde_json::json!({ "kind": "bearer", "value": value, "expires_at": unix_now() + lifetime })
        .to_string()
}

#[tokio::test]
async fn the_host_pushes_and_revokes_a_credential() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    let put = |seq: u64, body: String| {
        let mut value: serde_json::Value = serde_json::from_str(&body).unwrap();
        value
            .as_object_mut()
            .unwrap()
            .insert("seq".into(), serde_json::json!(seq));
        with_body(Method::PUT, CREDENTIAL, &host, &value.to_string())
    };

    // Not active yet.
    let (status, body) = send(&router, put(1, credential_body("TOKEN-1", 600))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("binding_inactive"));

    send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    let (status, body) = send(&router, put(2, credential_body("TOKEN-2", 600))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    let host_id = registry.only_host();
    let current = registry.credential(&host_id, "b1", unix_now()).unwrap();
    assert_eq!((current.credential, current.seq), (bearer("TOKEN-2"), 2));

    let api_key =
        serde_json::json!({ "kind": "api_key", "value": "KEY-3", "expires_at": unix_now() + 600 });
    let (status, _) = send(&router, put(3, api_key.to_string())).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        registry
            .credential(&host_id, "b1", unix_now())
            .unwrap()
            .credential,
        Credential::ApiKey("KEY-3".into())
    );

    let (status, body) = send(&router, request(Method::DELETE, CREDENTIAL, &host, Some(4))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    assert_eq!(registry.credential(&host_id, "b1", unix_now()), None);
    assert!(registry.is_active(&host_id, "b1"));

    // The longest C there may be.
    let longest = "x".repeat(MAX_CREDENTIAL_BYTES);
    let (status, _) = send(&router, put(5, credential_body(&longest, 600))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        registry
            .credential(&host_id, "b1", unix_now())
            .unwrap()
            .credential,
        bearer(&longest)
    );
}

#[tokio::test]
async fn a_malformed_credential_push_changes_nothing_and_echoes_nothing() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    let host_id = registry.only_host();
    let now = unix_now();
    let body = |kind: &str, value: &str, expires_at: u64| {
        serde_json::json!({ "kind": kind, "value": value, "expires_at": expires_at }).to_string()
    };
    let too_long = format!("SECRET{}", "x".repeat(MAX_CREDENTIAL_BYTES));
    let seq_body = |seq: u64, kind: &str, value: &str, expires_at: u64| {
        serde_json::json!({ "seq": seq, "kind": kind, "value": value, "expires_at": expires_at })
            .to_string()
    };
    for payload in [
        body("bearer", "SECRET", now + 60),
        seq_body(0, "bearer", "SECRET", now + 60),
        "{\"kind\":\"bearer\",\"value\":\"SECRET\"".to_owned(),
        seq_body(2, "cookie", "SECRET", now + 60),
        // serde's own messages would quote these: "unknown variant `SECRET`",
        // "invalid type: sequence" next to the value.
        seq_body(2, "SECRET", "x", now + 60),
        serde_json::json!({ "seq": 2, "kind": "bearer", "value": ["SECRET"], "expires_at": now + 60 })
            .to_string(),
        seq_body(2, "bearer", "", now + 60),
        seq_body(2, "bearer", "SECRET WITH SPACE", now + 60),
        seq_body(2, "bearer", "SECRET\u{e9}", now + 60),
        seq_body(2, "bearer", &too_long, now + 60),
        seq_body(2, "bearer", "SECRET", now),
        serde_json::json!({ "seq": 2, "kind": "bearer", "value": "SECRET", "expires_at": now + 60, "extra": 9 })
            .to_string(),
    ] {
        let req = with_body(Method::PUT, CREDENTIAL, &host, &payload);
        let (status, response) = send(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", &payload[..payload.len().min(80)]);
        assert!(response.contains("invalid_request"));
        assert!(!response.contains("SECRET"), "{response}");
    }
    assert_eq!(registry.credential(&host_id, "b1", now), None);
    assert_eq!(registry.record(&host_id, "b1"), Some((true, 1)));
}

#[tokio::test]
async fn a_credential_push_over_the_kernel_body_cap_is_refused() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    let host_id = registry.only_host();
    // A short credential padded with whitespace: valid JSON, over 1 MiB.
    let padding = " ".repeat(agent24_domain::http::MAX_BODY_BYTES);
    let payload = format!(
        "{}{padding}",
        serde_json::json!({ "seq": 2, "kind": "bearer", "value": "TOKEN", "expires_at": unix_now() + 600 })
    );
    let req = with_body(Method::PUT, CREDENTIAL, &host, &payload);
    let (status, body) = send(&router, req).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(body.contains("payload_too_large"), "{body}");
    assert_eq!(registry.credential(&host_id, "b1", unix_now()), None);
    assert_eq!(registry.record(&host_id, "b1"), Some((true, 1)));
}

#[tokio::test]
async fn the_host_activates_and_deactivates_a_binding() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);

    let (status, body) = send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    let host_id = registry.only_host();
    assert!(registry.is_active(&host_id, "b1"));

    let (status, _) = send(&router, request(Method::DELETE, BINDING, &host, Some(2))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(registry.record(&host_id, "b1"), Some((false, 2)));

    let (status, body) = send(&router, request(Method::PUT, BINDING, &host, Some(2))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("stale_sequence"));
    assert!(!registry.is_active(&host_id, "b1"));
}

#[tokio::test]
async fn an_activation_sets_the_endpoint_its_calls_use_and_a_bad_one_changes_nothing() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    let put = |seq: u64, mut body: serde_json::Value| {
        body.as_object_mut()
            .unwrap()
            .insert("seq".into(), serde_json::json!(seq));
        with_body(Method::PUT, BINDING, &host, &body.to_string())
    };
    let good = serde_json::json!({ "endpoint": "https://kb.example/api/v1",
                                   "knowledge_base_ids": ["kb-a", "kb-b"] });
    let (status, _) = send(&router, put(1, good)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let host_id = registry.only_host();
    let now = registry.binding_now(&host_id, "b1", unix_now()).unwrap();
    assert_eq!(now.generation, 1);
    assert_eq!(
        now.endpoint,
        Some(Endpoint {
            base_url: "https://kb.example/api/v1".into(),
            knowledge_base_ids: vec!["kb-a".into(), "kb-b".into()],
        })
    );

    let too_many: Vec<String> = (0..=MAX_KNOWLEDGE_BASES)
        .map(|i| format!("kb-{i}"))
        .collect();
    for (i, bad) in [
        serde_json::json!({ "endpoint": "http://kb.example/api/v1", "knowledge_base_ids": ["kb-a"] }),
        serde_json::json!({ "endpoint": "https://u:p@kb.example", "knowledge_base_ids": ["kb-a"] }),
        serde_json::json!({ "endpoint": "https://kb.example", "knowledge_base_ids": [] }),
        serde_json::json!({ "endpoint": "https://kb.example", "knowledge_base_ids": [" "] }),
        serde_json::json!({ "endpoint": "https://kb.example", "knowledge_base_ids": ["a", "a"] }),
        serde_json::json!({ "endpoint": "https://kb.example", "knowledge_base_ids": too_many }),
        serde_json::json!({ "endpoint": "https://kb.example", "knowledge_base_ids": ["a"], "x": 1 }),
        serde_json::json!({ "knowledge_base_ids": ["a"] }),
    ]
    .into_iter()
    .enumerate()
    {
        let (status, body) = send(&router, put(2 + i as u64, bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "case {i}");
        assert!(!body.contains("kb.example"), "{body}");
    }
    // Refused before the registry: still generation 1, endpoint as it was.
    let now = registry.binding_now(&host_id, "b1", unix_now()).unwrap();
    assert_eq!((now.generation, now.endpoint.is_some()), (1, true));

    // Loopback http is allowed; a deactivation drops the endpoint with C.
    let local = serde_json::json!({ "endpoint": "http://127.0.0.1:8080/api/v1",
                                    "knowledge_base_ids": ["kb-a"] });
    assert_eq!(
        send(&router, put(20, local)).await.0,
        StatusCode::NO_CONTENT
    );
    send(&router, request(Method::DELETE, BINDING, &host, Some(21))).await;
    assert_eq!(registry.binding_now(&host_id, "b1", unix_now()), None);
}

#[tokio::test]
async fn a_creative_runtime_is_refused() {
    let (state, _, creative) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    for (method, path) in every_route() {
        // The auth layer already refuses it (`knowledge.host` is not in the
        // Creative allow-list); the handler's audience check is the backstop.
        let req = with_body(method, path, &creative, &credential_body("t", 60));
        let (status, _) = send(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
    }
    assert!(registry.is_empty());
}

#[tokio::test]
async fn the_legacy_shared_token_is_refused() {
    let state = crate::server::tests::state().await;
    let token = state.token.to_string();
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    for (method, path) in every_route() {
        let req = with_body(method, path, &token, &credential_body("t", 60));
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert!(body.contains("host_authority_required"));
    }
    // Refused before the body is read: neither a malformed nor an oversized
    // one gets further than the host check.
    let oversized = " ".repeat(agent24_domain::http::MAX_BODY_BYTES + 1);
    for payload in ["not json", oversized.as_str()] {
        let req = with_body(Method::PUT, CREDENTIAL, &token, payload);
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("host_authority_required"));
    }
    assert!(registry.is_empty());
}

#[tokio::test]
async fn a_malformed_request_changes_nothing() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    let long_id = "x".repeat(MAX_BINDING_ID_BYTES + 1);
    let seq = r#"{"seq":1}"#;
    for path in [
        "/api/v1/host/knowledge/bindings/.",
        "/api/v1/host/knowledge/bindings/..",
        "/api/v1/host/knowledge/bindings/%2E%2E",
        "/api/v1/host/knowledge/bindings/%2E/credential",
        &format!("/api/v1/host/knowledge/bindings/{long_id}"),
        "/api/v1/host/knowledge/bindings/b%01",
        "/api/v1/host/knowledge/bindings/..%2F..%2Fx",
        "/api/v1/host/knowledge/bindings/a%5Cb",
        // Not UTF-8 once decoded.
        "/api/v1/host/knowledge/bindings/%C0%AF",
    ] {
        let req = with_body(Method::PUT, path, &host, seq);
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(body.contains("invalid_request"));
    }
    // Empty, not JSON, a missing or zero sequence.
    for payload in [
        "",
        "not-json",
        "{}",
        r#"{"seq":0}"#,
        r#"{"seq":-1}"#,
        r#"{"seq":"x"}"#,
    ] {
        let req = with_body(Method::PUT, BINDING, &host, payload);
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{payload}");
        assert!(body.contains("invalid_request"));
    }
    // A field the route does not take, on each route that takes only `seq`,
    // and on the full activation (the credential push has its own test).
    for (method, path, payload) in [
        (Method::PUT, BINDING, r#"{"seq":1,"x":1}"#),
        (
            Method::PUT,
            BINDING,
            r#"{"seq":1,"endpoint":"https://kb.example/api/v1","knowledge_base_ids":["kb-a"],"x":1}"#,
        ),
        (Method::DELETE, BINDING, r#"{"seq":1,"x":1}"#),
        (Method::DELETE, CREDENTIAL, r#"{"seq":1,"x":1}"#),
    ] {
        let req = with_body(method.clone(), path, &host, payload);
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{method} {path} {payload}");
        assert!(body.contains("invalid_request"));
    }
    assert!(registry.is_empty());
}

fn create_run_request(bearer: &str, body: serde_json::Value) -> Request<Body> {
    with_body(Method::POST, "/api/v1/runs", bearer, &body.to_string())
}

async fn run_count(state: &AppState) -> usize {
    state.store.list_runs(None).await.unwrap().len()
}

#[tokio::test]
async fn the_host_creates_a_run_pinned_to_a_binding_it_activated() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state.clone());
    send(&router, request(Method::PUT, BINDING, &host, Some(7))).await;
    let host_id = registry.only_host();

    let (status, body) = send(
        &router,
        create_run_request(
            &host,
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let run: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(run["input"]["knowledge_binding_id"], "b1");
    let run_id = run["id"].as_str().unwrap();
    assert_eq!(
        registry.resolve_run(run_id, Some("b1")),
        Ok(pin(&host_id, "b1", 7))
    );
    // The selector is in the stored run, where a tool reads it.
    let stored = state.store.get_run(run_id).await.unwrap().unwrap();
    assert_eq!(stored.input.knowledge_binding_id.as_deref(), Some("b1"));

    // Without the field nothing changes: no binding, null on the wire.
    let (status, body) = send(
        &router,
        create_run_request(&host, serde_json::json!({ "prompt": "hi" })),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let run: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(run["input"]["knowledge_binding_id"].is_null());
    assert_eq!(
        registry.resolve_run(run["id"].as_str().unwrap(), None),
        Err(KnowledgeRefusal::Unbound)
    );
}

#[tokio::test]
async fn a_bound_run_reads_back_what_its_knowledge_tools_did() {
    use agent24_protocol::{ToolCall, ToolCallStatus};
    let (state, host, _) = crate::server::tests::capability_state().await;
    let router = build_router(state.clone());
    send(&router, request(Method::PUT, BINDING, &host, Some(7))).await;
    let create = |body: serde_json::Value| {
        let router = router.clone();
        let host = host.clone();
        async move {
            let (status, body) = send(&router, create_run_request(&host, body)).await;
            assert_eq!(status, StatusCode::ACCEPTED, "{body}");
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        }
    };
    let bound = create(serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" })).await;
    let unbound = create(serde_json::json!({ "prompt": "hi" })).await;
    let knowledge = |id: &str| {
        let (router, host, id) = (router.clone(), host.clone(), id.to_owned());
        async move {
            let (status, body) = send(
                &router,
                request(Method::GET, &format!("/api/v1/runs/{id}"), &host, None),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            serde_json::from_str::<serde_json::Value>(&body).unwrap()["knowledge"].clone()
        }
    };
    // Answered without the tools: whatever sources the answer names.
    assert_eq!(
        knowledge(&bound).await,
        serde_json::json!({ "searched": false, "read_chunks": [] })
    );
    assert!(knowledge(&unbound).await.is_null());

    for (i, (tool, input)) in [
        ("knowledge_search", serde_json::json!({ "query": "q" })),
        (
            "knowledge_read",
            serde_json::json!({ "knowledge_id": "doc-7", "chunk_id": "chk-71", "content_tag": "ct" }),
        ),
    ]
    .into_iter()
    .enumerate()
    {
        state
            .store
            .insert_tool_call(&ToolCall {
                id: format!("tc_{i}"),
                run_id: bound.clone(),
                tool: tool.into(),
                input: input.as_object().unwrap().clone(),
                status: ToolCallStatus::Completed,
                output_summary: None,
                started_at: format!("2026-10-31T00:00:0{i}Z"),
                ended_at: None,
            })
            .await
            .unwrap();
    }
    assert_eq!(
        knowledge(&bound).await,
        serde_json::json!({
            "searched": true,
            "read_chunks": [{ "knowledge_id": "doc-7", "chunk_id": "chk-71" }]
        })
    );
    // A list of runs does not say.
    let (_, body) = send(&router, request(Method::GET, "/api/v1/runs", &host, None)).await;
    let runs: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        runs["runs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r.get("knowledge").is_none())
    );
}

#[tokio::test]
async fn a_run_naming_a_binding_it_may_not_use_is_refused_and_not_created() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let router = build_router(state.clone());
    send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    send(&router, request(Method::DELETE, BINDING, &host, Some(2))).await;
    for (binding, status, code) in [
        ("b1", StatusCode::CONFLICT, "knowledge_binding_inactive"),
        ("never", StatusCode::CONFLICT, "knowledge_binding_inactive"),
        ("..", StatusCode::BAD_REQUEST, "invalid_request"),
        ("a/b", StatusCode::BAD_REQUEST, "invalid_request"),
        ("", StatusCode::BAD_REQUEST, "invalid_request"),
    ] {
        let req = create_run_request(
            &host,
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": binding }),
        );
        let (got, body) = send(&router, req).await;
        assert_eq!(got, status, "{binding}: {body}");
        assert!(body.contains(code), "{binding}: {body}");
    }
    assert_eq!(run_count(&state).await, 0);

    // The legacy shared token has no host identity.
    let legacy = crate::server::tests::state().await;
    let token = legacy.token.to_string();
    let router = build_router(legacy.clone());
    let req = create_run_request(
        &token,
        serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
    );
    let (status, body) = send(&router, req).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(body.contains("host_authority_required"));
    assert_eq!(run_count(&legacy).await, 0);
}

#[tokio::test]
async fn a_session_keeps_the_activation_its_first_bound_run_had() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let router = build_router(state.clone());
    let new_session = || async {
        let req = with_body(
            Method::POST,
            "/api/v1/sessions",
            &host,
            r#"{"title":"","channel":"desktop"}"#,
        );
        let (status, body) = send(&router, req).await;
        assert!(status.is_success(), "{body}");
        let session: serde_json::Value = serde_json::from_str(&body).unwrap();
        session["id"].as_str().unwrap().to_owned()
    };
    let run_in = |session: String, binding: Option<&'static str>| {
        let mut body = serde_json::json!({ "prompt": "hi", "session_id": session });
        if let Some(binding) = binding {
            body["knowledge_binding_id"] = binding.into();
        }
        create_run_request(&host, body)
    };
    send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    let first = new_session().await;
    let (status, body) = send(&router, run_in(first.clone(), Some("b1"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = send(&router, run_in(first.clone(), Some("b1"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "same activation: {body}");

    // A new activation (another endpoint, say) is another source.
    send(&router, request(Method::PUT, BINDING, &host, Some(2))).await;
    let before = run_count(&state).await;
    let (status, body) = send(&router, run_in(first.clone(), Some("b1"))).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(body.contains("knowledge_session_rebound"), "{body}");
    assert_eq!(run_count(&state).await, before);

    let (status, body) = send(&router, run_in(new_session().await, Some("b1"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "a new session: {body}");
    let (status, body) = send(&router, run_in(first, None)).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "no binding, no Knowledge tools: {body}"
    );
}

#[tokio::test]
async fn a_session_holding_knowledge_results_without_a_pin_is_refused() {
    // As after a restart: the session took in Knowledge results, the pin
    // that tied them to an activation is gone.
    let (state, host, _) = crate::server::tests::capability_state().await;
    let router = build_router(state.clone());
    send(&router, request(Method::PUT, BINDING, &host, Some(1))).await;
    let mut sessions = Vec::new();
    for origin in [
        "tool:knowledge_search",
        "tool:knowledge_read",
        "tool:shell_exec",
    ] {
        let req = with_body(
            Method::POST,
            "/api/v1/sessions",
            &host,
            r#"{"title":"","channel":"desktop"}"#,
        );
        let (_, body) = send(&router, req).await;
        let session: serde_json::Value = serde_json::from_str(&body).unwrap();
        let id = session["id"].as_str().unwrap().to_owned();
        sqlx::query(
            "INSERT INTO session_tool_result_origins (session_id, origin, created_at) VALUES (?, ?, ?)",
        )
        .bind(&id)
        .bind(origin)
        .bind("2026-10-30T00:00:00Z")
        .execute(agent24_store::test_hooks::pool(&state.store))
        .await
        .unwrap();
        sessions.push(id);
    }
    let run_in = |session: &str| {
        create_run_request(
            &host,
            serde_json::json!({ "prompt": "hi", "session_id": session, "knowledge_binding_id": "b1" }),
        )
    };
    let before = run_count(&state).await;
    for session in &sessions[..2] {
        let (status, body) = send(&router, run_in(session)).await;
        assert_eq!(status, StatusCode::CONFLICT, "{body}");
        assert!(body.contains("knowledge_session_rebound"), "{body}");
    }
    assert_eq!(run_count(&state).await, before);
    let (status, body) = send(&router, run_in(&sessions[2])).await;
    assert_eq!(
        status,
        StatusCode::ACCEPTED,
        "another tool's results: {body}"
    );
}

// ---- K-B6: the desktop FirstPartyShell is a Knowledge host ----

const BINDING_TEMPLATE: &str = "/api/v1/host/knowledge/bindings/{binding_id}";
const CREDENTIAL_TEMPLATE: &str = "/api/v1/host/knowledge/bindings/{binding_id}/credential";

struct Allow;

#[async_trait::async_trait]
impl PresenceProvider for Allow {
    async fn confirm(
        &self,
        _req: PresenceRequest,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> PresenceOutcome {
        PresenceOutcome::Allowed
    }
}

struct Capture(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl PresenceProvider for Capture {
    async fn confirm(
        &self,
        req: PresenceRequest,
        _cancel: tokio_util::sync::CancellationToken,
    ) -> PresenceOutcome {
        self.0.lock().unwrap().push(req.summary);
        PresenceOutcome::Allowed
    }
}

fn shell_bearer(token: &IssuedToken) -> String {
    token.secret.as_str().to_owned()
}

fn shell_host_key() -> String {
    format!("shell:{DESKTOP_SHELL_IDENTIFIER}")
}

fn with_grant(method: Method, path: &str, bearer: &str, body: &str, grant: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .header(header::CONTENT_TYPE, "application/json")
        .header("X-Agent24-Elevation", grant)
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn shell_router(identifier: &str) -> (AppState, axum::Router, EnrolledPeer, IssuedToken) {
    let (mut state, _, _) = crate::server::tests::capability_state().await;
    state.presence_provider = Some(Arc::new(Allow));
    let peer = test_peer_as("daemon-test", identifier);
    let token = state.first_party.as_ref().unwrap().issue(&peer).unwrap();
    let router = build_router(state.clone());
    (state, router, peer, token)
}

async fn approve(
    state: &AppState,
    peer: &EnrolledPeer,
    token_id: &str,
    method: &str,
    path: &str,
    body: &str,
) -> String {
    let (pending, _challenge) = tokio::sync::oneshot::channel();
    state
        .first_party
        .as_ref()
        .unwrap()
        .elevate(
            peer,
            token_id,
            ElevateRequest {
                method: method.into(),
                path: path.into(),
                body_sha256: body_digest(body.as_bytes()),
                client_summary: "ignored".into(),
            },
            pending,
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap()
        .secret
        .as_str()
        .to_owned()
}

/// The approval sentence, after "{identifier} (pid {pid}) wants to: ".
fn approval_details(summary: &str) -> &str {
    summary.split_once("wants to: ").unwrap().1
}

async fn captured_shell() -> (AppState, EnrolledPeer, IssuedToken, Arc<Capture>) {
    let (state, _, _) = crate::server::tests::capability_state().await;
    let capture = Arc::new(Capture(Mutex::new(Vec::new())));
    state
        .first_party
        .as_ref()
        .unwrap()
        .set_provider(Some(capture.clone()));
    let peer = test_peer_as("daemon-test", DESKTOP_SHELL_IDENTIFIER);
    let token = state.first_party.as_ref().unwrap().issue(&peer).unwrap();
    (state, peer, token, capture)
}

#[tokio::test]
async fn desktop_shell_routes_without_a_grant_require_presence() {
    let (state, router, _peer, token) = shell_router(DESKTOP_SHELL_IDENTIFIER).await;
    let bearer = shell_bearer(&token);
    let credential = serde_json::json!({
        "seq": 1,
        "kind": "bearer",
        "value": "TOKEN",
        "expires_at": unix_now() + 600,
    })
    .to_string();
    let routes = [
        (Method::PUT, BINDING, r#"{"seq":1}"#.to_owned()),
        (Method::DELETE, BINDING, r#"{"seq":1}"#.to_owned()),
        (Method::PUT, CREDENTIAL, credential),
        (Method::DELETE, CREDENTIAL, r#"{"seq":1}"#.to_owned()),
    ];
    for (method, path, body) in routes {
        let (status, response) = send(&router, with_body(method, path, &bearer, &body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path} {response}");
        assert!(response.contains("presence_required"), "{path} {response}");
    }
    assert!(state.knowledge_host.is_empty());
}

#[tokio::test]
async fn desktop_shell_grant_activates_a_binding_a_daily_token_can_name() {
    let (state, router, peer, token) = shell_router(DESKTOP_SHELL_IDENTIFIER).await;
    let bearer = shell_bearer(&token);
    let body = r#"{"seq":5}"#;
    let (status, response) = send(&router, with_body(Method::PUT, BINDING, &bearer, body)).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    assert!(response.contains("presence_required"), "{response}");
    let grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, body).await;
    let (status, response) = send(
        &router,
        with_grant(Method::PUT, BINDING, &bearer, body, &grant),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    let key = shell_host_key();
    assert_eq!(state.knowledge_host.generation(&key, "b1"), Some(5));

    let (status, response) = send(
        &router,
        create_run_request(
            &bearer,
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    let run: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        state
            .knowledge_host
            .resolve_run(run["id"].as_str().unwrap(), Some("b1")),
        Ok(pin(&key, "b1", 5))
    );
}

#[tokio::test]
async fn renewal_keeps_the_desktop_shell_binding_and_its_generation() {
    let (state, router, peer, token) = shell_router(DESKTOP_SHELL_IDENTIFIER).await;
    let bearer = shell_bearer(&token);
    let body = r#"{"seq":5}"#;
    let (status, response) = send(&router, with_body(Method::PUT, BINDING, &bearer, body)).await;
    assert!(
        response.contains("presence_required"),
        "{status} {response}"
    );
    let grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, body).await;
    let (status, response) = send(
        &router,
        with_grant(Method::PUT, BINDING, &bearer, body, &grant),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    let key = shell_host_key();
    assert_eq!(state.knowledge_host.generation(&key, "b1"), Some(5));

    let renewed = state
        .first_party
        .as_ref()
        .unwrap()
        .renew(&peer, &token.token_id)
        .unwrap();
    assert_ne!(renewed.token_id, token.token_id);
    let (status, response) = send(
        &router,
        create_run_request(
            &bearer,
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{response}");

    let (status, response) = send(
        &router,
        create_run_request(
            &shell_bearer(&renewed),
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    let run: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(state.knowledge_host.generation(&key, "b1"), Some(5));
    assert_eq!(
        state
            .knowledge_host
            .resolve_run(run["id"].as_str().unwrap(), Some("b1")),
        Ok(pin(&key, "b1", 5))
    );
}

#[tokio::test]
async fn cli_shells_are_not_knowledge_hosts() {
    for identifier in ["agent24", "agent24d"] {
        let (state, router, peer, token) = shell_router(identifier).await;
        let bearer = shell_bearer(&token);
        let (status, response) = send(
            &router,
            create_run_request(
                &bearer,
                serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{identifier} {response}");
        assert!(
            response.contains("host_authority_required"),
            "{identifier} {response}"
        );
        assert_eq!(run_count(&state).await, 0, "{identifier}");

        let body = r#"{"seq":1}"#;
        // Refused before any approval text is kept: no dialog for a CLI shell.
        let (status, response) =
            send(&router, with_body(Method::PUT, BINDING, &bearer, body)).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{identifier} {response}");
        assert!(
            response.contains("host_authority_required"),
            "{identifier} {status} {response}"
        );
        let (pending, _challenge) = tokio::sync::oneshot::channel();
        let refused = state
            .first_party
            .as_ref()
            .unwrap()
            .elevate(
                &peer,
                &token.token_id,
                ElevateRequest {
                    method: "PUT".into(),
                    path: BINDING.into(),
                    body_sha256: body_digest(body.as_bytes()),
                    client_summary: "ignored".into(),
                },
                pending,
                tokio_util::sync::CancellationToken::new(),
            )
            .await;
        assert_eq!(
            refused.unwrap_err(),
            EnrollErrorCode::NotElevatedOperation,
            "{identifier}"
        );
        // The handler is the second, independent check: even with a grant
        // (its approval text put there directly), a CLI shell is no host.
        state
            .first_party
            .as_ref()
            .unwrap()
            .remember_knowledge_request(
                &token.token_id,
                "PUT",
                BINDING_TEMPLATE,
                BINDING,
                body.as_bytes(),
            );
        let grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, body).await;
        let (status, response) = send(
            &router,
            with_grant(Method::PUT, BINDING, &bearer, body, &grant),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{identifier} {response}");
        assert!(
            response.contains("host_authority_required"),
            "{identifier} {response}"
        );
        assert!(state.knowledge_host.is_empty(), "{identifier}");
    }
}

#[tokio::test]
async fn query_strings_are_not_the_sequence_and_a_grant_cannot_carry_one() {
    let (mut state, host, _) = crate::server::tests::capability_state().await;
    state.presence_provider = Some(Arc::new(Allow));
    let peer = test_peer_as("daemon-test", DESKTOP_SHELL_IDENTIFIER);
    let token = state.first_party.as_ref().unwrap().issue(&peer).unwrap();
    let router = build_router(state.clone());
    let bearer = shell_bearer(&token);

    let (status, response) = send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host, Some(1)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert!(response.contains("invalid_request"), "{response}");
    assert!(state.knowledge_host.is_empty());

    let body = r#"{"seq":1}"#;
    // Without a grant the shell gets the same 400, and nothing is kept to
    // approve: elevating that target is not an elevated operation.
    let (status, response) = send(
        &router,
        with_body(Method::PUT, &format!("{BINDING}?seq=1"), &bearer, body),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
    assert!(response.contains("invalid_request"), "{response}");
    assert!(state.knowledge_host.is_empty());
    let (pending, _challenge) = tokio::sync::oneshot::channel();
    let refused = state
        .first_party
        .as_ref()
        .unwrap()
        .elevate(
            &peer,
            &token.token_id,
            ElevateRequest {
                method: "PUT".into(),
                path: BINDING.into(),
                body_sha256: body_digest(body.as_bytes()),
                client_summary: "ignored".into(),
            },
            pending,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert_eq!(refused.unwrap_err(), EnrollErrorCode::NotElevatedOperation);

    let (status, response) = send(&router, with_body(Method::PUT, BINDING, &bearer, body)).await;
    assert!(
        response.contains("presence_required"),
        "{status} {response}"
    );
    let grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, body).await;
    let (status, response) = send(
        &router,
        with_grant(
            Method::PUT,
            &format!("{BINDING}?seq=1"),
            &bearer,
            body,
            &grant,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    assert!(response.contains("elevation_invalid"), "{response}");
    assert!(state.knowledge_host.is_empty());
}

#[tokio::test]
async fn product_host_and_desktop_shell_bindings_are_separate() {
    let (mut state, host, _) = crate::server::tests::capability_state().await;
    state.presence_provider = Some(Arc::new(Allow));
    let peer = test_peer_as("daemon-test", DESKTOP_SHELL_IDENTIFIER);
    let token = state.first_party.as_ref().unwrap().issue(&peer).unwrap();
    let router = build_router(state.clone());
    let registry = state.knowledge_host.clone();
    let bearer = shell_bearer(&token);

    send(&router, request(Method::PUT, BINDING, &host, Some(3))).await;
    let product_key = registry.only_host();
    assert!(product_key.starts_with("product:"), "{product_key}");
    let shell_key = shell_host_key();
    assert_ne!(product_key, shell_key);

    let body = r#"{"seq":9}"#;
    let (status, response) = send(&router, with_body(Method::PUT, BINDING, &bearer, body)).await;
    assert!(
        response.contains("presence_required"),
        "{status} {response}"
    );
    let grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, body).await;
    let (status, response) = send(
        &router,
        with_grant(Method::PUT, BINDING, &bearer, body, &grant),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{response}");
    assert_eq!(registry.record(&product_key, "b1"), Some((true, 3)));
    assert_eq!(registry.record(&shell_key, "b1"), Some((true, 9)));
    assert_eq!(registry.generation(&product_key, "b1"), Some(3));
    assert_eq!(registry.generation(&shell_key, "b1"), Some(9));

    let (status, _) = send(&router, request(Method::DELETE, BINDING, &host, Some(4))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(registry.generation(&product_key, "b1"), None);
    assert_eq!(registry.record(&product_key, "b1"), Some((false, 4)));
    assert_eq!(registry.generation(&shell_key, "b1"), Some(9));

    let (status, response) = send(
        &router,
        create_run_request(
            &host,
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{response}");
    assert!(
        response.contains("knowledge_binding_inactive"),
        "{response}"
    );

    let (status, response) = send(
        &router,
        create_run_request(
            &bearer,
            serde_json::json!({ "prompt": "hi", "knowledge_binding_id": "b1" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED, "{response}");
    let run: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(
        registry.resolve_run(run["id"].as_str().unwrap(), Some("b1")),
        Ok(pin(&shell_key, "b1", 9))
    );
}

#[tokio::test]
async fn a_credential_summary_omits_the_secret() {
    let (state, peer, token, capture) = captured_shell().await;
    let expires = unix_now() + 3_600;
    let until = chrono::DateTime::<chrono::Utc>::from_timestamp(i64::try_from(expires).unwrap(), 0)
        .unwrap()
        .to_rfc3339();
    let body = serde_json::json!({
        "seq": 4,
        "kind": "bearer",
        "value": "ZZSECRETTOKEN",
        "expires_at": expires,
    })
    .to_string();
    state
        .first_party
        .as_ref()
        .unwrap()
        .remember_knowledge_request(
            &token.token_id,
            "PUT",
            CREDENTIAL_TEMPLATE,
            CREDENTIAL,
            body.as_bytes(),
        );
    let _grant = approve(&state, &peer, &token.token_id, "PUT", CREDENTIAL, &body).await;
    let shown = capture.0.lock().unwrap();
    let summary = shown.last().unwrap();
    assert!(!summary.contains("ZZSECRETTOKEN"), "{summary}");
    assert_eq!(
        approval_details(summary),
        format!(
            "replace the Knowledge credential of binding \"b1\" (\"bearer\", valid until {until}); sequence 4"
        )
    );
}

#[tokio::test]
async fn a_credential_longer_than_a_shown_value_still_has_a_summary() {
    // A JWT is far longer than a shown string may be; the value is never
    // shown, so its length must not stop the push from being approved.
    let (state, peer, token, capture) = captured_shell().await;
    let secret = format!("eyJ{}", "A".repeat(600));
    let body = serde_json::json!({
        "seq": 5,
        "kind": "bearer",
        "value": secret,
        "expires_at": unix_now() + 3_600,
    })
    .to_string();
    state
        .first_party
        .as_ref()
        .unwrap()
        .remember_knowledge_request(
            &token.token_id,
            "PUT",
            CREDENTIAL_TEMPLATE,
            CREDENTIAL,
            body.as_bytes(),
        );
    let _grant = approve(&state, &peer, &token.token_id, "PUT", CREDENTIAL, &body).await;
    let shown = capture.0.lock().unwrap();
    let summary = shown.last().unwrap();
    assert!(!summary.contains("AAAA"), "{summary}");
    assert!(
        approval_details(summary).starts_with("replace the Knowledge credential of binding \"b1\""),
        "{summary}"
    );
}

#[tokio::test]
async fn a_percent_encoded_binding_id_is_shown_as_the_handler_decodes_it() {
    let (state, peer, token, capture) = captured_shell().await;
    let path = "/api/v1/host/knowledge/bindings/%62%31";
    let body = r#"{"seq":2}"#;
    state
        .first_party
        .as_ref()
        .unwrap()
        .remember_knowledge_request(
            &token.token_id,
            "DELETE",
            BINDING_TEMPLATE,
            path,
            body.as_bytes(),
        );
    let _grant = approve(&state, &peer, &token.token_id, "DELETE", path, body).await;
    let shown = capture.0.lock().unwrap();
    assert_eq!(
        approval_details(shown.last().unwrap()),
        "deactivate Knowledge binding \"b1\"; sequence 2"
    );
}

#[tokio::test]
async fn an_activation_summary_names_the_endpoint_and_the_knowledge_bases() {
    let (state, peer, token, capture) = captured_shell().await;
    let body = serde_json::json!({
        "seq": 3,
        "endpoint": "https://kb.example/api/v1",
        "knowledge_base_ids": ["kb-a", "kb-b"],
    })
    .to_string();
    state
        .first_party
        .as_ref()
        .unwrap()
        .remember_knowledge_request(
            &token.token_id,
            "PUT",
            BINDING_TEMPLATE,
            BINDING,
            body.as_bytes(),
        );
    let _grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, &body).await;
    let shown = capture.0.lock().unwrap();
    assert_eq!(
        approval_details(shown.last().unwrap()),
        "activate Knowledge binding \"b1\" for \"https://kb.example/api/v1\"; knowledge bases: [\"kb-a\", \"kb-b\"]; sequence 3"
    );
}

#[tokio::test]
async fn an_unparseable_knowledge_body_is_not_an_elevated_operation() {
    let (state, peer, token, capture) = captured_shell().await;
    let body = r#"{"seq":0}"#;
    let before = capture.0.lock().unwrap().len();
    state
        .first_party
        .as_ref()
        .unwrap()
        .remember_knowledge_request(
            &token.token_id,
            "PUT",
            BINDING_TEMPLATE,
            BINDING,
            body.as_bytes(),
        );
    let (pending, _challenge) = tokio::sync::oneshot::channel();
    let refused = state
        .first_party
        .as_ref()
        .unwrap()
        .elevate(
            &peer,
            &token.token_id,
            ElevateRequest {
                method: "PUT".into(),
                path: BINDING.into(),
                body_sha256: body_digest(body.as_bytes()),
                client_summary: "ignored".into(),
            },
            pending,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert_eq!(refused.unwrap_err(), EnrollErrorCode::NotElevatedOperation);
    assert_eq!(capture.0.lock().unwrap().len(), before);
}

#[tokio::test]
async fn a_binding_id_with_an_invisible_character_is_not_remembered() {
    let (state, peer, token, capture) = captured_shell().await;
    let path = "/api/v1/host/knowledge/bindings/b\u{3164}1";
    let body = r#"{"seq":1}"#;
    let before = capture.0.lock().unwrap().len();
    state
        .first_party
        .as_ref()
        .unwrap()
        .remember_knowledge_request(
            &token.token_id,
            "PUT",
            BINDING_TEMPLATE,
            path,
            body.as_bytes(),
        );
    let (pending, _challenge) = tokio::sync::oneshot::channel();
    let refused = state
        .first_party
        .as_ref()
        .unwrap()
        .elevate(
            &peer,
            &token.token_id,
            ElevateRequest {
                method: "PUT".into(),
                path: path.into(),
                body_sha256: body_digest(body.as_bytes()),
                client_summary: "ignored".into(),
            },
            pending,
            tokio_util::sync::CancellationToken::new(),
        )
        .await;
    assert_eq!(refused.unwrap_err(), EnrollErrorCode::NotElevatedOperation);
    assert_eq!(capture.0.lock().unwrap().len(), before);
}

#[tokio::test]
async fn an_elevation_grant_is_bound_to_the_knowledge_body() {
    let (state, router, peer, token) = shell_router(DESKTOP_SHELL_IDENTIFIER).await;
    let bearer = shell_bearer(&token);
    let approved =
        r#"{"seq":5,"endpoint":"https://kb.example/api/v1","knowledge_base_ids":["kb-a"]}"#;
    let replayed =
        r#"{"seq":6,"endpoint":"https://kb.example/api/v1","knowledge_base_ids":["kb-a"]}"#;
    let (status, response) =
        send(&router, with_body(Method::PUT, BINDING, &bearer, approved)).await;
    assert!(
        response.contains("presence_required"),
        "{status} {response}"
    );
    let grant = approve(&state, &peer, &token.token_id, "PUT", BINDING, approved).await;
    let (status, response) = send(
        &router,
        with_grant(Method::PUT, BINDING, &bearer, replayed, &grant),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{response}");
    assert!(response.contains("elevation_invalid"), "{response}");
    assert!(state.knowledge_host.is_empty());
}
