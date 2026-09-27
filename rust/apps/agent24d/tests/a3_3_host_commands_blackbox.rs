//! A3-3 — the black-box acceptance test for the reverse-command surface
//! (`docs/design/A3-ATTACHED-MODULE.md` §6, §9 judgement C5):
//! `POST /api/v1/os/{name}/commands/{command}`.
//!
//! Harness shape (real daemon subprocess, real Python "fake AgentEar" that
//! imports NOTHING from this codebase, driven interactively over its own
//! stdin/stdout) is copied from `a3_2b_attach_blackbox.rs` — see that file's
//! module doc for the rationale behind each piece reused here verbatim. It
//! is duplicated rather than shared via a common test module on purpose:
//! A3-2b is still mid-review on the branch this PR is stacked on, and this
//! file must not need touching (or risk a merge conflict) if that one's
//! harness shifts under review.
//!
//! Scenarios, one `#[test]` each, all against a REAL running daemon and a
//! REAL attached connection unless noted:
//!
//! - [`c5_speak_reaches_the_module_and_the_body_passes_through_byte_for_byte`]
//!   — the declared command's `_a24/command/invoke` frame reaches the
//!   module, `params.name`/`params.body` match the REST request exactly
//!   (using a vendored AgentEar `agentear.command/1` fixture as the literal
//!   body, proving pass-through fidelity against a real-world shape, not a
//!   hand-rolled one), and the module's `{"accepted":true}` comes back as a
//!   `200 {"result":...}`.
//! - [`c5_an_undeclared_command_is_403_with_zero_frames_sent`] — a command
//!   name not in the manifest's `host_commands` never reaches the module.
//! - [`c5_an_unregistered_module_is_404`] and
//!   [`c5_a_registered_but_never_attached_module_is_503`] — steps ①/④ of
//!   §6.1 with no live connection at all.
//! - [`c5_no_answer_within_5s_is_504`] — the module receives the frame but
//!   never answers; the REST call is refused at the 5s deadline.
//! - [`c5_a_dropped_connection_after_the_frame_was_sent_is_502_connection_lost`]
//!   — the module receives the frame, then the connection dies before any
//!   answer — the outcome is unknown, mapped to `502 connection_lost`.
//! - [`c5_malformed_and_rpc_error_responses_map_to_502_module_error`] — a
//!   well-formed JSON-RPC error, and three malformed shapes (`result` not an
//!   object, neither `result` nor `error`, a non-object `error`) all land on
//!   `502 module_error`, the well-formed one carrying `rpc_code`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

// ───────────────────────── daemon harness (copied shape, see module doc) ─────────────────────────

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
    #[allow(dead_code)]
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

fn start(home: &Path) -> Daemon {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home);
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

/// See `a3_2b_attach_blackbox.rs::tmp_home` for why this is a short `/tmp`
/// path rather than `std::env::temp_dir()`.
static HOME_COUNTER: AtomicU64 = AtomicU64::new(0);

