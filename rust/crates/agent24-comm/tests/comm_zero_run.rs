//! COMM-5b T2: six inbound shapes stay passive when read through comm.
//! This runs the real comm router and its verified-child runner with a
//! deterministic Hyphae fixture. The local model-shaped HTTP counter also
//! distinguishes health checks from inference requests; inference counts
//! only `/v1/chat/completions`.

#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread;
use std::time::Duration;

use agent24_comm::{
    Account, CommState, DaemonCtx, HyphaeDaemonSupervisor, HyphaeRunner, MemoryPasswordStore,
    Password, PasswordStore, VerifiedBinary, binary::sha256_of, router,
};
use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

const SAMPLES: &str = include_str!("fixtures/comm_inbound_six.json");

struct ModelCounter {
    address: std::net::SocketAddr,
    health_requests: Arc<AtomicUsize>,
    inference_requests: Arc<AtomicUsize>,
    stopped: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl ModelCounter {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind local model counter");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let address = listener.local_addr().expect("local address");
        let health_requests = Arc::new(AtomicUsize::new(0));
        let inference_requests = Arc::new(AtomicUsize::new(0));
        let stopped = Arc::new(AtomicBool::new(false));
        let health_counter = health_requests.clone();
        let counter = inference_requests.clone();
        let stop_loop = stopped.clone();
        let thread = thread::spawn(move || {
            while !stop_loop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(_) => break,
                };
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).unwrap_or(0);
                let line = String::from_utf8_lossy(&request[..read]);
                if line
                    .lines()
                    .next()
                    .is_some_and(|line| line.starts_with("POST /v1/chat/completions "))
                {
                    counter.fetch_add(1, Ordering::SeqCst);
                }
                if line
                    .lines()
                    .next()
                    .is_some_and(|line| line.starts_with("GET /health "))
                {
                    health_counter.fetch_add(1, Ordering::SeqCst);
                }
                let body = if line.starts_with("GET /health ") {
                    "{\"status\":\"ok\"}"
                } else {
                    "{\"choices\":[{\"message\":{\"content\":\"ok\"}}]}"
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
            }
        });
        Self {
            address,
            health_requests,
            inference_requests,
            stopped,
            thread: Some(thread),
        }
    }

    fn count(&self) -> usize {
        self.inference_requests.load(Ordering::SeqCst)
    }

    fn health_count(&self) -> usize {
        self.health_requests.load(Ordering::SeqCst)
    }
}

impl Drop for ModelCounter {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("counter server thread exits");
        }
    }
}

async fn install_fake(dir: &Path, samples: &Value, model_url: &str) -> VerifiedBinary {
    let source = dir.join("hyphae-fake.sh");
    let script = r#"#!/bin/sh
set -eu
case "$1:$2" in
  identity:list) printf '%s\n' '{"ok":true,"data":[{"nickname":"alice","npub":"npub1x","default":true,"encrypted":true}]}' ;;
  contact:list) printf '%s\n' '{"ok":true,"data":[]}' ;;
  relay:list) printf '%s\n' '{"ok":true,"data":{"relays":["ws://127.0.0.1:1"],"source":"config","configured":true}}' ;;
  history:inbox) printf '%s\n' '{"ok":true,"data":{"messages":__SAMPLES__}}' ;;
  storage:outbox) printf '%s\n' '{"ok":true,"data":[]}' ;;
  daemon:--identity) python3 -c 'import urllib.request; urllib.request.urlopen("__MODEL_URL__/health", timeout=3).read()'; cat >/dev/null; exec sleep 9999 ;;
  *) printf '%s\n' '{"ok":true,"data":{}}' ;;
esac
"#
    .replace("__MODEL_URL__", model_url)
    .replace(
        "__SAMPLES__",
        &serde_json::to_string(samples).expect("serialize inbound fixtures"),
    );
    std::fs::write(&source, script).expect("write fake Hyphae executable");
    let bytes = tokio::fs::read(&source).await.expect("read fake binary");
    VerifiedBinary::install(&source, sha256_of(&bytes), &dir.join("bin"))
        .await
        .expect("install verified fake binary")
}

async fn post(app: &Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .body(Body::empty())
        .expect("build POST");
    let response = app.clone().oneshot(request).await.expect("route POST");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read response");
    (
        status,
        serde_json::from_slice(&body).expect("JSON response"),
    )
}

async fn get(app: &Router, uri: &str) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("GET")
        .uri(uri)
        .body(Body::empty())
        .expect("build GET");
    let response = app.clone().oneshot(request).await.expect("route GET");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read response");
    (
        status,
        serde_json::from_slice(&body).expect("JSON response"),
    )
}

