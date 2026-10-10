//! T2 drives the real router in a child test process so model URL environment
//! injection cannot race other workspace tests. Python is mandatory.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::{
    Account, CommState, DaemonCtx, HyphaeDaemonSupervisor, HyphaeRunner, MemoryPasswordStore,
    Password, PasswordStore, VerifiedBinary, binary::sha256_of, router,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use tower::ServiceExt;

const SAMPLES: &str = include_str!("fixtures/comm_inbound_six.json");
const COUNTER: &str = r#"
import json
from http.server import BaseHTTPRequestHandler, HTTPServer
count = 0
class Handler(BaseHTTPRequestHandler):
 def log_message(self, *args): pass
 def do_GET(self):
  global count
  if self.path != '/counts': count += 1
  body = json.dumps({'requests': count}).encode()
  self.send_response(200); self.send_header('Content-Length', str(len(body))); self.end_headers(); self.wfile.write(body)
 def do_POST(self):
  global count
  self.rfile.read(int(self.headers.get('Content-Length', 0)))
  count += 1
  self.send_response(200); self.send_header('Content-Length', '2'); self.end_headers(); self.wfile.write(b'{}')
server = HTTPServer(('127.0.0.1', 0), Handler)
print(server.server_port, flush=True)
server.serve_forever()
"#;

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn stub_request(port: u16, method: &str, path: &str) -> Value {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    write!(
        stream,
        "{method} {path} HTTP/1.0\r\nhost: localhost\r\ncontent-length: 0\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap()
}

async fn call(app: &Router, method: &str, uri: &str, body: Value) -> Value {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
            .unwrap();
    assert_eq!(status, StatusCode::OK, "{method} {uri}: {body}");
    body
}

async fn scenario(root: &Path) {
    let fixture: Value = serde_json::from_str(SAMPLES).unwrap();
    let samples = fixture["samples"].clone();
    assert_eq!(
        samples
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["category"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "plain",
            "kind-30078",
            "query",
            "response",
            "receipt",
            "canary"
        ]
    );
    let source = root.join("hyphae-fake.sh");
    let script = r#"#!/bin/sh
set -eu
case "$1:$2" in
 identity:list) echo '{"ok":true,"data":[{"nickname":"alice","npub":"npub1x","default":true,"encrypted":true}]}' ;;
 contact:list) echo '{"ok":true,"data":[]}' ;;
 relay:list) echo '{"ok":true,"data":{"relays":["ws://127.0.0.1:1"],"source":"config","configured":true}}' ;;
 history:inbox|storage:outbox) cat "$HOME/.hyphae/zero-run-samples.json" ;;
 daemon:--identity) cat >/dev/null; ticks=0; while [ "$ticks" -lt 600 ] && kill -0 __TEST_PID__ 2>/dev/null; do ticks=$((ticks+1)); echo comm5b-alive; sleep 0.1; done ;;
 agent:inbox) echo '{"ok":true,"data":{}}' ;;
 *) exit 1 ;;
