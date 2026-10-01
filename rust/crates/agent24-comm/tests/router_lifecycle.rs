//! COMM-2a acceptance: drives the REAL comm router (not `HyphaeRunner`
//! directly — `tests/real_binary.rs` and `tests/keystore_write_lock.rs`
//! already cover that layer) against the real Hyphae CLI, in a throwaway
//! `HOME`, end to end: `create -> list -> use -> contact add -> relay set ->
//! list`, plus the 10-concurrent-creates acceptance criterion (COMM-HYPHAE.md
//! §8's COMM-2a row).
//!
//! Requires `HYPHAE_TEST_BIN` (see `tests/real_binary.rs`'s own doc comment
//! for what it may point at); without it every test here prints why it's
//! skipping and returns early. Never touches the real `~/.hyphae` —
//! `CommState::ready` is always built over a throwaway `tempdir()` HOME, and
//! every password store here is a [`MemoryPasswordStore`].

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    CommState, HyphaeLock, HyphaeRunner, MemoryPasswordStore, Sha256Digest, VerifiedBinary,
    current_platform, router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

/// See `tests/real_binary.rs`'s own doc comment: `HYPHAE_TEST_BIN` may be
/// either the proposal's unreproducible acceptance binary (this hash) or a
/// binary matching the embedded `hyphae.lock.json` for the current platform.
const REFERENCE_SHA256: &str = "bc30dcf7bcf8b5c1865a3e995518c2bdab224bd8d6a3a064c4d7fc780de5e2b7";

/// Builds a `CommState::ready` over a fresh `tempdir()` HOME from
/// `HYPHAE_TEST_BIN`, or `None` (having already explained why) so callers
/// bail out early exactly like the other `HYPHAE_TEST_BIN`-gated tests do.
async fn comm_state_from_env() -> Option<(CommState, tempfile::TempDir)> {
    let test_bin = std::env::var("HYPHAE_TEST_BIN").ok()?;
    let source = PathBuf::from(&test_bin)
        .canonicalize()
        .unwrap_or_else(|e| panic!("HYPHAE_TEST_BIN={test_bin:?} is not readable: {e}"));
    let bytes = tokio::fs::read(&source)
        .await
        .unwrap_or_else(|e| panic!("HYPHAE_TEST_BIN={test_bin:?} is not readable: {e}"));
    let actual = agent24_comm::binary::sha256_of(&bytes);

    let platform = current_platform();
    let reference = Sha256Digest::from_hex(REFERENCE_SHA256).expect("valid reference sha256");
    let expected = if actual == reference {
        reference
    } else {
        HyphaeLock::embedded()
            .expect("embedded lock parses")
            .expected_for(&platform)
            .unwrap_or_else(|e| {
                panic!(
                    "HYPHAE_TEST_BIN (sha256 {}) matches neither the documented reference \
                     binary nor hyphae.lock.json's {platform} entry: {e}",
                    actual.to_hex()
                )
            })
    };

    let tmp = tempfile::tempdir().expect("tempdir");
    let home = tmp.path().join("hyphae-home");
    tokio::fs::create_dir_all(&home)
        .await
        .expect("create hyphae-home");
    let install_dir = tmp.path().join("bin");
    let bin = VerifiedBinary::install(&source, expected, &install_dir)
        .await
        .expect("HYPHAE_TEST_BIN must match the hash it was just selected against");
    let runner = Arc::new(HyphaeRunner::new(
        bin,
        home.clone(),
        Duration::from_secs(15),
    ));
    let state = CommState::ready(runner, Arc::new(MemoryPasswordStore::new()), home);
    Some((state, tmp))
}

async fn call(router: &Router, method: &str, uri: &str, body: Value) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    (status, value)
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    (status, value)
}

#[tokio::test]
async fn create_list_use_contact_relay_round_trip_against_the_real_binary() {
    let Some((state, _tmp)) = comm_state_from_env().await else {
        eprintln!(
            "skipping create_list_use_contact_relay_round_trip_against_the_real_binary: \
             HYPHAE_TEST_BIN is not set"
        );
        return;
    };
    let app = router(state);

    // create
    let (status, body) = call(
        &app,
        "POST",
        "/identity",
        json!({"nickname": "primary", "default": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let npub = body["data"]["npub"]
        .as_str()
        .expect("identity create must return an npub")
        .to_owned();

    // list
    let (status, body) = get(&app, "/identity").await;
    assert_eq!(status, StatusCode::OK);
    let identities = body["data"].as_array().expect("identity list is an array");
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0]["nickname"], "primary");
    assert_eq!(identities[0]["encrypted"], true);

    // use (set default) — a no-op here (already default), but exercises the
    // route and its keystore-write path.
    let (status, _body) = call(
        &app,
        "POST",
        "/identity/default",
        json!({"nickname": "primary"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    // contact add — npub validated against the real identity's own, so this
    // also proves `is_valid_npub` accepts what Hyphae actually generates.
    let (status, body) = call(
        &app,
        "POST",
        "/contact",
        json!({"nickname": "self-as-contact", "npub": npub, "role": "human"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    // contact list
    let (status, body) = get(&app, "/contact").await;
    assert_eq!(status, StatusCode::OK);
    let contacts = body["data"].as_array().expect("contact list is an array");
    assert_eq!(contacts.len(), 1);
    assert_eq!(contacts[0]["nickname"], "self-as-contact");

    // relay set — one call, full replace.
    let (status, body) = call(
        &app,
        "PUT",
        "/relay",
        json!({"relays": ["wss://relay.example.invalid"]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["configured"], true);
    assert_eq!(body["data"]["source"], "config");

    // relay list — same shape, read back.
    let (status, body) = get(&app, "/relay").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["data"]["configured"], true);
    let relays = body["data"]["relays"]
        .as_array()
        .expect("relays is an array");
    assert!(relays.iter().any(|r| r == "wss://relay.example.invalid"));
}

/// COMM-HYPHAE.md §8's COMM-2a acceptance: 10 concurrent `POST /identity`
/// must all survive — `KeystoreWriteLock` (COMM-1b, proven directly in
/// `tests/keystore_write_lock.rs`) serializes every one of them, this time
/// reached through the REAL REST path (`create_identity`'s own
/// `runner.keystore_lock().acquire()`), not by calling the runner directly.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ten_concurrent_identity_creates_all_survive_through_the_router() {
    let Some((state, _tmp)) = comm_state_from_env().await else {
        eprintln!(
            "skipping ten_concurrent_identity_creates_all_survive_through_the_router: \
             HYPHAE_TEST_BIN is not set"
        );
        return;
    };
    let app = router(state);

    const N: usize = 10;
    let mut tasks = Vec::with_capacity(N);
    for i in 0..N {
        let app = app.clone();
        tasks.push(tokio::spawn(async move {
            let body = json!({"nickname": format!("concurrent-{i}")});
            call(&app, "POST", "/identity", body).await
        }));
    }
    for (i, task) in tasks.into_iter().enumerate() {
        let (status, body) = task.await.expect("task must not panic");
        assert_eq!(status, StatusCode::OK, "create {i} failed: {body:?}");
    }

    let (status, body) = get(&app, "/identity").await;
    assert_eq!(status, StatusCode::OK);
    let identities = body["data"].as_array().expect("identity list is an array");
    assert_eq!(
        identities.len(),
        N,
        "expected all {N} identities to survive the concurrent creates, found {}: {identities:?}",
        identities.len()
    );
}
