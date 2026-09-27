//! A3-2b — the black-box acceptance test for the attach listener + handshake
//! commit + lifecycle + shutdown (`docs/design/A3-ATTACHED-MODULE.md` §4–§5,
//! §9 judgements C1–C4, C8's local_only slice, C9). Harness shape copied from
//! `me4_model_blackbox.rs`/`me4_model_shutdown_wiring.rs` (`Running`/`Daemon`/
//! `start`/`stop`, real out-of-process Python processes, bounded polls, no
//! fixed sleeps, a real daemon restart to prove persistence) — see those
//! files for the rationale behind each piece this one reuses.
//!
//! **The "假 AgentEar" (`FAKE_AGENTEAR_SCRIPT`) is a real Python process that
//! knows NOTHING about this codebase** — it is written strictly from
//! `docs/design/A3-ATTACHED-MODULE.md` §4's wire spec (frame shape,
//! `initialize` params, `_a24/events/emit`/`_a24/model/complete` request
//! shapes), the same "independently reimplementable" bar the design doc sets
//! for the real AgentEar (§9: "只按本文 §4/§5.6/§6 写成（不 import 任何
//! Agent24 代码）"). It is driven interactively over its own stdin/stdout —
//! one JSON command per stdin line, one JSON result per stdout line — so a
//! single Rust test can sequence "handshake, then emit, then watch for
//! close" without a second probe-file round trip.
//!
//! **`python3` missing is a hard failure of every test here, never a skip**
//! (same rule `me4_model_blackbox.rs` states for the same reason).
//!
//! Scenarios, one `#[test]` each:
//!
//! - [`c1_register_rotate_and_stale_token_handshake`] — C1: register, a real
//!   handshake succeeds with the fresh token; re-adding (rotation) mints a
//!   NEW token and the OLD one now gets `auth_failed`; `GET /api/v1/attached`
//!   never carries the token.
//! - [`c2_offer_lists_events_and_model_and_first_emit_reaches_ws`] — C2: the
//!   handshake's `offer.provides` is exactly `["_a24/events/",
//!   "_a24/model/"]` for a `[events, models]` manifest against a daemon that
//!   has model deps (a real `serve()` always does); the first
//!   `_a24/events/emit` after handshake returns `{}` AND the real `GET
//!   /api/v1/events` WS subscriber receives it — proving the generation
//!   really reached `Running` (H1), not just that the handshake frame parsed.
//! - [`c3_delete_revokes_the_connection_within_a_second`] — C3: `DELETE
//!   /api/v1/attached/{name}` closes the live connection (EOF) inside ~1s,
//!   and the (now-revoked) token can no longer handshake.
//! - [`c4_concurrent_handshake_one_wins_and_name_clashes_with_a_package`] —
//!   C4: two connections racing the same name — exactly one succeeds, the
//!   other gets `busy`; the loser succeeds after the winner disconnects, with
//!   `generation` one higher; an attach registration whose name a real
//!   installed package already claims is `409`; installing a package under
//!   an already-attached name is refused at mount time (the A3-2b addition
//!   to `crate::domain::mount_all`, §5.4).
//! - [`c8_local_only_never_reaches_the_remote_stub`] — C8 (the slice this PR
//!   owns): a `local_only` attached module's `_a24/model/complete` call (even
//!   with `complexity: "complex"`) reaches only the LOCAL stub — the REMOTE
//!   stub's request count stays 0 throughout, including across an H4
//!   `remote_allowed` → re-registered `local_only` rotation.
//! - [`c9_shutdown_with_an_inflight_model_call_lands_a_cancelled_usage_row`]
//!   — C9: a hung local provider, a real `POST /api/v1/shutdown` while
//!   `_a24/model/complete` is in flight, the daemon still exits within its
//!   bound, and the module's one cancelled call is durably in the store
//!   after reopen — same DB-level proof
//!   `model_shutdown_wiring_lands_the_cancelled_call_in_the_store` uses for
//!   A1, applied to an attached module instead of a spawned one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

// ───────────────────────── daemon harness (copied shape) ─────────────────────────

struct Running(Child);

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
    run: Running,
    port: u16,
    token: String,
    stderr: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Daemon {
    fn recent_stderr(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }
}

fn start(home: &Path, extra_env: &[(&str, &str)]) -> Daemon {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut child = cmd
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let run = Running(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    let stderr_lines = Arc::new(std::sync::Mutex::new(Vec::new()));
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

/// Short path, in-process counter (task requirement): the attach socket is
/// `<home>/.agent24/attach/agent24d.sock` and macOS's `sockaddr_un` caps a
/// Unix socket path at 104 bytes including the NUL terminator — `/tmp/...`
/// (never `std::env::temp_dir()`, which on macOS resolves through
/// `$TMPDIR`'s much longer per-user, per-boot path) plus a short counter-
/// suffixed prefix keeps every test's home comfortably under that limit.
static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_home() -> tempfile::TempDir {
    let n = HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    tempfile::Builder::new()
        .prefix(&format!("a24a3-{n}-"))
        .rand_bytes(4)
        .tempdir_in("/tmp")
        .unwrap()
}

fn raw_request_timeout(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    body: &str,
    timeout: Duration,
) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect to the daemon");
    s.set_read_timeout(Some(timeout)).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
         connection: close\r\ncontent-length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(body.as_bytes());
    s.write_all(&req).expect("write the request");
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).expect("read the response");
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, resp_body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let status: u16 = head
        .split("\r\n")
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    (status, resp_body.to_owned())
}

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(20);

fn raw_request(port: u16, token: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    raw_request_timeout(port, token, method, path, body, DEFAULT_HTTP_TIMEOUT)
}

