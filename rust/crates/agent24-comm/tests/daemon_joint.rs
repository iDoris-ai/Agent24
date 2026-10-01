//! COMM-4a joint round: the real Hyphae daemon, supervised by COMM-4a's
//! `HyphaeDaemonSupervisor`, driven **through the REST routes** (not by
//! calling the supervisor directly) against a real `hyphae-relay` and a
//! bare-CLI counterparty B — mirroring `tests/joint_round1.rs`'s harness
//! shape (see `docs/comm/JOINT-ROUND1.md`) but for daemon supervision
//! instead of one-shot commands.
//!
//! `history inbox` is pull-based (JOINT-ROUND1 finding F1): the daemon's own
//! `--watch-interval` (Hyphae's default, 30s — COMM-4a's own start command
//! intentionally omits the flag and relies on that default, matching
//! COMM-HYPHAE.md §6.1's literal command) is what pulls a counterparty's
//! message into A's `history inbox`, so this test polls for up to that long.
//!
//! Requires `HYPHAE_JOINT_BIN` and `HYPHAE_JOINT_RELAY` (`#[ignore]`d
//! otherwise, same as `joint_round1.rs`):
//! ```text
//! HYPHAE_JOINT_BIN=... HYPHAE_JOINT_RELAY=... \
//!   cargo test -p agent24-comm --test daemon_joint -- --ignored --nocapture
//! ```
//! All state lives under one `tempfile::TempDir`; the real `~/.hyphae` and
//! `~/.agent24` are never touched; both child processes (relay, B's one-shot
//! CLI calls) are stopped by their own pid, never `pkill -f`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    CommState, DaemonCtx, HyphaeDaemonSupervisor, HyphaeLock, HyphaeRunner, MemoryPasswordStore,
    VerifiedBinary, current_platform, parse_envelope, router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::{Value, json};
use tower::ServiceExt;

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
            data_dir.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyphae-relay")
}

fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// B: a bare Hyphae CLI user, same shape as `joint_round1.rs`'s `BSide`.
struct BSide {
    bin: PathBuf,
    home: PathBuf,
}

