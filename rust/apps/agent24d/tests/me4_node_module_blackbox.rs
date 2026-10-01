//! ME4-5.4.1 / T14 — the black-box acceptance test for
//! `docs/specs/WIRE-OOP-MODULE.md`: **does the document alone let a
//! different-language implementation work, without looking at this repo's
//! own Rust protocol code?**
//!
//! `examples/node-module/` was written ONLY against WIRE-OOP-MODULE.md — it
//! does not `require`/`import` any crate from this repository. This test
//! drives it exactly the way `me3f_blackbox.rs` (#262, ME-3f) drives its
//! Python mock module: build the daemon once, install the package OUTSIDE
//! anything the daemon was compiled with, restart, and prove mount → routing
//! proxy → event forwarding → memory read/write all round-trip for real over
//! the wire.
//!
//! **Scope, deliberately narrower than ME-3f**: PLAN-ME4-OS-CAPABILITIES.md
//! §五 T14's acceptance criterion also mentions a scheduler upsert/fired round
//! trip. The reference module in `examples/node-module/` only declares and
//! uses `events`/`memory` (ME4-5.4.1's task brief: "原型，最小可用" — prototype,
//! minimal viable), so this file does not attempt scheduler or model or
//! approval — those three method families are documented to the same level
//! of detail in WIRE-OOP-MODULE.md §5.3–§5.5 but not exercised here. Recorded
//! as `docs/agent/followups.md` FU-104.
//!
//! **`node` availability**: unlike this crate's Python-based black-box tests
//! (which treat a missing `python3` as a hard failure — see
//! `a3_3_host_commands_blackbox.rs`), this repo has never depended on `node`
//! being on `PATH` before ME4-5.4.1 added it. So a missing `node` here is a
//! SKIP with an explicit printed reason, not a panic — per the task's own
//! instruction. On any machine that does have `node` (this one does,
//! confirmed `v24.21.0` at authoring time), the test runs for real.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The root of the git checkout, computed from this crate's own manifest
/// directory (`rust/apps/agent24d`) rather than assumed — `CARGO_MANIFEST_DIR`
/// is set by cargo for every test binary, so this does not depend on the
/// process's current working directory when `cargo test` was invoked.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent() // rust/apps
        .and_then(Path::parent) // rust
        .and_then(Path::parent) // repo root
        .expect("rust/apps/agent24d is two directories under the repo root")
        .to_path_buf()
}

/// `Some(reason)` if `node` is not runnable on this machine — printed and
/// used to skip, per this task's explicit instruction (unlike this crate's
/// Python-based black-box tests, which hard-fail on a missing interpreter).
fn missing_node() -> Option<String> {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => None,
        Ok(out) => Some(format!(
            "`node --version` exited non-zero: {}",
            String::from_utf8_lossy(&out.stderr)
        )),
        Err(e) => Some(format!("`node` is not runnable on PATH: {e}")),
    }
}

/// Install `examples/node-module/` under `<home>/.agent24/packages/node-ref/`
/// — the same on-disk shape `agent24 os install` produces, and the exact
/// three files this repo ships in `examples/node-module/` (not an inline
/// copy that could silently drift from what a real user would install).
fn install(home: &Path) {
    let src = repo_root().join("examples/node-module");
    let dst = home.join(".agent24/packages/node-ref");
    std::fs::create_dir_all(&dst).unwrap();
    for name in ["domain-os.yml", "index.js", "package.json"] {
        let from = src.join(name);
        std::fs::copy(&from, dst.join(name))
            .unwrap_or_else(|e| panic!("copying {}: {e}", from.display()));
    }
}

fn get(port: u16, token: &str, path: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nconnection: close\r\n\r\n"
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split(' ').nth(1)?.parse().ok()?;
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())?;
    Some((status, body))
}

/// The daemon — graceful-first on every exit path, same reasoning as
/// `me3f_blackbox.rs`'s `Running`: a bare `std::process::Child` dropped by an
/// early panic does not terminate the OS process, and unconditional SIGKILL
/// was empirically found (there, Codex review) to orphan the mock module.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
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