fn tmp_home() -> tempfile::TempDir {
    let n = HOME_COUNTER.fetch_add(1, Ordering::Relaxed);
    tempfile::Builder::new()
        .prefix(&format!("a24a33-{n}-"))
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

/// Longer than `COMMAND_TIMEOUT` (5s, `agent24_os_proto::attach_mux`) so a
/// 504 test can observe the REAL timeout rather than this socket's own.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

fn post(port: u16, token: &str, path: &str, body: &serde_json::Value) -> (u16, serde_json::Value) {
    let (status, resp) =
        raw_request_timeout(port, token, "POST", path, &body.to_string(), HTTP_TIMEOUT);
    (status, serde_json::from_str(&resp).unwrap_or_default())
}

/// Same as [`post`] but with a RAW string body — needed for the oversized-
/// body test, which must not go through `serde_json::Value` (that would
/// normalise/re-serialise it).
fn post_raw(port: u16, token: &str, path: &str, body: &str) -> (u16, serde_json::Value) {
    let (status, resp) = raw_request_timeout(port, token, "POST", path, body, HTTP_TIMEOUT);
    (status, serde_json::from_str(&resp).unwrap_or_default())
}

// ───────────────────────── attach manifest + REST helpers ─────────────────────────

fn attach_manifest(name: &str, caps: &[&str], host_commands: &[&str]) -> String {
    let hc = if host_commands.is_empty() {
        String::new()
    } else {
        format!("host_commands: [{}]\n", host_commands.join(", "))
    };
    format!(
        "name: {name}\nversion: \"1\"\nroute_namespace: /api/v1/{name}\n\
         event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
         impl_kind: attached_process\nkernel_capabilities: [{}]\n{hc}",
        caps.join(", ")
    )
}

fn register_ok(d: &Daemon, manifest: &str) -> serde_json::Value {
    let (status, body) = post(
        d.port,
        &d.token,
        "/api/v1/attached",
        &serde_json::json!({"manifest": manifest, "allow_relax": false}),
    );
    assert!(
        (200..300).contains(&status),
        "registration failed: {status} {body} — daemon stderr:\n{}",
        d.recent_stderr()
    );
    body
}

fn post_command(
    d: &Daemon,
    name: &str,
    command: &str,
    body: &serde_json::Value,
) -> (u16, serde_json::Value) {
    post(
        d.port,
        &d.token,
        &format!("/api/v1/os/{name}/commands/{command}"),
        body,
    )
}

// ───────────────────────── the fake AgentEar (Python, wire-spec-only) ─────────────────────────
//
// Same script as `a3_2b_attach_blackbox.rs`'s `FAKE_AGENTEAR_SCRIPT` — see
// that file's own doc comment for the full command reference
// (`call`/`call_nowait`/`send_raw`/`recv_line`/`wait_closed`/`close`). This
// PR's scenarios use only `recv_line` (to observe the kernel-originated
// `_a24/command/invoke` frame) and `send_raw` (to answer it, correctly or
// not) — no NEW capability is needed, so the script is copied verbatim
// rather than forked.
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

fn write_fake_agentear_script(dir: &Path) -> PathBuf {
    let path = dir.join("fake_agentear.py");
    std::fs::write(&path, FAKE_AGENTEAR_SCRIPT).unwrap();
    path
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
            "python3 must be installed and on PATH — required by A3-3's black-box judgements, a \
             hard failure rather than a skip",
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
    let line = fae.read_line();
    fae.handshake = line["response"].clone();
    fae
}

impl FakeAgentEar {
    fn read_line(&mut self) -> serde_json::Value {
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
        self.read_line()
    }

    /// Blocks (bounded by `timeout_secs`) for the NEXT line the module's
    /// socket receives — used to observe a kernel-originated
    /// `_a24/command/invoke` frame. `{"timeout": true}` in the reply means
    /// nothing arrived within the bound (used to assert "zero frames sent").
    fn recv_line(&mut self, timeout_secs: u64) -> serde_json::Value {
        self.command(serde_json::json!({"op": "recv_line", "timeout": timeout_secs}))
    }

    fn send_raw(&mut self, line: &serde_json::Value) {
        self.command(serde_json::json!({"op": "send_raw", "line": line.to_string()}));
    }

    fn close(&mut self) {
        let _ = self.command(serde_json::json!({"op": "close"}));
    }
}

/// Registers `name` (with `host_commands`) and performs a real handshake,
/// returning the daemon, the (still-connected) fake AgentEar, and the
/// manifest text (kept around only so callers that need it for a second
/// registration can reuse it — none currently do, but it costs nothing to
/// hand back).
fn attached_and_handshaken(
    home: &Path,
    caps: &[&str],
    host_commands: &[&str],
) -> (Daemon, FakeAgentEar) {
    let script = write_fake_agentear_script(home);
    let manifest_path = home.join("agentear.yml");
    let manifest = attach_manifest("agentear", caps, host_commands);
    std::fs::write(&manifest_path, &manifest).unwrap();

    let d = start(home);
    let reg = register_ok(&d, &manifest);
    let socket_path = reg["socket_path"].as_str().unwrap().to_owned();
    let token = reg["token"].as_str().unwrap().to_owned();

    let fae = spawn_fake_agentear(
        &script,
        &manifest_path,
        &socket_path,
        "agentear",
        &token,
        caps,
    );
    assert!(
        fae.handshake.get("result").is_some(),
        "handshake must succeed: {:?} — daemon stderr:\n{}",
        fae.handshake,
        d.recent_stderr()
    );
    (d, fae)
}

fn load_fixture(name: &str) -> serde_json::Value {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/agentear/contracts/fixtures/command/"
    );
    let bytes = std::fs::read(format!("{path}{name}")).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

// ───────────────────────── C5 ─────────────────────────

/// C5, positive path: the REST body reaches the module verbatim inside
/// `_a24/command/invoke {name, body}`, and the module's `{"accepted":true}`
/// comes back as a `200 {"result":...}`. Uses the vendored AgentEar
/// `agentear.command/1` fixture `command/valid/speak_full.json` as the
/// literal request body (`tests/fixtures/agentear/SOURCE` records the exact
/// commit it was copied from) — a real-world shape, not a hand-rolled one,
/// and proof that the kernel does not touch it (§6.1: "内核不解析 body 的业务
/// schema").
#[test]
fn c5_speak_reaches_the_module_and_the_body_passes_through_byte_for_byte() {
    let home = tmp_home();
    let (d, mut fae) =
        attached_and_handshaken(home.path(), &["events"], &["speak", "stop_playback"]);

    let body = load_fixture("valid/speak_full.json");
    let body_for_rest = body.clone();
    let expected_command_id = body["command_id"].clone();

    let handle = std::thread::spawn(move || post_command(&d, "agentear", "speak", &body_for_rest));

    let received = fae.recv_line(10);
    assert_eq!(received["timeout"], serde_json::Value::Null);
    let line = received["line"]
        .as_str()
        .unwrap_or_else(|| panic!("no line arrived: {received:?}"));
    let frame: serde_json::Value = serde_json::from_str(line).unwrap();
    assert_eq!(frame["method"], "_a24/command/invoke");
    assert_eq!(frame["params"]["name"], "speak");
    assert_eq!(
        frame["params"]["body"], body,
        "the REST body must reach the module byte-for-byte (as parsed JSON)"
    );
    assert_eq!(frame["params"]["body"]["command_id"], expected_command_id);
    let id = frame["id"].clone();

    fae.send_raw(&serde_json::json!({"jsonrpc": "2.0", "id": id, "result": {"accepted": true}}));

    let (status, resp) = handle.join().unwrap();
    assert_eq!(status, 200, "{resp}");
    assert_eq!(resp["result"], serde_json::json!({"accepted": true}));

    fae.close();
}

/// C5: a command not declared in the manifest's `host_commands` is refused
/// with `403` and the module receives **zero frames** for it.
#[test]
fn c5_an_undeclared_command_is_403_with_zero_frames_sent() {
    let home = tmp_home();
    let (d, mut fae) = attached_and_handshaken(home.path(), &["events"], &["speak"]);

    let (status, body) = post_command(&d, "agentear", "dance", &serde_json::json!({}));
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["code"], "forbidden");

    let received = fae.recv_line(1);
    assert_eq!(
        received["timeout"], true,
        "an undeclared command must never reach the module: {received:?}"
    );

    fae.close();
}

