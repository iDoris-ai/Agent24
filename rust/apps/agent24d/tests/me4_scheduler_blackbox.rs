//! ME4-1.5.1 — the black-box acceptance test for the scheduler callback
//! (`docs/design/ME4-S1-scheduler-callback.md`, judgement C6; PLAN
//! `docs/agent/PLAN-ME4-OS-CAPABILITIES.md` §三 ME4-1.5.1). Harness shape
//! copied from `me3f_blackbox.rs` (`Daemon`/`Running`/`start`/`stop`/`get`),
//! with `start()` additionally setting `A24_SCHEDULER_TICK_SECS=1` so a real
//! tick loop notices a due module schedule inside a few seconds instead of
//! the production default (10s).
//!
//! **Six scenarios, one real out-of-process Python module, one real daemon
//! restart, real wall-clock time — no fixed sleeps waiting for something to
//! happen, only bounded polls:**
//!
//! 1. The module upserts `routine.x` (`At` = its own start time + 15s) TWICE
//!    right after its first handshake → `GET /api/v1/schedules` shows
//!    exactly one row for this module (idempotent upsert, design §6.2).
//! 2. A REAL tick (`A24_SCHEDULER_TICK_SECS=1`) reaches `At` — the module
//!    receives a real `POST /api/v1/blackbox/_a24/scheduler/fired` and
//!    records `x-a24-fire-id`/`trigger`/`scheduled_for` into its probe file.
//! 3. The daemon is restarted (same `$HOME`, same already-built binary, no
//!    rebuild) while `At` is still in the future; the module re-upserts the
//!    SAME key on its second boot → still exactly one row, and the design
//!    §11 C6 point 2 timing (module sleeps to `At + 2s` before handshaking,
//!    so `At` falls inside the second boot's `Starting` window) proves the
//!    fire was RECORDED (by tick) strictly before the module's handshake
//!    completed, yet was only DELIVERED after — a delayed delivery, not a
//!    coincidence — and `consecutive_failures` stays 0 throughout (being
//!    deferred is not a failure, design §4.1/§9).
//! 4. `POST /schedules/{id}/run_now` — positive control: a SEPARATE
//!    `fire_id` from the tick's, delivered independently.
//! 5. A client that talks directly to the daemon's HTTP port (not through
//!    the module) cannot forge a fired call: reserved-path variants (design
//!    §7.2/C3.1) get `404`, unable-to-canonicalise variants (§7.2/C3.2) get
//!    `400` — and the module's probe file gains zero new entries either way
//!    (ME4-1.3.2's `judge()` runs before the module ever sees a byte).
//! 6. While the module's fired handler is deliberately holding an
//!    in-flight fired request open, the module is hot-disabled
//!    (`POST /api/v1/os/blackbox/stop`, `Supervisors::disable`) — the SAME
//!    generation moves to `Draining`. The handler then makes a REAL
//!    `_a24/memory/private/remember` callback bound to that fired request's
//!    own `x-a24-request-id`: it succeeds (design §5.2's "投递进行中 ...
//!    Draining 期间仍能通过 `admit_callback_bound`"), while the identical
//!    call with a made-up id is refused `draining` — the real-`Generation`,
//!    real-Draining counterpart to `memory_callback.rs`'s own unit test of
//!    the same rule, which only ever synthesizes a `Generation` by hand.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use agent24_scheduler::next_fire::{fmt_iso, parse_iso};
use agent24_scheduler::{FireId, FireTrigger};

/// The out-of-process module. A single long-lived process across BOTH daemon
/// lifetimes' worth of module-process restarts is not attempted here — each
/// daemon start spawns a fresh module process, and the module tells first
/// boot from second boot by whether its own `at` marker file (in its OWN
/// data dir, which persists across daemon restarts because `$HOME` is the
/// same tempdir both times) already exists.
///
/// Scenario 6's "hold the fired request open" is driven by a plain flag file
/// (`block_next_fired`) the Rust side touches right before the `run_now` it
/// wants blocked, and released by another flag file (`go_after_drain`) the
/// Rust side touches once it has confirmed the hot-disable took effect —
/// both are the same file-based signalling `me3f_blackbox.rs`'s
/// `dump_atomic` already establishes as this repo's pattern for a Python
/// probe talking to its Rust driver without a second network channel.
const MODULE_SCRIPT: &str = r#"import calendar, hashlib, json, os, socket, sys, time
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
data_dir = os.environ["A24_DATA_DIR"]

