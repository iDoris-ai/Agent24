//! ME4-5.4.1 / T14 — the black-box acceptance test for
//! `docs/specs/WIRE-OOP-MODULE.md` & `docs/design/A3-ATTACHED-MODULE.md`:
//! **does the document alone let a different-language implementation work,
//! without looking at this repo's own Rust protocol code?**
//!
//! `examples/node-module/` was written ONLY against WIRE-OOP-MODULE.md and
//! A3-ATTACHED-MODULE.md — it does not `require`/`import` any crate from this
//! repository. Per task N3, installed packages must be signed native executables,
//! so interpreter-based modules (such as Node.js) attach to the daemon via the
//! A3 attach route (`POST /api/v1/attached`).
//!
//! This test drives the Node reference module as an attached module: build the
//! daemon once, register the module's manifest via the REST attach API, launch
//! the Node process independently to connect to the attach socket, and prove
//! handshake, event emission, and probe persistence all round-trip for real over
//! the wire.
//!
//! **`node` availability**: a missing `node` is a SKIP with an explicit printed
//! reason, not a panic.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "support/signature_command.rs"]
mod signature_command;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The root of the git checkout, computed from this crate's own manifest
/// directory (`rust/apps/agent24d`) rather than assumed.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // rust/apps
        .and_then(Path::parent) // rust
        .and_then(Path::parent) // repo root
        .expect("rust/apps/agent24d is two directories under the repo root")
        .to_path_buf()
}

/// `Some(reason)` if `node` is not runnable on this machine (only `NotFound` skips).
fn missing_node() -> Option<String> {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => None,
        Ok(out) => panic!(
            "`node --version` exited non-zero ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            Some(format!("`node` is not runnable on PATH: {e}"))
        }
        Err(e) => panic!("`node --version` failed: {e}"),
    }
}

fn raw_request(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    body: &str,
) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\nauthorization: Bearer {token}\r\n\
         connection: close\r\ncontent-length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(body.as_bytes());
    s.write_all(&req).ok()?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).ok()?;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let status: u16 = head
        .split("\r\n")
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    Some((status, resp_body.to_owned()))
}

fn get(port: u16, token: &str, path: &str) -> Option<(u16, String)> {
    raw_request(port, token, "GET", path, "")
}

fn post(port: u16, token: &str, path: &str, body: &serde_json::Value) -> (u16, String) {
    raw_request(port, token, "POST", path, &body.to_string())
        .unwrap_or((0, "connection failed".into()))
}

/// The daemon or subprocess — graceful-first on drop.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_some() {
            return;
        }
        #[allow(clippy::cast_possible_wrap)]
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32)
            && rustix::process::kill_process(pid, rustix::process::Signal::Term).is_ok()
        {
            let by = Instant::now() + Duration::from_secs(10);
            while self.0.try_wait().is_ok_and(|s| s.is_none()) && Instant::now() < by {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Daemon {
    #[allow(dead_code)]
    run: Running,
    port: u16,
    token: String,
    stderr: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Daemon {
    fn recent_stderr(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }
}

/// Start the already-built `agent24d` binary against `home`.
fn start(home: &Path) -> Daemon {
    let mut child = signature_command::clear_command(env!("CARGO_BIN_EXE_agent24d"))
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let run = Running(child);
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    let stderr_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = stderr_lines.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    Daemon {
        run,
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
        stderr: stderr_lines,
    }
}

fn stop(d: Daemon) {
    drop(d);
}

fn attached_list_entry(port: u16, token: &str, name: &str) -> serde_json::Value {
    let (status, body) =
        get(port, token, "/api/v1/attached").expect("the daemon answered /api/v1/attached");
    assert_eq!(status, 200, "{body}");
    let list: serde_json::Value = serde_json::from_str(&body).expect("the list");
    list["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} in the attached list: {body}"))
}

static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_home() -> tempfile::TempDir {
    let n = HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    tempfile::Builder::new()
        .prefix(&format!("a24node-{n}-"))
        .rand_bytes(4)
        .tempdir_in("/tmp")
        .unwrap()
}

/// Connect to the real `GET /api/v1/events` WS endpoint.
fn spawn_ws_subscriber(port: u16, token: &str) -> std::sync::mpsc::Receiver<serde_json::Value> {
    use tokio_tungstenite::tungstenite;
    use tungstenite::client::IntoClientRequest;

    let (tx, rx) = std::sync::mpsc::channel();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let url = format!("ws://127.0.0.1:{port}/api/v1/events");
    let token = token.to_owned();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async move {
            let mut request = url.into_client_request().expect("a valid ws:// url");
            request
                .headers_mut()
                .insert("Authorization", format!("Bearer {token}").parse().unwrap());
            let (mut socket, _) = tokio_tungstenite::connect_async(request)
                .await
                .expect("the real WS upgrade must succeed");
            let _ = ready_tx.send(());
            use futures::StreamExt;
            while let Some(Ok(tungstenite::Message::Text(text))) = socket.next().await {
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
                    continue;
                };
                if tx.send(value).is_err() {
                    break;
                }
            }
        });
    });
    ready_rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the WS subscriber never finished its upgrade");
    rx
}