/// C5 / §6.1 step ①: a name that was never registered at all is `404`.
#[test]
fn c5_an_unregistered_module_is_404() {
    let home = tmp_home();
    let d = start(home.path());
    let (status, body) = post_command(&d, "nobody", "speak", &serde_json::json!({}));
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["error"]["code"], "not_found");
    stop(d);
}

/// C5 / §6.1 step ④: registered (and the command IS declared) but never
/// attached — `503 module_not_ready`.
#[test]
fn c5_a_registered_but_never_attached_module_is_503() {
    let home = tmp_home();
    let manifest = attach_manifest("agentear", &["events"], &["speak"]);
    let d = start(home.path());
    register_ok(&d, &manifest);

    let (status, body) = post_command(&d, "agentear", "speak", &serde_json::json!({}));
    assert_eq!(status, 503, "{body}");
    assert_eq!(body["error"]["code"], "module_not_ready");
    stop(d);
}

/// C5: the module receives the frame but never answers — the REST call is
/// refused at the 5s deadline with `504`, and the outcome is explicitly
/// "unknown" (no claim the module didn't act on it).
#[test]
fn c5_no_answer_within_5s_is_504() {
    let home = tmp_home();
    let (d, mut fae) = attached_and_handshaken(home.path(), &["events"], &["speak"]);

    let handle = std::thread::spawn(move || {
        post_command(&d, "agentear", "speak", &serde_json::json!({"schema": "x"}))
    });

    let received = fae.recv_line(10);
    assert_eq!(
        received["timeout"],
        serde_json::Value::Null,
        "the frame must actually arrive before we deliberately never answer it"
    );
    // Deliberately never respond.

    let (status, body) = handle.join().unwrap();
    assert_eq!(status, 504, "{body}");
    assert_eq!(body["error"]["code"], "timeout");

    fae.close();
}