def dump_atomic(name, value):
    # Same rationale as me3f_blackbox.rs's dump_atomic: a concurrent reader
    # (the Rust test polling this exact file) must never see a truncated or
    # partially-written JSON document.
    path = os.path.join(data_dir, name)
    tmp = path + ".tmp"
    with open(tmp, "w") as out:
        json.dump(value, out)
    os.replace(tmp, path)

def append_atomic(name, value):
    path = os.path.join(data_dir, name)
    try:
        with open(path) as fh:
            items = json.load(fh)
    except (FileNotFoundError, json.JSONDecodeError):
        items = []
    items.append(value)
    dump_atomic(name, items)

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
    at_path = os.path.join(data_dir, "at")
    is_restart = os.path.exists(at_path)
    if is_restart:
        with open(at_path) as fh:
            at = fh.read().strip()
        at_epoch = calendar.timegm(time.strptime(at, "%Y-%m-%dT%H:%M:%SZ"))
        # design v3.1 M: sleep to `At + 2s` so `At` falls squarely inside
        # THIS boot's Starting window; the budget is < STARTUP_TIMEOUT (10s)
        # — exceeding it means the Rust side scheduled the restart too far
        # from `At` and this run must fail loudly, not silently race.
        sleep_for = (at_epoch + 2) - time.time()
        if sleep_for > 8:
            with open(os.path.join(data_dir, "error.txt"), "w") as out:
                out.write(f"computed sleep_for={sleep_for} exceeds the 8s budget")
            sys.exit(1)
        if sleep_for > 0:
            time.sleep(sleep_for)
    else:
        at = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(time.time() + 15))
        tmp = at_path + ".tmp"
        with open(tmp, "w") as out:
            out.write(at)
        os.replace(tmp, at_path)

    init_resp = rpc("initialize", {
        "protocol_versions": {"min": 1, "max": 1000}, "module": "blackbox",
        "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
        "capabilities": ["scheduler", "memory"]})
    provides = init_resp.get("result", {}).get("offer", {}).get("provides", [])

    if is_restart:
        # Recorded BEFORE the re-upsert: the moment handshake finished, for
        # the Rust side to compare against the tick's own `fired_at` (the
        # instant the fire was RECORDED, which design §4.2 requires to have
        # already happened while this boot was still Starting).
        dump_atomic("handshake_2_time.json", {
            "handshake_time": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        })
        upsert = rpc("_a24/scheduler/upsert", {"key": "routine.x", "spec": {"type": "at", "ts": at}})
        dump_atomic("startup_2.json", {"provides": provides, "upsert": upsert, "at": at})
    else:
        upsert1 = rpc("_a24/scheduler/upsert", {"key": "routine.x", "spec": {"type": "at", "ts": at}})
        upsert2 = rpc("_a24/scheduler/upsert", {"key": "routine.x", "spec": {"type": "at", "ts": at}})
        dump_atomic("startup_1.json", {
            "provides": provides, "upsert1": upsert1, "upsert2": upsert2, "at": at,
        })

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
                break
            body_bytes += chunk
        try:
            body = json.loads(body_bytes.decode() or "{}")
        except Exception:
            body = {}
        rid = headers.get("x-a24-request-id", "")

        block_flag = os.path.join(data_dir, "block_next_fired")
        blocking = os.path.exists(block_flag)
        if blocking:
            os.remove(block_flag)
            dump_atomic("handler_blocked.json", {
                "request_id": rid, "fire_id": headers.get("x-a24-fire-id", ""),
            })
            go_path = os.path.join(data_dir, "go_after_drain")
            deadline = time.time() + 20
            while not os.path.exists(go_path) and time.time() < deadline:
                time.sleep(0.05)
            good = rpc("_a24/memory/private/remember", {
                "kind": "note", "body": {"text": "c46-bound"}, "request_id": rid,
            })
            bad = rpc("_a24/memory/private/remember", {
                "kind": "note", "body": {"text": "c46-ghost"},
                "request_id": "ghost-not-in-flight",
            })
            dump_atomic("handler_result.json", {
                "good": good, "bad": bad, "saw_go": os.path.exists(go_path),
            })

        append_atomic("fires.json", {
            "key": body.get("key"),
            "trigger": body.get("trigger"),
            "scheduled_for": body.get("scheduled_for"),
            "fired_at": body.get("fired_at"),
            "fire_id_header": headers.get("x-a24-fire-id", ""),
            "schedule_key_header": headers.get("x-a24-schedule-key", ""),
        })

        out = b"ok"
        conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(out), out))
        conn.close()
