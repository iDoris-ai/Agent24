#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{Request, header};
use http_body_util::BodyExt;
use tower::ServiceExt;

use super::*;
use crate::server::build_router;

impl KnowledgeHostRegistry {
    /// (active, high-water mark) of one record.
    fn record(&self, host: &str, binding: &str) -> Option<(bool, u64)> {
        let entries = self.entries.lock().unwrap();
        let entry = entries.get(&(host.to_owned(), binding.to_owned()))?;
        Some((entry.active, entry.seq))
    }

    fn is_empty(&self) -> bool {
        self.entries.lock().unwrap().is_empty()
    }

    /// The host of the only record there is.
    fn only_host(&self) -> String {
        let entries = self.entries.lock().unwrap();
        assert_eq!(entries.len(), 1);
        entries.keys().next().unwrap().0.clone()
    }
}

#[test]
fn a_change_older_than_one_already_applied_is_refused_and_changes_nothing() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    // The deactivation (seq 3) overtook the activation it supersedes (seq 2).
    registry.apply("host", "b1", 3, Change::Deactivate).unwrap();
    assert_eq!(
        registry.apply("host", "b1", 2, Change::Activate),
        Err(ChangeError::StaleSequence)
    );
    assert!(!registry.is_active("host", "b1"));
    // The same number twice is stale as well: a retry takes a new one.
    assert_eq!(
        registry.apply("host", "b1", 3, Change::Deactivate),
        Err(ChangeError::StaleSequence)
    );
    assert_eq!(registry.record("host", "b1"), Some((false, 3)));
}

#[test]
fn concurrent_changes_end_on_the_highest_sequence_number() {
    // Check and store happen under one lock: whatever order the threads run
    // in, the record ends on seq 64 and its change, and every lower number
    // that arrived after a higher one was refused.
    let registry = std::sync::Arc::new(KnowledgeHostRegistry::default());
    let threads: Vec<_> = (1..=64u64)
        .rev()
        .map(|seq| {
            let registry = registry.clone();
            std::thread::spawn(move || {
                let change = if seq % 2 == 0 {
                    Change::Activate
                } else {
                    Change::Deactivate
                };
                registry.apply("host", "b1", seq, change).is_ok()
            })
        })
        .collect();
    let applied = threads
        .into_iter()
        .map(|t| t.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert!(applied >= 1);
    assert_eq!(registry.record("host", "b1"), Some((true, 64)));
}

#[test]
fn deactivation_keeps_the_high_water_mark() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    assert!(registry.is_active("host", "b1"));
    registry.apply("host", "b1", 2, Change::Deactivate).unwrap();
    assert!(!registry.is_active("host", "b1"));
    registry.apply("host", "b1", 3, Change::Activate).unwrap();
    assert_eq!(registry.record("host", "b1"), Some((true, 3)));
}

#[test]
fn a_deactivation_may_come_first_and_still_blocks_an_older_activation() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 5, Change::Deactivate).unwrap();
    assert_eq!(
        registry.apply("host", "b1", 4, Change::Activate),
        Err(ChangeError::StaleSequence)
    );
    assert!(!registry.is_active("host", "b1"));
}

#[test]
fn records_belong_to_the_host_capability_that_wrote_them() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host-a", "b1", 7, Change::Activate).unwrap();
    assert!(!registry.is_active("host-b", "b1"));
    assert_eq!(registry.record("host-b", "b1"), None);
    // Another host starts from its own empty record for the same id.
    registry.apply("host-b", "b1", 1, Change::Activate).unwrap();
    registry
        .apply("host-b", "b1", 2, Change::Deactivate)
        .unwrap();
    assert!(registry.is_active("host-a", "b1"));
}