esac
"#.replace("__TEST_PID__", &std::process::id().to_string());
    std::fs::write(&source, script).unwrap();
    let binary = VerifiedBinary::install(
        &source,
        sha256_of(&tokio::fs::read(&source).await.unwrap()),
        &root.join("bin"),
    )
    .await
    .unwrap();
    let home = root.join("hyphae-home");
    tokio::fs::create_dir_all(home.join(".hyphae"))
        .await
        .unwrap();
    tokio::fs::write(
        home.join(".hyphae/keystore.json"),
        br#"{"salt":"dGVzdHNhbHQ="}"#,
    )
    .await
    .unwrap();
    tokio::fs::write(
        home.join(".hyphae/zero-run-samples.json"),
        serde_json::to_vec(&json!({"ok":true,"data":samples})).unwrap(),
    )
    .await
    .unwrap();
    let runner = Arc::new(HyphaeRunner::new(
        binary,
        home.clone(),
        Duration::from_secs(5),
    ));
    let store = Arc::new(MemoryPasswordStore::new());
    store
        .put(
            &Account::from_salt("dGVzdHNhbHQ="),
            &Password::new(b"testpass".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    let log = root.join("logs/hyphae-daemon.log");
    let daemon = Arc::new(HyphaeDaemonSupervisor::spawn(DaemonCtx {
        runner: runner.clone(),
        password_store: store.clone(),
        home: home.clone(),
        pid_path: root.join("hyphae-daemon.pid"),
        log_path: log.clone(),
        autostart_path: root.join("daemon-autostart.json"),
        grace: Duration::from_millis(250),
        ready_after: Duration::from_millis(100),
    }));
    let app = router(CommState::ready(runner, store, home).with_daemon(daemon.clone()));
    let result = tokio::spawn(async move {
        call(&app, "POST", "/daemon/start", json!({})).await;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            let body = call(&app, "GET", "/daemon", Value::Null).await;
            if body["data"]["process"]["state"] == "running"
                && tokio::fs::read_to_string(&log)
                    .await
                    .unwrap_or_default()
                    .contains("comm5b-alive")
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "daemon readiness: {body}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        call(&app, "POST", "/inbox/pull", json!({"as":"alice"})).await;
        for _ in 0..20 {
            for path in [
                "/identity",
                "/contact",
                "/relay",
                "/history?as=alice&limit=20",
                "/outbox",
                "/daemon",
            ] {
                let body = call(&app, "GET", path, Value::Null).await;
                if path.starts_with("/history") || path == "/outbox" {
                    assert_eq!(body["data"], samples, "passive payload must be unchanged");
                }
            }
        }
        call(&app, "POST", "/daemon/stop", json!({})).await;
    })
    .await;
    daemon
        .shutdown(tokio::time::Instant::now() + Duration::from_secs(5))
        .await;
    if let Err(error) = result {
        std::panic::resume_unwind(error.into_panic());
    }
}

#[test]
fn six_inbound_shapes_and_read_routes_do_not_dispatch_work() {
    if let Some(root) = std::env::var_os("COMM5B_T2_CHILD_ROOT") {
        // Verify all fallback endpoints really refer to the parent's counter.
        let urls: Vec<_> = ["OLLAMA_URL", "OPENAI_BASE_URL", "A24_BASE_URL", "OMLX_URL"]
            .map(|k| std::env::var(k).unwrap())
            .into();
        assert!(urls.iter().all(|url| url == &urls[0]));
        tokio::runtime::Runtime::new()
            .unwrap()
            .block_on(scenario(Path::new(&root)));
        return;
    }
    let mut counter = ChildGuard(
        Command::new("python3")
            .args(["-u", "-c", COUNTER])
            .stdout(Stdio::piped())
            .spawn()
            .expect("python3 is required; missing Python fails T2"),
    );
    let stdout = counter.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        tx.send(line).unwrap();
    });
    let port: u16 = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("counter readiness")
        .trim()
        .parse()
        .unwrap();
    assert_eq!(stub_request(port, "GET", "/counts")["requests"], 0);
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap());
    child
        .args([
            "--exact",
            "six_inbound_shapes_and_read_routes_do_not_dispatch_work",
            "--nocapture",
        ])
        .env("COMM5B_T2_CHILD_ROOT", root.path());
    for key in ["OLLAMA_URL", "OPENAI_BASE_URL", "A24_BASE_URL", "OMLX_URL"] {
        child.env(key, format!("http://127.0.0.1:{port}"));
    }
    let mut child = ChildGuard(child.spawn().unwrap());
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success(), "T2 child failed");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "T2 child timeout");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        stub_request(port, "GET", "/counts")["requests"],
        0,
        "startup, passive history/outbox, poll and stop must cause zero backend/module requests"
    );
    // Same live stub: ANY business request is counted, including health and
    // unknown routes. These controls happen only after the zero-run assertion.
    stub_request(port, "POST", "/v1/chat/completions");
    stub_request(port, "POST", "/module-call");
    stub_request(port, "GET", "/unexpected-health");
    assert_eq!(stub_request(port, "GET", "/counts")["requests"], 3);
}
