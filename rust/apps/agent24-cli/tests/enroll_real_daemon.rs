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

fn skill_markdown(name: &str, version: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\nversion: {version}\ndescription: d\nrequires: []\nevals: evals/check.jsonl\n---\n{body}"
    )
}

fn write_import(home: &Path, name: &str, body: &str, evals: &str) -> (PathBuf, PathBuf) {
    let dir = home.join("imports").join(name);
    std::fs::create_dir_all(dir.join("evals")).unwrap();
    let skill = dir.join("SKILL.md");
    let eval_path = dir.join("evals").join("check.jsonl");
    std::fs::write(&skill, skill_markdown(name, "1.2.0", body)).unwrap();
    std::fs::write(&eval_path, evals).unwrap();
    (skill, eval_path)
}

fn evals_lines(expects: &[&str]) -> String {
    expects
        .iter()
        .map(|expect| json!({"input": "i", "expect": expect, "judge": "contains"}).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn cli_text(out: &Output) -> (String, String) {
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn pointer_generation(home: &Path, id: &str) -> String {
    sqlite(
        home,
        &format!(
            "SELECT COALESCE((SELECT generation FROM skill_pointer WHERE skill_id='{id}'), -1);"
        ),
    )
}

/// install 之后 list 能看到，show 不打印正文。重扫写入的是用户目录里的另一条。
#[test]
fn real_skill_install_is_listed_and_show_omits_the_body() {
    let host = Host::start("deny");
    let (skill, evals) = write_import(
        host.home(),
        "weekly-report",
        "hello world\n",
        &evals_lines(&["hello world"]),
    );
    let installed = host.cli(&[
        "skill",
        "install",
        skill.to_str().unwrap(),
        "--evals",
        evals.to_str().unwrap(),
    ]);
    let (stdout, stderr) = cli_text(&installed);
    assert!(
        installed.status.success(),
        "stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("weekly-report"), "{stdout}");
    assert!(!stdout.contains("hello world"), "{stdout}");
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "-1");

    let listed = host.cli(&["skill", "list"]);
    let (stdout, stderr) = cli_text(&listed);
    assert!(listed.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("名称"), "{stdout}");
    assert!(stdout.contains("weekly-report"), "{stdout}");
    assert!(stdout.contains("1.2.0"), "{stdout}");
    assert!(stdout.contains("imported"), "{stdout}");
    assert!(stdout.contains("未激活"), "{stdout}");

    let raw = host.cli(&["skill", "list", "--json"]);
    let (stdout, stderr) = cli_text(&raw);
    assert!(raw.status.success(), "stdout={stdout} stderr={stderr}");
    let body: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(body["skills"][0]["skill_id"], "weekly-report");
    assert!(body["skills"][0].get("pointer").is_none(), "{body}");

    let shown = host.cli(&["skill", "show", "weekly-report"]);
    let (stdout, stderr) = cli_text(&shown);
    assert!(shown.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("未激活"), "{stdout}");
    assert!(!stdout.contains("hello world"), "{stdout}");

    let user = host.home().join(".agent24/skills/desk-note/evals");
    std::fs::create_dir_all(&user).unwrap();
    std::fs::write(
        user.parent().unwrap().join("SKILL.md"),
        skill_markdown("desk-note", "1.2.0", "desk\n"),
    )
    .unwrap();
    std::fs::write(user.join("check.jsonl"), evals_lines(&["desk"])).unwrap();
    let scanned = host.cli(&["skill", "rescan"]);
    let (stdout, stderr) = cli_text(&scanned);
    assert!(scanned.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(
        stdout.contains("desk-note") || stdout.contains("写入: 1"),
        "{stdout}"
    );
    let listed = host.cli(&["skill", "list"]);
    let (stdout, _) = cli_text(&listed);
    assert!(stdout.contains("desk-note"), "{stdout}");
    assert!(stdout.contains("user"), "{stdout}");
}

/// 没有桌面确认时激活失败，指针不动。带上假提升头会变成 elevation_invalid，
/// 这条对 presence_denied 和 Waiting 的断言会红。
#[test]
fn real_skill_activate_without_confirmation_does_not_move_the_pointer() {
    let host = Host::start("deny");
    let (skill, evals) = write_import(
        host.home(),
        "weekly-report",
        "hello world\n",
        &evals_lines(&["hello world"]),
    );
    let installed = host.cli(&[
        "skill",
        "install",
        skill.to_str().unwrap(),
        "--evals",
        evals.to_str().unwrap(),
    ]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let out = host.cli_env(
        &["skill", "activate", "weekly-report"],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&out);
    assert!(!out.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(
        stderr.contains("Waiting for approval"),
        "跳过预检直接请求提升以外的失败不会停在桌面确认: {stderr}"
    );
    assert!(
        stderr.contains("presence_denied"),
        "日常令牌直接带假提升头不会得到 presence_denied: {stderr}"
    );
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "-1");
    assert_eq!(
        sqlite(host.home(), "SELECT COUNT(*) FROM skill_activation;"),
        "0"
    );
}

/// 确认之后指针变化。同一目标再激活是 unchanged，代次不变，不再请求确认。
/// 跳过预检会在第二次打出 Waiting；假提升头则第一次就到不了 activated。
#[test]
fn real_skill_activate_confirm_then_unchanged_skips_confirmation() {
    let host = Host::start("allow");
    let (skill, evals) = write_import(
        host.home(),
        "weekly-report",
        "hello world\n",
        &evals_lines(&["hello world"]),
    );
    let installed = host.cli(&[
        "skill",
        "install",
        skill.to_str().unwrap(),
        "--evals",
        evals.to_str().unwrap(),
    ]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let out = host.cli_env(
        &["skill", "activate", "weekly-report"],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&out);
    assert!(out.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("Waiting for approval"), "{stderr}");
    assert!(stdout.contains("activated"), "{stdout}");
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "1");
    let listed = host.cli(&["skill", "list"]);
    let (list_out, _) = cli_text(&listed);
    assert!(list_out.contains('/'), "{list_out}");
    assert!(!list_out.contains("未激活"), "{list_out}");
    assert!(list_out.contains("1/1"), "{list_out}");

    let again = host.cli_env(
        &["skill", "activate", "weekly-report"],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&again);
    assert!(again.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stdout.contains("unchanged"), "{stdout}");
    assert!(
        !stderr.contains("Waiting for approval"),
        "同一目标再激活不应请求确认: {stderr}"
    );
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "1");
    assert_eq!(
        sqlite(
            host.home(),
            "SELECT COUNT(*) FROM skill_activation WHERE skill_id='weekly-report';"
        ),
        "1"
    );
}

/// 未接受的失败打印 skill_eval_failed 和用例摘要，指针不动，也不请求确认。
/// 假提升头得不到这个码；跳过预检则会打出 Waiting。
#[test]
fn real_skill_activate_prints_unaccepted_eval_failure_without_asking() {
    let host = Host::start("allow");
    let (skill, evals) = write_import(
        host.home(),
        "weekly-report",
        "hello world\n",
        &evals_lines(&["hello world", "absent text"]),
    );
    let installed = host.cli(&[
        "skill",
        "install",
        skill.to_str().unwrap(),
        "--evals",
        evals.to_str().unwrap(),
    ]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let out = host.cli_env(
        &["skill", "activate", "weekly-report"],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&out);
    assert!(!out.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("skill_eval_failed"), "{stderr}");
    assert!(stderr.contains("用例:"), "{stderr}");
    assert!(stderr.contains("passed=false"), "{stderr}");
    assert!(stderr.contains("失败:"), "{stderr}");
    assert!(!stderr.contains("absent text"), "{stderr}");
    assert!(
        !stderr.contains("Waiting for approval"),
        "预检就该返回评测失败，不应请求确认: {stderr}"
    );
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "-1");

    let failed_line = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("失败:"))
        .unwrap()
        .trim();
    assert!(
        failed_line.starts_with("--accept "),
        "失败提示要能直接复制: {stderr}"
    );
    let failed = failed_line.trim_start_matches("--accept ").trim();
    assert!(!failed.is_empty() && !failed.contains(' '), "{failed_line}");
    let accepted = host.cli_env(
        &["skill", "activate", "weekly-report", "--accept", failed],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&accepted);
    assert!(accepted.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("Waiting for approval"), "{stderr}");
    assert!(stdout.contains("activated"), "{stdout}");
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "1");
}

/// 激活两次后回滚到第一次，指针回到那一对。确认夹具与其它 activate 用例相同。
#[test]
fn real_skill_activate_rollback_restores_the_first_pair() {
    let host = Host::start("allow");
    let (skill, evals) = write_import(
        host.home(),
        "weekly-report",
        "hello world\n",
        &evals_lines(&["hello world"]),
    );
    let installed = host.cli(&[
        "skill",
        "install",
        skill.to_str().unwrap(),
        "--evals",
        evals.to_str().unwrap(),
    ]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let first = host.cli_env(
        &["skill", "activate", "weekly-report"],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&first);
    assert!(first.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("Waiting for approval"), "{stderr}");
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "1");
    let first_pair = pointer_revisions(host.home(), "weekly-report");

    // 同 semver 改正文会被拒。只升版本，评测字节不动，第二次激活只换正文一侧。
    std::fs::write(
        &skill,
        skill_markdown("weekly-report", "1.2.1", "hello world again\n"),
    )
    .unwrap();
    let installed = host.cli(&[
        "skill",
        "install",
        skill.to_str().unwrap(),
        "--evals",
        evals.to_str().unwrap(),
    ]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    let second = host.cli_env(
        &["skill", "activate", "weekly-report"],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&second);
    assert!(second.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("Waiting for approval"), "{stderr}");
    assert!(stdout.contains("activated"), "{stdout}");
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "2");
    assert_ne!(pointer_revisions(host.home(), "weekly-report"), first_pair);

    let shown = host.cli(&["skill", "show", "weekly-report", "--json"]);
    let (stdout, stderr) = cli_text(&shown);
    assert!(shown.status.success(), "stdout={stdout} stderr={stderr}");
    let detail: Value = serde_json::from_str(stdout.trim()).unwrap();
    let activation_id = detail["activations"][0]["activation_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        format!(
            "{}/{}",
            detail["activations"][0]["body_revision_id"]
                .as_str()
                .unwrap(),
            detail["activations"][0]["evals_revision_id"]
                .as_str()
                .unwrap()
        ),
        first_pair
    );

    let rolled = host.cli_env(
        &[
            "skill",
            "activate",
            "weekly-report",
            "--rollback",
            &activation_id,
        ],
        &[],
        Duration::from_secs(30),
    );
    let (stdout, stderr) = cli_text(&rolled);
    assert!(rolled.status.success(), "stdout={stdout} stderr={stderr}");
    assert!(stderr.contains("Waiting for approval"), "{stderr}");
    assert!(stdout.contains("rolled_back"), "{stdout}");
    assert_eq!(pointer_revisions(host.home(), "weekly-report"), first_pair);
    assert_eq!(pointer_generation(host.home(), "weekly-report"), "3");
}

fn pointer_revisions(home: &Path, id: &str) -> String {
    sqlite(
        home,
        &format!(
            "SELECT body_revision_id || '/' || evals_revision_id FROM skill_pointer WHERE skill_id='{id}';"
        ),
    )
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