#[test]
fn a_host_holds_a_bounded_number_of_bindings() {
    let registry = KnowledgeHostRegistry::default();
    for i in 0..MAX_BINDINGS_PER_HOST {
        registry
            .apply("host", &format!("b{i}"), 1, Change::Deactivate)
            .unwrap();
    }
    assert_eq!(
        registry.apply("host", "one-more", 1, Change::Activate),
        Err(ChangeError::TooManyBindings)
    );
    assert_eq!(registry.record("host", "one-more"), None);
    // Existing records still change, and other hosts are unaffected.
    registry.apply("host", "b0", 2, Change::Activate).unwrap();
    registry
        .apply("other-host", "one-more", 1, Change::Activate)
        .unwrap();
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

#[test]
fn credential_follows_pushes_revokes_and_deactivation() {
    let registry = KnowledgeHostRegistry::default();
    let now = unix_now();
    let later = now + 600;
    // Only an active binding takes a credential; the refusal leaves no record.
    assert_eq!(
        registry.push_credential("host", "b1", 1, bearer("c1"), later),
        Err(ChangeError::BindingInactive)
    );
    assert_eq!(registry.record("host", "b1"), None);

    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    registry
        .push_credential("host", "b1", 2, bearer("c2"), later)
        .unwrap();
    let current = registry.credential("host", "b1", now).unwrap();
    assert_eq!((current.credential, current.seq), (bearer("c2"), 2));

    // A revoke drops C at once and leaves the binding active.
    registry
        .apply("host", "b1", 3, Change::RevokeCredential)
        .unwrap();
    assert_eq!(registry.credential("host", "b1", now), None);
    assert!(registry.is_active("host", "b1"));

    registry
        .push_credential("host", "b1", 4, Credential::ApiKey("k4".into()), later)
        .unwrap();
    assert_eq!(
        registry.credential("host", "b1", now).unwrap().credential,
        Credential::ApiKey("k4".into())
    );
    // Deactivation drops C too, and reactivating does not bring it back.
    registry.apply("host", "b1", 5, Change::Deactivate).unwrap();
    assert_eq!(registry.credential("host", "b1", now), None);
    registry.apply("host", "b1", 6, Change::Activate).unwrap();
    assert_eq!(registry.credential("host", "b1", now), None);
}

#[test]
fn a_push_overtaken_by_a_revoke_cannot_bring_the_old_credential_back() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    // The revoke (seq 3) arrives before the push it supersedes (seq 2).
    registry
        .apply("host", "b1", 3, Change::RevokeCredential)
        .unwrap();
    assert_eq!(
        registry.push_credential("host", "b1", 2, bearer("old"), later),
        Err(ChangeError::StaleSequence)
    );
    assert_eq!(registry.credential("host", "b1", unix_now()), None);
    // A revoke may also come before the binding was ever activated.
    registry
        .apply("host", "b2", 2, Change::RevokeCredential)
        .unwrap();
    assert_eq!(
        registry.apply("host", "b2", 1, Change::Activate),
        Err(ChangeError::StaleSequence)
    );
}

#[test]
fn a_credential_is_not_handed_out_once_it_has_expired() {
    let registry = KnowledgeHostRegistry::default();
    let expires_at = unix_now() + 10;
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    registry
        .push_credential("host", "b1", 2, bearer("c"), expires_at)
        .unwrap();
    assert!(registry.credential("host", "b1", expires_at - 1).is_some());
    assert_eq!(registry.credential("host", "b1", expires_at), None);
    // Dropped, not merely hidden: an earlier clock does not see it again.
    assert_eq!(registry.credential("host", "b1", expires_at - 1), None);
}

#[test]
fn a_reactivation_that_overtakes_a_removal_does_not_keep_the_credential() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    for (binding, removal) in [("b1", Change::Deactivate), ("b2", Change::RevokeCredential)] {
        registry
            .apply("host", binding, 1, Change::Activate)
            .unwrap();
        registry
            .push_credential("host", binding, 2, bearer("c2"), later)
            .unwrap();
        // The host removed C (3) and then activated again (4); 4 arrives first.
        registry
            .apply("host", binding, 4, Change::Activate)
            .unwrap();
        assert_eq!(
            registry.apply("host", binding, 3, removal),
            Err(ChangeError::StaleSequence)
        );
        assert_eq!(registry.credential("host", binding, unix_now()), None);
        assert_eq!(registry.record("host", binding), Some((true, 4)));
    }
}

