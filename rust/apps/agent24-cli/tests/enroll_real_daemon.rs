//! S4 integration: real CLI and capabilities child daemon, no fake HTTP/UDS server.
//! S3 is absent here, so only the daemon's test-enrollment feature substitutes
//! presence and unsigned identity. S5 must validate signed binaries on macOS.
//!
//! The daemon speaks first (`challenge`) and the CLI's `hello` must echo its
//! nonce (design §10 R8), so these tests are the end-to-end check that the
//! real CLI and the real daemon agree on that order. macOS only: elsewhere
//! the daemon has no per-writer peer token and rejects every enrolment frame
//! by design (slice §3.1), so enrolment can never succeed there.

#![cfg(target_os = "macos")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn daemon_binary() -> &'static Path {
    static BINARY: OnceLock<PathBuf> = OnceLock::new();
    BINARY.get_or_init(|| {
        let exe = std::env::current_exe().unwrap();
        let profile = exe.parent().unwrap().parent().unwrap();
        // Isolate the test build so it cannot replace the normal daemon binary
        // or contend with the outer cargo test's artifact lock.
        let target = profile.parent().unwrap().join("enroll-e2e");
        let out = Command::new(env!("CARGO"))
            .args([
                "build",
                "-p",
                "agent24d",
                "--features",
                "test-enrollment",
                "--bin",
                "agent24d",
                "--target-dir",
            ])
            .arg(&target)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        target.join("debug/agent24d")
    })
}

struct Host {
    child: Child,
    _lease: ChildStdin,
    _home: tempfile::TempDir,
    port: u16,
    token: String,
}

impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Host {
    fn start(decision: &str) -> Self {
        let home = tempfile::Builder::new()
            .prefix("a24real")
            .tempdir_in("/tmp")
            .unwrap();
        let stderr_path = home.path().join("daemon-stderr.log");
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let mut child = Command::new(daemon_binary())
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("AGENT24_TEST_PRESENCE", decision)
            .args([
                "serve",
                "--auth-mode",
                "capabilities",
                "--host-bootstrap-stdio",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .spawn()
            .unwrap();
        let lease = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        // Own the child immediately, so even a failed startup is reaped.
        let mut host = Self {
            child,
            _lease: lease,
            _home: home,
            port: 0,
            token: String::new(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut line = String::new();
            BufReader::new(stdout).read_line(&mut line).unwrap();
            let _ = tx.send(line);
        });
        let line = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("real daemon ready record");
        let ready: Value = serde_json::from_str(&line).unwrap_or_else(|err| {
            panic!(
                "real daemon startup failed: {err}; {}",
                std::fs::read_to_string(stderr_path).unwrap()
            );
        });
        assert_eq!(ready["auth_mode"], "capabilities");
        host.port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
        host.token = ready["product_host_token"].as_str().unwrap().to_owned();
        let discovery: Value = serde_json::from_slice(
            &std::fs::read(host._home.path().join(".agent24/daemon.json")).unwrap(),
        )
        .unwrap();
        assert!(discovery.get("token").is_none());
        assert!(discovery["enroll_endpoint"].is_string());
        host
    }

    fn request(&self, method: &str, path: &str, body: &Value) -> (u16, Value) {
        let body = body.to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nauthorization: Bearer {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", self.port, self.token, body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        (status, serde_json::from_str(body).unwrap())
    }

    /// Run the real CLI with `args`, under the same isolation as [`Self::add`].
    fn cli(&self, args: &[&str]) -> Output {
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent24"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self._home.path())
            .env(
                "AGENT24D_BIN",
                self._home.path().join("no-ephemeral-daemon"),
            )
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("real CLI exceeded integration deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }

    fn add(&self) -> Output {
        let manifest = self._home.path().join("agentear.yml");
        std::fs::write(&manifest, manifest_text("agentear")).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_agent24"))
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self._home.path())
            // Discovery failure must not silently start a legacy daemon.
            .env(
                "AGENT24D_BIN",
                self._home.path().join("no-ephemeral-daemon"),
            )
            .args(["os", "attach", "add"])
            .arg(manifest)
            .arg("--json")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("real CLI exceeded integration deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
    }
}

fn manifest_text(name: &str) -> String {
    format!(
        "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\nevent_module: {name}\ndata_dir: ~/.agent24/os/{name}/\nimpl_kind: attached_process\nkernel_capabilities: [events, models]\n"
    )
}

#[test]
fn real_capabilities_attach_allow_matches_201_shape() {
    let host = Host::start("allow");
    let out = host.add();
    assert!(
        out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let body: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["name"], "agentear");
    assert!(body["token"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Waiting for approval in the Agent24 app")
    );
    // Compare against an actual 201 from the same real handler using ProductHost.
    let (status, baseline) = host.request(
        "POST",
        "/api/v1/attached",
        &json!({"manifest":manifest_text("baseline"), "allow_relax":false}),
    );
    assert_eq!(status, 201, "baseline ProductHost attach: {baseline}");
    assert_eq!(
        body.as_object().unwrap().keys().collect::<Vec<_>>(),
        baseline.as_object().unwrap().keys().collect::<Vec<_>>()
    );
    let (status, list) = host.request("GET", "/api/v1/attached", &Value::Null);
    assert_eq!(status, 200);
    assert!(
        list["modules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["name"] == "agentear")
    );
}

#[test]
fn real_capabilities_attach_deny_has_no_registration() {
    let host = Host::start("deny");
    let out = host.add();
    assert!(
        !out.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let body: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "presence_denied", "{body}");
    let (status, list) = host.request("GET", "/api/v1/attached", &Value::Null);
    assert_eq!(status, 200);
    assert!(list["modules"].as_array().unwrap().is_empty());
}

/// I-1: `idoris status` enrols with the real daemon and reads the everyday
/// status route with the first-party token, without an approval.
#[test]
fn real_capabilities_idoris_status_json() {
    let host = Host::start("deny");
    let out = host.cli(&["idoris", "status", "--json"]);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
    assert_eq!(out.stdout.iter().filter(|b| **b == b'\n').count(), 1);
    let body: Value = serde_json::from_slice(&out.stdout).unwrap();
    let (status, baseline) = host.request("GET", "/api/v1/idoris/status", &Value::Null);
    assert_eq!(status, 200, "baseline ProductHost status: {baseline}");
    assert_eq!(body["state"], baseline["state"], "{body}");
    assert!(!stderr.contains("Waiting for approval"), "{stderr}");
}

/// I-1: an elevated iDoris route through the real daemon's presence path.
/// Approved, the request reaches the lifecycle handler; denied, it does not.
#[test]
fn real_capabilities_idoris_stop_elevates() {
    for decision in ["allow", "deny"] {
        let host = Host::start(decision);
        let out = host.cli(&["idoris", "stop", "--json"]);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains("Waiting for approval in the Agent24 app"),
            "{stderr}"
        );
        let body: Value = serde_json::from_slice(&out.stdout).unwrap();
        if decision == "allow" {
            assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
            assert_eq!(body["action"], "stop", "{body}");
            assert_eq!(body["result"], "succeeded", "{body}");
        } else {
            assert!(!out.status.success(), "stdout={stdout}");
            assert_eq!(body["error"]["code"], "presence_denied", "{body}");
        }
    }
}
