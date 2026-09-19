//! T9 / ME-3f — the black-box acceptance test for "a third party can write
//! their own domain OS and Agent24 loads it, zero kernel changes" (SPEC
//! `SPEC-ME3-OUT-OF-PROCESS.md:596`, ME-3f row): **build the daemon once;
//! generate and install a package OUTSIDE this repository; without touching
//! the source or rebuilding, restart the daemon; mount → routing proxy →
//! event forwarding → memory read/write → approval round trip must all be
//! green.**
//!
//! Design (kept short — this is acceptance-test plumbing, not a new
//! architectural decision; the mechanisms it exercises were each already
//! designed and reviewed under T7/T8/T8.5/T8.5c-P/T8.5c-W):
//!
//! - **"Outside the repo"**: [`tmp_home`] returns a `tempfile` directory under
//!   the OS temp dir, physically unrelated to this git checkout. The package
//!   is written under `<tmp_home>/.agent24/packages/blackbox/`, the same
//!   layout `agent24 os install <dir>` produces and that `agent24d` reads at
//!   startup — this test writes the files directly (the same choice
//!   `daemon_modules.rs` already made, see its own module doc: "driven
//!   through the real binary, because the parts that matter live in
//!   `serve`") rather than shelling out to the `agent24` CLI, since the
//!   thing under test is the daemon's mount/proxy/callback path, not the CLI
//!   argument parser.
//! - **"Not rebuilt"**: the daemon binary is the one `cargo test` already
//!   built (`env!("CARGO_BIN_EXE_agent24d")`, resolved once at compile time).
//!   The test starts it, stops it, and starts it again against the same
//!   `$HOME` — a real second process lifetime, not a simulated one — with no
//!   `cargo build` in between.
//! - **The five round trips**: a single Python module (spawned as the real
//!   out-of-process child, matching `daemon_modules.rs`'s and
//!   `domain.rs`'s `*_PROBE_MODULE` fixtures) declares
//!   `kernel_capabilities: [events, memory, approval]` and, over its real
//!   callback socket after a real `initialize` handshake, makes one real
//!   `_a24/events/emit`, one real `_a24/memory/private/remember` and one
//!   real `_a24/memory/private/recall` call, writing each JSON-RPC response
//!   to its data directory. Its HTTP handler (reached through the real
//!   kernel proxy) reads the real `x-a24-request-id`/`x-a24-approval-token`
//!   headers the proxy mints for that request (`agent24_os_proto::proxy`)
//!   and uses them for one real `_a24/approval/gate` call — approval
//!   submission requires a request actually in flight, so that round trip
//!   cannot happen at handshake time like the other three.
//!
//! What this file does NOT attempt: a real Node.js module (that is T13/T14's
//! wire-doc/SDK conformance test, which must prove the documentation is
//! complete without looking at this repo's own protocol code — a Python
//! script written by the test author proves nothing about that); a real
//! multi-request concurrency stress test (already covered by
//! `os_memory_page.rs`'s admission-sharing judgements and
//! `proxy.rs`'s own suite).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// The out-of-process module: declares `events`/`memory`/`approval`, does two
/// real callback round trips right after handshaking (remember, recall —
/// neither needs a live HTTP request), then serves HTTP requests through the
/// real proxy IN A LOOP — more than one, because the Rust side may need to
/// retry its trigger if the WS subscriber it connects before this loop races
/// the daemon's own subscription setup (see `spawn_ws_subscriber`'s doc).
/// Inside each request's handler — because both calls genuinely cannot
/// happen any earlier — it reads the real per-request headers, submits
/// `_a24/approval/gate` once with a DELIBERATELY WRONG token (must be
/// rejected without consuming the real one), once for real, and emits
/// `_a24/events/emit` — moved here, not right after handshake, so a WS
/// subscriber proves delivery, not just the callback's ack.
const BLACKBOX_MODULE: &str = r#"import hashlib, json, os, socket, sys
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
data_dir = os.environ["A24_DATA_DIR"]

