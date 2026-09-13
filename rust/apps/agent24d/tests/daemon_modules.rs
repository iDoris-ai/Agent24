//! The daemon starts out-of-process packages and stops them when it stops
//! (ME3-SUP slice 4) — driven through the real binary, because the parts that
//! matter live in `serve`: signal handling, the shutdown order, the bounded
//! exit. A unit test of `mount_all` exercises none of them.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A package module: HTTP on the listener the kernel hands it — `/slow`
/// marks that it has started (`slow-entered`) and answers only once the test
/// creates `release`; anything else at once — its pid in its data directory,
/// the handshake with its own manifest's digest, and an exit when the callback
/// connection ends (D1).
const MODULE: &str = r#"import hashlib, json, os, socket, threading, time
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
with open(os.path.join(os.environ["A24_DATA_DIR"], "pid"), "w") as f:
    f.write(str(os.getpid()))
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
def answer(conn):
    head = b""
    while b"\r\n\r\n" not in head:
        chunk = conn.recv(4096)
        if not chunk:
            return
        head += chunk
    path = head.split(b" ")[1]
    if path.endswith(b"/slow"):
        data = os.environ["A24_DATA_DIR"]
        open(os.path.join(data, "slow-entered"), "w").close()
        release = os.path.join(data, "release")
        deadline = time.time() + 5
        while not os.path.exists(release) and time.time() < deadline:
            time.sleep(0.01)
        body = b"slow done"
    else:
        body = b"hello"
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body))
    conn.close()
def serve():
    while True:
        conn, _ = listener.accept()
        threading.Thread(target=answer, args=(conn,), daemon=True).start()
threading.Thread(target=serve, daemon=True).start()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": "remote",
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
while f.readline():
    pass
"#;

fn install(home: &Path) {
    let dir = home.join(".agent24/packages/remote");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: remote\nversion: \"0.1.0\"\nroute_namespace: /api/v1/remote\n\
         event_module: remote\ndata_dir: ~/.agent24/os/remote/\n\
         kernel_capabilities: []\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod.py"), MODULE).unwrap();
}

/// `GET path` with the daemon's token: `(status, body)`, or `None` if the
/// daemon could not be reached.
fn get(port: u16, token: &str, path: &str) -> Option<(u16, String)> {
    call(port, token, "GET", path, "")
}

/// `method path` with the daemon's token and a JSON `body`: `(status, body)`,
/// or `None` if the daemon could not be reached.
fn call(port: u16, token: &str, method: &str, path: &str, body: &str) -> Option<(u16, String)> {
    let mut s = TcpStream::connect(("127.0.0.1", port)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut raw = String::new();
    s.read_to_string(&mut raw).ok()?;
    let status = raw.split(' ').nth(1)?.parse().ok()?;
    let body = raw.split_once("\r\n\r\n").map(|(_, b)| b.to_owned())?;
    Some((status, body))
}

/// The daemon, killed if the test fails before it stops it — and the module
/// with it, once its pid is known — so a failing test leaves no process behind.
struct Running {
    daemon: std::process::Child,
    module: Option<i32>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.module {
            // Usually already gone: that is what the test asserted.
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn alive(pid: i32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stderr(Stdio::null())
        .status()
        .unwrap()
        .success()
}

/// A started daemon: its port and token, and its log line by line — so a test
/// can wait for it to have HEARD something, not only for it to have been sent.
struct Daemon {
    run: Running,
    port: u16,
    token: String,
    log: std::sync::mpsc::Receiver<String>,
}

fn start(home: &Path) -> Daemon {
    let daemon = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut run = Running {
        daemon,
        module: None,
    };
    // Bounded: a daemon that stalls before its ready line fails the test
    // rather than hanging it.
    let stdout = run.daemon.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let stderr = run.daemon.stderr.take().unwrap();
    let (log_tx, log) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if log_tx.send(line).is_err() {
                break;
            }
        }
    });
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    Daemon {
        run,
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
        log,
    }
}

