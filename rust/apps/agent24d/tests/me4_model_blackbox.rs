//! ME4-4.3.1 — the black-box acceptance test for the inference callback
//! (`docs/design/ME4-S2-model-callback.md`, judgement J14). Harness shape
//! copied from `me3f_blackbox.rs`/`me4_scheduler_blackbox.rs`
//! (`Daemon`/`Running`/`start`/`stop`, real out-of-process Python modules,
//! bounded polls, no fixed sleeps) — see those files for the rationale
//! behind each piece this one reuses verbatim.
//!
//! **Two real Python `http.server`-shaped stand-ins for an OpenAI-compatible
//! backend (design §2.3/J16's `[::ffff:127.0.0.1]` trick for the "remote"
//! one), three real out-of-process Python modules, one real daemon — no
//! fixed sleeps, only bounded polls. `python3` missing is a hard failure of
//! this test, never a skip (task text).**
//!
//! Scenarios (task text's "至少包括" list, scoped down from J14's full set —
//! see "What this file does NOT attempt" below):
//!
//! 1. `m_local` (`kernel_capabilities: [models]`, no `model_access` →
//!    LocalOnly) calls `_a24/model/complete` with no `request_id` → the
//!    local stub receives it, the mapped result carries `tier: "local"`,
//!    the stub's `model_id`, and its `usage`; the REMOTE stub's request
//!    count stays 0 (it was never even dialled).
//! 2. **LocalOnly negative control** (design v2 M5 / v3's `[::ffff:127.0.0.1]`
//!    trick, J14): the local stub is switched to answer `503`, so the ONLY
//!    provider a LocalOnly call may use is down → `unavailable`/`no_provider`
//!    — and the remote stub's count is still 0 (a LocalOnly call must never
//!    reach a remote provider, health or no health).
//! 3. **Positive control**: `m_remote` (`model_access: remote_allowed`) with
//!    `complexity: "complex"` → routed to the remote-labelled stub (bound to
//!    `OLLAMA_URL=http://[::ffff:127.0.0.1]:<port>`, which J16/§2.3 must
//!    judge Remote even though the bytes land on the loopback interface) →
//!    `tier: "remote"`, remote stub count 1.
//! 4. A module that never requested `models` (`m_none`) calling
//!    `_a24/model/complete` → `forbidden` — the method is registered
//!    unconditionally (design §2.4/J2), so this is the handler's own check,
//!    not a missing-method 404/`-32601`.
//! 5. `GET /api/v1/usage?module=<name>` reflects scenarios 1–3 exactly
//!    (`by_served.local`/`by_served.remote`/`by_served.none` — a failed call
//!    lands under `none`, design §6.2) and the forbidden call in scenario 4
//!    left NO row at all (design §6.3: forbidden/busy/rate_limited never
//!    reach the usage sink) — waited for with a bounded poll (the write is
//!    async, `UsageRecorder`'s own channel, design §6.3), never a fixed
//!    sleep. A real daemon restart (same `$HOME`, no rebuild) proves the
//!    counts are STORED, not merely in-memory (design J11/J14).
//!
//! **What this file does NOT attempt** (J14's fuller set, deliberately left
//! to the unit-level judgements that already cover them — task text scopes
//! the blackbox down to "至少包括" 1–4 plus usage, concurrency only "if the
//! cost is controllable"): the hot-disable-while-in-flight revocation round
//! trip (J7(d)/(b)'s mechanism, already proven at the handler level with a
//! real TCP stub); the per-module concurrency/fairness ceiling (J9, already
//! proven with a frozen clock); cross-generation rate-limit persistence
//! (J10). Each of those already has a real-provider-or-real-TCP-stub unit
//! test in `model_callback.rs`; re-proving them through a full daemon +
//! Python module round trip here would multiply this file's runtime and
//! flakiness surface for coverage that already exists.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The out-of-process module: handshakes once (declaring exactly the
/// `kernel_capabilities`/`model_access` its own package baked in), then
/// serves HTTP requests in a loop — each one is this test's trigger to make
/// ONE real `_a24/model/complete` call, with the request BODY (a small JSON
/// object) as the override on top of a fixed base `messages` array. The
/// module answers with the raw JSON-RPC envelope it got back (`{"result":
/// ...}` or `{"error": ...}`) as its own HTTP body — the Rust side parses
/// that directly instead of needing a second probe-file round trip, since
/// (unlike the scheduler/T9 blackboxes) every call here is triggered
/// synchronously by an HTTP request this test itself is already making.
///
/// `__NAME__` / `__CAPS_JSON__` are substituted per module by [`install`].
const MODULE_SCRIPT: &str = r#"import hashlib, json, os, socket, sys
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
data_dir = os.environ["A24_DATA_DIR"]

cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
f = cb.makefile("rb")
next_id = [0]
def rpc(method, params):
    next_id[0] += 1
    this_id = str(next_id[0])
    req = {"jsonrpc": "2.0", "id": this_id, "method": method, "params": params}
    cb.sendall((json.dumps(req) + "\n").encode())
    line = f.readline()
    if not line:
        raise RuntimeError(f"callback socket closed waiting for a response to {method}")
    resp = json.loads(line)
    if resp.get("id") != this_id:
        raise RuntimeError(
            f"response id {resp.get('id')!r} != request id {this_id!r} for {method}: {resp}"
        )
    return resp

try:
    init_resp = rpc("initialize", {
        "protocol_versions": {"min": 1, "max": 1000}, "module": "__NAME__",
        "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
        "capabilities": __CAPS_JSON__})
    provides = init_resp.get("result", {}).get("offer", {}).get("provides", [])
    tmp = os.path.join(data_dir, "provides.json.tmp")
    with open(tmp, "w") as out:
        json.dump(provides, out)
    os.replace(tmp, os.path.join(data_dir, "provides.json"))

    listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
    while True:
        conn, _ = listener.accept()
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
                raise RuntimeError(
                    f"connection closed after {len(body_bytes)}/{content_length} "
                    f"declared body bytes; headers={headers}"
                )
            body_bytes += chunk
        overrides = json.loads(body_bytes.decode()) if body_bytes.strip() else {}

        params = {"messages": [{"role": "user", "content": "hi"}]}
        for key in ("complexity", "max_tokens", "request_id"):
            if key in overrides:
                params[key] = overrides[key]

        resp = rpc("_a24/model/complete", params)
        out_body = json.dumps(resp).encode()
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\nContent-Type: application/json\r\n\r\n%s" % (len(out_body), out_body))
        conn.close()
except Exception as e:
    with open(os.path.join(data_dir, "error.txt"), "w") as out:
        out.write(repr(e))
    print(f"model blackbox module __NAME__ failed: {e!r}", file=sys.stderr)
    raise

while f.readline():
    pass
"#;