impl BSide {
    fn run(&self, args: &[&str], password: Option<&str>) -> (i32, agent24_comm::Envelope) {
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
        let code = output.status.code().expect("B side exited via signal");
        let envelope = parse_envelope(code, &output.stdout, &output.stderr)
            .unwrap_or_else(|e| panic!("B side envelope parse failed for {args:?}: {e}"));
        (code, envelope)
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
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
#[ignore]
async fn daemon_start_receives_a_real_message_then_stops() {
    let Ok(bin_path) = std::env::var("HYPHAE_JOINT_BIN") else {
        eprintln!(
            "skipping daemon_start_receives_a_real_message_then_stops: HYPHAE_JOINT_BIN is not set"
        );
        return;
    };
    let Ok(relay_path) = std::env::var("HYPHAE_JOINT_RELAY") else {
        eprintln!(
            "skipping daemon_start_receives_a_real_message_then_stops: HYPHAE_JOINT_RELAY is not set"
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
        std::fs::create_dir_all(dir).unwrap();
    }

    // A side: the production lock-verification path, same as joint_round1.
    let platform = current_platform();
    let lock = HyphaeLock::embedded().expect("embedded hyphae.lock.json parses");
    let expected = lock
        .expected_for(&platform)
        .unwrap_or_else(|e| panic!("no lock entry for {platform}: {e}"));
    let verified = VerifiedBinary::install(&bin_path, expected, &install_dir)
        .await
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_BIN must match hyphae.lock.json: {e}"));
    println!(
        "baseline: platform={platform} sha256={}",
        verified.sha256().to_hex()
    );
    let runner = Arc::new(HyphaeRunner::new(
        verified,
        home_a.clone(),
        Duration::from_secs(20),
    ));
    let password_store = Arc::new(MemoryPasswordStore::new());

    let pid_path = tmp.path().join("hyphae-daemon.pid");
    let log_path = tmp.path().join("logs").join("hyphae-daemon.log");
    let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(DaemonCtx {
        runner: runner.clone(),
        password_store: password_store.clone(),
        home: home_a.clone(),
        pid_path: pid_path.clone(),
        log_path,
    }));
    let state = CommState::ready(runner.clone(), password_store, home_a.clone())
        .with_daemon(daemon.clone());
    let app = router(state);

    let b = BSide {
        bin: bin_path.clone(),
        home: home_b.clone(),
    };

    let port = free_port();
    let relay_child = spawn_relay(&relay_path, port, &relay_data);
    wait_for_port(port, Duration::from_secs(5));
    let relay_url = format!("ws://127.0.0.1:{port}");
    println!("relay: listening on {relay_url}");

    // ---- configure A through the REAL routes ----
    let (status, body) = call(
        &app,
        "POST",
        "/identity",
        json!({"nickname": "a", "default": true}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    let npub_a = body["data"]["npub"].as_str().expect("A npub").to_owned();

    let (status, body) = call(&app, "PUT", "/relay", json!({"relays": [relay_url]})).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["source"], "config");

    // ---- configure B with the bare CLI, same as joint_round1 ----
    let (code, envelope) = b.run(
        &["identity", "create", "--nickname", "b"],
        Some("daemon-joint-b-synthetic"),
    );
    assert_eq!(code, 0, "{envelope:?}");
    let agent24_comm::Envelope::Ok { data: b_data } = &envelope else {
        panic!("B identity create failed")
    };
    let npub_b = b_data["npub"].as_str().expect("B npub").to_owned();
    println!("npub_a = {npub_a}\nnpub_b = {npub_b}");

    let (code, envelope) = b.run(
        &["contact", "add", "--nickname", "a", "--npub", &npub_a],
        None,
    );
    assert_eq!(code, 0, "{envelope:?}");
    let (code, envelope) = b.run(&["relay", "set", "--relay", &relay_url], None);
    assert_eq!(code, 0, "{envelope:?}");

    // ---- start A's daemon THROUGH THE ROUTE ----
    let (status, body) = call(&app, "POST", "/daemon/start", json!({})).await;
    assert_eq!(status, StatusCode::OK, "daemon start failed: {body:?}");
    assert_eq!(body["data"]["process"]["state"], "running", "{body:?}");

    let (status, body) = get(&app, "/daemon").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["data"]["process"]["state"], "running",
        "GET /comm/daemon should show running: {body:?}"
    );

    // ---- B sends a message to A ----
    let content = "daemon-joint-hello";
    let (code, envelope) = b.run(
        &[
            "agent",
            "msg",
            "--from",
            "b",
            "--to",
            &npub_a,
            "--content",
            content,
        ],
        Some("daemon-joint-b-synthetic"),
    );
    assert_eq!(code, 0, "B send failed: {envelope:?}");
    let agent24_comm::Envelope::Ok { data } = &envelope else {
        panic!("B send failed: {envelope:?}")
    };
    let event_id = data["event_id"]
        .as_str()
        .expect("event_id present")
        .to_owned();
    println!("event_id = {event_id}");

    // ---- A's daemon must pull it in on its own (watch-interval, default 30s) ----
    let deadline = std::time::Instant::now() + Duration::from_secs(45);
    let mut seen = false;
    while std::time::Instant::now() < deadline {
        let envelope = runner
            .run(agent24_comm::Invocation {
                args: vec![
                    "history".into(),
                    "inbox".into(),
                    "--as".into(),
                    "a".into(),
                    "--limit".into(),
                    "20".into(),
                ],
                password: None,
                timeout: None,
            })
            .await
            .expect("history inbox should at least produce an envelope");
        if let agent24_comm::Envelope::Ok { data } = envelope
            && let Some(rows) = data.as_array()
            && rows
                .iter()
                .any(|m| m.get("id").and_then(Value::as_str) == Some(event_id.as_str()))
        {
            seen = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    assert!(
        seen,
        "A's history inbox never showed event_id {event_id} within 45s of the daemon running"
    );
    println!(
        "confirmed: the supervised daemon pulled B's message into A's history inbox on its own"
    );

    // ---- stop, through the route ----
    let (status, body) = call(&app, "POST", "/daemon/stop", json!({})).await;
    assert_eq!(status, StatusCode::OK, "{body:?}");
    assert_eq!(body["data"]["process"]["state"], "stopped");
    assert!(!pid_path.exists(), "pid file must be gone after stop");

    stop_child(relay_child);
    println!(
        "==== daemon_joint: start -> running -> received via watch-interval -> stop, all through the route ===="
    );
    drop(tmp);
}
