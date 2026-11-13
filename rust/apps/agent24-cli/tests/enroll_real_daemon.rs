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

#[path = "support/signature_command.rs"]
mod signature_command;

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
        Self::start_with(decision, &[])
    }

    fn start_with(decision: &str, extra: &[(&str, &str)]) -> Self {
        let home = tempfile::Builder::new()
            .prefix("a24real")
            .tempdir_in("/tmp")
            .unwrap();
        let stderr_path = home.path().join("daemon-stderr.log");
        let stderr = std::fs::File::create(&stderr_path).unwrap();
        let mut cmd = signature_command::clear_command(daemon_binary());
        cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", home.path())
            .env("AGENT24_TEST_PRESENCE", decision);
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let mut child = cmd
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
        let bearer = self.token.clone();
        self.request_as(method, path, &bearer, body)
    }

    fn request_as(&self, method: &str, path: &str, bearer: &str, body: &Value) -> (u16, Value) {
        let body = body.to_string();
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        write!(stream, "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{}\r\nauthorization: Bearer {bearer}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}", self.port, body.len()).unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).unwrap();
        let status = response.split_whitespace().nth(1).unwrap().parse().unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        (status, serde_json::from_str(body).unwrap())
    }

    /// Run the real CLI with `args`, under the same isolation as [`Self::add`].
    fn cli(&self, args: &[&str]) -> Output {
        self.cli_env(args, &[], Duration::from_secs(15))
    }

    fn cli_env(&self, args: &[&str], extra: &[(&str, &str)], limit: Duration) -> Output {
        let mut cmd = signature_command::clear_command(env!("CARGO_BIN_EXE_agent24"));
        cmd.env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self._home.path())
            .env(
                "AGENT24D_BIN",
                self._home.path().join("no-ephemeral-daemon"),
            );
        for (key, value) in extra {
            cmd.env(key, value);
        }
        let mut child = cmd
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + limit;
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
        let mut child = signature_command::clear_command(env!("CARGO_BIN_EXE_agent24"))
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

/// Desktop confirm in this process is `AGENT24_TEST_PRESENCE=allow` on the
/// daemon (`test-enrollment`). There is no desktop window in the test.
#[test]
fn real_bridge_token_ingests_one_wechat_message() {
    assert_eq!(stamp(0), "1970-01-01T00:00:00Z");
    let host = Host::start_with("allow", &[("A24_DEV_TOOL", "1")]);
    let (status, listed) = host.request(
        "PUT",
        "/api/v1/inbound/allowlist",
        &json!({"entries":[{"source":"wechat","actor":"alice"}]}),
    );
    assert_eq!(status, 200, "{listed}");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    let activated = stamp(now - 120);
    let at = stamp(now);
    sqlite(
        host.home(),
        &format!(
            "INSERT INTO inbound_source_watermark (source, activated_at, max_age_secs) \
             VALUES ('wechat', '{activated}', 600);"
        ),
    );
    let out = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "stderr={stderr} stdout_has_tokenish={}",
        out.stdout
            .windows(8)
            .any(|w| w == b"eyJ" || w.starts_with(b"a24"))
    );
    let stdout = String::from_utf8(out.stdout).unwrap();
    let path = host.home().join(".agent24/dev/wechat-bridge.token");
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    assert_eq!(mode(&path), 0o600);
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let token = file["token"].as_str().unwrap();
    let expires_at = file["expires_at"].as_u64().unwrap();
    assert!(!token.is_empty());
    assert!(!stdout.contains(token), "stdout leaked the bearer");
    assert!(!stderr.contains(token), "stderr leaked the bearer");
    assert_eq!(
        stdout,
        format!("{}\nexpires_at: {expires_at}\n", path.display())
    );
    let body = json!({
        "id": "wechat:1001",
        "actor": "alice",
        "at": at,
        "text": "hello",
    });
    let (status, first) = host.request_as("POST", "/api/v1/inbound/events", token, &body);
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["outcome"], "started", "{first}");
    let (status, again) = host.request_as("POST", "/api/v1/inbound/events", token, &body);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["outcome"], "duplicate", "{again}");
    assert_eq!(sqlite(host.home(), "SELECT COUNT(*) FROM runs;"), "1");
}

/// The daemon gate is off. The CLI must not ask the desktop and then 404.
/// Answering `presence_required` before the 404 turns the approval assertion red.
#[test]
fn bridge_token_does_not_elevate_when_the_daemon_gate_is_off() {
    let host = Host::start("allow");
    let out = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("没有设置 A24_DEV_TOOL=1"), "{stderr}");
    assert!(!stderr.contains("Waiting for approval"), "{stderr}");
}

