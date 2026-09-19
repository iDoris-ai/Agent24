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

/// The out-of-process module: declares `events`/`memory`/`approval`, does
/// three real callback round trips right after handshaking (emit, remember,
/// recall — none of which need a live HTTP request), then serves exactly one
/// HTTP request through the real proxy, reads the real per-request headers
/// off it, and makes the fourth callback round trip (`approval/gate`) using
/// those — the one call that genuinely cannot happen before a request exists.
const BLACKBOX_MODULE: &str = r#"import hashlib, json, os, socket
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
data_dir = os.environ["A24_DATA_DIR"]

cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
f = cb.makefile("rb")
next_id = [0]
def rpc(method, params):
    next_id[0] += 1
    req = {"jsonrpc": "2.0", "id": str(next_id[0]), "method": method, "params": params}
    cb.sendall((json.dumps(req) + "\n").encode())
    return json.loads(f.readline())

init_resp = rpc("initialize", {
    "protocol_versions": {"min": 1, "max": 1000}, "module": "blackbox",
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": ["events", "memory", "approval"]})
provides = init_resp.get("result", {}).get("offer", {}).get("provides", [])

emit_resp = rpc("_a24/events/emit", {"kind": "task.transitioned", "payload": {"probe": "t9"}})
remember_resp = rpc("_a24/memory/private/remember", {"kind": "t9-note", "body": {"text": "t9-blackbox"}})
recall_resp = rpc("_a24/memory/private/recall", {"query": "t9-note", "page_size": 10})

with open(os.path.join(data_dir, "callback_probe.json"), "w") as out:
    json.dump({
        "provides": provides,
        "emit_response": emit_resp,
        "remember_response": remember_resp,
        "recall_response": recall_resp,
    }, out)

listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
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

gate_resp = rpc("_a24/approval/gate", {
    "action": "schedule_callback",
    "target": "2099-01-01T00:00:00Z",
    "payload": {},
    "request_id": headers.get("x-a24-request-id", ""),
    "approval_token": headers.get("x-a24-approval-token", ""),
})
with open(os.path.join(data_dir, "gate_probe.json"), "w") as out:
    json.dump({"headers_seen": headers, "gate_response": gate_resp}, out)

body = b"hello"
conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body))
conn.close()

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

/// The daemon, killed on drop so a failing test leaves no process behind.
struct Running(std::process::Child);

impl Drop for Running {
    fn drop(&mut self) {
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
}

/// Start the already-built `agent24d` binary against `home` — no `cargo
/// build` here or anywhere else in this file. Matches `daemon_modules.rs`'s
/// `start`.
fn start(home: &Path) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
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
    Daemon {
        run: Running(child),
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
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

    // ── 1. Mount + 2. Routing proxy ──────────────────────────────────────
    // `os list` can report `mounted` slightly before the module has finished
    // its handshake (spawned + registered vs. ready to actually serve a
    // proxied request are two different moments) — `daemon_modules.rs`'s own
    // `serving()` helper hits the same thing and retries the real HTTP call
    // rather than gating on the list, so this does the same. The module's
    // handler also does the approval round trip (round trip 5) before it
    // answers, so this call exercises proxy + approval together — approval
    // submission needs a request actually in flight, so it cannot be tested
    // any earlier than this.
    let deadline = Instant::now() + Duration::from_secs(30);
    let (status, body) = loop {
        if let Some((status, body)) = get(d2.port, &d2.token, "/api/v1/blackbox/hi")
            && (status == 200 || Instant::now() >= deadline)
        {
            break (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "the module never answered through the real proxy"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert_eq!((status, body.as_str()), (200, "hello"));
    assert_eq!(
        os_list_entry(d2.port, &d2.token, "blackbox")["state"],
        "mounted"
    );

    // ── 3/4. Event forwarding + memory read/write ───────────────────────
    let probe_path = home.path().join(".agent24/os/blackbox/callback_probe.json");
    let probe: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&probe_path).unwrap()).unwrap();
    assert!(
        probe["provides"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| "_a24/events/emit".starts_with(p.as_str().unwrap())),
        "the handshake offer must cover events: {probe}"
    );
    assert_eq!(
        probe["emit_response"]["result"],
        serde_json::json!({}),
        "a real _a24/events/emit call must succeed: {probe}"
    );
    assert!(
        probe["remember_response"].get("result").is_some(),
        "a real _a24/memory/private/remember call must succeed: {probe}"
    );
    let recall_result = &probe["recall_response"]["result"];
    assert!(
        recall_result.is_object(),
        "a real _a24/memory/private/recall call must succeed: {probe}"
    );
    let items = recall_result["items"]
        .as_array()
        .unwrap_or_else(|| panic!("recall result must have an items array: {probe}"));
    assert!(
        items.iter().any(|i| i["kind"] == "t9-note"),
        "recall must read back the note this same test just remembered: {probe}"
    );

    // ── 5. Approval round trip ───────────────────────────────────────────
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
        gate["gate_response"]["result"]["decision"], "pending",
        "a real _a24/approval/gate submission using the real per-request headers must succeed: {gate}"
    );

    stop(d2);
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
         event_module: not-really-in-process\ndata_dir: ~/.agent24/os/nripp/\n\
         kernel_capabilities: []\nimpl_kind: in_process_crate\n",
    )
    .unwrap();
    let d = start(home.path());
    // Whether a manifest this malformed even reaches a reportable `os list`
    // entry, or is dropped at discovery, is not this test's business — what
    // must be true regardless is that no client can ever reach it: proven at
    // the one boundary a black-box test is entitled to look at, the HTTP
    // route.
    let (status, _) = get(d.port, &d.token, "/api/v1/not-really-in-process/hi")
        .expect("the daemon answered (refused or 404, but answered)");
    assert_ne!(
        status, 200,
        "a crate this binary never compiled in must not mount just because \
         its manifest asked to"
    );
    stop(d);
}
