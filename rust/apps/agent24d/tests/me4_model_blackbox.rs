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
//! Scenarios (`docs/design/ME4-S2-model-callback.md`'s own J14 entry, §8):
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
//! 6. **The real revocation path** (design J14's own "本地桩对下一次调用挂起…"
//!    bullet): the local stub is put into a `"hang"` mode (accepts the
//!    request, answers nothing, then blocks on its own `recv` to learn
//!    whether ITS PEER — this daemon — closed the connection). A background
//!    thread starts a call against `m_local`; once the stub has actually
//!    received that request, this test issues the real disable —
//!    `PATCH /api/v1/os/m_local {"enabled":false}`, the exact REST route
//!    `agent24 os disable m_local` itself calls — against the running
//!    daemon, and the stub (not an in-process fixture) is polled for having
//!    observed the close. **Negative control**: before that `PATCH` is ever
//!    sent, the stub must still be sitting in its `"waiting"` phase, never
//!    `"done"`, for a real window — proving the close asserted afterwards is
//!    caused by the disable and not some unrelated timeout in the harness.
//!
//! **What this file does NOT attempt**: the per-module concurrency/fairness
//! ceiling (J9, already proven with a frozen clock) and cross-generation
//! rate-limit persistence (J10) — each already has a real-provider-or-real-
//! TCP-stub unit test in `model_callback.rs`; re-proving them through a full
//! daemon + Python module round trip here would multiply this file's runtime
//! and flakiness surface for coverage that already exists.

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
/// answers `503`, simulating a down backend for J14's negative control;
/// `"hang"` answers nothing at all — it accepts the request, then blocks on
/// its own `recv()` to learn whether ITS PEER (this daemon) closed the
/// connection, for J14's real-revocation-path scenario (H1): the very
/// instant it starts that wait it writes `hang_result.json` as
/// `{"phase":"waiting","closed":null}` (so a poller can tell "received the
/// request, now genuinely blocked" from "hasn't been dialled yet"), then
/// once `recv()` returns — empty bytes or a reset both count as "closed",
/// a timeout (60s, just a safety net; the real bound is the Rust side's own
/// poll deadline) counts as "not closed" — overwrites it with
/// `{"phase":"done","closed":<bool>}`.
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
        if mode == "hang":
            dump_atomic("hang_result.json", {"phase": "waiting", "closed": None})
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
/// `me3f_blackbox.rs`'s `get`, extended with a body and method for `POST`,
/// and a caller-chosen read timeout: H1's hang-then-disable scenario fires a
/// call that may sit on the wire far longer than the 20s every other request
/// in this file is happy with (the real per-module disable drain can run for
/// tens of seconds — see [`call_model_bg`]).
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

fn get(port: u16, token: &str, path: &str) -> (u16, String) {
    raw_request(port, token, "GET", path, "")
}

fn post(port: u16, token: &str, path: &str, body: &str) -> (u16, String) {
    raw_request(port, token, "POST", path, body)
}

fn patch(port: u16, token: &str, path: &str, body: &str) -> (u16, String) {
    raw_request(port, token, "PATCH", path, body)
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

/// H1's own long-lived variant of [`call_model`], spawned on a background
/// thread while the local stub is in `"hang"` mode: the successful attempt
/// may legitimately sit on the wire for as long as the real per-module
/// disable takes to actually cut it off (`DISABLE_REVOCATION_BOUND` below) —
/// the ordinary [`raw_request`]'s 20s default would fire spuriously there.
/// Still retries on `module_not_ready` like [`call_model`] does (a fresh
/// restart, `d2`, may not yet have this proxied route ready the instant
/// `wait_mounted` returns — same race `call_model` itself documents) — every
/// attempt uses `timeout`, harmless for the fast 503 retries and necessary
/// for the one that actually reaches the (hung) local stub.
fn call_model_bg(
    port: u16,
    token: &str,
    name: &str,
    timeout: Duration,
) -> std::thread::JoinHandle<(u16, String)> {
    let token = token.to_owned();
    let name = name.to_owned();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (status, body) = raw_request_timeout(
                port,
                &token,
                "POST",
                &format!("/api/v1/{name}/call"),
                "{}",
                timeout,
            );
            if status != 503 || !body.contains("module_not_ready") || Instant::now() >= deadline {
                return (status, body);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })
}