/// A minimal OpenAI-compatible-shaped stand-in for oMLX/Ollama: a raw socket
/// accept loop (not `http.server`'s class — the same choice
/// `me4_scheduler_blackbox.rs`'s fired listener already made, for the same
/// reason: full control over the exact bytes and response mode with no
/// framework in the way). Reads `sys.argv[1]` (its control directory) and
/// `sys.argv[2]` (the `model` id it reports) at startup; prints
/// `{"port": N}` once it is listening so the Rust side never has to guess a
/// free port.
///
/// Per-request behaviour is driven by a `mode` file in the control
/// directory, re-read on EVERY request (never cached) — `"ok"` (default,
/// missing file included) answers a normal completion; `"unavailable"`
/// answers `503`, simulating a down backend for J14's negative control.
/// Every request is logged (mode + body) to `requests.json`
/// (append-atomic, same rationale as the other blackbox scripts' own
/// `dump_atomic`: a concurrent reader must never see a torn write) — the
/// Rust side reads its length as "how many times was this stub actually
/// dialled", the one fact the LocalOnly negative control and the remote
/// positive control both hinge on.
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
        append_atomic("requests.json", {
            "mode": mode,
            "request_line": lines[0].decode(errors="replace") if lines else "",
            "body": body_bytes.decode(errors="replace"),
        })
        if mode == "unavailable":
            status = b"503 Service Unavailable"
            out = json.dumps({"error": {"message": "stub backend is down", "code": "server_error"}}).encode()
        else:
            status = b"200 OK"
            out = json.dumps({
                "choices": [{"message": {"role": "assistant", "content": f"hello from {model_id}"}}],
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

/// Write one module's package under `<home>/.agent24/packages/<name>/` — the
/// same on-disk shape `agent24 os install` produces (`me3f_blackbox.rs`'s and
/// `me4_scheduler_blackbox.rs`'s own `install` do the identical thing).
/// `extra_manifest_lines` is where `model_access: remote_allowed\n` goes for
/// `m_remote`; empty for the other two.
fn install(home: &Path, name: &str, kernel_capabilities: &str, extra_manifest_lines: &str) {
    let dir = home.join(".agent24/packages").join(name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        format!(
            "name: {name}\nversion: \"0.1.0\"\nroute_namespace: /api/v1/{name}\n\
             event_module: {name}\ndata_dir: ~/.agent24/os/{name}/\n\
             kernel_capabilities: [{kernel_capabilities}]\n{extra_manifest_lines}\
             impl_kind: out_of_process_provider\n\
             spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n"
        ),
    )
    .unwrap();
    let caps_json = serde_json::to_string(
        &kernel_capabilities
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    let script = MODULE_SCRIPT
        .replace("__NAME__", name)
        .replace("__CAPS_JSON__", &caps_json);
    std::fs::write(dir.join("mod.py"), script).unwrap();
}

/// One raw HTTP round trip over a plain `TcpStream`, `connection: close` so a
/// single `read_to_end` sees the whole response — same shape as
/// `me3f_blackbox.rs`'s `get`, extended with a body and method for `POST`.
fn raw_request(port: u16, token: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect to the daemon");
    s.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
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

fn get(port: u16, token: &str, path: &str) -> (u16, String) {
    raw_request(port, token, "GET", path, "")
}

fn post(port: u16, token: &str, path: &str, body: &str) -> (u16, String) {
    raw_request(port, token, "POST", path, body)
}

/// Call `_a24/model/complete` through module `name`'s HTTP trigger and parse
/// the JSON-RPC envelope it hands back. `body_overrides` is the raw JSON
/// object (`"{}"`, `r#"{"complexity":"complex"}"#`, ...) the module layers
/// onto its fixed base `messages` array.
///
/// `os list` can report a module `mounted` slightly before it can actually
/// serve a proxied request (handshake vs. registered are two different
/// moments — `me3f_blackbox.rs`'s own `a_package_from_outside_the_repo` hits
/// the same thing and retries its first proxied call rather than gating on
/// the list) — bounded retry on the daemon's own `module_not_ready` here,
/// same reasoning.
fn call_model(port: u16, token: &str, name: &str, body_overrides: &str) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (status, body) = post(port, token, &format!("/api/v1/{name}/call"), body_overrides);
        if status == 200 {
            return serde_json::from_str(&body)
                .unwrap_or_else(|e| panic!("the module's HTTP body was not JSON ({e}): {body}"));
        }
        assert!(
            status == 503 && body.contains("module_not_ready") && Instant::now() < deadline,
            "the module's own HTTP trigger answered {status}: {body}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Graceful-first shutdown, matching `me3f_blackbox.rs::Running` verbatim.
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

/// Start the already-built `agent24d` binary against `home`, with
/// `OMLX_URL`/`OLLAMA_URL` pointed at this test's two stubs (`ModelRouter::
/// from_env`, `router.rs:258`, reads them once at `serve()` startup — design
/// §2.3/J16). Same shape as `me3f_blackbox.rs::start`.
fn start(home: &Path, omlx_url: &str, ollama_url: &str) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("OMLX_URL", omlx_url)
        .env("OLLAMA_URL", ollama_url)
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

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24-me4-4-3-1")
        .tempdir_in("/tmp")
        .unwrap()
}

fn os_state(port: u16, token: &str, name: &str) -> serde_json::Value {
    let (status, body) = get(port, token, "/api/v1/os");
    assert_eq!(status, 200, "{body}");
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    list["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} in the list: {body}"))
}

/// Bounded poll for `mounted` — same reasoning `me4_scheduler_blackbox.rs`
/// documents on its own copy of this helper.
fn wait_mounted(port: u16, token: &str, name: &str, stderr: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = os_state(port, token, name);
        if state["state"] == "mounted" {
            return;
        }
        assert!(
            !matches!(state["state"].as_str(), Some("degraded" | "refused")),
            "{name} reached a terminal state while waiting for \"mounted\": {state} \
             (detail: {}); daemon stderr:\n{}",
            state["detail"],
            stderr()
        );
        assert!(
            Instant::now() < deadline,
            "{name} never reached state \"mounted\": {state}; daemon stderr:\n{}",
            stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// `GET /api/v1/usage?module=<name>`, parsed. No polling here — callers that
/// need to wait for the async usage writer use [`wait_for_usage`] instead.
fn get_usage(port: u16, token: &str, name: &str) -> serde_json::Value {
    let (status, body) = get(port, token, &format!("/api/v1/usage?module={name}"));
    assert_eq!(status, 200, "{body}");
    serde_json::from_str(&body).unwrap()
}

/// Bounded poll (design §6.3: the usage writer is a separate task behind a
/// channel, `try_send` — the caller never awaits the write) for
/// `GET /api/v1/usage?module=<name>` to satisfy `ready`. Never a fixed sleep.
fn wait_for_usage(
    port: u16,
    token: &str,
    name: &str,
    ready: impl Fn(&serde_json::Value) -> bool,
    stderr: impl Fn() -> String,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let usage = get_usage(port, token, name);
        if ready(&usage) {
            return usage;
        }
        assert!(
            Instant::now() < deadline,
            "usage for {name} never reached the expected shape within 10s: {usage}; \
             daemon stderr:\n{}",
            stderr()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Spawns [`STUB_SCRIPT`] and blocks (bounded) until it reports the port it
/// bound. `python3` missing/failing to spawn is a hard panic here, never a
/// skip (task text: "缺 python3 时测试直接失败，不许跳过").
struct Stub {
    #[allow(dead_code)]
    run: Running,
    port: u16,
    control_dir: std::path::PathBuf,
    #[allow(dead_code)]
    stderr: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Stub {
    fn request_count(&self) -> usize {
        std::fs::read(self.control_dir.join("requests.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.as_array().map(Vec::len))
            .unwrap_or(0)
    }

    fn set_mode(&self, mode: &str) {
        let path = self.control_dir.join("mode");
        let tmp = self.control_dir.join("mode.tmp");
        std::fs::write(&tmp, mode).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
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
        .expect(
            "python3 must be installed and on PATH — required by ME4-4.3.1's black-box \
             judgement J14 (a real OpenAI-compatible stand-in), a hard failure rather than \
             a skip",
        );
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
        .recv_timeout(Duration::from_secs(10))
        .expect("the stub never printed its {\"port\": ...} line within 10s");
    let ready: serde_json::Value =
        serde_json::from_str(&ready).expect("the stub's ready line was not JSON");
    let stderr_lines = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = stderr_lines.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            sink.lock().unwrap().push(line);
        }
    });
    Stub {
        run,
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        control_dir: control_dir.to_owned(),
        stderr: stderr_lines,
    }
}

/// **ME4-4.3.1 — the black-box acceptance test.** See this file's module doc
/// for the scenarios. `#[test]`, not `#[tokio::test]`: every wait below is a
/// real bounded poll over real wall-clock time, driving real second/third
/// processes.
#[test]
fn model_complete_blackbox_round_trip() {
    let home = tmp_home();

    let local_ctl = home.path().join("stub-local");
    let remote_ctl = home.path().join("stub-remote");
    let local_stub = start_stub(&local_ctl, "stub-model-local");
    let remote_stub = start_stub(&remote_ctl, "stub-model-remote");

    // v3's own trick (design §2.3/J16, J14): the remote stub is bound to
    // plain `127.0.0.1`, but reported to the daemon under the IPv4-mapped
    // IPv6 form — bytes land on loopback either way, but `env_local_tier`
    // (`router.rs:163`) must judge THIS string Remote, not Local, or a
    // LocalOnly call could reach it.
    let omlx_url = format!("http://127.0.0.1:{}", local_stub.port);
    let ollama_url = format!("http://[::ffff:127.0.0.1]:{}", remote_stub.port);

    install(home.path(), "m_local", "models", "");
    install(
        home.path(),
        "m_remote",
        "models",
        "model_access: remote_allowed\n",
    );
    install(home.path(), "m_none", "", "");

    let d1 = start(home.path(), &omlx_url, &ollama_url);
    wait_mounted(d1.port, &d1.token, "m_local", || d1.recent_stderr());
    wait_mounted(d1.port, &d1.token, "m_remote", || d1.recent_stderr());
    wait_mounted(d1.port, &d1.token, "m_none", || d1.recent_stderr());

    // ── scenario 1: normal LocalOnly success ────────────────────────────
    let r1 = call_model(d1.port, &d1.token, "m_local", "{}");
    assert!(r1.get("error").is_none(), "{r1}");
    assert_eq!(r1["result"]["tier"], "local", "{r1}");
    assert_eq!(r1["result"]["model_id"], "stub-model-local", "{r1}");
    assert_eq!(r1["result"]["usage"]["prompt_tokens"], 11, "{r1}");
    assert_eq!(r1["result"]["usage"]["completion_tokens"], 7, "{r1}");
    assert!(
        !r1["result"]["text"].as_str().unwrap_or_default().is_empty(),
        "{r1}"
    );
    assert_eq!(
        remote_stub.request_count(),
        0,
        "a LocalOnly call must never dial the remote stub"
    );

    // ── scenario 2: LocalOnly negative control (design v2 M5) — the ONLY
    //    provider a LocalOnly call may use goes down; the remote stub must
    //    still see zero requests. ─────────────────────────────────────────
    local_stub.set_mode("unavailable");
    let r2 = call_model(d1.port, &d1.token, "m_local", "{}");
    assert!(r2.get("result").is_none(), "{r2}");
    assert_eq!(r2["error"]["data"]["kind"], "unavailable", "{r2}");
    assert_eq!(r2["error"]["data"]["cause"], "no_provider", "{r2}");
    assert_eq!(r2["error"]["data"]["retryable"], true, "{r2}");
    assert_eq!(
        remote_stub.request_count(),
        0,
        "the negative control must not have reached the remote stub either: {r2}"
    );
    local_stub.set_mode("ok");

    // ── scenario 3: positive control — remote_allowed + complex routes to
    //    the remote-labelled stub. ───────────────────────────────────────
    let r3 = call_model(
        d1.port,
        &d1.token,
        "m_remote",
        r#"{"complexity":"complex"}"#,
    );
    assert!(r3.get("error").is_none(), "{r3}");
    assert_eq!(r3["result"]["tier"], "remote", "{r3}");
    assert_eq!(r3["result"]["model_id"], "stub-model-remote", "{r3}");
    assert_eq!(
        remote_stub.request_count(),
        1,
        "the positive control must be the remote stub's first and only request so far: {r3}"
    );

    // ── scenario 4: a module that never requested `models` is forbidden —
    //    the method is registered unconditionally (design §2.4/J2), so this
    //    is NOT a missing-method 404/-32601. ─────────────────────────────
    let r4 = call_model(d1.port, &d1.token, "m_none", "{}");
    assert!(r4.get("result").is_none(), "{r4}");
    assert_eq!(r4["error"]["data"]["kind"], "forbidden", "{r4}");

    // ── scenario 5: usage by module — bounded poll for the async writer,
    //    exact figures from scenarios 1-3, and the forbidden call in
    //    scenario 4 left no row at all. ───────────────────────────────────
    let usage_local = wait_for_usage(
        d1.port,
        &d1.token,
        "m_local",
        |u| u["by_served"]["local"]["calls_ok"] == 1 && u["by_served"]["none"]["calls_failed"] == 1,
        || d1.recent_stderr(),
    );
    assert_eq!(usage_local["totals"]["calls_ok"], 1, "{usage_local}");
    assert_eq!(usage_local["totals"]["calls_failed"], 1, "{usage_local}");
    assert_eq!(usage_local["totals"]["calls_cancelled"], 0, "{usage_local}");
    assert_eq!(
        usage_local["by_served"]["local"]["prompt_tokens"], 11,
        "{usage_local}"
    );
    assert_eq!(
        usage_local["by_served"]["local"]["completion_tokens"], 7,
        "{usage_local}"
    );
    assert_eq!(
        usage_local["by_served"]["remote"]["calls_ok"], 0,
        "m_local must never appear under the remote bucket: {usage_local}"
    );

    let usage_remote = wait_for_usage(
        d1.port,
        &d1.token,
        "m_remote",
        |u| u["by_served"]["remote"]["calls_ok"] == 1,
        || d1.recent_stderr(),
    );
    assert_eq!(usage_remote["totals"]["calls_ok"], 1, "{usage_remote}");
    assert_eq!(usage_remote["totals"]["calls_failed"], 0, "{usage_remote}");

    let usage_none = get_usage(d1.port, &d1.token, "m_none");
    assert_eq!(
        usage_none["totals"]["calls_ok"], 0,
        "a forbidden call must not be metered at all: {usage_none}"
    );
    assert_eq!(usage_none["totals"]["calls_failed"], 0, "{usage_none}");
    assert_eq!(usage_none["totals"]["calls_cancelled"], 0, "{usage_none}");

    stop(d1);

    // ── restart persistence: the counts are STORED, not merely
    //    in-memory (design J11/J14) — same $HOME, no rebuild. ────────────
    let d2 = start(home.path(), &omlx_url, &ollama_url);
    wait_mounted(d2.port, &d2.token, "m_local", || d2.recent_stderr());
    let usage_after_restart = get_usage(d2.port, &d2.token, "m_local");
    assert_eq!(
        usage_after_restart["totals"]["calls_ok"], 1,
        "{usage_after_restart}"
    );
    assert_eq!(
        usage_after_restart["totals"]["calls_failed"], 1,
        "{usage_after_restart}"
    );
    stop(d2);
}

/// v3.1 L-4: a real oMLX smoke test — NOT part of the ordinary suite (no
/// stub, no CI-controlled backend). Run with the exact test name so it is
/// not swept in with any other `#[ignore]`d test:
/// `cargo test -p agent24d --test me4_model_blackbox -- --exact real_omlx_smoke --ignored`.
/// Requires a real oMLX server already listening on `OMLX_URL`
/// (`http://127.0.0.1:8088` by default) with a model actually loaded.
#[test]
#[ignore = "requires a real oMLX server listening on OMLX_URL; run with --exact --ignored"]
fn real_omlx_smoke() {
    let home = tmp_home();
    install(home.path(), "m_local", "models", "");
    let omlx_url = std::env::var("OMLX_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned());
    // A real remote provider is deliberately NOT configured — this smoke
    // test only proves the LocalOnly path against a real backend.
    let d = start(home.path(), &omlx_url, "http://127.0.0.1:1");
    wait_mounted(d.port, &d.token, "m_local", || d.recent_stderr());
    let r = call_model(d.port, &d.token, "m_local", "{}");
    assert!(
        r.get("error").is_none(),
        "a real oMLX server on {omlx_url} must answer a LocalOnly call: {r}"
    );
    assert_eq!(r["result"]["tier"], "local", "{r}");
    assert!(
        !r["result"]["text"].as_str().unwrap_or_default().is_empty(),
        "{r}"
    );
    stop(d);
}