#[tokio::test]
async fn six_inbound_shapes_and_read_routes_do_not_dispatch_work() {
    let dir = tempfile::tempdir().expect("temporary state directory");
    let counter = ModelCounter::start();
    let baseline_inference = counter.count();
    assert_eq!(baseline_inference, 0, "fresh shared counter starts at zero");
    assert!(
        std::process::Command::new("python3")
            .arg("--version")
            .status()
            .expect("python3 is required for the fake daemon's actual counter probe")
            .success(),
        "python3 is required for the fake daemon's actual counter probe"
    );
    let fixture: Value = serde_json::from_str(SAMPLES).expect("six-message fixture");
    let samples = fixture["samples"].as_array().expect("samples array");
    assert_eq!(samples.len(), 6);
    let model_url = format!("http://{}", counter.address);
    let binary = install_fake(dir.path(), &fixture["samples"], &model_url).await;
    let home = dir.path().join("hyphae-home");
    tokio::fs::create_dir_all(home.join(".hyphae"))
        .await
        .expect("create HOME");
    tokio::fs::write(
        home.join(".hyphae/keystore.json"),
        br#"{"salt":"dGVzdHNhbHQ="}"#,
    )
    .await
    .expect("create fake keystore salt");
    let runner = Arc::new(HyphaeRunner::new(
        binary,
        home.clone(),
        Duration::from_secs(5),
    ));
    let store = Arc::new(MemoryPasswordStore::new());
    store
        .put(
            &Account::from_salt("dGVzdHNhbHQ="),
            &Password::new(b"testpass".to_vec()).expect("password"),
        )
        .await
        .expect("store fixture password");
    let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(DaemonCtx {
        runner: runner.clone(),
        password_store: store.clone(),
        home: home.clone(),
        pid_path: dir.path().join("hyphae-daemon.pid"),
        log_path: dir.path().join("logs/hyphae-daemon.log"),
        autostart_path: dir.path().join("daemon-autostart.json"),
        grace: Duration::from_millis(250),
        ready_after: Duration::from_millis(100),
    }));
    let app = router(CommState::ready(runner, store, home).with_daemon(daemon));

    let (start_status, start_body) = post(&app, "/daemon/start").await;
    assert_eq!(
        start_status,
        StatusCode::OK,
        "start fake daemon: {start_body}"
    );
    let running_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let (status, body) = get(&app, "/daemon").await;
        assert_eq!(status, StatusCode::OK, "get daemon status: {body}");
        if body["data"]["process"]["state"] == "running" {
            break;
        }
        assert!(
            std::time::Instant::now() < running_deadline,
            "fake daemon did not become running: {body}"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let health_deadline = std::time::Instant::now() + Duration::from_secs(3);
    while counter.health_count() == 0 && std::time::Instant::now() < health_deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        counter.health_count() > 0,
        "started fake daemon must call the injected counter URL's health endpoint"
    );
    let mut history_seen = false;
    for _ in 0..20 {
        for path in [
            "/identity",
            "/contact",
            "/relay",
            "/history?as=alice&limit=20",
            "/outbox",
            "/daemon",
        ] {
            let (status, body) = get(&app, path).await;
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            if path.starts_with("/history") {
                assert_eq!(
                    body["data"]["messages"], fixture["samples"],
                    "six inbound shapes pass through unchanged"
                );
                history_seen = true;
            }
        }
    }
    assert!(
        history_seen,
        "the passive history endpoint must be exercised"
    );
    assert_eq!(
        counter.count(),
        baseline_inference,
        "read routes must not reach inference"
    );
    // A health probe is explicitly not counted as inference. A request on
    // the model completion path is the required positive control for this
    // same counter, proving it is live and classifies traffic correctly.
    assert!(
        counter.health_count() > 0,
        "fake daemon must use the injected model health URL"
    );
    assert_eq!(
        counter.count(),
        baseline_inference,
        "health probe is not inference"
    );
    let mut inference =
        std::net::TcpStream::connect(counter.address).expect("counter inference TCP");
    inference
        .write_all(
            b"POST /v1/chat/completions HTTP/1.1\r\nhost: local\r\ncontent-length: 2\r\n\r\n{}",
        )
        .expect("write positive control");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while counter.count() == 0 && std::time::Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        counter.count(),
        baseline_inference + 1,
        "positive control must increment inference count"
    );

    assert_eq!(
        counter.count(),
        baseline_inference + 1,
        "comm read routes caused inference traffic"
    );
    drop(inference);
    let (stop_status, stop_body) = post(&app, "/daemon/stop").await;
    assert_eq!(stop_status, StatusCode::OK, "stop fake daemon: {stop_body}");
    assert_eq!(counter.count(), baseline_inference + 1);
}
