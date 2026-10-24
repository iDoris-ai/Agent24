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
        let operation =
            crate::server::required_operation(&method, "/api/v1/host/knowledge/bindings/b1");
        assert_eq!(operation, Operation::KnowledgeHost);
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
async fn a_creative_runtime_is_refused() {
    let (state, _, creative) = crate::server::tests::capability_state().await;
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    for method in [Method::PUT, Method::DELETE] {
        // The auth layer already refuses it (`knowledge.host` is not in the
        // Creative allow-list); the handler's audience check is the backstop.
        let (status, _) = send(
            &router,
            request(method, &format!("{BINDING}?seq=1"), &creative),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    assert!(registry.is_empty());
}

#[tokio::test]
async fn the_legacy_shared_token_is_refused() {
    let state = crate::server::tests::state().await;
    let token = state.token.to_string();
    let registry = state.knowledge_host.clone();
    let router = build_router(state);
    for method in [Method::PUT, Method::DELETE] {
        let (status, body) = send(
            &router,
            request(method, &format!("{BINDING}?seq=1"), &token),
        )
        .await;
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
        format!("/api/v1/host/knowledge/bindings/{long_id}?seq=1"),
        "/api/v1/host/knowledge/bindings/b%01?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/..%2F..%2Fx?seq=1".to_owned(),
        "/api/v1/host/knowledge/bindings/a%5Cb?seq=1".to_owned(),
    ] {
        let (status, body) = send(&router, request(Method::PUT, &path, &host)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert!(body.contains("invalid_request"));
    }
    assert!(registry.is_empty());
}