def dump_atomic(name, value):
    # gate_probe.json in particular gets rewritten on every retry (the Rust
    # side may trigger more than one request) while the Rust side can be
    # reading it concurrently the moment it observes a WS event — a plain
    # `open(..., "w")` truncates before writing, so a read landing in that
    # window sees an empty or partial file. Write-to-temp + rename is atomic
    # on the same filesystem (POSIX rename(2)), so any concurrent reader
    # sees either the old complete file or the new complete one, never
    # neither.
    path = os.path.join(data_dir, name)
    tmp = path + ".tmp"
    with open(tmp, "w") as out:
        json.dump(value, out)
    os.replace(tmp, path)

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
        "protocol_versions": {"min": 1, "max": 1000}, "module": "blackbox",
        "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
        "capabilities": ["events", "memory", "approval"]})
    provides = init_resp.get("result", {}).get("offer", {}).get("provides", [])

    remember_resp = rpc("_a24/memory/private/remember", {"kind": "t9-note", "body": {"text": "t9-blackbox"}})
    recall_resp = rpc("_a24/memory/private/recall", {"query": "t9-note", "page_size": 10})

    dump_atomic("callback_probe.json", {
        "provides": provides,
        "remember_response": remember_resp,
        "recall_response": recall_resp,
    })

    listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
    # Serves MORE THAN ONE request: the Rust side may need to retry the HTTP
    # trigger if its WS subscription (a separate, unsynchronized connection)
    # was not yet live in time for the FIRST emit — each retry gets its own
    # real request_id/approval_token from the proxy, so retrying is not
    # replaying anything, it is a fresh real request every time.
    while True:
        conn, _ = listener.accept()
        head = b""
        while b"\r\n\r\n" not in head:
            chunk = conn.recv(4096)
            if not chunk:
                break
            head += chunk
        lines = head.split(b"\r\n")
        headers = {}
        for line in lines[1:]:
            if b":" in line:
                k, v = line.split(b":", 1)
                headers[k.strip().lower().decode()] = v.strip().decode()

        gate_params = {
            "action": "schedule_callback",
            "target": "2099-01-01T00:00:00Z",
            "payload": {},
            "request_id": headers.get("x-a24-request-id", ""),
        }
        # Negative control FIRST, same request_id: a wrong token must be
        # refused and — per the kernel's own admission semantics — must NOT
        # consume the real token, so the correct call right after it still
        # succeeds.
        bad_gate_resp = rpc("_a24/approval/gate", {
            **gate_params, "approval_token": "definitely-not-the-real-token",
        })
        gate_resp = rpc("_a24/approval/gate", {
            **gate_params, "approval_token": headers.get("x-a24-approval-token", ""),
        })
        emit_resp = rpc("_a24/events/emit", {"kind": "task.transitioned", "payload": {"probe": "t9"}})
        dump_atomic("gate_probe.json", {
            "headers_seen": headers,
            "bad_gate_response": bad_gate_resp,
            "gate_response": gate_resp,
            "emit_response": emit_resp,
        })

        body = b"hello"
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body))
        conn.close()
except Exception as e:
    with open(os.path.join(data_dir, "error.txt"), "w") as out:
        out.write(repr(e))
    print(f"blackbox module failed: {e!r}", file=sys.stderr)
    raise

while f.readline():
    pass
"#;

/// Write the blackbox package under `<home>/.agent24/packages/blackbox/` —
/// the same on-disk shape `agent24 os install` produces.
fn install(home: &Path) {
    let dir = home.join(".agent24/packages/blackbox");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: blackbox\nversion: \"0.1.0\"\nroute_namespace: /api/v1/blackbox\n\
         event_module: blackbox\ndata_dir: ~/.agent24/os/blackbox/\n\
         kernel_capabilities: [events, memory, approval]\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod.py"), BLACKBOX_MODULE).unwrap();
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