/// C5: the module receives the frame, then the connection dies before any
/// answer arrives — `502 connection_lost` (distinct from `503`: the frame
/// WAS sent, so the module may have acted on it).
#[test]
fn c5_a_dropped_connection_after_the_frame_was_sent_is_502_connection_lost() {
    let home = tmp_home();
    let (d, mut fae) = attached_and_handshaken(home.path(), &["events"], &["stop_playback"]);

    let handle = std::thread::spawn(move || {
        post_command(&d, "agentear", "stop_playback", &serde_json::json!({}))
    });

    let received = fae.recv_line(10);
    assert_eq!(received["timeout"], serde_json::Value::Null);
    fae.close();

    let (status, body) = handle.join().unwrap();
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "connection_lost");
}

/// C5 / §6.1 M5: a well-formed JSON-RPC error, and three malformed shapes,
/// all collapse to `502 module_error` — the well-formed one alone carries
/// `rpc_code`.
#[test]
fn c5_malformed_and_rpc_error_responses_map_to_502_module_error() {
    let home = tmp_home();
    let (d, mut fae) = attached_and_handshaken(home.path(), &["events"], &["speak"]);

    let one_round_trip = |fae: &mut FakeAgentEar, d: &Daemon, response_line: serde_json::Value| {
        let d_port = d.port;
        let d_token = d.token.clone();
        let handle = std::thread::spawn(move || {
            let (status, resp) = post(
                d_port,
                &d_token,
                "/api/v1/os/agentear/commands/speak",
                &serde_json::json!({}),
            );
            (status, resp)
        });
        let received = fae.recv_line(10);
        let id = serde_json::from_str::<serde_json::Value>(received["line"].as_str().unwrap())
            .unwrap()["id"]
            .clone();
        let mut line = response_line;
        line["id"] = id;
        line["jsonrpc"] = serde_json::json!("2.0");
        fae.send_raw(&line);
        handle.join().unwrap()
    };

    // A well-formed JSON-RPC error.
    let (status, body) = one_round_trip(
        &mut fae,
        &d,
        serde_json::json!({"error": {"code": -32602, "message": "bad params"}}),
    );
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "module_error");
    assert_eq!(body["error"]["details"]["rpc_code"], -32602);

    // `result` present but not an object.
    let (status, body) = one_round_trip(&mut fae, &d, serde_json::json!({"result": 1}));
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "module_error");
    assert_eq!(body["error"]["message"], "malformed response");
    assert!(body["error"]["details"]["rpc_code"].is_null());

    // Neither `result` nor `error`.
    let (status, body) = one_round_trip(&mut fae, &d, serde_json::json!({}));
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "module_error");
    assert_eq!(body["error"]["message"], "malformed response");

    // `error` present but not an object.
    let (status, body) = one_round_trip(&mut fae, &d, serde_json::json!({"error": "x"}));
    assert_eq!(status, 502, "{body}");
    assert_eq!(body["error"]["code"], "module_error");
    assert_eq!(body["error"]["message"], "malformed response");

    fae.close();
}

/// C5 (400 branch, §6.1 step ③): a body over the 64 KiB cap is refused
/// before anything is sent to the module.
#[test]
fn c5_an_oversized_body_is_400_with_zero_frames_sent() {
    let home = tmp_home();
    let (d, mut fae) = attached_and_handshaken(home.path(), &["events"], &["speak"]);

    let pad = "x".repeat(70 * 1024);
    let big_body = format!(r#"{{"pad":"{pad}"}}"#);
    let (status, body) = post_raw(
        d.port,
        &d.token,
        "/api/v1/os/agentear/commands/speak",
        &big_body,
    );
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["error"]["code"], "invalid_request");

    let received = fae.recv_line(1);
    assert_eq!(
        received["timeout"], true,
        "an oversized body must never reach the module: {received:?}"
    );

    fae.close();
}
