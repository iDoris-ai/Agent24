//! COMM-4b: `GET /comm/daemon`'s three-state model (`process`/`relay_probe`/
//! `catch_up`) and manual relay probing, proven through the REST routes
//! against a fake "hyphae" binary (same harness shape as
//! `tests/daemon_supervise.rs`/`tests/router_lifecycle.rs`).
//!
//! Acceptance (COMM-HYPHAE.md task table, COMM-4b row):
//! - relay 停掉时 probe 结果为 `connected:false`
//!   -> [`relay_probe_reports_connected_false_when_relay_is_down_and_records_it`]
//! - 日志中出现 incomplete 行后，状态变为 `incomplete`
//!   -> [`catch_up_becomes_incomplete_once_the_daemon_log_has_the_marker_line`]
//! - 任何输入都不会产生 `complete` 状态
//!   -> [`catch_up_state_is_never_literally_complete_through_the_rest_route`]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::binary::sha256_of;
use agent24_comm::{
    Account, CommState, DaemonCtx, HyphaeDaemonSupervisor, HyphaeRunner, MemoryPasswordStore,
    Password, PasswordStore, VerifiedBinary, router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

/// `identity list`/`relay list` answer fixed, valid data (same shape as
/// `daemon_supervise.rs`'s fixture — this test never actually starts the
/// daemon child, only probes and reads status). `relay info` succeeds iff
/// `$HOME/.hyphae/relay_up` exists; otherwise it reproduces Hyphae's own
/// `relay info` contract for an unreachable relay
/// (`docs/agent/cli-communication-contract.md`: "失败走网络错误信封和退出码
/// 2") — a `network_error` envelope on stderr, exit 2.
const SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  identity)
    echo '{"ok":true,"data":[{"nickname":"alice","npub":"npub1x","default":true,"encrypted":true}]}'
    exit 0
    ;;
  relay)
    case "$2" in
      info)
        if [ -f "$HOME/.hyphae/relay_up" ]; then
          echo '{"ok":true,"data":{"url":"wss://relay.example","connected":true}}'
          exit 0
        else
          echo '{"ok":false,"error":"network_error","message":"dial tcp 127.0.0.1:1: connect: connection refused"}' >&2
          exit 2
        fi
        ;;
      *)
        echo '{"ok":true,"data":{"relays":["wss://relay.example"],"source":"config"}}'
        exit 0
        ;;
    esac
    ;;
  daemon)
    cat > /dev/null
    exec sleep 9999
    ;;
esac
"#;

async fn install(dir: &Path) -> VerifiedBinary {
    let source = dir.join("hyphae-fake.sh");
    tokio::fs::write(&source, SCRIPT).await.unwrap();
    let bytes = tokio::fs::read(&source).await.unwrap();
    let expected = sha256_of(&bytes);
    VerifiedBinary::install(&source, expected, &dir.join("bin"))
        .await
        .unwrap()
}

/// Builds a full `router(CommState)` app with a real [`HyphaeDaemonSupervisor`]
/// wired in (so `relay_probe`/`catch_up` have somewhere to land), against
/// the fake binary above. `log_path` is handed back so a test can write a
/// log line directly, the same way Hyphae's own daemon child would append
/// one.
async fn app_with_daemon(dir: &Path) -> (Router, std::path::PathBuf) {
    let home = dir.join("hyphae-home");
    tokio::fs::create_dir_all(home.join(".hyphae"))
        .await
        .unwrap();
    tokio::fs::write(
        home.join(".hyphae").join("keystore.json"),
        br#"{"salt":"dGVzdHNhbHQ="}"#,
    )
    .await
    .unwrap();
    let bin = install(dir).await;
    let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
    let store = Arc::new(MemoryPasswordStore::new());
    store
        .put(
            &Account::from_salt("dGVzdHNhbHQ="),
            &Password::new(b"testpass".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    let log_path = dir.join("logs").join("hyphae-daemon.log");
    let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(DaemonCtx {
        runner: runner.clone(),
        password_store: store.clone(),
        home: home.clone(),
        pid_path: dir.join("hyphae-daemon.pid"),
        log_path: log_path.clone(),
        autostart_path: dir.join("daemon-autostart.json"),
        grace: Duration::from_millis(500),
        ready_after: Duration::from_millis(200),
    }));
    let state = CommState::ready(runner, store, home).with_daemon(daemon);
    (router(state), log_path)
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
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn get(router: &Router, uri: &str) -> (StatusCode, Value) {
    let req = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let resp = router.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

// -------------------------------------------------------------------------
// 判据 1: relay 停掉时 probe 结果为 connected:false
// -------------------------------------------------------------------------

#[tokio::test]
async fn relay_probe_reports_connected_false_when_relay_is_down_and_records_it() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _log_path) = app_with_daemon(tmp.path()).await;
    // No `relay_up` marker written: the fake binary's `relay info` takes the
    // "relay is down" branch.

    let (status, body) = call(&app, "POST", "/relay/probe", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["ok"], true, "{body:?}");
    assert_eq!(
        body["data"]["connected"], false,
        "a down relay must be a *result*, not an HTTP error: {body:?}"
    );
    assert!(
        body["data"]["error"].as_str().is_some(),
        "the network failure's message should still be visible: {body:?}"
    );
    // Codex COMM-4b review, Medium #4: a `network_error` envelope from
    // Hyphae carries no `url` field at all, so the probe target must come
    // from comm's OWN resolution (mirroring `relay list`'s explicit >
    // config > default precedence) when the caller didn't name one — not
    // be left `null` just because the request body omitted it.
    assert_eq!(
        body["data"]["url"], "wss://relay.example",
        "a default-target probe failure must still report which address was \
         actually probed: {body:?}"
    );

    // §5.2: the result must also land in GET /comm/daemon's relay_probe.
    let (status, body) = get(&app, "/daemon").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["relay_probe"]["connected"], false, "{body:?}");
    assert_eq!(
        body["data"]["relay_probe"]["url"], "wss://relay.example",
        "{body:?}"
    );
    assert!(
        body["data"]["relay_probe"]["at_ms"]
            .as_u64()
            .is_some_and(|ms| ms > 0),
        "{body:?}"
    );
}