/// Wait until the package answers through the proxy — once it has shaken
/// hands — and return its pid, which `Running` then kills on a failure.
fn serving(d: &mut Daemon, home: &Path) -> i32 {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((200, body)) = get(d.port, &d.token, "/api/v1/remote/hi") {
            assert_eq!(body, "hello");
            break;
        }
        assert!(Instant::now() < deadline, "the package never answered");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pid: i32 = std::fs::read_to_string(home.join(".agent24/os/remote/pid"))
        .unwrap()
        .parse()
        .unwrap();
    d.run.module = Some(pid);
    pid
}

/// Start a `/slow` request and wait until the module is working on it — not
/// for a guessed sleep.
fn slow_in_flight(d: &Daemon, home: &Path) -> std::thread::JoinHandle<Option<(u16, String)>> {
    let slow = {
        let (port, token) = (d.port, d.token.clone());
        std::thread::spawn(move || get(port, &token, "/api/v1/remote/slow"))
    };
    let entered = home.join(".agent24/os/remote/slow-entered");
    let by = Instant::now() + Duration::from_secs(10);
    while !entered.exists() {
        assert!(
            Instant::now() < by,
            "the slow request never reached the module"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    slow
}

/// Wait for the daemon to log a line containing `needle`.
fn logged(d: &Daemon, needle: &str, what: &str) {
    let by = Instant::now() + Duration::from_secs(5);
    loop {
        let left = by.saturating_duration_since(Instant::now());
        match d.log.recv_timeout(left) {
            Ok(line) if line.contains(needle) => return,
            Ok(_) => {}
            Err(_) => panic!("{what}"),
        }
    }
}

fn gone_within(pid: i32, secs: u64, what: &str) {
    let by = Instant::now() + Duration::from_secs(secs);
    while alive(pid) {
        assert!(Instant::now() < by, "{what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24")
        .tempdir_in("/tmp")
        .unwrap()
}

/// SIGTERM with a request in flight at a started package: the request is
/// answered by the module — drained, not abandoned (SPEC §4) — the daemon
/// exits within its bound (TASKS B2: ~2s; asserted with slack for a loaded
/// machine), and the module process is gone.
#[test]
fn a_sigterm_drains_the_packages_request_and_stops_it() {
    let home = tmp_home();
    install(home.path());
    let mut d = start(home.path());
    let pid = serving(&mut d, home.path());
    let slow = slow_in_flight(&d, home.path());

    let t0 = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-TERM", &d.run.daemon.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    // Released only once the module's run is DRAINING — the supervisor logs
    // that after the generation refuses new work — with the request still held
    // by the module. Releasing it earlier would prove nothing.
    logged(
        &d,
        "draining before the stop",
        "the daemon never began draining the module",
    );
    std::fs::write(home.path().join(".agent24/os/remote/release"), b"").unwrap();

    assert_eq!(
        slow.join().unwrap(),
        Some((200, "slow done".to_owned())),
        "the request in flight at the shutdown was not drained"
    );
    let exited = loop {
        if let Some(status) = d.run.daemon.try_wait().unwrap() {
            break status;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the daemon did not exit within 10s of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let took = t0.elapsed();
    assert!(exited.success(), "{exited:?}");
    // TASKS B2: within 2s — the watchdog guarantees it — with slack for a
    // loaded machine's scheduling.
    assert!(
        took < Duration::from_millis(2500),
        "the daemon took {took:?} to exit"
    );
    assert!(
        !home.path().join(".agent24/daemon.json").exists(),
        "the discovery state file outlived the daemon"
    );
    gone_within(pid, 2, "the module outlived the daemon");
}

/// The module named `remote` in an `/api/v1/os` list.
fn remote(list: &str) -> serde_json::Value {
    let list: serde_json::Value = serde_json::from_str(list).expect("the list");
    list["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "remote")
        .cloned()
        .expect("remote in the list")
}

/// `agent24 os disable` on a running package (SUP-5): the request it holds
/// is drained — answered by the module — while new ones are refused; then the
/// module process is gone, the list says `disabled` with no restart needed,
/// its namespace answers 503, and the daemon itself keeps serving.
#[test]
fn a_disable_drains_and_stops_the_running_package() {
    let home = tmp_home();
    install(home.path());
    let mut d = start(home.path());
    let pid = serving(&mut d, home.path());
    let slow = slow_in_flight(&d, home.path());

    let (status, list) = call(
        d.port,
        &d.token,
        "PATCH",
        "/api/v1/os/remote",
        r#"{"enabled":false}"#,
    )
    .expect("the daemon answered the disable");
    assert_eq!(status, 200, "{list}");
    let m = remote(&list);
    assert_eq!(
        (&m["enabled"], &m["state"], &m["detail"]),
        (
            &serde_json::json!(false),
            &serde_json::json!("disabled"),
            &serde_json::json!("stopping")
        ),
        "{m}"
    );
    assert_eq!(m["restart_required"], false, "{m}");

    // Once the disable has answered, a new request is refused — straight
    // away, not once some later log line says so (review of SUP-5, round 1).
    let refused = get(d.port, &d.token, "/api/v1/remote/hi").expect("the daemon answered");
    assert_eq!(
        refused.0, 503,
        "a new request after the disable: {refused:?}"
    );
    assert!(refused.1.contains("module_draining"), "{refused:?}");
    // The request it holds is not abandoned: released only once the run is
    // DRAINING.
    logged(
        &d,
        "draining before the stop",
        "the disable never began draining the module",
    );
    std::fs::write(home.path().join(".agent24/os/remote/release"), b"").unwrap();
    assert_eq!(
        slow.join().unwrap(),
        Some((200, "slow done".to_owned())),
        "the request in flight at the disable was not drained"
    );

    gone_within(pid, 5, "the disabled module is still running");
    logged(
        &d,
        "disabled and stopped",
        "the disable never reported the module stopped",
    );
    let (status, list) = get(d.port, &d.token, "/api/v1/os").expect("the list");
    assert_eq!(status, 200, "{list}");
    let m = remote(&list);
    assert_eq!(m["state"], "disabled", "{m}");
    assert!(m.get("detail").is_none(), "{m}");
    assert_eq!(m["restart_required"], false, "{m}");
    let stopped = get(d.port, &d.token, "/api/v1/remote/hi").expect("the daemon answered");
    assert_eq!(stopped.0, 503, "the stopped namespace: {stopped:?}");
    assert!(stopped.1.contains("module_stopping"), "{stopped:?}");
    assert!(
        d.run.daemon.try_wait().unwrap().is_none(),
        "the daemon exited with the module"
    );
}

/// A shutdown that begins while a disable is still draining its module cuts
/// that stop short at the budget any module gets: the daemon still exits
/// within its bound and the module is gone, although the disable's own drain
/// would have held it far longer — and the stop is reported as cut short
/// (SUP-5; that the shutdown also WAITS for it is pinned by
/// `the_shutdown_waits_for_the_stops_disables_began`).
#[test]
fn a_sigterm_during_a_disables_drain_stops_the_module_in_bound() {
    let home = tmp_home();
    install(home.path());
    let mut d = start(home.path());
    let pid = serving(&mut d, home.path());
    // Never released: the module holds it until its own 5s deadline.
    let _slow = slow_in_flight(&d, home.path());
    let (status, list) = call(
        d.port,
        &d.token,
        "PATCH",
        "/api/v1/os/remote",
        r#"{"enabled":false}"#,
    )
    .expect("the daemon answered the disable");
    assert_eq!(status, 200, "{list}");
    logged(
        &d,
        "draining before the stop",
        "the disable never began draining the module",
    );

    let t0 = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-TERM", &d.run.daemon.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let exited = loop {
        if let Some(status) = d.run.daemon.try_wait().unwrap() {
            break status;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "the daemon did not exit within 10s of SIGTERM"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    let took = t0.elapsed();
    assert!(exited.success(), "{exited:?}");
    assert!(
        took < Duration::from_millis(2500),
        "the daemon took {took:?} to exit"
    );
    gone_within(pid, 2, "the disabled module outlived the daemon");
    logged(
        &d,
        "was disabled but did not stop cleanly",
        "the shutdown did not wait for the disable's stop",
    );
}