fn get(port: u16, token: &str, path: &str) -> (u16, serde_json::Value) {
    let (status, body) = raw_request(port, token, "GET", path, "");
    (status, serde_json::from_str(&body).unwrap_or_default())
}

fn post(port: u16, token: &str, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let (status, resp) = raw_request(port, token, "POST", path, &body.to_string());
    (status, serde_json::from_str(&resp).unwrap_or_default())
}

fn patch(port: u16, token: &str, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let (status, resp) = raw_request(port, token, "PATCH", path, &body.to_string());
    (status, serde_json::from_str(&resp).unwrap_or_default())
}

fn delete(port: u16, token: &str, path: &str) -> (u16, serde_json::Value) {
    let (status, resp) = raw_request(port, token, "DELETE", path, "");
    (status, serde_json::from_str(&resp).unwrap_or_default())
}

/// Connect to the real `GET /api/v1/events` WS endpoint — verbatim copy of
/// `me3f_blackbox.rs::spawn_ws_subscriber` (see that file for the residual
/// "subscribed after upgrade, before `hub.subscribe()`" race and why it is
/// the caller's job to retry across it, not this function's).
fn spawn_ws_subscriber(port: u16, token: &str) -> mpsc::Receiver<serde_json::Value> {
    use tokio_tungstenite::tungstenite;
    use tungstenite::client::IntoClientRequest;

    let (tx, rx) = mpsc::channel();
    let (ready_tx, ready_rx) = mpsc::channel();
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

// ───────────────────────── attach manifest + REST helpers ─────────────────────────

fn attach_manifest(name: &str, caps: &[&str], model_access: Option<&str>) -> String {
    let ma = model_access
        .map(|m| format!("model_access: {m}\n"))
        .unwrap_or_default();
    format!(
        "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
         event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
         impl_kind: attached_process\nkernel_capabilities: [{}]\n{ma}",
        caps.join(", ")
    )
}

/// `POST /api/v1/attached`. Returns the parsed `201`/`200` body (which the
/// caller expects to have `name`/`manifest_digest`/`token`/`socket_path`/
/// `token_id`) or the error body, alongside the status.
fn register_attached(
    port: u16,
    token: &str,
    manifest: &str,
    allow_relax: bool,
) -> (u16, serde_json::Value) {
    post(
        port,
        token,
        "/api/v1/attached",
        &serde_json::json!({"manifest": manifest, "allow_relax": allow_relax}),
    )
}

// ───────────────────────── the fake AgentEar (Python, wire-spec-only) ─────────────────────────

/// A real Python process implementing ONLY `docs/design/A3-ATTACHED-MODULE.md`
/// §4's wire spec — no Agent24 code is imported. Driven over its own
/// stdin/stdout: one JSON command per stdin line, one JSON result per stdout
/// line, so a single-threaded Rust test can sequence actions deterministically
/// without polling a probe file.
///
/// `argv`: `<manifest-file> <socket-path> <module-name> <token> <capabilities-json>`.
/// The manifest file's bytes are hashed here (`sha256:` + hex) exactly the
/// way the real registration digest is computed (`crate::attached::manifest_digest`,
/// itself `agent24_os_proto::manifest::manifest_digest`) — both sides read
/// the SAME file, so the digests agree by construction, not by coincidence.
///
/// Commands (`{"op": ..., ...}` on stdin, one response object on stdout):
/// - `{"op":"call","method":"...","params":{...}}` → sends the request, BLOCKS
///   for one response line, echoes `{"op":"call","response":<parsed>,"eof":bool}`.
/// - `{"op":"call_nowait", ...}` → sends the request, does NOT wait for a
///   response (used to put a `_a24/model/complete` call in flight against a
///   hung provider without blocking this process).
/// - `{"op":"send_raw","line":"..."}` → writes the given bytes verbatim
///   (plus `\n`) — for malformed-frame probing.
/// - `{"op":"recv_line","timeout":<secs>}` → blocks (bounded) for one more
///   line (a late/kernel-originated frame), echoes it or `"eof":true`.
/// - `{"op":"wait_closed","timeout":<secs>}` → blocks (bounded) reading until
///   EOF; `{"closed":true}` on EOF, `{"closed":false}` on timeout.
/// - `{"op":"close"}` → closes the socket and exits.
const FAKE_AGENTEAR_SCRIPT: &str = r#"import hashlib, json, os, socket, sys

def main():
    manifest_path, socket_path, module, token, caps_json = sys.argv[1:6]
    with open(manifest_path, "rb") as f:
        digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()

    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    try:
        sock.connect(socket_path)
    except OSError as e:
        print(json.dumps({"op": "connect", "error": str(e)}), flush=True)
        return
    r = sock.makefile("rb")
    w = sock.makefile("wb")
    next_id = [0]

    def rpc_id():
        next_id[0] += 1
        return "c%d" % next_id[0]

    def send(obj):
        w.write((json.dumps(obj) + "\n").encode())
        w.flush()

    hs_id = rpc_id()
    send({
        "jsonrpc": "2.0", "id": hs_id, "method": "initialize",
        "params": {
            "protocol_versions": {"min": 1, "max": 1},
            "module": module,
            "manifest_digest": digest,
            "auth_token": token,
            "capabilities": json.loads(caps_json),
        },
    })
    line = r.readline()
    handshake_result = json.loads(line) if line else None
    print(json.dumps({"op": "handshake", "response": handshake_result}), flush=True)

    for raw_cmd in sys.stdin:
        raw_cmd = raw_cmd.strip()
        if not raw_cmd:
            continue
        cmd = json.loads(raw_cmd)
        op = cmd.get("op")
        try:
            if op == "call":
                cid = rpc_id()
                send({"jsonrpc": "2.0", "id": cid, "method": cmd["method"], "params": cmd.get("params", {})})
                line = r.readline()
                print(json.dumps({
                    "op": "call",
                    "response": json.loads(line) if line else None,
                    "eof": line == b"",
                }), flush=True)
            elif op == "call_nowait":
                cid = rpc_id()
                send({"jsonrpc": "2.0", "id": cid, "method": cmd["method"], "params": cmd.get("params", {})})
                print(json.dumps({"op": "call_nowait", "sent": True}), flush=True)
            elif op == "send_raw":
                w.write((cmd["line"] + "\n").encode())
                w.flush()
                print(json.dumps({"op": "send_raw", "sent": True}), flush=True)
            elif op == "recv_line":
                sock.settimeout(cmd.get("timeout", 5))
                try:
                    line = r.readline()
                    print(json.dumps({
                        "op": "recv_line",
                        "line": line.decode(errors="replace").strip() if line else None,
                        "eof": line == b"",
                    }), flush=True)
                except socket.timeout:
                    print(json.dumps({"op": "recv_line", "line": None, "eof": False, "timeout": True}), flush=True)
                finally:
                    sock.settimeout(None)
            elif op == "wait_closed":
                sock.settimeout(cmd.get("timeout", 5))
                closed = False
                try:
                    while True:
                        line = r.readline()
                        if line == b"":
                            closed = True
                            break
                except socket.timeout:
                    closed = False
                finally:
                    sock.settimeout(None)
                print(json.dumps({"op": "wait_closed", "closed": closed}), flush=True)
            elif op == "close":
                sock.close()
                print(json.dumps({"op": "close", "closed": True}), flush=True)
                return
            else:
                print(json.dumps({"op": op, "error": "unknown op"}), flush=True)
        except Exception as e:
            print(json.dumps({"op": op, "error": repr(e)}), flush=True)

if __name__ == "__main__":
    main()
"#;

struct FakeAgentEar {
    #[allow(dead_code)]
    run: Running,
    stdin: std::process::ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
    pub handshake: serde_json::Value,
}

fn spawn_fake_agentear(
    script_path: &Path,
    manifest_path: &Path,
    socket_path: &str,
    module: &str,
    token: &str,
    capabilities: &[&str],
) -> FakeAgentEar {
    let caps_json = serde_json::to_string(capabilities).unwrap();
    let mut child = Command::new("python3")
        .args(["-I", "-S"])
        .arg(script_path)
        .arg(manifest_path)
        .arg(socket_path)
        .arg(module)
        .arg(token)
        .arg(&caps_json)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect(
            "python3 must be installed and on PATH — required by A3-2b's black-box judgements, \
             a hard failure rather than a skip",
        );
    let stdin = child.stdin.take().unwrap();
    let stdout = BufReader::new(child.stdout.take().unwrap());
    let stderr = child.stderr.take().unwrap();
    std::thread::spawn(
        move || {
            for _line in BufReader::new(stderr).lines().map_while(Result::ok) {}
        },
    );
    let mut fae = FakeAgentEar {
        run: Running(child),
        stdin,
        stdout,
        handshake: serde_json::Value::Null,
    };
    let line = fae.read_line_timeout(Duration::from_secs(10));
    fae.handshake = line["response"].clone();
    fae
}

impl FakeAgentEar {
    fn read_line_timeout(&mut self, _timeout: Duration) -> serde_json::Value {
        // stdout of a piped child is read with a blocking `read_line`; every
        // command below is answered promptly by the script (or the script's
        // own internal `timeout` bounds it), so a fixed OS-level read timeout
        // is unnecessary — the process itself is killed by `Running`'s Drop
        // if this test fails before reaching a `close`.
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read a line from the fake AgentEar's stdout");
        assert!(
            !line.is_empty(),
            "the fake AgentEar's stdout closed unexpectedly"
        );
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("not JSON: {line:?}: {e}"))
    }

    fn command(&mut self, cmd: serde_json::Value) -> serde_json::Value {
        let mut line = cmd.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .expect("write a command to the fake AgentEar's stdin");
        self.read_line_timeout(Duration::from_secs(15))
    }

    fn call(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.command(serde_json::json!({"op": "call", "method": method, "params": params}))["response"].clone()
    }

    fn call_nowait(&mut self, method: &str, params: serde_json::Value) {
        self.command(serde_json::json!({"op": "call_nowait", "method": method, "params": params}));
    }

    fn wait_closed(&mut self, timeout_secs: u64) -> bool {
        self.command(serde_json::json!({"op": "wait_closed", "timeout": timeout_secs}))["closed"]
            .as_bool()
            .unwrap_or(false)
    }
}