/// **ME4-5.4.1 / T14 (Task N3) — the acceptance test.**
/// Proves that the Node reference module in `examples/node-module/`, written
/// strictly against the wire specifications without Rust crate dependencies,
/// attaches successfully to `agent24d` via `POST /api/v1/attached`, completes
/// the `initialize` handshake, and successfully executes a callback round trip
/// (`_a24/events/emit`) whose result is durably observed on the WS subscriber.
#[test]
fn a_node_module_written_only_against_the_wire_doc() {
    if let Some(reason) = missing_node() {
        eprintln!(
            "SKIP me4_node_module_blackbox::a_node_module_written_only_against_the_wire_doc: {reason}"
        );
        return;
    }

    let home = tmp_home();
    let d = start(home.path());

    // ── 1. Initial state: nothing attached ──────────────────────────────
    let empty = get(d.port, &d.token, "/api/v1/attached").expect("the daemon answered");
    assert_eq!(empty.0, 200, "{}", empty.1);
    let list: serde_json::Value = serde_json::from_str(&empty.1).unwrap();
    assert!(
        list["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "node-ref"),
        "node-ref must not exist before it has been registered: {}",
        empty.1
    );

    // ── 2. Attach registration via REST ─────────────────────────────────
    let manifest_path = repo_root().join("examples/node-module/domain-os.yml");
    let manifest_yaml = std::fs::read_to_string(&manifest_path)
        .unwrap_or_else(|e| panic!("reading {}: {e}", manifest_path.display()));
    let (status, reg_body) = post(
        d.port,
        &d.token,
        "/api/v1/attached",
        &serde_json::json!({
            "manifest": manifest_yaml,
            "allow_relax": false,
        }),
    );
    assert_eq!(
        status,
        201,
        "registration failed: {status} {reg_body} — daemon stderr:\n{}",
        d.recent_stderr()
    );
    let reg: serde_json::Value =
        serde_json::from_str(&reg_body).expect("the registration response");
    assert_eq!(reg["name"], "node-ref");
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let auth_token = reg["token"].as_str().unwrap().to_owned();

    // Before connecting, status is detached
    let entry = attached_list_entry(d.port, &d.token, "node-ref");
    assert_eq!(entry["attach_status"], "detached", "{entry}");

    // ── 3. Subscribe to the real WS event consumer ──────────────────────
    let events = spawn_ws_subscriber(d.port, &d.token);

    // ── 4. Launch Node reference module process ─────────────────────────
    let script = repo_root().join("examples/node-module/index.js");
    let node_data_dir = home.path().join(".agent24/os/node-ref");
    let node_child = Command::new("node")
        .arg(&script)
        .arg(&socket_path)
        .arg(&auth_token)
        .arg(&node_data_dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning node reference module");
    let mut node_run = Running(node_child);

    // ── 5. Event forwarding, observed at the real WS consumer boundary ──
    let overall_deadline = Instant::now() + Duration::from_secs(15);
    let event = loop {
        let remaining = overall_deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the WS subscriber never received the node module's event; daemon stderr:\n{}",
            d.recent_stderr()
        );
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(event) if event["type"] == "module" && event["payload"]["module"] == "node-ref" => {
                break event;
            }
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "the WS subscriber thread ended; daemon stderr:\n{}",
                    d.recent_stderr()
                )
            }
        }
    };
    assert_eq!(event["payload"]["module"], "node-ref", "{event}");
    assert_eq!(event["payload"]["kind"], "task.transitioned", "{event}");
    assert_eq!(event["payload"]["payload"]["phase"], "startup", "{event}");

    // ── 6. Verification of the probe snapshot file ───────────────────────
    let probe_path = node_data_dir.join("startup_probe.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    let probe = loop {
        if let Ok(text) = std::fs::read_to_string(&probe_path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "startup_probe.json never appeared at {}; daemon stderr:\n{}",
            probe_path.display(),
            d.recent_stderr()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let provides = probe["offer"]["provides"]
        .as_array()
        .expect("offer.provides array");
    assert!(
        provides.iter().any(|p| p == "_a24/events/"),
        "offer must include _a24/events/: {probe}"
    );

    // ── 7. Host command echo round-trip over real REST API ──────────────
    let (status, echo_resp) = post(
        d.port,
        &d.token,
        "/api/v1/os/node-ref/commands/echo",
        &serde_json::json!({"message": "hello from node module"}),
    );
    assert_eq!(status, 200, "echo failed: {status} {echo_resp}");
    let echo_json: serde_json::Value = serde_json::from_str(&echo_resp).unwrap();
    assert_eq!(echo_json["result"]["echo"], "hello from node module");

    // Invalid parameters -> mapped to 502 module_error with rpc_code -32602
    let (bad_status, bad_resp) = post(
        d.port,
        &d.token,
        "/api/v1/os/node-ref/commands/echo",
        &serde_json::json!({"invalid_arg": 123}),
    );
    assert_eq!(bad_status, 502, "{bad_resp}");
    let bad_json: serde_json::Value = serde_json::from_str(&bad_resp).unwrap();
    assert_eq!(bad_json["error"]["code"], "module_error");
    assert_eq!(bad_json["error"]["details"]["rpc_code"], -32602);

    // ── 8. Verify attached state in /api/v1/attached & /api/v1/os ──────
    let attached_entry = attached_list_entry(d.port, &d.token, "node-ref");
    assert_eq!(
        attached_entry["attach_status"], "attached",
        "{attached_entry}"
    );
    let current_generation = attached_entry["generation"].as_u64().unwrap_or(0);
    assert!(current_generation >= 1);

    let (os_status, os_body) = get(d.port, &d.token, "/api/v1/os").expect("/api/v1/os response");
    assert_eq!(os_status, 200, "{os_body}");
    let os_view: agent24_protocol::DomainOsList =
        serde_json::from_str(&os_body).expect("valid DomainOsList JSON from /api/v1/os");
    let os_modules = &os_view.modules;
    // os_routes contract: dynamic attached modules are managed under /api/v1/attached,
    // not present in state.os_reports (installed packages & compiled-in modules).
    assert!(
        os_modules.iter().all(|m| m.name != "node-ref"),
        "node-ref must not be present in /api/v1/os modules: {os_view:?}"
    );
    assert!(
        os_view.registry_error.is_none(),
        "registry_error must be null: {os_view:?}"
    );

    // Disconnect the module and verify detached state & generation
    drop(node_run.0.kill());
    let _ = node_run.0.wait();

    let by = Instant::now() + Duration::from_secs(5);
    let detached_entry = loop {
        let entry = attached_list_entry(d.port, &d.token, "node-ref");
        if entry["attach_status"] == "detached" {
            break entry;
        }
        assert!(
            Instant::now() < by,
            "status never transitioned to detached: {entry}"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(detached_entry["attach_status"], "detached");
    assert!(
        detached_entry["generation"].is_null(),
        "generation must be null when detached: {detached_entry}"
    );

    let (os_after_status, os_after_body) =
        get(d.port, &d.token, "/api/v1/os").expect("/api/v1/os response");
    assert_eq!(os_after_status, 200, "{os_after_body}");
    let os_after_view: agent24_protocol::DomainOsList =
        serde_json::from_str(&os_after_body).expect("valid DomainOsList JSON after disconnect");
    let os_after_modules = &os_after_view.modules;
    assert!(
        os_after_modules.iter().all(|m| m.name != "node-ref"),
        "node-ref must remain absent from /api/v1/os modules: {os_after_view:?}"
    );
    assert!(
        os_after_view.registry_error.is_none(),
        "registry_error must be null after disconnect: {os_after_view:?}"
    );

    stop(d);
}

/// Guard test & mutation positive control:
/// When an invalid handshake token is passed to the Node module, the attach
/// handshake MUST fail with `auth_failed`, the process must exit, and no event
/// may reach the WS subscriber.
#[test]
fn wrong_token_handshake_is_rejected() {
    if let Some(reason) = missing_node() {
        eprintln!("SKIP me4_node_module_blackbox::wrong_token_handshake_is_rejected: {reason}");
        return;
    }

    let home = tmp_home();
    let d = start(home.path());

    let manifest_path = repo_root().join("examples/node-module/domain-os.yml");
    let manifest_yaml = std::fs::read_to_string(&manifest_path).unwrap();
    let (status, reg_body) = post(
        d.port,
        &d.token,
        "/api/v1/attached",
        &serde_json::json!({
            "manifest": manifest_yaml,
            "allow_relax": false,
        }),
    );
    assert_eq!(status, 201, "{reg_body}");
    let reg: serde_json::Value = serde_json::from_str(&reg_body).unwrap();
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();

    let events = spawn_ws_subscriber(d.port, &d.token);

    let script = repo_root().join("examples/node-module/index.js");
    let node_data_dir = home.path().join(".agent24/os/node-ref");
    let mut node_child = Command::new("node")
        .arg(&script)
        .arg(&socket_path)
        .arg("wrong-auth-token-for-guard-test")
        .arg(&node_data_dir)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawning node reference module with wrong token");
    let stderr_pipe = node_child.stderr.take().unwrap();
    let node_stderr_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = node_stderr_lines.clone();
    let stderr_reader_handle = std::thread::spawn(move || {
        for line in BufReader::new(stderr_pipe).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    let mut node_run = Running(node_child);

    // The node process must exit quickly due to handshake rejection
    let by = Instant::now() + Duration::from_secs(10);
    let mut exited = false;
    while Instant::now() < by {
        if let Ok(Some(exit_status)) = node_run.0.try_wait() {
            assert!(
                !exit_status.success(),
                "node process must fail when token is invalid"
            );
            exited = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(exited, "node child never exited on rejected handshake");
    let _ = node_run.0.wait();
    stderr_reader_handle
        .join()
        .expect("join stderr reader thread");

    // 收集 node 的 stderr，明确断言是鉴权失败（auth_failed）而不是任意非零退出
    let collected_stderr = node_stderr_lines.lock().unwrap().join("\n");
    assert!(
        collected_stderr.contains("auth_failed"),
        "expected auth_failed in node stderr, got:\n{collected_stderr}"
    );

    // 在完整观察窗口里过滤全部 node-ref 事件，确认没有任何事件
    let window_deadline = Instant::now() + Duration::from_secs(1);
    while Instant::now() < window_deadline {
        match events.recv_timeout(Duration::from_millis(100)) {
            Ok(event) => {
                assert!(
                    event["type"] != "module" || event["payload"]["module"] != "node-ref",
                    "unexpected node-ref event arrived during observation window: {event}"
                );
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!("WS subscriber disconnected unexpectedly during observation window");
            }
        }
    }

    // 确认状态仍为 detached
    let entry = attached_list_entry(d.port, &d.token, "node-ref");
    assert_eq!(
        entry["attach_status"], "detached",
        "state must remain detached after failed auth: {entry}"
    );

    // Startup probe file must NOT exist
    let probe_path = node_data_dir.join("startup_probe.json");
    assert!(
        !probe_path.exists(),
        "probe file must not be created on rejected handshake"
    );

    stop(d);
}