/// How long H1's revocation scenario waits for the local stub to observe its
/// connection close after the real disable is sent. `os_routes.rs`'s own
/// `DISABLE_DRAIN` (30s) bounds how long a module's supervisor lets an
/// in-flight, unbound (no `request_id`) call run before forcing the module's
/// process to stop — which is what severs this connection — so this bound
/// must clear 30s with real margin, never assume the cut is instant.
const DISABLE_REVOCATION_BOUND: Duration = Duration::from_secs(45);

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
/// §2.3/J16). Same shape as `me3f_blackbox.rs::start`. `extra_env` is layered
/// on top (after `OMLX_URL`/`OLLAMA_URL`, so it can override either) — empty
/// for every caller except [`real_omlx_smoke`] (L5), which uses it to pass
/// `OMLX_API_KEY`/`DEFAULT_MODEL` through from this test process's own
/// environment into the spawned daemon's (`env_clear()` below would
/// otherwise silently drop them and fall back to the built-in defaults,
/// `agent24-models/src/router.rs:261`/`:263`, which is not what a real smoke
/// run against the operator's own oMLX server wants).
fn start(home: &Path, omlx_url: &str, ollama_url: &str, extra_env: &[(&str, &str)]) -> Daemon {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("OMLX_URL", omlx_url)
        .env("OLLAMA_URL", ollama_url);
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

    /// The `mode` field this stub itself recorded for the MOST RECENT request
    /// it actually received (L1) — distinct from `request_count`, which only
    /// says how many, not what each one saw.
    fn last_request_mode(&self) -> Option<String> {
        let v: serde_json::Value =
            serde_json::from_slice(&std::fs::read(self.control_dir.join("requests.json")).ok()?)
                .ok()?;
        v.as_array()?.last()?["mode"].as_str().map(str::to_owned)
    }

    fn set_mode(&self, mode: &str) {
        let path = self.control_dir.join("mode");
        let tmp = self.control_dir.join("mode.tmp");
        std::fs::write(&tmp, mode).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }

    /// `hang_result.json`, written only in `"hang"` mode — `None` before the
    /// stub has received anything; `Some(("waiting", None))` once it has
    /// received the request and is blocked on its own `recv`; `Some(("done",
    /// Some(closed)))` once that `recv` returned (H1).
    fn hang_status(&self) -> Option<(String, Option<bool>)> {
        let v: serde_json::Value = std::fs::read(self.control_dir.join("hang_result.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())?;
        let phase = v["phase"].as_str()?.to_owned();
        let closed = v["closed"].as_bool();
        Some((phase, closed))
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

    let d1 = start(home.path(), &omlx_url, &ollama_url, &[]);
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
    assert_eq!(
        local_stub.request_count(),
        1,
        "scenario 1 must be the local stub's first and only request so far: {r1}"
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
    assert_eq!(
        local_stub.request_count(),
        2,
        "the negative control must itself have dialled the local stub once more: {r2}"
    );
    assert_eq!(
        local_stub.last_request_mode().as_deref(),
        Some("unavailable"),
        "the local stub's own record of that request must show the mode this negative \
         control set, not a stale one from before: {r2}"
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
    let (local_before_r4, remote_before_r4) =
        (local_stub.request_count(), remote_stub.request_count());
    let r4 = call_model(d1.port, &d1.token, "m_none", "{}");
    assert!(r4.get("result").is_none(), "{r4}");
    assert_eq!(r4["error"]["data"]["kind"], "forbidden", "{r4}");
    assert_eq!(
        local_stub.request_count(),
        local_before_r4,
        "a forbidden call must never reach any provider: {r4}"
    );
    assert_eq!(
        remote_stub.request_count(),
        remote_before_r4,
        "a forbidden call must never reach any provider: {r4}"
    );

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
    let d2 = start(home.path(), &omlx_url, &ollama_url, &[]);
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

    // ── L4: the OTHER two modules' usage rows persisted across the restart
    //    exactly as well — `m_none`'s forbidden call left nothing at all
    //    (still true after a restart, not merely before one), and
    //    `m_remote`'s single successful remote call is still there. ───────
    let usage_none_after_restart = get_usage(d2.port, &d2.token, "m_none");
    assert_eq!(
        usage_none_after_restart["totals"]["calls_ok"], 0,
        "{usage_none_after_restart}"
    );
    assert_eq!(
        usage_none_after_restart["totals"]["calls_failed"], 0,
        "{usage_none_after_restart}"
    );
    assert_eq!(
        usage_none_after_restart["totals"]["calls_cancelled"], 0,
        "{usage_none_after_restart}"
    );
    let usage_remote_after_restart = get_usage(d2.port, &d2.token, "m_remote");
    assert_eq!(
        usage_remote_after_restart["totals"]["calls_ok"], 1,
        "{usage_remote_after_restart}"
    );
    assert_eq!(
        usage_remote_after_restart["totals"]["calls_failed"], 0,
        "{usage_remote_after_restart}"
    );
    assert_eq!(
        usage_remote_after_restart["by_served"]["remote"]["calls_ok"], 1,
        "{usage_remote_after_restart}"
    );

    // ── H1: the real revocation path — `agent24 os disable m_local` while a
    //    call is genuinely in flight against the real local stub, observed
    //    from the stub's own side (design J14's own "本地桩对下一次调用
    //    挂起…" bullet — the fuller judgement text this file's module doc
    //    used to claim was "already proven" purely at the handler level;
    //    it was not proven end-to-end through a real disable until now). ──
    local_stub.set_mode("hang");
    let before_hang = local_stub.request_count();
    let hang_call = call_model_bg(d2.port, &d2.token, "m_local", DISABLE_REVOCATION_BOUND);

    // The stub must have actually received this call — i.e. it is now
    // genuinely blocked, not merely "about to be dialled" — before either
    // control below means anything.
    let received_by = Instant::now() + Duration::from_secs(10);
    while local_stub.request_count() == before_hang {
        assert!(
            Instant::now() < received_by,
            "the hang call never reached the local stub within 10s"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        local_stub
            .hang_status()
            .as_ref()
            .map(|(phase, _)| phase.as_str()),
        Some("waiting"),
        "the stub logged the request but is not (yet) blocked on it"
    );

    // **Negative control**: nothing has been disabled yet — for a real
    // window, the stub must keep reporting "waiting", never "done". If this
    // ever saw "done" here, the close proven below would not be caused by
    // the disable that follows.
    let no_disable_yet_until = Instant::now() + Duration::from_millis(500);
    while Instant::now() < no_disable_yet_until {
        assert_ne!(
            local_stub.hang_status(),
            Some(("done".to_owned(), Some(true))),
            "the stub observed its connection close before any disable was sent"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The real disable: the exact REST route `agent24 os disable m_local`
    // itself calls (`os_routes.rs::patch_os`).
    let (disable_status, disable_body) = patch(
        d2.port,
        &d2.token,
        "/api/v1/os/m_local",
        r#"{"enabled":false}"#,
    );
    assert_eq!(
        disable_status, 200,
        "disabling m_local: {disable_status} {disable_body}"
    );

    // Bounded poll — real per-module disables drain for up to
    // `os_routes.rs`'s `DISABLE_DRAIN` (30s) before the module's process is
    // actually stopped, which is what severs this connection; never a fixed
    // sleep.
    let closed_by = Instant::now() + DISABLE_REVOCATION_BOUND;
    loop {
        let status = local_stub.hang_status();
        if let Some((phase, closed)) = &status
            && phase == "done"
        {
            assert_eq!(
                *closed,
                Some(true),
                "the stub's own connection ended, but not because its peer closed it: {status:?}"
            );
            break;
        }
        assert!(
            Instant::now() < closed_by,
            "the local stub never observed its connection close after disabling m_local \
             (status: {status:?}); daemon stderr:\n{}",
            d2.recent_stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = hang_call.join();

    stop(d2);
}

/// v3.1 L-4: a real oMLX smoke test — NOT part of the ordinary suite (no
/// stub, no CI-controlled backend). Run with the exact test name so it is
/// not swept in with any other `#[ignore]`d test:
/// `cargo test -p agent24d --test me4_model_blackbox -- --exact real_omlx_smoke --ignored`.
/// Requires a real oMLX server already listening on `OMLX_URL`
/// (`http://127.0.0.1:8088` by default) with a model actually loaded. L5:
/// `OMLX_API_KEY`/`DEFAULT_MODEL`, if this test process itself has them set,
/// are passed through to the spawned daemon (`start`'s `extra_env`) — the
/// daemon's own `env_clear()` would otherwise silently fall back to the
/// built-in defaults (`xiaobao8088`/`Qwen3-8B-4bit`), which is not what a
/// real oMLX server configured with a different key or model wants.
#[test]
#[ignore = "requires a real oMLX server listening on OMLX_URL; run with --exact --ignored"]
fn real_omlx_smoke() {
    let home = tmp_home();
    install(home.path(), "m_local", "models", "");
    let omlx_url = std::env::var("OMLX_URL").unwrap_or_else(|_| "http://127.0.0.1:8088".to_owned());
    let omlx_key = std::env::var("OMLX_API_KEY").ok();
    let default_model = std::env::var("DEFAULT_MODEL").ok();
    let mut extra_env = Vec::new();
    if let Some(k) = &omlx_key {
        extra_env.push(("OMLX_API_KEY", k.as_str()));
    }
    if let Some(m) = &default_model {
        extra_env.push(("DEFAULT_MODEL", m.as_str()));
    }
    // `OLLAMA_URL` points at a dead port on loopback — NOT "no remote
    // provider configured" (a router with no `OLLAMA_URL` at all falls back
    // to its own default, `router.rs:271`, which could be a live host on
    // this machine); pointing it at a closed port on 127.0.0.1 guarantees
    // any accidental remote dial fails fast instead of quietly succeeding,
    // which is what actually keeps this smoke test LocalOnly-only.
    let d = start(home.path(), &omlx_url, "http://127.0.0.1:1", &extra_env);
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