/// Start the already-built `agent24d` binary against `home` — no `cargo
/// build` here or anywhere else in this file, same as `me3f_blackbox.rs`.
fn start(home: &Path) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
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

fn os_list_entry(port: u16, token: &str, name: &str) -> serde_json::Value {
    let (status, body) = get(port, token, "/api/v1/os").expect("the daemon answered /api/v1/os");
    assert_eq!(status, 200, "{body}");
    let list: serde_json::Value = serde_json::from_str(&body).expect("the list");
    list["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} in the list: {body}"))
}

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24-node-ref")
        .tempdir_in("/tmp")
        .unwrap()
}

/// Connect to the real `GET /api/v1/events` WS endpoint — identical to
/// `me3f_blackbox.rs`'s `spawn_ws_subscriber`, see that file's doc comment
/// for why the retry-based consumption pattern at the one call site below is
/// needed (the function returning is not proof the server has subscribed
/// yet, and the hub has no replay).
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

/// **ME4-5.4.1 / T14 — the acceptance test.** A daemon starts with nothing
/// installed, the Node reference module is installed OUTSIDE anything the
/// daemon was compiled with while it is down, and the daemon is started
/// again — a real second process, no rebuild — after which mount, routing
/// proxy, event forwarding, and memory read/write all round-trip for real
/// over the wire, driven by a module written only against
/// `docs/specs/WIRE-OOP-MODULE.md`.
#[test]
fn a_node_module_written_only_against_the_wire_doc() {
    if let Some(reason) = missing_node() {
        eprintln!(
            "SKIP me4_node_module_blackbox::a_node_module_written_only_against_the_wire_doc: {reason}"
        );
        return;
    }

    let home = tmp_home();

    // First lifetime: nothing installed yet — proves the mount that follows
    // is caused by the install-then-restart sequence below.
    let d1 = start(home.path());
    let empty = get(d1.port, &d1.token, "/api/v1/os").expect("the daemon answered");
    assert_eq!(empty.0, 200, "{}", empty.1);
    let list: serde_json::Value = serde_json::from_str(&empty.1).unwrap();
    assert!(
        list["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "node-ref"),
        "node-ref must not exist before it has even been installed: {}",
        empty.1
    );
    stop(d1);

    // Install the Node module OUTSIDE anything the daemon was compiled
    // with, while it is down.
    install(home.path());

    // Second lifetime: a real restart, same binary, same $HOME.
    let d2 = start(home.path());

    // Subscribe to the real WS event consumer BEFORE triggering any request
    // that makes the module emit — see `spawn_ws_subscriber`'s doc for why
    // this alone does not close the race, and why the trigger below retries.
    let events = spawn_ws_subscriber(d2.port, &d2.token);

    // ── 1. Mount + 2. Routing proxy ──────────────────────────────────────
    let deadline = Instant::now() + Duration::from_secs(30);
    let (status, body) = loop {
        if let Some((status, body)) = get(d2.port, &d2.token, "/api/v1/node-ref/hello")
            && (status == 200 || Instant::now() >= deadline)
        {
            break (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "the node module never answered through the real proxy; daemon stderr:\n{}",
            d2.recent_stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        status,
        200,
        "body={body} daemon stderr:\n{}",
        d2.recent_stderr()
    );
    let first_response: serde_json::Value =
        serde_json::from_str(&body).unwrap_or_else(|e| panic!("{e}: {body}"));
    assert_eq!(first_response["ok"], true, "{first_response}");
    assert_eq!(
        os_list_entry(d2.port, &d2.token, "node-ref")["state"],
        "mounted"
    );

    // ── 3. Event forwarding, observed at the real consumer boundary ─────
    // The startup emit happens right after the handshake, independent of
    // any HTTP request; the hello-route emit happens inside the handler
    // above. Either is an acceptable sighting — the retry loop below fires
    // additional requests (each one emits again) until the WS subscriber,
    // which may have raced the module's very first emit, actually observes
    // one — same reasoning and same shape as `me3f_blackbox.rs`.
    let overall_deadline = Instant::now() + Duration::from_secs(30);
    let mut next_retry_at = Instant::now() + Duration::from_secs(3);
    let event = loop {
        let remaining = overall_deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the WS subscriber connected before the request never received \
             the node module's event after retrying the trigger; daemon stderr:\n{}",
            d2.recent_stderr()
        );
        if Instant::now() >= next_retry_at {
            let (port, token) = (d2.port, d2.token.clone());
            std::thread::spawn(move || {
                let _ = get(port, &token, "/api/v1/node-ref/hello");
            });
            next_retry_at = Instant::now() + Duration::from_secs(3);
        }
        let step = overall_deadline
            .saturating_duration_since(Instant::now())
            .min(next_retry_at.saturating_duration_since(Instant::now()));
        match events.recv_timeout(step) {
            Ok(event) if event["type"] == "module" => break event,
            Ok(_) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                panic!(
                    "the WS subscriber thread ended; daemon stderr:\n{}",
                    d2.recent_stderr()
                )
            }
        }
    };
    assert_eq!(event["payload"]["module"], "node-ref", "{event}");
    assert_eq!(event["payload"]["kind"], "task.transitioned", "{event}");

    // ── 4. Memory read/write, correlated by ID and body ─────────────────
    // Read straight from the HTTP response body captured above (round trip
    // 1/2), which already carries the real remember+recall result — no need
    // to also read the probe file, though the module writes one too.
    let remembered_id = first_response["remembered"]["id"].clone();
    assert!(
        remembered_id.as_str().is_some_and(|s| !s.is_empty()),
        "a real _a24/memory/private/remember call must return a non-empty string id: {first_response}"
    );
    let items = first_response["recalled"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("recall result must have an items array: {first_response}"));
    let recalled = items
        .iter()
        .find(|i| i["id"] == remembered_id)
        .unwrap_or_else(|| {
            panic!("recall must contain the EXACT record just remembered (by id): {first_response}")
        });
    assert_eq!(recalled["kind"], "node-ref-note", "{first_response}");
    assert_eq!(
        recalled["body"]["text"], "hello from the node reference module",
        "{first_response}"
    );

    // Cross-check against the atomic probe file the module also writes —
    // proves the write-to-temp-then-rename mechanism (used by the module for
    // its own diagnostics, same technique as `me3f_blackbox.rs`'s
    // `dump_atomic`) produces a real, internally-consistent snapshot.
    //
    // NOT compared for equality against `remembered_id` above: the retry
    // loop in round trip 3 may have fired additional `/hello` requests after
    // the first one, and each overwrites this file with a FRESH
    // remember+recall pair — so by the time this reads the file, it may
    // reflect a later request than the one `first_response` came from. What
    // must hold regardless of which request last wrote it is internal
    // consistency: the id it just remembered is also present in the SAME
    // response's recall.
    let probe_path = home.path().join(".agent24/os/node-ref/callback_probe.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    let probe = loop {
        if let Ok(text) = std::fs::read_to_string(&probe_path)
            && let Ok(value) = serde_json::from_str::<serde_json::Value>(&text)
        {
            break value;
        }
        assert!(
            Instant::now() < deadline,
            "callback_probe.json never appeared/parsed at {}; daemon stderr:\n{}",
            probe_path.display(),
            d2.recent_stderr()
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    let probe_id = probe["remembered"]["id"].clone();
    assert!(
        probe_id.as_str().is_some_and(|s| !s.is_empty()),
        "the probe file's remember must be a non-empty id: {probe}"
    );
    let probe_items = probe["recalled"]["items"]
        .as_array()
        .unwrap_or_else(|| panic!("probe recall result must have an items array: {probe}"));
    assert!(
        probe_items.iter().any(|i| i["id"] == probe_id),
        "the probe file's own recall must contain the id it just remembered: {probe}"
    );

    stop(d2);
}