/// The daemon — graceful-first, on EVERY exit path (a panicking assertion
/// unwinds through this exactly the same as a normal `stop()`, since both
/// just drop the `Daemon`/`Running`). The module is spawned into its own
/// process group (`agent24_os_proto::launch`) and it is the daemon's OWN
/// supervisor that reaps that group on a clean shutdown —
/// `agent24_os_proto::supervise` documents that a SIGKILLed daemon never
/// runs that logic. SIGTERM and a bounded wait give the real shutdown path
/// (the one that reaps the module) a chance to run; SIGKILL is only the
/// fallback for a daemon that does not exit in time. Confirmed empirically
/// (Codex review): unconditional SIGKILL here left the Python module
/// orphaned, blocked forever in `listener.accept()` — `ps` showed 34
/// accumulated from this file's own prior runs before this fix, 0 after.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
        let pid = self.0.id();
        if Command::new("kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .is_ok_and(|s| s.success())
        {
            let by = Instant::now() + Duration::from_secs(10);
            while self.0.try_wait().is_ok_and(|s| s.is_none()) && Instant::now() < by {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        // Fallback: not reached if SIGTERM above already got it (`kill()`
        // and `wait()` on an already-exited/reaped `Child` are no-ops), so
        // this never double-reaps or re-signals a process that is gone.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

struct Daemon {
    // Never read directly — kept alive so `Running`'s `Drop` kills the
    // process when `Daemon` goes out of scope (see `stop`).
    #[allow(dead_code)]
    run: Running,
    port: u16,
    token: String,
    /// The daemon's stderr, line by line — surfaced on a timeout panic so a
    /// hang is diagnosable instead of just "the module never answered"
    /// (Codex review: Python failures were fail-closed but silent).
    stderr: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
}

impl Daemon {
    fn recent_stderr(&self) -> String {
        self.stderr.lock().unwrap().join("\n")
    }
}

/// Start the already-built `agent24d` binary against `home` — no `cargo
/// build` here or anywhere else in this file. Matches `daemon_modules.rs`'s
/// `start`.
///
/// The `Child` is wrapped into `Running` IMMEDIATELY after a successful
/// `spawn()` — before the readiness wait below, which can itself panic on a
/// timeout or malformed ready line. A bare `std::process::Child` dropped by
/// an early panic here is NOT terminated (dropping a `Child` is a no-op on
/// the OS process); wrapping first means that panic still unwinds through
/// `Running`'s `Drop` and the daemon (and anything it had already spawned)
/// gets the same graceful-then-SIGKILL cleanup as every other exit path
/// (Codex review).
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
        .prefix("a24-t9")
        .tempdir_in("/tmp")
        .unwrap()
}

/// **T9 / ME-3f — the acceptance test.** A daemon starts with nothing
/// installed, a package is generated and installed OUTSIDE the repo while it
/// is down, and the daemon is started again — a real second process, the
/// same already-built binary, no rebuild — after which mount, routing proxy,
/// event forwarding, memory read/write and approval submission all round-trip
/// for real over the wire.
#[test]
fn a_package_from_outside_the_repo() {
    let home = tmp_home();

    // First lifetime: nothing installed yet. Proves the mount that follows is
    // caused by the install-then-restart sequence, not by packages the daemon
    // happened to pick up some other way.
    let d1 = start(home.path());
    let empty = get(d1.port, &d1.token, "/api/v1/os").expect("the daemon answered");
    assert_eq!(empty.0, 200, "{}", empty.1);
    let list: serde_json::Value = serde_json::from_str(&empty.1).unwrap();
    assert!(
        list["modules"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["name"] != "blackbox"),
        "blackbox must not exist before it has even been installed: {}",
        empty.1
    );
    stop(d1);

    // Generate and install the package OUTSIDE the repo while the daemon is
    // down — this is the "not rebuilt" boundary: nothing from here on touches
    // source or runs `cargo build`.
    install(home.path());

    // Second lifetime: a real restart, same binary, same $HOME.
    let d2 = start(home.path());

    // Subscribe to the real WS event consumer BEFORE triggering any request
    // that makes the module emit — the callback's own `{}` ack proves the
    // kernel accepted the call, not that anything downstream received it.
    // `spawn_ws_subscriber` blocks until the CLIENT side of the upgrade
    // completes, but that is not proof the SERVER has reached
    // `hub.subscribe()` yet (it runs inside the spawned `client_loop` task,
    // a moment strictly after the upgrade response is sent) — the broadcast
    // hub has no replay, so a request fired in that gap's event is lost for
    // good, not merely delayed. Rather than a fixed sleep (still racy, just
    // narrower), round trip 3 below retries the HTTP trigger — each retry is
    // a fresh real request with its own request_id/approval_token, not a
    // replay — until an event is actually observed, bounding the race to "at
    // most a handful of harmless extra requests" instead of "flaky".
    let events = spawn_ws_subscriber(d2.port, &d2.token);

    // ── 1. Mount + 2. Routing proxy ──────────────────────────────────────
    // `os list` can report `mounted` slightly before the module has finished
    // its handshake (spawned + registered vs. ready to actually serve a
    // proxied request are two different moments) — `daemon_modules.rs`'s own
    // `serving()` helper hits the same thing and retries the real HTTP call
    // rather than gating on the list, so this does the same.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (status, body) = loop {
        if let Some((status, body)) = get(d2.port, &d2.token, "/api/v1/blackbox/hi")
            && (status == 200 || Instant::now() >= deadline)
        {
            break (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "the module never answered through the real proxy; daemon stderr:\n{}",
            d2.recent_stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!(
        (status, body.as_str()),
        (200, "hello"),
        "daemon stderr:\n{}",
        d2.recent_stderr()
    );
    assert_eq!(
        os_list_entry(d2.port, &d2.token, "blackbox")["state"],
        "mounted"
    );

    // ── 3. Event forwarding, observed at the real consumer boundary ─────
    // The module's handler also submits a real `_a24/approval/gate`, which
    // itself broadcasts `module-approval.required` on the same bus — so each
    // attempt reads until it finds a `module`-typed frame rather than
    // assuming the first one it sees is it.
    //
    // The retry trigger runs on ITS OWN thread and is never awaited directly
    // — a synchronous `get()` in the middle of this loop would let its own
    // (up to 10s) socket read timeout blow straight through
    // `overall_deadline`, or return just after an event it caused already
    // landed, only for the deadline check right after it to discard that
    // event as "too late". Every wait below is bounded by
    // `overall_deadline` alone, computed fresh each iteration — never by a
    // fixed per-attempt window that could itself outlive it.
    let overall_deadline = Instant::now() + Duration::from_secs(30);
    let mut next_retry_at = Instant::now() + Duration::from_secs(3);
    let event = loop {
        let remaining = overall_deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "the WS subscriber connected before the request never received \
             the module's event after retrying the trigger; daemon stderr:\n{}",
            d2.recent_stderr()
        );
        if Instant::now() >= next_retry_at {
            // Fire-and-forget: this thread's own success/failure is not
            // asserted on — the FIRST trigger already proved the proxy round
            // trip (round trips 1/2 above); a retry here exists only to give
            // the module another chance to emit in case the previous one
            // raced the WS subscription. If the daemon is genuinely gone,
            // `recv_timeout` below will time out on `overall_deadline` and
            // fail with a clear message instead.
            let (port, token) = (d2.port, d2.token.clone());
            std::thread::spawn(move || {
                let _ = get(port, &token, "/api/v1/blackbox/hi");
            });
            next_retry_at = Instant::now() + Duration::from_secs(3);
        }
        // Recomputed AFTER the possible spawn above, not reused from the
        // `remaining` taken at the top of this iteration — spawning a
        // thread is normally sub-millisecond but is not free, and reusing a
        // stale value here is exactly the kind of small, needless deadline
        // overrun a later reviewer would have to re-derive this same fix to
        // close.
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
    assert_eq!(event["payload"]["module"], "blackbox", "{event}");
    assert_eq!(event["payload"]["kind"], "task.transitioned", "{event}");
    assert_eq!(
        event["payload"]["payload"],
        serde_json::json!({"probe": "t9"}),
        "{event}"
    );

    // ── 4. Memory read/write, correlated by ID and body ─────────────────
    let probe_path = home.path().join(".agent24/os/blackbox/callback_probe.json");
    let probe: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&probe_path).unwrap()).unwrap();
    assert!(
        probe["provides"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| "_a24/memory/private/".starts_with(p.as_str().unwrap())),
        "the handshake offer must cover memory: {probe}"
    );
    let remembered_id = probe["remember_response"]["result"]["id"].clone();
    assert!(
        remembered_id.as_str().is_some_and(|s| !s.is_empty()),
        "a real _a24/memory/private/remember call must return a non-empty string id: {probe}"
    );
    let recall_result = &probe["recall_response"]["result"];
    let items = recall_result["items"]
        .as_array()
        .unwrap_or_else(|| panic!("recall result must have an items array: {probe}"));
    let recalled = items
        .iter()
        .find(|i| i["id"] == remembered_id)
        .unwrap_or_else(|| {
            panic!("recall must contain the EXACT record just remembered (by id): {probe}")
        });
    assert_eq!(recalled["kind"], "t9-note", "{probe}");
    assert_eq!(recalled["body"]["text"], "t9-blackbox", "{probe}");

    // ── 5. Approval round trip — a bad token is rejected, the real one
    //      still works (proving the reject path did not burn it) ────────
    let gate_path = home.path().join(".agent24/os/blackbox/gate_probe.json");
    let gate: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&gate_path).unwrap()).unwrap();
    assert!(
        !gate["headers_seen"]["x-a24-request-id"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "the proxy must mint a real per-request id header: {gate}"
    );
    assert!(
        !gate["headers_seen"]["x-a24-approval-token"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "the proxy must mint a real per-request approval token header: {gate}"
    );
    assert_eq!(
        gate["bad_gate_response"]["error"]["data"]["kind"], "token_invalid",
        "a submission with the right request_id but a wrong token must be \
         rejected as token_invalid — proving this isn't an unconditional \
         'pending' regression: {gate}"
    );
    assert_eq!(
        gate["gate_response"]["result"]["decision"], "pending",
        "the SAME request_id, now with the real per-request token, must \
         still succeed — proving the rejected attempt did not consume it: {gate}"
    );

    stop(d2);
}

/// Connect to the real `GET /api/v1/events` WS endpoint and hand back a
/// channel of decoded [`agent24_protocol::Event`] JSON values, one per
/// frame. Blocks until the CLIENT side of the WS upgrade completes — this is
/// NOT proof the server has reached `hub.subscribe()` yet (that runs inside
/// the task the upgrade handler spawns, a moment strictly after the upgrade
/// response is sent), and the hub has no replay, so an event fired in that
/// gap is lost for good, not merely delayed. A caller cannot treat "this
/// function returned" as "no event before my next line can be missed" — see
/// the retry loop at the one call site for how that residual race is
/// actually closed.
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

/// Negative control: a package declared `impl_kind: in_process_crate` (the
/// SPEC-mandated opposite of this file's subject) is still refused when it
/// is not one of the crates compiled into the kernel — proving this file's
/// green result comes from `out_of_process_provider` support landing, not
/// from the mount path having quietly stopped checking `impl_kind` at all.
#[test]
fn an_in_process_declaration_for_an_uncompiled_crate_is_still_refused() {
    let home = tmp_home();
    let dir = home.path().join(".agent24/packages/not-really-in-process");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: not-really-in-process\nversion: \"0.1.0\"\n\
         route_namespace: /api/v1/not-really-in-process\n\
         event_module: not-really-in-process\n\
         data_dir: ~/.agent24/os/not-really-in-process/\n\
         kernel_capabilities: []\nimpl_kind: in_process_crate\n",
    )
    .unwrap();
    let d = start(home.path());
    let entry = os_list_entry(d.port, &d.token, "not-really-in-process");
    assert_eq!(
        entry["state"], "refused",
        "a crate this binary never compiled in must be reported REFUSED, not \
         silently absent or mounted: {entry}"
    );
    // `Refused` also covers several unrelated causes (duplicate/reserved
    // name, manifest identity mismatch, ...) — pin the SPECIFIC detail this
    // test means to exercise, not just the generic `refused` state, so a
    // regression that started refusing this manifest for some other reason
    // could not silently keep this test green.
    assert_eq!(
        entry["detail"],
        "a package on disk must declare an out-of-process provider with a \
         spawn command; in-process modules are compiled in",
        "{entry}"
    );
    let (status, _) =
        get(d.port, &d.token, "/api/v1/not-really-in-process/hi").expect("the daemon answered");
    assert_eq!(
        status, 404,
        "a refused module must have no route at all — not 200, and not some \
         other non-200 status that could equally mean 'mounted but broken'"
    );
    stop(d);
}