#[tokio::test]
async fn relay_probe_reports_connected_true_when_relay_is_up() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _log_path) = app_with_daemon(tmp.path()).await;
    tokio::fs::write(
        tmp.path()
            .join("hyphae-home")
            .join(".hyphae")
            .join("relay_up"),
        b"",
    )
    .await
    .unwrap();

    let (status, body) = call(&app, "POST", "/relay/probe", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["connected"], true, "{body:?}");
    assert_eq!(body["data"]["error"], Value::Null, "{body:?}");

    let (_, body) = get(&app, "/daemon").await;
    assert_eq!(body["data"]["relay_probe"]["connected"], true, "{body:?}");
    assert_eq!(
        body["data"]["relay_probe"]["url"], "wss://relay.example",
        "{body:?}"
    );
}

#[tokio::test]
async fn relay_probe_is_none_in_daemon_status_before_any_probe_ever_ran() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, _log_path) = app_with_daemon(tmp.path()).await;
    let (status, body) = get(&app, "/daemon").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["data"]["relay_probe"],
        Value::Null,
        "§5.2: relay_probe is null until a manual probe has ever run: {body:?}"
    );
}

// -------------------------------------------------------------------------
// 判据 2: 日志中出现 incomplete 行后，状态变为 incomplete
// -------------------------------------------------------------------------

#[tokio::test]
async fn catch_up_becomes_incomplete_once_the_daemon_log_has_the_marker_line() {
    let tmp = tempfile::tempdir().unwrap();
    let (app, log_path) = app_with_daemon(tmp.path()).await;

    let (_, body) = get(&app, "/daemon").await;
    assert_eq!(
        body["data"]["catch_up"]["state"], "unknown",
        "no log yet: {body:?}"
    );

    if let Some(parent) = log_path.parent() {
        tokio::fs::create_dir_all(parent).await.unwrap();
    }
    // The real line, verbatim from Hyphae's `internal/daemon/daemon.go`:
    // `fmt.Printf("[%s] ⚠️  Inbox scan incomplete: %v\n", ...)`.
    tokio::fs::write(
        &log_path,
        "🚀 Starting daemon for 'alice'\n\
         [10:00:00] ⚠️  Inbox scan incomplete: dial tcp 127.0.0.1:4: connect: connection refused\n",
    )
    .await
    .unwrap();

    let (status, body) = get(&app, "/daemon").await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["catch_up"]["state"], "incomplete", "{body:?}");
    assert!(
        body["data"]["catch_up"]["last_incomplete_at_ms"]
            .as_u64()
            .is_some_and(|ms| ms > 0),
        "{body:?}"
    );
}

// -------------------------------------------------------------------------
// 判据 3: 任何输入都不会产生 complete 状态
// -------------------------------------------------------------------------

#[tokio::test]
async fn catch_up_state_is_never_literally_complete_through_the_rest_route() {
    let samples: &[&[u8]] = &[
        b"",
        b"Inbox scan incomplete: timeout",
        b"Catch-up complete\n",
        b"inbox scan complete, 0 incomplete\n",
        b"complete complete complete",
    ];
    for sample in samples {
        let tmp = tempfile::tempdir().unwrap();
        let (app, log_path) = app_with_daemon(tmp.path()).await;
        if let Some(parent) = log_path.parent() {
            tokio::fs::create_dir_all(parent).await.unwrap();
        }
        tokio::fs::write(&log_path, sample).await.unwrap();

        let (status, body) = get(&app, "/daemon").await;
        assert_eq!(status, StatusCode::OK, "{body:?}");
        let state = body["data"]["catch_up"]["state"].as_str().unwrap();
        assert_ne!(
            state, "complete",
            "sample {sample:?} must never produce \"complete\": {body:?}"
        );
        assert!(
            matches!(state, "unknown" | "incomplete"),
            "sample {sample:?} -> unexpected catch_up state {state:?}"
        );
    }
}