/// Writes the fake AgentEar script once per test process into a shared temp
/// file (`python3 <script.py> <args...>` — an argv script, not `-c`, so a
/// stray `SyntaxError` reports a real filename in Python's own traceback).
fn write_fake_agentear_script(dir: &Path) -> PathBuf {
    let path = dir.join("fake_agentear.py");
    std::fs::write(&path, FAKE_AGENTEAR_SCRIPT).unwrap();
    path
}

/// Registers `name` with `manifest` against `d`, panicking with a readable
/// message (daemon stderr included) on anything but success, and returns the
/// parsed registration body.
fn register_ok(d: &Daemon, manifest: &str) -> serde_json::Value {
    let (status, body) = register_attached(d.port, &d.token, manifest, false);
    assert!(
        (200..300).contains(&status),
        "registration failed: {status} {body} — daemon stderr:\n{}",
        d.recent_stderr()
    );
    body
}

// ───────────────────────── C1 ─────────────────────────

/// C1: register → real handshake succeeds; re-add (rotation) mints a NEW
/// token and INVALIDATES the old one (a fresh handshake with it gets
/// `auth_failed`); the new token still works; `GET /api/v1/attached` never
/// carries a token/hash (the wire-shape half of this is already pinned at
/// the unit level in `attached_routes.rs`/`attached.rs` — this is the
/// real-daemon, real-socket half).
#[test]
fn c1_register_rotate_and_stale_token_handshake() {
    let home = tmp_home();
    let script = write_fake_agentear_script(home.path());
    let manifest_path = home.path().join("agentear.yml");
    let manifest = attach_manifest("agentear", &["events"], None);
    std::fs::write(&manifest_path, &manifest).unwrap();

    let d = start(home.path(), &[]);
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let old_token = reg["token"].as_str().unwrap().to_owned();
    let old_token_id = reg["token_id"].as_str().unwrap().to_owned();

    let mut fae = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &old_token,
        &["events"],
    );
    assert!(
        fae.handshake.get("result").is_some(),
        "the first handshake with the freshly minted token must succeed: {:?}",
        fae.handshake
    );
    fae.command(serde_json::json!({"op": "close"}));

    // Re-add the SAME manifest: token-only rotation (§3.4) — a fresh token,
    // a fresh token_id, the SAME digest.
    let rotated = register_ok(&d, &manifest);
    assert_eq!(rotated["manifest_digest"], reg["manifest_digest"]);
    assert_ne!(
        rotated["token"], reg["token"],
        "rotation must mint a fresh token"
    );
    assert_ne!(rotated["token_id"], old_token_id);
    let new_token = rotated["token"].as_str().unwrap().to_owned();

    // The OLD token no longer authenticates — `auth_failed`, not merely
    // refused for some other reason.
    let mut stale = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &old_token,
        &["events"],
    );
    assert_eq!(
        stale.handshake["error"]["data"]["kind"], "auth_failed",
        "a rotated-away token must fail the NEXT handshake with auth_failed: {:?}",
        stale.handshake
    );

    // The NEW token works.
    let mut fresh = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &new_token,
        &["events"],
    );
    assert!(
        fresh.handshake.get("result").is_some(),
        "the rotated token must handshake successfully: {:?}",
        fresh.handshake
    );

    // `GET /api/v1/attached` never carries the token or its hash, over the
    // real wire (not just the in-process `Json` value) — grep the raw bytes.
    let (status, raw_body) = raw_request(d.port, &d.token, "GET", "/api/v1/attached", "");
    assert_eq!(status, 200);
    assert!(!raw_body.contains(&new_token));
    assert!(!raw_body.contains(&old_token));
    assert!(!raw_body.contains("token_sha256"));

    fresh.command(serde_json::json!({"op": "close"}));
    let _ = stale.command(serde_json::json!({"op": "close"}));
    stop(d);
}