except Exception as e:
    with open(os.path.join(data_dir, "error.txt"), "w") as out:
        out.write(repr(e))
    print(f"scheduler blackbox module failed: {e!r}", file=sys.stderr)
    raise

while f.readline():
    pass
"#;

/// Write the package under `<home>/.agent24/packages/blackbox/` — the same
/// on-disk shape `agent24 os install` produces (`me3f_blackbox.rs::install`
/// does the identical thing for its own module).
fn install(home: &Path) {
    let dir = home.join(".agent24/packages/blackbox");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: blackbox\nversion: \"0.1.0\"\nroute_namespace: /api/v1/blackbox\n\
         event_module: blackbox\ndata_dir: ~/.agent24/os/blackbox/\n\
         kernel_capabilities: [scheduler, memory]\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod.py"), MODULE_SCRIPT).unwrap();
}

/// One raw HTTP round trip over a plain `TcpStream` — deliberately NOT a
/// `hyper`/`http::Uri`-based client: the whole point of scenario 5 is to
/// send exact, already-percent-encoded byte sequences (`%2e%2e`, `..;`, a
/// literal backslash, ...) verbatim as the request-target, with no URI
/// parser normalising them out from under the test before they reach the
/// daemon. `path` is spliced directly into the request line.
///
/// Retries a handful of times on a bare I/O error (`ConnectionReset` in
/// particular — observed empirically against this same daemon/hyper stack:
/// occasionally a fresh connection is accepted and then reset before the
/// response is fully read, even for a request the server ends up answering
/// on the very next attempt). `me3f_blackbox.rs`'s own `get()` tolerates the
/// identical class of transient failure by returning `Option` and looping at
/// the call site; this does the equivalent retrying INSIDE the helper so
/// every one-shot call site here (there are many: `get`/`post` plus
/// scenario 5's forged requests) gets it for free rather than each needing
/// its own bounded loop. A real HTTP response (any status code) is never
/// retried — only a failure to complete the raw byte exchange is.
fn raw_request(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> (u16, HashMap<String, String>, String) {
    let mut last_err = None;
    for attempt in 0..5 {
        match try_raw_request(port, token, method, path, extra_headers, body) {
            Ok(v) => return v,
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(Duration::from_millis(100 * (attempt + 1)));
            }
        }
    }
    panic!(
        "raw HTTP request {method} {path} kept failing after 5 attempts: {:?}",
        last_err.unwrap()
    );
}

fn try_raw_request(
    port: u16,
    token: &str,
    method: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
    body: &[u8],
) -> std::io::Result<(u16, HashMap<String, String>, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
         connection: close\r\ncontent-length: {}\r\n",
        body.len()
    );
    for (k, v) in extra_headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())?;
    s.write_all(body)?;
    let mut raw = Vec::new();
    s.read_to_end(&mut raw)?;
    let raw = String::from_utf8_lossy(&raw).into_owned();
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw.as_str(), ""));
    let mut lines = head.split("\r\n");
    let status: u16 = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let mut headers = HashMap::new();
    for line in lines {
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
    }
    Ok((status, headers, body.to_owned()))
}

fn get(port: u16, token: &str, path: &str) -> (u16, String) {
    let (status, _, body) = raw_request(port, token, "GET", path, &[], b"");
    (status, body)
}

fn post(port: u16, token: &str, path: &str, body: &str) -> (u16, String) {
    let (status, _, body) = raw_request(port, token, "POST", path, &[], body.as_bytes());
    (status, body)
}