/// A second issue revokes the id still in the file. The first bearer is 401.
/// Skipping that revoke leaves the first bearer able to post.
#[test]
fn a_second_bridge_token_revokes_the_first() {
    let host = Host::start_with("allow", &[("A24_DEV_TOOL", "1")]);
    let first = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    assert!(
        first.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let path = host.home().join(".agent24/dev/wechat-bridge.token");
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let first_token = file["token"].as_str().unwrap().to_owned();
    let second = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(second.status.success(), "{stderr}");
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let second_token = file["token"].as_str().unwrap();
    assert_ne!(first_token, second_token);
    let (status, body) = host.request_as(
        "POST",
        "/api/v1/inbound/events",
        &first_token,
        &json!({"id": "wechat:1001", "actor": "alice", "at": stamp(0), "text": "hello"}),
    );
    assert_eq!(status, 401, "{body}");
    let (status, body) = host.request_as(
        "POST",
        "/api/v1/inbound/events",
        second_token,
        &json!({"id": "wechat:1002", "actor": "alice", "at": stamp(0), "text": "hello"}),
    );
    assert_ne!(status, 401, "{body}");
}

/// A re-issue that cannot publish the new file must not revoke the bearer
/// still in the file. The user-immutable flag blocks the rename. The first
/// bearer is not 401. Clearing the flag and issuing again makes it 401.
/// Revoking before the write turns the first assertion red.
#[test]
fn a_failed_reissue_leaves_the_previous_bridge_token_usable() {
    let host = Host::start_with("allow", &[("A24_DEV_TOOL", "1")]);
    let first = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    assert!(
        first.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&first.stderr)
    );
    let path = host.home().join(".agent24/dev/wechat-bridge.token");
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let first_token = file["token"].as_str().unwrap().to_owned();
    set_user_immutable(&path, true);
    let _clear = ClearUserImmutable(&path);
    let second = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    let stderr = String::from_utf8_lossy(&second.stderr);
    assert!(!second.status.success(), "{stderr}");
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(file["token"].as_str().unwrap(), first_token);
    let (status, body) = host.request_as(
        "POST",
        "/api/v1/inbound/events",
        &first_token,
        &json!({"id": "wechat:1001", "actor": "alice", "at": stamp(0), "text": "hello"}),
    );
    assert_ne!(status, 401, "{body}");
    set_user_immutable(&path, false);
    let third = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    let stderr = String::from_utf8_lossy(&third.stderr);
    assert!(third.status.success(), "{stderr}");
    let (status, body) = host.request_as(
        "POST",
        "/api/v1/inbound/events",
        &first_token,
        &json!({"id": "wechat:1002", "actor": "alice", "at": stamp(0), "text": "hello"}),
    );
    assert_eq!(status, 401, "{body}");
}

fn set_user_immutable(path: &Path, on: bool) {
    let flag = if on { "uchg" } else { "nouchg" };
    let out = Command::new("chflags")
        .arg(flag)
        .arg(path)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "chflags {flag}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `uchg` blocks deleting the temp home. Clear it even when the test panics.
struct ClearUserImmutable<'a>(&'a Path);

impl Drop for ClearUserImmutable<'_> {
    fn drop(&mut self) {
        let _ = Command::new("chflags").arg("nouchg").arg(self.0).status();
    }
}

/// The shell revokes the bridge token it just wrote. The bearer is then 401
/// and the file is gone. Skipping the delete leaves the path in place.
#[test]
fn real_shell_revokes_its_bridge_token() {
    let host = Host::start_with("allow", &[("A24_DEV_TOOL", "1")]);
    let out = host.cli_env(
        &["dev", "bridge-token"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    assert!(
        out.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let path = host.home().join(".agent24/dev/wechat-bridge.token");
    let file: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let token = file["token"].as_str().unwrap().to_owned();
    assert!(!file["capability_id"].as_str().unwrap().is_empty());
    let revoked = host.cli_env(
        &["dev", "bridge-token", "--revoke"],
        &[("A24_DEV_TOOL", "1")],
        Duration::from_secs(30),
    );
    let stderr = String::from_utf8_lossy(&revoked.stderr);
    assert!(revoked.status.success(), "{stderr}");
    assert!(stderr.contains("Waiting for approval"), "{stderr}");
    assert!(!path.exists());
    let (status, body) = host.request_as(
        "POST",
        "/api/v1/inbound/events",
        &token,
        &json!({"id": "wechat:1001", "actor": "alice", "at": stamp(0), "text": "hello"}),
    );
    assert_eq!(status, 401, "{body}");
}

impl Host {
    fn home(&self) -> &Path {
        self._home.path()
    }
}

fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::symlink_metadata(path)
        .unwrap()
        .permissions()
        .mode()
        & 0o777
}

fn sqlite(home: &Path, statement: &str) -> String {
    let db = home.join(".agent24/agent24.db");
    let out = Command::new("sqlite3")
        .arg(&db)
        .arg(format!("PRAGMA busy_timeout=5000; {statement}"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "sqlite {statement}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // `PRAGMA busy_timeout` prints the value it set. The statement result is
    // the last line.
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .rev()
        .find(|line| !line.is_empty())
        .unwrap_or("")
        .to_owned()
}

/// `YYYY-MM-DDTHH:MM:SSZ` from a unix second. Same spelling the daemon stores.
fn stamp(unix: i64) -> String {
    let days = unix.div_euclid(86_400);
    let sod = unix.rem_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = u64::try_from(z - era * 146_097).unwrap();
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = i64::try_from(yoe).unwrap() + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    let hour = sod / 3600;
    let minute = (sod % 3600) / 60;
    let second = sod % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}