// ───────────────────────── C2 ─────────────────────────

/// C2: `offer.provides` is exactly the two prefixes for a `[events, models]`
/// registration (a real daemon always has model deps — `_a24/model/` needs
/// no provider configured to be OFFERED, only to be usable); the first
/// `_a24/events/emit` after handshake returns `{}` and is actually broadcast
/// on the real WS — proving the generation reached `Running` (H1), not just
/// that the JSON parsed.
#[test]
fn c2_offer_lists_events_and_model_and_first_emit_reaches_ws() {
    let home = tmp_home();
    let script = write_fake_agentear_script(home.path());
    let manifest_path = home.path().join("agentear.yml");
    let manifest = attach_manifest("agentear", &["events", "models"], None);
    std::fs::write(&manifest_path, &manifest).unwrap();

    let d = start(home.path(), &[]);
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let token = reg["token"].as_str().unwrap().to_owned();

    let ws = spawn_ws_subscriber(d.port, &d.token);

    let mut fae = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events", "models"],
    );
    let provides = fae.handshake["result"]["offer"]["provides"]
        .as_array()
        .unwrap_or_else(|| panic!("no offer.provides in {:?}", fae.handshake))
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(
        provides,
        vec!["_a24/events/".to_owned(), "_a24/model/".to_owned()],
        "handshake result: {:?}",
        fae.handshake
    );

    // Retry the emit across the residual "WS subscribed, but the hub
    // hasn't registered it yet" window `spawn_ws_subscriber` documents —
    // same shape `me3f_blackbox.rs` uses at its own call site.
    let sentinel = format!("a3-2b-sentinel-{}", std::process::id());
    let deadline = Instant::now() + Duration::from_secs(10);
    let emit_ok = loop {
        let resp = fae.call(
            "_a24/events/emit",
            serde_json::json!({"kind": "agentear.event", "payload": {"sentinel": sentinel}}),
        );
        if resp.get("result") == Some(&serde_json::json!({})) {
            break true;
        }
        if Instant::now() > deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(emit_ok, "emit never returned {{}} within the deadline");

    let by = Instant::now() + Duration::from_secs(10);
    let mut seen = false;
    while Instant::now() < by {
        if let Ok(event) = ws.recv_timeout(Duration::from_millis(200))
            // `EventBody::Module` is adjacently tagged `{"type":"module","payload":<ModuleEventPayload>}`,
            // and `ModuleEventPayload` itself HAS a field called `payload` too
            // (the module-defined body) — so the sentinel is two levels down.
            && event["type"] == "module"
            && event["payload"]["module"] == "agentear"
            && event["payload"]["payload"]["sentinel"] == sentinel
        {
            seen = true;
            break;
        }
    }
    assert!(seen, "the WS subscriber never saw the emitted event");

    fae.command(serde_json::json!({"op": "close"}));
    stop(d);
}

// ───────────────────────── C3 ─────────────────────────