/// Graceful-first shutdown, matching `me3f_blackbox.rs::Running` verbatim
/// (see that file's own doc comment for why: SIGTERM + a bounded wait so the
/// daemon's own supervisor reaps the module's process group, SIGKILL only as
/// the fallback for a daemon that does not exit in time).
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

/// Start the already-built `agent24d` binary against `home`. Same shape as
/// `me3f_blackbox.rs::start`, plus `A24_SCHEDULER_TICK_SECS=1` (task text):
/// the production default is 10s, which would make scenario 2/4/6's real
/// tick/pump activity needlessly slow for a test whose module schedules
/// already have a 60s-independent one-shot `At` spec.
fn start(home: &Path) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("A24_SCHEDULER_TICK_SECS", "1")
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
        .prefix("a24-me4-1-5-1")
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

/// Bounded poll for `mounted` — same reasoning `me3f_blackbox.rs` documents
/// on its own retry loop: `os list` can report `mounted` slightly before the
/// module can actually serve a proxied request, but here we only need the
/// state, not a live HTTP round trip through it (the module never answers
/// plain GETs — only fired POSTs).
fn wait_mounted(port: u16, token: &str, name: &str, stderr: impl Fn() -> String) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let state = os_state(port, token, name);
        if state["state"] == "mounted" {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "{name} never reached state \"mounted\": {state}; daemon stderr:\n{}",
            stderr()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn data_dir(home: &Path) -> std::path::PathBuf {
    home.join(".agent24/os/blackbox")
}

fn read_probe(home: &Path, name: &str) -> Option<serde_json::Value> {
    std::fs::read(data_dir(home).join(name))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
}

fn wait_for_probe(
    home: &Path,
    name: &str,
    timeout: Duration,
    stderr: impl Fn() -> String,
) -> serde_json::Value {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(v) = read_probe(home, name) {
            return v;
        }
        if let Ok(err) = std::fs::read_to_string(data_dir(home).join("error.txt")) {
            panic!("the module recorded an error: {err}");
        }
        assert!(
            Instant::now() < deadline,
            "probe {name} never appeared within {timeout:?}; daemon stderr:\n{}",
            stderr()
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Fires recorded so far with a given `trigger`, newest last — `fires.json`
/// is an append-only array the module writes one entry per fired POST it
/// answers (see `MODULE_SCRIPT`).
fn fires_with_trigger(home: &Path, trigger: &str) -> Vec<serde_json::Value> {
    read_probe(home, "fires.json")
        .and_then(|v| v.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|f| f["trigger"] == trigger)
        .collect()
}

fn fires_len(home: &Path) -> usize {
    read_probe(home, "fires.json")
        .and_then(|v| v.as_array().map(Vec::len))
        .unwrap_or(0)
}

fn schedule_row(port: u16, token: &str) -> serde_json::Value {
    let (status, body) = get(port, token, "/api/v1/schedules");
    assert_eq!(status, 200, "{body}");
    let list: serde_json::Value = serde_json::from_str(&body).unwrap();
    let rows: Vec<&serde_json::Value> = list["schedules"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["owner"]["module"] == "blackbox")
        .collect();
    assert_eq!(
        rows.len(),
        1,
        "the module must own exactly one schedule row: {body}"
    );
    rows[0].clone()
}

/// **ME4-1.5.1 — the black-box acceptance test.** See this file's module doc
/// for the six scenarios. `#[test]`, not `#[tokio::test]`: every wait below
/// is a real bounded poll over real wall-clock time, driving a real second
/// process — there is no async runtime to hand control to here.
#[test]
fn scheduler_callback_blackbox_round_trip() {
    let home = tmp_home();
    install(home.path());

    // ── boot 1 ────────────────────────────────────────────────────────────
    let d1 = start(home.path());
    wait_mounted(d1.port, &d1.token, "blackbox", || d1.recent_stderr());

    let startup_1 = wait_for_probe(
        home.path(),
        "startup_1.json",
        Duration::from_secs(15),
        || d1.recent_stderr(),
    );
    assert_eq!(
        startup_1["upsert1"]["result"]["outcome"], "created",
        "{startup_1}"
    );
    assert_eq!(
        startup_1["upsert2"]["result"]["outcome"], "unchanged",
        "{startup_1}"
    );
    let provides = startup_1["provides"].as_array().unwrap();
    assert!(
        provides
            .iter()
            .any(|p| "_a24/scheduler/upsert".starts_with(p.as_str().unwrap())),
        "the granted handshake must offer scheduler: {startup_1}"
    );
    let at = startup_1["at"].as_str().unwrap().to_owned();
    let at_dt = parse_iso(&at).unwrap();

    // ── scenario 1: idempotent upsert -> exactly one row ────────────────
    let row = schedule_row(d1.port, &d1.token);
    let schedule_id = row["id"].as_str().unwrap().to_owned();
    assert_eq!(row["next_run_at"], at, "{row}");
    assert_eq!(row["consecutive_failures"], 0, "{row}");

    // Design v3.1 M's front guard, run for real: reading `at` and getting
    // here must have taken nowhere near 10 of the 15 seconds `At` is out —
    // if it did, something is badly wrong with this environment and the
    // test must fail loudly instead of racing the restart below.
    let now = chrono::Utc::now();
    assert!(
        now < at_dt - chrono::Duration::seconds(5),
        "the test is already within 5s of `At` ({at}) right after boot 1 \
         (now={now}) — the environment is too slow for this test's timing \
         budget; failing loudly instead of racing the restart"
    );
    assert!(
        fires_with_trigger(home.path(), "tick").is_empty(),
        "no tick fire can exist before the daemon has even ticked once \
         against a due `At`"
    );

    // ── restart the daemon before `At`, timed so the SECOND boot's own
    //    `sleep to At+2` computation lands well inside its 8s budget (design
    //    §11 C6 point 2): restart 4 seconds before `At`, so boot 2 sleeps
    //    ~6s before handshaking — comfortably under the 8s cap and under the
    //    10s STARTUP_TIMEOUT, while still giving the post-`mount_all` tick
    //    loop (1s cadence) several seconds to notice `At` is due and record
    //    a fire while boot 2 is still Starting. ──────────────────────────
    let restart_at = at_dt - chrono::Duration::seconds(4);
    let sleep_ms = (restart_at - chrono::Utc::now()).num_milliseconds().max(0);
    std::thread::sleep(Duration::from_millis(u64::try_from(sleep_ms).unwrap_or(0)));
    stop(d1);

    // ── boot 2 (restart) ─────────────────────────────────────────────────
    let d2 = start(home.path());
    wait_mounted(d2.port, &d2.token, "blackbox", || d2.recent_stderr());

    // ── scenario 3: re-upsert on restart -> still exactly one row, and the
    //    handshake time is on record so scenario 2's delay proof can use it.
    let startup_2 = wait_for_probe(
        home.path(),
        "startup_2.json",
        Duration::from_secs(15),
        || d2.recent_stderr(),
    );
    assert_eq!(
        startup_2["upsert"]["result"]["outcome"], "unchanged",
        "re-upserting the identical spec/enabled/label on restart must be a \
         no-op, never a new row: {startup_2}"
    );
    let row_after_restart = schedule_row(d2.port, &d2.token);
    assert_eq!(row_after_restart["id"], schedule_id, "{row_after_restart}");
    let handshake_2 = wait_for_probe(
        home.path(),
        "handshake_2_time.json",
        Duration::from_secs(5),
        || d2.recent_stderr(),
    );
    let handshake_2_time = parse_iso(handshake_2["handshake_time"].as_str().unwrap()).unwrap();

    // ── scenario 2: a REAL tick reaches `At` -> a REAL fired POST, recorded
    //    into the probe file with the exact deterministic fire_id design
    //    §4.2 specifies. Waited for AFTER boot 2 comes up, up to the 30s cap
    //    design §11 C6 point 2 states.
    let tick_fires = {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let fires = fires_with_trigger(home.path(), "tick");
            if !fires.is_empty() {
                break fires;
            }
            if let Ok(err) = std::fs::read_to_string(data_dir(home.path()).join("error.txt")) {
                panic!("the module recorded an error: {err}");
            }
            assert!(
                Instant::now() < deadline,
                "no tick-triggered fire arrived within 30s of restart; daemon stderr:\n{}",
                d2.recent_stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert_eq!(tick_fires.len(), 1, "{tick_fires:?}");
    let tick_fire = &tick_fires[0];
    let expected_tick_fire_id = FireId::derive(FireTrigger::Tick, &schedule_id, at_dt);
    assert_eq!(
        tick_fire["fire_id_header"],
        expected_tick_fire_id.as_str(),
        "{tick_fire}"
    );
    assert_eq!(tick_fire["scheduled_for"], at, "{tick_fire}");
    assert_eq!(tick_fire["schedule_key_header"], "routine.x", "{tick_fire}");
    // The delay proof (design §11 C6 point 2): the fire's `fired_at` — set
    // when TICK RECORDED it — must be strictly earlier than the moment the
    // module finished handshaking. Recorded-before-Running is exactly
    // "delayed after Starting", not "coincidentally fired once Running".
    let fired_at = parse_iso(tick_fire["fired_at"].as_str().unwrap()).unwrap();
    assert!(
        fired_at < handshake_2_time,
        "the fire must be recorded (fired_at={fired_at}) strictly before the \
         module's handshake completed (handshake_time={handshake_2_time}) — \
         otherwise this is not proof of a delayed, Starting-period delivery"
    );
    // Not a failure: being deferred during Starting must never count against
    // the schedule (design §4.1/§9).
    let row_after_delivery = schedule_row(d2.port, &d2.token);
    assert_eq!(
        row_after_delivery["consecutive_failures"], 0,
        "{row_after_delivery}"
    );

    // ── scenario 4: run_now -> a DIFFERENT fire_id, delivered independently
    //    (positive control: proves run_now is a real, distinct trigger, not
    //    an alias for the tick's own fire). ──────────────────────────────
    let (status, body) = post(
        d2.port,
        &d2.token,
        &format!("/api/v1/schedules/{schedule_id}/run_now"),
        "",
    );
    assert_eq!(status, 202, "{body}");
    let run_now_resp: serde_json::Value = serde_json::from_str(&body).unwrap();
    let run_now_fire_id = run_now_resp["fire_id"].as_str().unwrap().to_owned();
    assert_ne!(
        run_now_fire_id,
        expected_tick_fire_id.as_str(),
        "run_now must mint a fire_id distinct from the tick's own"
    );
    let run_now_fires = {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let fires: Vec<serde_json::Value> = fires_with_trigger(home.path(), "run_now")
                .into_iter()
                .filter(|f| f["fire_id_header"] == run_now_fire_id)
                .collect();
            if !fires.is_empty() {
                break fires;
            }
            assert!(
                Instant::now() < deadline,
                "the run_now fire never arrived; daemon stderr:\n{}",
                d2.recent_stderr()
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    };
    assert_eq!(run_now_fires.len(), 1, "{run_now_fires:?}");

    // ── scenario 5: a client that talks straight to the HTTP port cannot
    //    forge a fired call. At least 5 reserved-path encodings -> 404, and
    //    at least 3 non-canonicalisable variants (including the two design
    //    §11 C6 point 3 names explicitly) -> 400 — zero new probe entries
    //    either way (design §7.1: judged and refused before admission, the
    //    module sees zero bytes). ─────────────────────────────────────────
    const RESERVED: &[&str] = &[
        "/_a24/scheduler/fired",
        "/_A24/scheduler/fired",
        "//_a24/scheduler/fired",
        "/%5fa24/scheduler/fired",
        "/%255fa24/scheduler/fired",
        "/_a24;x=1/scheduler/fired",
    ];
    const REJECTED: &[&str] = &[
        "/x/..;/_a24/scheduler/fired",
        "/_a24/scheduler/fired/../../..",
        "/x%2F..%2F_a24/scheduler/fired",
    ];
    let fires_before_forgery = fires_len(home.path());
    let forged_body = serde_json::json!({
        "key": "routine.x", "trigger": "tick", "scheduled_for": at, "fired_at": at,
    })
    .to_string();
    for suffix in RESERVED {
        let path = format!("/api/v1/blackbox{suffix}");
        let (status, _headers, body) = raw_request(
            d2.port,
            &d2.token,
            "POST",
            &path,
            &[
                ("x-a24-fire-id", expected_tick_fire_id.as_str()),
                ("x-a24-schedule-key", "routine.x"),
                ("content-type", "application/json"),
            ],
            forged_body.as_bytes(),
        );
        assert_eq!(status, 404, "{path}: {body}");
    }
    for suffix in REJECTED {
        let path = format!("/api/v1/blackbox{suffix}");
        let (status, _, body) = raw_request(
            d2.port,
            &d2.token,
            "POST",
            &path,
            &[
                ("x-a24-fire-id", expected_tick_fire_id.as_str()),
                ("x-a24-schedule-key", "routine.x"),
                ("content-type", "application/json"),
            ],
            forged_body.as_bytes(),
        );
        assert_eq!(status, 400, "{path}: {body}");
    }
    assert_eq!(
        fires_len(home.path()),
        fires_before_forgery,
        "no forged/reserved-path request may reach the module's fired \
         handler — the probe file must gain zero new entries"
    );

    // ── scenario 6: hot-disable while a fired handler is deliberately
    //    holding the request open -> the handler's bound memory callback
    //    (using the fired request's own x-a24-request-id) still succeeds
    //    during Draining; a made-up id does not. ────────────────────────
    // `fire_id` is only domain-separated by (trigger, schedule_id,
    // scheduled_for) at SECOND precision (design §4.2) — a run_now fired in
    // the SAME wall-clock second as scenario 4's would mint the identical
    // id (and idempotently return it, not a new fire). Cross into a fresh
    // second before triggering this one so it is unambiguously its own fire.
    let scenario4_scheduled_for = run_now_fires[0]["scheduled_for"]
        .as_str()
        .unwrap()
        .to_owned();
    while fmt_iso(chrono::Utc::now()) == scenario4_scheduled_for {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::fs::write(data_dir(home.path()).join("block_next_fired"), b"").unwrap();
    let (status, body) = post(
        d2.port,
        &d2.token,
        &format!("/api/v1/schedules/{schedule_id}/run_now"),
        "",
    );
    assert_eq!(status, 202, "{body}");
    let blocked_fire_id = serde_json::from_str::<serde_json::Value>(&body).unwrap()["fire_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(
        blocked_fire_id, run_now_fire_id,
        "the blocked run_now must be its own fire, not a repeat of scenario 4's"
    );

    let blocked = wait_for_probe(
        home.path(),
        "handler_blocked.json",
        Duration::from_secs(10),
        || d2.recent_stderr(),
    );
    let bound_request_id = blocked["request_id"].as_str().unwrap().to_owned();
    assert!(
        !bound_request_id.is_empty(),
        "the fired request must have carried a real x-a24-request-id: {blocked}"
    );

    // Hot-disable: `stop_now_os` waits (up to 2s) for admission to close
    // before answering, so by the time this returns, the generation is
    // already Draining — while the ALREADY-admitted fired request the
    // module is mid-handling stays admitted (design §5.2).
    let (status, body) = post(d2.port, &d2.token, "/api/v1/os/blackbox/stop", "");
    assert_eq!(status, 200, "{body}");
    let stop_resp: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(stop_resp["stopped"], true, "{stop_resp}");

    // Release the module to make its bound (and its ghost) memory calls now
    // that the generation is confirmed Draining.
    std::fs::write(data_dir(home.path()).join("go_after_drain"), b"").unwrap();

    let result = wait_for_probe(
        home.path(),
        "handler_result.json",
        Duration::from_secs(10),
        || d2.recent_stderr(),
    );
    assert_eq!(result["saw_go"], true, "{result}");
    assert!(
        result["good"].get("error").is_none(),
        "a memory callback bound to the fired request's own, still-in-flight \
         x-a24-request-id must succeed even while the generation is \
         Draining: {result}"
    );
    assert!(
        !result["good"]["result"]["id"]
            .as_str()
            .unwrap_or_default()
            .is_empty(),
        "{result}"
    );
    assert_eq!(
        result["bad"]["error"]["data"]["kind"], "draining",
        "a callback carrying an id that was never in flight must be refused \
         `draining`, not silently admitted or downgraded to unbound: {result}"
    );

    stop(d2);
}