#[test]
fn a_credential_belongs_to_the_host_capability_that_pushed_it() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    registry.apply("host-a", "b1", 1, Change::Activate).unwrap();
    registry
        .push_credential("host-a", "b1", 2, bearer("a"), later)
        .unwrap();
    assert_eq!(registry.credential("host-b", "b1", unix_now()), None);
    // Another host cannot push onto host-a's binding either.
    assert_eq!(
        registry.push_credential("host-b", "b1", 3, bearer("b"), later),
        Err(ChangeError::BindingInactive)
    );
}

#[test]
fn debug_output_never_carries_the_credential() {
    let current = CurrentCredential {
        credential: bearer("SECRET-BEARER"),
        seq: 1,
        expires_at: 0,
    };
    let printed = format!("{current:?} {:?}", Credential::ApiKey("SECRET-KEY".into()));
    assert!(!printed.contains("SECRET"), "{printed}");
}

// ---- over HTTP, through the kernel router and its auth layer ----

const BINDING: &str = "/api/v1/host/knowledge/bindings/b1";

fn request(method: Method, path: &str, bearer: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header(header::AUTHORIZATION, format!("Bearer {bearer}"))
        .body(Body::empty())
        .unwrap()
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
        with_body(
            Method::PUT,
            &format!("{CREDENTIAL}?seq={seq}"),
            &host,
            &body,
        )
    };

    // Not active yet.
    let (status, body) = send(&router, put(1, credential_body("TOKEN-1", 600))).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("binding_inactive"));

    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
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

    let (status, body) = send(
        &router,
        request(Method::DELETE, &format!("{CREDENTIAL}?seq=4"), &host),
    )
    .await;
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
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
    let host_id = registry.only_host();
    let now = unix_now();
    let body = |kind: &str, value: &str, expires_at: u64| {
        serde_json::json!({ "kind": kind, "value": value, "expires_at": expires_at }).to_string()
    };
    let too_long = format!("SECRET{}", "x".repeat(MAX_CREDENTIAL_BYTES));
    for (query, payload) in [
        ("", body("bearer", "SECRET", now + 60)),
        ("?seq=0", body("bearer", "SECRET", now + 60)),
        ("?seq=2", "{\"kind\":\"bearer\",\"value\":\"SECRET\"".to_owned()),
        ("?seq=2", body("cookie", "SECRET", now + 60)),
        // serde's own messages would quote these: "unknown variant `SECRET`",
        // "invalid type: sequence" next to the value.
        ("?seq=2", body("SECRET", "x", now + 60)),
        (
            "?seq=2",
            serde_json::json!({ "kind": "bearer", "value": ["SECRET"], "expires_at": now + 60 })
                .to_string(),
        ),
        ("?seq=2", body("bearer", "", now + 60)),
        ("?seq=2", body("bearer", "SECRET WITH SPACE", now + 60)),
        ("?seq=2", body("bearer", "SECRET\u{e9}", now + 60)),
        ("?seq=2", body("bearer", &too_long, now + 60)),
        ("?seq=2", body("bearer", "SECRET", now)),
        (
            "?seq=2",
            serde_json::json!({ "kind": "bearer", "value": "SECRET", "expires_at": now + 60, "seq": 9 })
                .to_string(),
        ),
    ] {
        let req = with_body(Method::PUT, &format!("{CREDENTIAL}{query}"), &host, &payload);
        let (status, response) = send(&router, req).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{query} {}", &payload[..payload.len().min(80)]);
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
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
    let host_id = registry.only_host();
    // A short credential padded with whitespace: valid JSON, over 1 MiB.
    let padding = " ".repeat(agent24_domain::http::MAX_BODY_BYTES);
    let payload = format!("{}{padding}", credential_body("TOKEN", 600));
    let req = with_body(Method::PUT, &format!("{CREDENTIAL}?seq=2"), &host, &payload);
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

    let (status, body) = send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(body.is_empty());
    let host_id = registry.only_host();
    assert!(registry.is_active(&host_id, "b1"));

    let (status, _) = send(
        &router,
        request(Method::DELETE, &format!("{BINDING}?seq=2"), &host),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(registry.record(&host_id, "b1"), Some((false, 2)));

    let (status, body) = send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=2"), &host),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert!(body.contains("stale_sequence"));
    assert!(!registry.is_active(&host_id, "b1"));
}

#[tokio::test]
async fn an_activation_sets_the_endpoint_its_calls_use_and_a_bad_one_changes_nothing() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    let put = |seq: u64, body: serde_json::Value| {
        with_body(
            Method::PUT,
            &format!("{BINDING}?seq={seq}"),
            &host,
            &body.to_string(),
        )
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
    send(
        &router,
        request(Method::DELETE, &format!("{BINDING}?seq=21"), &host),
    )
    .await;
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
        let req = with_body(
            method,
            &format!("{path}?seq=1"),
            &creative,
            &credential_body("t", 60),
        );
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
        let req = with_body(
            method,
            &format!("{path}?seq=1"),
            &token,
            &credential_body("t", 60),
        );
        let (status, body) = send(&router, req).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{path}");
        assert!(body.contains("host_authority_required"));
    }
    // Refused before the body is read: neither a malformed nor an oversized
    // one gets further than the host check.
    let oversized = " ".repeat(agent24_domain::http::MAX_BODY_BYTES + 1);
    for payload in ["not json", oversized.as_str()] {
        let req = with_body(Method::PUT, &format!("{CREDENTIAL}?seq=1"), &token, payload);
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
    for path in [
        BINDING.to_owned(),
        format!("{BINDING}?seq=x"),
        format!("{BINDING}?seq=-1"),
        // Sequence numbers start at 1.
        format!("{BINDING}?seq=0"),
        "/api/v1/host/knowledge/bindings/.?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/..?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/%2E%2E?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/%2E/credential?seq=1".to_owned(),
        format!("/api/v1/host/knowledge/bindings/{long_id}?seq=1"),
        "/api/v1/host/knowledge/bindings/b%01?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/..%2F..%2Fx?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/a%5Cb?seq=1".to_owned(),
        // Not UTF-8 once decoded.
        "/api/v1/host/knowledge/bindings/%C0%AF?seq=1".to_owned(),
    ] {
        let (status, body) = send(&router, request(Method::PUT, &path, &host)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(body.contains("invalid_request"));
    }
    assert!(registry.is_empty());
}

// ── run binding (PR-C) ────────────────────────────────────────────────────────

fn pin(host: &str, binding: &str, generation: u64) -> RunBinding {
    RunBinding {
        host: host.to_owned(),
        binding: binding.to_owned(),
        generation,
    }
}

#[test]
fn the_generation_moves_with_each_activation_only() {
    let registry = KnowledgeHostRegistry::default();
    let later = unix_now() + 600;
    assert_eq!(registry.generation("host", "b1"), None);
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    assert_eq!(registry.generation("host", "b1"), Some(1));
    // Rotating or revoking C is not a new generation.
    registry
        .push_credential("host", "b1", 2, bearer("c"), later)
        .unwrap();
    registry
        .apply("host", "b1", 3, Change::RevokeCredential)
        .unwrap();
    assert_eq!(registry.generation("host", "b1"), Some(1));
    registry.apply("host", "b1", 4, Change::Deactivate).unwrap();
    assert_eq!(registry.generation("host", "b1"), None);
    registry.apply("host", "b1", 5, Change::Activate).unwrap();
    assert_eq!(registry.generation("host", "b1"), Some(5));
    // Another host's records are its own.
    assert_eq!(registry.generation("other", "b1"), None);
}

#[test]
fn a_run_resolves_only_to_the_binding_host_and_generation_it_was_pinned_to() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    registry.pin_run("run_1", pin("host", "b1", 1));

    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Ok(pin("host", "b1", 1))
    );
    // No selector: unbound, whatever is pinned.
    assert_eq!(
        registry.resolve_run("run_1", None),
        Err(KnowledgeRefusal::Unbound)
    );
    // A selector without a pin (say, after a restart) is never bound anew.
    assert_eq!(
        registry.resolve_run("run_2", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    // A selector that is not the pinned binding.
    assert_eq!(
        registry.resolve_run("run_1", Some("b2")),
        Err(KnowledgeRefusal::Stale)
    );

    // Deactivated, then activated again: a new generation, so the run is stale.
    registry.apply("host", "b1", 2, Change::Deactivate).unwrap();
    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    registry.apply("host", "b1", 3, Change::Activate).unwrap();
    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );

    // The same binding and generation under another host does not count.
    registry.apply("other", "b1", 1, Change::Activate).unwrap();
    registry.pin_run("run_3", pin("host", "b1", 1));
    assert_eq!(
        registry.resolve_run("run_3", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );

    assert_eq!(KnowledgeRefusal::Unbound.code(), "knowledge_unbound");
    assert_eq!(KnowledgeRefusal::Stale.code(), "knowledge_binding_stale");
}

#[test]
fn the_oldest_run_pin_goes_first_once_the_cap_is_reached() {
    let registry = KnowledgeHostRegistry::default();
    registry.apply("host", "b1", 1, Change::Activate).unwrap();
    for i in 0..=MAX_PINNED_RUNS {
        registry.pin_run(&format!("run_{i}"), pin("host", "b1", 1));
    }
    assert_eq!(
        registry.resolve_run("run_0", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    assert!(registry.resolve_run("run_1", Some("b1")).is_ok());
    let last = format!("run_{MAX_PINNED_RUNS}");
    assert!(registry.resolve_run(&last, Some("b1")).is_ok());
    // Pinning a run again does not take a second place in line.
    registry.pin_run("run_1", pin("host", "b1", 1));
    assert_eq!(registry.runs.lock().unwrap().order.len(), MAX_PINNED_RUNS);
    registry.pin_run("run_new", pin("host", "b1", 1));
    assert_eq!(
        registry.resolve_run("run_1", Some("b1")),
        Err(KnowledgeRefusal::Stale)
    );
    assert!(registry.resolve_run("run_2", Some("b1")).is_ok());
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
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=7"), &host),
    )
    .await;
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
async fn a_run_naming_a_binding_it_may_not_use_is_refused_and_not_created() {
    let (state, host, _) = crate::server::tests::capability_state().await;
    let router = build_router(state.clone());
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
    send(
        &router,
        request(Method::DELETE, &format!("{BINDING}?seq=2"), &host),
    )
    .await;
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
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
    let first = new_session().await;
    let (status, body) = send(&router, run_in(first.clone(), Some("b1"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    let (status, body) = send(&router, run_in(first.clone(), Some("b1"))).await;
    assert_eq!(status, StatusCode::ACCEPTED, "same activation: {body}");

    // A new activation (another endpoint, say) is another source.
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=2"), &host),
    )
    .await;
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

#[test]
fn a_session_pin_is_exact() {
    let registry = KnowledgeHostRegistry::default();
    assert!(registry.pin_session("s1", &pin("host", "b1", 1), false));
    assert!(registry.pin_session("s1", &pin("host", "b1", 1), true));
    assert!(!registry.pin_session("s1", &pin("host", "b1", 2), false));
    assert!(!registry.pin_session("s1", &pin("host", "b2", 1), false));
    assert!(!registry.pin_session("s1", &pin("other", "b1", 1), false));
    assert!(registry.pin_session("s2", &pin("host", "b1", 2), false));
}

#[test]
fn no_new_pin_for_a_session_with_knowledge_results_and_none_past_the_cap() {
    let registry = KnowledgeHostRegistry::default();
    assert!(!registry.pin_session("s1", &pin("host", "b1", 1), true));
    assert!(!registry.pin_session("s1", &pin("host", "b1", 1), true));

    // Never dropped: past the cap a new session is refused, a pinned one
    // keeps its pin.
    assert!(registry.pin_session("s2", &pin("host", "b1", 1), false));
    for n in 1..MAX_PINNED_SESSIONS {
        assert!(registry.pin_session(&format!("other-{n}"), &pin("host", "b1", 1), false));
    }
    assert!(!registry.pin_session("late", &pin("host", "b1", 1), false));
    assert!(registry.pin_session("s2", &pin("host", "b1", 1), true));
    assert!(!registry.pin_session("s2", &pin("host", "b1", 2), false));
}

#[tokio::test]
async fn a_session_holding_knowledge_results_without_a_pin_is_refused() {
    // As after a restart: the session took in Knowledge results, the pin
    // that tied them to an activation is gone.
    let (state, host, _) = crate::server::tests::capability_state().await;
    let router = build_router(state.clone());
    send(
        &router,
        request(Method::PUT, &format!("{BINDING}?seq=1"), &host),
    )
    .await;
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