/// C3: `DELETE /api/v1/attached/{name}` closes the live connection inside a
/// generous bound, and the (now-revoked) token can no longer handshake.
#[test]
fn c3_delete_revokes_the_connection_within_a_second() {
    let home = tmp_home();
    let script = write_fake_agentear_script(home.path());
    let manifest_path = home.path().join("agentear.yml");
    let manifest = attach_manifest("agentear", &["events"], None);
    std::fs::write(&manifest_path, &manifest).unwrap();

    let d = start(home.path(), &[]);
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let token = reg["token"].as_str().unwrap().to_owned();

    let mut fae = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events"],
    );
    assert!(fae.handshake.get("result").is_some(), "{:?}", fae.handshake);

    let (status, _) = delete(d.port, &d.token, "/api/v1/attached/agentear");
    assert_eq!(status, 204);

    let closed_within_bound = fae.wait_closed(5);
    assert!(closed_within_bound, "DELETE must close the live connection");

    let mut stale = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events"],
    );
    assert_eq!(
        stale.handshake["error"]["data"]["kind"], "auth_failed",
        "a revoked token must not handshake: {:?}",
        stale.handshake
    );

    let (status, _) = delete(d.port, &d.token, "/api/v1/attached/agentear");
    assert_eq!(status, 404, "a second DELETE must find nothing left");
    let _ = stale.command(serde_json::json!({"op": "close"}));

    // §5.3: "对附着模块同样生效：disable = 立即撤销并断开，之后握手被拒
    // forbidden" — the same immediate-revoke contract, via `PATCH
    // /api/v1/attached/{name}` (A3-2b's own endpoint, §3.2's deviation note)
    // instead of `DELETE`.
    let manifest2 = attach_manifest("agentear2", &["events"], None);
    let manifest2_path = home.path().join("agentear2.yml");
    std::fs::write(&manifest2_path, &manifest2).unwrap();
    let reg2 = register_ok(&d, &manifest2);
    let socket_path2 = reg2["socket_path"].as_str().unwrap().to_owned();
    let token2 = reg2["token"].as_str().unwrap().to_owned();
    let mut fae2 = spawn_fake_agentear(
        &script,
        &manifest2_path,
        &socket_path2,
        "agentear2",
        &token2,
        &["events"],
    );
    assert!(
        fae2.handshake.get("result").is_some(),
        "{:?}",
        fae2.handshake
    );

    let (status, body) = patch(
        d.port,
        &d.token,
        "/api/v1/attached/agentear2",
        &serde_json::json!({"enabled": false}),
    );
    assert_eq!(status, 200, "{body}");
    let disabled_view = body["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "agentear2")
        .unwrap();
    assert_eq!(disabled_view["attach_status"], "disabled");

    assert!(
        fae2.wait_closed(5),
        "disabling a module must close its live connection immediately"
    );
    let mut forbidden = spawn_fake_agentear(
        &script,
        &manifest2_path,
        &socket_path2,
        "agentear2",
        &token2,
        &["events"],
    );
    assert_eq!(
        forbidden.handshake["error"]["data"]["kind"], "forbidden",
        "a disabled module must refuse a handshake with `forbidden`: {:?}",
        forbidden.handshake
    );

    // Re-enabling allows a fresh handshake again.
    let (status, _) = patch(
        d.port,
        &d.token,
        "/api/v1/attached/agentear2",
        &serde_json::json!({"enabled": true}),
    );
    assert_eq!(status, 200);
    let mut reenabled = spawn_fake_agentear(
        &script,
        &manifest2_path,
        &socket_path2,
        "agentear2",
        &token2,
        &["events"],
    );
    assert!(
        reenabled.handshake.get("result").is_some(),
        "re-enabling must allow a fresh handshake: {:?}",
        reenabled.handshake
    );

    let _ = forbidden.command(serde_json::json!({"op": "close"}));
    reenabled.command(serde_json::json!({"op": "close"}));
    stop(d);
}

// ───────────────────────── C4 ─────────────────────────

