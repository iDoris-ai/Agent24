//! COMM-3 end-to-end acceptance (COMM-HYPHAE.md §8's COMM-3 row; §8.2 steps
//! 3–4, the parts COMM-3 can already cover on its own — daemon supervision
//! is COMM-4a).
//!
//! A drives the REAL comm router (`CommState::ready` + `router()`, exactly
//! what `agent24d` mounts at `/api/v1/comm`) against the real, hash-verified
//! Hyphae CLI binary. B is a bare `std::process::Command` user, standing in
//! for an ordinary Hyphae CLI user with no router in front of it at all
//! (`history inbox` is pull-based — JOINT-ROUND1 F1 — so B must run `agent
//! inbox` itself; COMM-3 only adds `POST /comm/inbox/pull` for A's OWN
//! inbox).
//!
//! Flow: A creates an identity and sends through the router to B; B pulls
//! and finds the same event_id in its history. The relay is then stopped
//! and A sends again — this must come back `ok:true`, layer `L1`,
//! `published_to:0` (COMM-HYPHAE.md §5.1: relay-unreachable sends still
//! succeed locally). The relay is restarted and the SAME event_id is
//! retried through `POST /comm/outbox/{event_id}/retry`; the retry
//! response's `event_id` must be unchanged, and B's history must pick it up
//! exactly once.
//!
//! Requires `HYPHAE_JOINT_BIN` (a Hyphae binary matching this crate's
//! embedded `hyphae.lock.json` for the current platform — a mismatch is a
//! hard failure via `VerifiedBinary::install`, not a skip) and
//! `HYPHAE_JOINT_RELAY` (a `hyphae-relay` binary, hash unchecked —
//! COMM-HYPHAE.md §8.1 notes it is a joint-test tool, not a runtime
//! dependency). Without either, this test prints why and returns early.
//!
//! All state (both HOMEs, the relay's data dir, A's verified-binary install
//! dir) lives under one `tempfile::TempDir`; the real `~/.hyphae` is never
//! touched. The relay and B's child processes are stopped by their own pid
//! (`Child::kill`), never `pkill -f`. Passwords are synthetic test fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    CommState, HyphaeLock, HyphaeRunner, MemoryPasswordStore, VerifiedBinary, current_platform,
    router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

// A's keystore password is generated and held internally by the router's
// `create_identity` flow (an in-memory `PasswordStore` here) — this test
// never supplies or sees it, unlike B's, which this bare-CLI harness must
// pass on B's behalf.
const PASSWORD_B: &str = "comm3-e2e-b-synthetic-2b9a";

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