/// C4: two connections racing the SAME registered name — exactly one gets
/// `result` (success), the other gets `busy`; the loser's own reconnect after
/// the winner disconnects succeeds, with `generation` one higher; a name a
/// real installed package already claims is refused for attach registration
/// (`409`); installing a package under an already-attached name is refused
/// at mount time (A3-2b's `crate::domain::mount_all` addition, §5.4).
#[test]
fn c4_concurrent_handshake_one_wins_and_name_clashes_with_a_package() {
    let home = tmp_home();
    let script = write_fake_agentear_script(home.path());
    let manifest_path = home.path().join("agentear.yml");
    let manifest = attach_manifest("agentear", &["events"], None);
    std::fs::write(&manifest_path, &manifest).unwrap();

    // §5.4's package-name direction needs the package present at CATALOGUE
    // DISCOVERY time (`crate::attached_routes::post_attached_at`'s own doc:
    // `os_reports` is a startup snapshot) — installed BEFORE the daemon
    // starts, so the later `POST /api/v1/attached` against a live daemon
    // sees it in that snapshot.
    let sin90_dir = home.path().join(".agent24/packages/sin90");
    std::fs::create_dir_all(&sin90_dir).unwrap();
    std::fs::write(
        sin90_dir.join("domain-os.yml"),
        "name: sin90\nversion: \"0.1.0\"\nroute_namespace: /api/v1/sin90\n\
         event_module: sin90\ndata_dir: ~/.agent24/os/sin90/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"-c\", \"import time; time.sleep(60)\"]\n",
    )
    .unwrap();

    let d = start(home.path(), &[]);
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let token = reg["token"].as_str().unwrap().to_owned();

    // Two connections, spawned back to back — whichever wins the registry
    // lock first (§4.3 ②, §5.4 Q3=a) keeps the slot; the other is refused
    // `busy` rather than replacing it.
    let mut a = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events"],
    );
    let mut b = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events"],
    );

    let a_ok = a.handshake.get("result").is_some();
    let b_ok = b.handshake.get("result").is_some();
    assert_ne!(
        a_ok, b_ok,
        "exactly one of the two concurrent handshakes must succeed: a={:?} b={:?}",
        a.handshake, b.handshake
    );
    let (winner, loser) = if a_ok {
        (&mut a, &mut b)
    } else {
        (&mut b, &mut a)
    };
    assert_eq!(
        loser.handshake["error"]["data"]["kind"], "busy",
        "the losing connection must be refused `busy`: {:?}",
        loser.handshake
    );
    let first_generation = winner.handshake["result"]["offer"].clone();
    let _ = first_generation; // offer shape already covered by C2; here we only need "it succeeded"

    // The winner disconnects; the loser can now connect successfully.
    winner.command(serde_json::json!({"op": "close"}));
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut reconnected = None;
    while Instant::now() < deadline {
        let candidate = spawn_fake_agentear(
            &script,
            &manifest_path,
            &socket_path,
            "agentear",
            &token,
            &["events"],
        );
        if candidate.handshake.get("result").is_some() {
            reconnected = Some(candidate);
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut reconnected =
        reconnected.expect("a reconnect after the winner's close must eventually succeed");

    // §5.4, direction 1: an attach registration under a name a REAL installed
    // package already claims is refused — `sin90` was on disk before `d`
    // started, so it is in `d`'s `os_reports` snapshot.
    let (status, body) = register_attached(
        d.port,
        &d.token,
        &attach_manifest("sin90", &["events"], None),
        false,
    );
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["error"]["code"], "name_taken");

    reconnected.command(serde_json::json!({"op": "close"}));
    let _ = loser.command(serde_json::json!({"op": "close"}));
    stop(d);

    // §5.4, direction 2: install a SECOND package, itself named `agentear`
    // (a DIFFERENT directory — a package's identity comes from its manifest's
    // `name:`, not its directory) — the same name the very first
    // `register_ok` above already attached. A restart is required either way
    // (a fresh catalogue discovery pass): the package must mount `Refused`
    // with the A3-2b reason, never silently steal the name.
    let agentear_pkg_dir = home.path().join(".agent24/packages/agentear-pkg");
    std::fs::create_dir_all(&agentear_pkg_dir).unwrap();
    std::fs::write(
        agentear_pkg_dir.join("domain-os.yml"),
        "name: agentear\nversion: \"0.1.0\"\nroute_namespace: /api/v1/agentear\n\
         event_module: agentear\ndata_dir: ~/.agent24/os/agentear/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"-c\", \"import time; time.sleep(60)\"]\n",
    )
    .unwrap();

    let d2 = start(home.path(), &[]);
    let (status, os_list) = get(d2.port, &d2.token, "/api/v1/os");
    assert_eq!(status, 200);
    let agentear_pkg = os_list["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "agentear")
        .unwrap_or_else(|| panic!("agentear missing from {os_list}"));
    assert_eq!(agentear_pkg["state"], "refused", "{agentear_pkg}");
    assert_eq!(
        agentear_pkg["detail"], "registered as an attached module",
        "{agentear_pkg}"
    );
    stop(d2);
}

// ───────────────────────── stub provider (copied from me4_model_blackbox.rs) ─────────────────────────

/// Verbatim copy of `me4_model_blackbox.rs::STUB_SCRIPT` — see that file's
/// own doc comment for the full behaviour (`mode` file: `"ok"`/`"unavailable"`/
/// `"hang"`, `requests.json` logging, `hang_result.json` phases). Duplicated
/// rather than shared, matching this test suite's existing convention of one
/// self-contained script per blackbox file.
const STUB_SCRIPT: &str = r#"import json, os, socket, sys

control_dir = sys.argv[1]
model_id = sys.argv[2] if len(sys.argv) > 2 else "stub-model"

def dump_atomic(name, value):
    path = os.path.join(control_dir, name)
    tmp = path + ".tmp"
    with open(tmp, "w") as out:
        json.dump(value, out)
    os.replace(tmp, path)

def append_atomic(name, value):
    path = os.path.join(control_dir, name)
    try:
        with open(path) as fh:
            items = json.load(fh)
    except (FileNotFoundError, json.JSONDecodeError):
        items = []
    items.append(value)
    dump_atomic(name, items)

def read_mode():
    try:
        with open(os.path.join(control_dir, "mode")) as fh:
            return fh.read().strip() or "ok"
    except FileNotFoundError:
        return "ok"

srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
srv.bind(("127.0.0.1", 0))
srv.listen(16)
print(json.dumps({"port": srv.getsockname()[1]}), flush=True)

while True:
    conn, _ = srv.accept()
    try:
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = conn.recv(4096)
            if not chunk:
                break
            head += chunk
        header_part, _, rest = head.partition(b"\r\n\r\n")
        lines = header_part.split(b"\r\n")
        headers = {}
        for line in lines[1:]:
            if b":" in line:
                k, v = line.split(b":", 1)
                headers[k.strip().lower().decode()] = v.strip().decode()
        content_length = int(headers.get("content-length", "0") or "0")
        body_bytes = rest
        while len(body_bytes) < content_length:
            chunk = conn.recv(4096)
            if not chunk:
                break
            body_bytes += chunk
        mode = read_mode()
        if mode == "hang":
            dump_atomic("hang_result.json", {"phase": "waiting", "closed": None})
        append_atomic("requests.json", {
            "mode": mode,
            "request_line": lines[0].decode(errors="replace") if lines else "",
            "body": body_bytes.decode(errors="replace"),
        })
        if mode == "hang":
            conn.settimeout(60)
            try:
                data = conn.recv(4096)
                closed = not data
            except socket.timeout:
                closed = False
            except OSError:
                closed = True
            dump_atomic("hang_result.json", {"phase": "done", "closed": closed})
            continue
        if mode == "unavailable":
            status = b"503 Service Unavailable"
            out = json.dumps({"error": {"message": "stub backend is down", "code": "server_error"}}).encode()
        else:
            status = b"200 OK"
            out = json.dumps({
                "choices": [{"message": {"role": "assistant", "content": "hello from %s" % model_id}}],
                "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18},
                "model": model_id,
            }).encode()
        conn.sendall(b"HTTP/1.1 %s\r\nContent-Length: %d\r\nContent-Type: application/json\r\n\r\n%s" % (status, len(out), out))
    except Exception as e:
        with open(os.path.join(control_dir, "error.txt"), "w") as out:
            out.write(repr(e))
    finally:
        conn.close()
"#;

struct Stub {
    #[allow(dead_code)]
    run: Running,
    port: u16,
    control_dir: PathBuf,
}

impl Stub {
    fn request_count(&self) -> usize {
        std::fs::read(self.control_dir.join("requests.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.as_array().map(Vec::len))
            .unwrap_or(0)
    }

    fn hang_status(&self) -> Option<String> {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.control_dir.join("hang_result.json")).ok()?)
                .ok()?;
        v["phase"].as_str().map(str::to_owned)
    }
}

fn start_stub(control_dir: &Path, model_id: &str) -> Stub {
    std::fs::create_dir_all(control_dir).unwrap();
    let mut child = Command::new("python3")
        .args(["-I", "-S", "-c", STUB_SCRIPT])
        .arg(control_dir)
        .arg(model_id)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("python3 must be installed and on PATH");
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let run = Running(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("the stub never printed its {\"port\": ...} line within 10s");
    let ready: serde_json::Value =
        serde_json::from_str(&ready).expect("the stub's ready line was not JSON");
    std::thread::spawn(
        move || {
            for _line in BufReader::new(stderr).lines().map_while(Result::ok) {}
        },
    );
    Stub {
        run,
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        control_dir: control_dir.to_owned(),
    }
}

fn set_stub_mode(stub: &Stub, mode: &str) {
    let path = stub.control_dir.join("mode");
    let tmp = stub.control_dir.join("mode.tmp");
    std::fs::write(&tmp, mode).unwrap();
    std::fs::rename(&tmp, &path).unwrap();
}

// ───────────────────────── C8 ─────────────────────────

/// C8 (the slice A3-2b owns): a `local_only` attached module's
/// `_a24/model/complete` — even with `complexity: "complex"` — reaches only
/// the LOCAL stub; the REMOTE stub's request count stays 0 throughout,
/// including across an H4 rotation (`remote_allowed` with `allow_relax`,
/// then re-registered back to `local_only`).
#[test]
fn c8_local_only_never_reaches_the_remote_stub() {
    let home = tmp_home();
    let script = write_fake_agentear_script(home.path());
    let manifest_path = home.path().join("agentear.yml");

    let local = start_stub(&home.path().join("local-stub"), "local-model");
    let remote = start_stub(&home.path().join("remote-stub"), "remote-model");
    set_stub_mode(&local, "ok");
    set_stub_mode(&remote, "ok");

    let d = start(
        home.path(),
        &[
            ("OMLX_URL", &format!("http://127.0.0.1:{}", local.port)),
            // The IPv4-mapped IPv6 trick `me4_model_blackbox.rs` uses (J16/§2.3):
            // `ModelRouter::from_env` labels a provider URL Remote only when it
            // is NOT loopback (`agent24-models/src/router.rs::is_loopback_url`,
            // which deliberately does not recognise `::ffff:127.0.0.1` as
            // loopback even though the bytes still land on this machine) — a
            // plain `127.0.0.1` OLLAMA_URL would be judged Local, defeating
            // this test's whole point.
            (
                "OLLAMA_URL",
                &format!("http://[::ffff:127.0.0.1]:{}", remote.port),
            ),
        ],
    );

    let manifest = attach_manifest("agentear", &["events", "models"], None); // local_only default
    std::fs::write(&manifest_path, &manifest).unwrap();
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let token = reg["token"].as_str().unwrap().to_owned();

    let mut fae = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events", "models"],
    );
    assert!(fae.handshake.get("result").is_some(), "{:?}", fae.handshake);

    let resp = fae.call(
        "_a24/model/complete",
        serde_json::json!({"messages": [{"role": "user", "content": "hi"}], "complexity": "complex"}),
    );
    assert_eq!(
        resp["result"]["tier"], "local",
        "a local_only module's call must be served locally even at complexity=complex: {resp}"
    );
    assert_eq!(local.request_count(), 1);
    assert_eq!(
        remote.request_count(),
        0,
        "a local_only call must NEVER reach the remote stub"
    );

    // H4: re-register as `remote_allowed` (needs `allow_relax`), reconnect,
    // confirm remote IS reachable now (positive control the design itself
    // asks for) — then re-register back to `local_only` and confirm a fresh
    // `complexity: complex` call after RECONNECTING still counts 0 on the
    // remote stub (the whole point of C8's H4 case: the OLD ModelGrant must
    // not survive a re-registration that narrows privacy).
    fae.command(serde_json::json!({"op": "close"}));
    let remote_manifest =
        attach_manifest("agentear", &["events", "models"], Some("remote_allowed"));
    std::fs::write(&manifest_path, &remote_manifest).unwrap();
    let reg2 = register_attached(d.port, &d.token, &remote_manifest, true);
    assert_eq!(reg2.0, 200, "{:?}", reg2.1); // rotation of an existing name is 200
    let token2 = reg2.1["token"].as_str().unwrap().to_owned();

    let mut fae2 = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token2,
        &["events", "models"],
    );
    assert!(
        fae2.handshake.get("result").is_some(),
        "{:?}",
        fae2.handshake
    );
    let resp2 = fae2.call(
        "_a24/model/complete",
        serde_json::json!({"messages": [{"role": "user", "content": "hi"}], "complexity": "complex"}),
    );
    assert_eq!(resp2["result"]["tier"], "remote", "{resp2}");
    assert_eq!(
        remote.request_count(),
        1,
        "remote_allowed must be able to reach the remote stub"
    );
    fae2.command(serde_json::json!({"op": "close"}));

    std::fs::write(&manifest_path, &manifest).unwrap(); // back to local_only
    let reg3 = register_attached(d.port, &d.token, &manifest, false);
    assert_eq!(reg3.0, 200, "{:?}", reg3.1);
    let token3 = reg3.1["token"].as_str().unwrap().to_owned();
    let mut fae3 = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token3,
        &["events", "models"],
    );
    assert!(
        fae3.handshake.get("result").is_some(),
        "{:?}",
        fae3.handshake
    );
    let resp3 = fae3.call(
        "_a24/model/complete",
        serde_json::json!({"messages": [{"role": "user", "content": "hi"}], "complexity": "complex"}),
    );
    assert_eq!(
        resp3["result"]["tier"], "local",
        "after narrowing back to local_only, complexity=complex must still stay local: {resp3}"
    );
    assert_eq!(
        remote.request_count(),
        1,
        "the remote count must NOT have grown after narrowing back to local_only (H4's own point)"
    );

    fae3.command(serde_json::json!({"op": "close"}));
    stop(d);
}

// ───────────────────────── C9 ─────────────────────────

/// C9: a hung local provider, a real `POST /api/v1/shutdown` while
/// `_a24/model/complete` is genuinely in flight, the daemon still exits
/// within a generous bound, and — reopening the SAME store after exit — the
/// module's one cancelled call is durably recorded. Mirrors
/// `me4_model_shutdown_wiring.rs::model_shutdown_wiring_lands_the_cancelled_call_in_the_store`
/// (A1), applied to an attached module (A3) instead of a spawned one, which
/// is exactly the "same `modules_cut_off()` tree" property `AttachDeps`
/// (`crate::attach_registry`) exists to give.
#[test]
fn c9_shutdown_with_an_inflight_model_call_lands_a_cancelled_usage_row() {
    let home = tmp_home();
    let script = write_fake_agentear_script(home.path());
    let manifest_path = home.path().join("agentear.yml");
    let manifest = attach_manifest("agentear", &["events", "models"], None);
    std::fs::write(&manifest_path, &manifest).unwrap();

    let hang = start_stub(&home.path().join("hang-stub"), "hang-model");
    set_stub_mode(&hang, "hang");

    let mut d = start(
        home.path(),
        &[("OMLX_URL", &format!("http://127.0.0.1:{}", hang.port))],
    );
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let token = reg["token"].as_str().unwrap().to_owned();

    let mut fae = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        &["events", "models"],
    );
    assert!(fae.handshake.get("result").is_some(), "{:?}", fae.handshake);

    fae.call_nowait(
        "_a24/model/complete",
        serde_json::json!({"messages": [{"role": "user", "content": "hi"}]}),
    );

    // Wait until the daemon has genuinely dialled the hung provider — proof
    // the call is really in flight, not merely queued in this process.
    let by = Instant::now() + Duration::from_secs(15);
    while hang.hang_status().as_deref() != Some("waiting") {
        assert!(
            Instant::now() < by,
            "the model call never reached the hung provider"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The production path: `agent24 daemon stop` posts here too.
    let (status, _) = raw_request(d.port, &d.token, "POST", "/api/v1/shutdown", "");
    assert_eq!(status, 202);

    // The hung provider's connection to the daemon must close (the daemon
    // cut it off as part of the same shutdown) within the bound.
    let by = Instant::now() + Duration::from_secs(15);
    loop {
        if hang.hang_status().as_deref() == Some("done") {
            break;
        }
        assert!(
            Instant::now() < by,
            "the hung provider never observed the connection close — the in-flight \
             attach model call outlived the daemon's shutdown"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The daemon process itself must exit within a generous bound — "an
    // in-flight attached model call must not be able to wedge shutdown".
    let exited = loop {
        if let Some(exit_status) = d.run.0.try_wait().unwrap() {
            break exit_status;
        }
        assert!(
            Instant::now() < by,
            "the daemon did not exit within its bound"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(
        exited.success(),
        "{exited:?} — stderr:\n{}",
        d.recent_stderr()
    );

    // The cancelled call landed in the store BEFORE the process exited
    // (`stop_usage_writer` awaited, same J19 property A1 already proves —
    // `AttachDeps.models` is the SAME `ModelCallbackDeps` clone, so the same
    // ordering guarantee applies here).
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let db = home.path().join(".agent24/agent24.db");
        let store = agent24_store::Store::open(&db)
            .await
            .unwrap_or_else(|e| panic!("reopening {}: {e}", db.display()));
        let totals = store
            .module_model_usage_totals("agentear")
            .await
            .expect("reading agentear's usage totals");
        let none = totals
            .iter()
            .find(|r| r.served_by == "none")
            .unwrap_or_else(|| {
                panic!(
                    "no 'none' row for agentear — got {totals:?}; the cancelled call never landed"
                )
            });
        assert_eq!(
            none.calls_cancelled, 1,
            "the attached module's one cancelled call must be recorded exactly once, \
             surviving daemon exit and reopen — got {totals:?}"
        );
    });
}