fn wait_for_port(port: u16, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("relay did not start listening on 127.0.0.1:{port} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_relay(relay_bin: &Path, port: u16, data_dir: &Path) -> Child {
    Command::new(relay_bin)
        .args([
            "-listen",
            "127.0.0.1",
            "-port",
            &port.to_string(),
            "-data-dir",
            data_dir.to_str().expect("utf8 data dir"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyphae-relay")
}

/// Stops a child by its specific pid (never by name/pattern) and reaps it.
fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// B side: an ordinary Hyphae CLI user, bare `std::process::Command`, own
/// throwaway `HOME` — deliberately NOT going through this crate's router or
/// `HyphaeRunner`, so this test also proves the router's A side interops
/// with a plain CLI user on the other end of a conversation.
struct BSide {
    bin: PathBuf,
    home: PathBuf,
}

impl BSide {
    fn run(&self, args: &[&str], password: Option<&str>) -> Value {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        if password.is_some() {
            cmd.arg("--password-stdin");
        }
        cmd.env("HOME", &self.home);
        cmd.env("HYPHAE_OUTPUT", "json");
        cmd.current_dir(&self.home);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(if password.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd.spawn().expect("spawn hyphae (B side)");
        if let Some(password) = password {
            let mut stdin = child.stdin.take().expect("stdin pipe");
            stdin
                .write_all(password.as_bytes())
                .expect("write password to stdin");
            drop(stdin);
        }
        let output = child.wait_with_output().expect("wait for hyphae (B side)");
        assert!(
            output.status.success(),
            "B side {args:?} failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|e| panic!("B side {args:?} did not print one JSON object: {e}"))
    }
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

#[tokio::test]
#[ignore]
async fn send_pull_retry_round_trip_against_real_binaries() {
    let Ok(bin_path) = std::env::var("HYPHAE_JOINT_BIN") else {
        eprintln!(
            "skipping send_pull_retry_round_trip_against_real_binaries: HYPHAE_JOINT_BIN is \
             not set"
        );
        return;
    };
    let Ok(relay_path) = std::env::var("HYPHAE_JOINT_RELAY") else {
        eprintln!(
            "skipping send_pull_retry_round_trip_against_real_binaries: HYPHAE_JOINT_RELAY is \
             not set"
        );
        return;
    };

    let bin_path = PathBuf::from(&bin_path)
        .canonicalize()
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_BIN={bin_path:?} is not readable: {e}"));
    let relay_path = PathBuf::from(&relay_path)
        .canonicalize()
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_RELAY={relay_path:?} is not readable: {e}"));

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_a = tmp.path().join("home-a");
    let home_b = tmp.path().join("home-b");
    let install_dir = tmp.path().join("install-a");
    let relay_data = tmp.path().join("relay-data");
    for dir in [&home_a, &home_b, &install_dir, &relay_data] {
        std::fs::create_dir_all(dir).expect("create tmp subdir");
    }

    // A side: the production lock-verification path, no test override —
    // same discipline as JOINT-ROUND1 (docs/comm/JOINT-ROUND1.md §1).
    let platform = current_platform();
    let lock = HyphaeLock::embedded().expect("embedded hyphae.lock.json parses");
    let expected = lock
        .expected_for(&platform)
        .unwrap_or_else(|e| panic!("no hyphae.lock.json entry for platform {platform}: {e}"));
    let verified = VerifiedBinary::install(&bin_path, expected, &install_dir)
        .await
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_BIN must match hyphae.lock.json: {e}"));
    let runner = Arc::new(HyphaeRunner::new(
        verified,
        home_a.clone(),
        Duration::from_secs(20),
    ));
    let state = CommState::ready(runner, Arc::new(MemoryPasswordStore::new()), home_a);
    let app = router(state);

    let b = BSide {
        bin: bin_path,
        home: home_b,
    };

    let port = free_port();
    let mut relay_child = spawn_relay(&relay_path, port, &relay_data);
    wait_for_port(port, Duration::from_secs(5));
    let relay_url = format!("ws://127.0.0.1:{port}");

    // ---- Step 1: identities on both sides ----------------------------
    let (status, body) = call(
        &app,
        "POST",
        "/identity",
        json!({"nickname": "a", "default": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let npub_a = body["data"]["npub"].as_str().expect("A npub").to_owned();

    let b_identity = b.run(&["identity", "create", "--nickname", "b"], Some(PASSWORD_B));
    let npub_b = b_identity["data"]["npub"]
        .as_str()
        .expect("B npub")
        .to_owned();

    // ---- Step 2: mutual contacts + relay set --------------------------
    let (status, body) = call(
        &app,
        "POST",
        "/contact",
        json!({"nickname": "b", "npub": npub_b}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");

    b.run(
        &["contact", "add", "--nickname", "a", "--npub", &npub_a],
        None,
    );

    let (status, body) = call(
        &app,
        "PUT",
        "/relay",
        json!({"relays": [relay_url.clone()]}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["configured"], true);

    b.run(&["relay", "set", "--relay", &relay_url], None);

    // ---- Step 3: A sends to B through the router; B pulls and sees it --
    let (status, body) = call(
        &app,
        "POST",
        "/send",
        json!({"to": npub_b, "content": "comm3-e2e-1", "from": "a"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["data"]["layer"], "L2",
        "relay is up, so the first send should be L2: {body:?}"
    );
    let event_id_1 = body["data"]["event_id"]
        .as_str()
        .expect("event_id present")
        .to_owned();

    b.run(&["agent", "inbox", "--as", "b"], Some(PASSWORD_B));
    let history = b.run(&["history", "inbox", "--as", "b", "--limit", "10"], None);
    let ids: Vec<&str> = history["data"]
        .as_array()
        .expect("history data is an array")
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    assert!(
        ids.contains(&event_id_1.as_str()),
        "B history should contain {event_id_1}, got {ids:?}"
    );

    // ---- Step 4: stop the relay, send again through the router ---------
    stop_child(relay_child);
    std::thread::sleep(Duration::from_millis(300));

    let (status, body) = call(
        &app,
        "POST",
        "/send",
        json!({"to": npub_b, "content": "comm3-e2e-2", "from": "a"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["data"]["published_to"], 0,
        "relay is down, expected published_to:0: {body:?}"
    );
    assert_eq!(
        body["data"]["layer"], "L1",
        "relay is down, expected layer L1: {body:?}"
    );
    let event_id_2 = body["data"]["event_id"]
        .as_str()
        .expect("event_id present")
        .to_owned();

    // ---- Step 5: restart the relay, retry through the router ------------
    relay_child = spawn_relay(&relay_path, port, &relay_data);
    wait_for_port(port, Duration::from_secs(5));

    let (status, body) = call(
        &app,
        "POST",
        &format!("/outbox/{event_id_2}/retry"),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(
        body["data"]["event_id"], event_id_2,
        "retry must echo back the SAME event_id it was asked to retry"
    );

    b.run(&["agent", "inbox", "--as", "b"], Some(PASSWORD_B));
    let history = b.run(&["history", "inbox", "--as", "b", "--limit", "10"], None);
    let ids_after_retry: Vec<&str> = history["data"]
        .as_array()
        .expect("history data is an array")
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str))
        .collect();
    let e2_count = ids_after_retry
        .iter()
        .filter(|id| **id == event_id_2)
        .count();
    assert_eq!(
        e2_count, 1,
        "E2={event_id_2} should appear exactly once after retry, got {ids_after_retry:?}"
    );

    stop_child(relay_child);
    drop(tmp);
    println!(
        "==== COMM-3 send/pull/retry round trip against real binaries: all assertions passed ===="
    );
}
