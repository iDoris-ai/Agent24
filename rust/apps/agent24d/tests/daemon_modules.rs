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

/// The daemon, killed if the test fails before it stops it — and the module
/// with it, once its pid is known — so a failing test leaves no process behind.
struct Running {
    daemon: std::process::Child,
    module: Option<i32>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.module {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
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

/// SIGTERM with a request in flight at a started package: the request is
/// answered by the module — drained, not abandoned (SPEC §4) — the daemon
/// exits within its bound (TASKS B2: ~2s; asserted with slack for a loaded
/// machine), and the module process is gone.
#[test]
fn a_sigterm_drains_the_packages_request_and_stops_it() {
    let home = tempfile::Builder::new()
        .prefix("a24")
        .tempdir_in("/tmp")
        .unwrap();
    install(home.path());
    let daemon = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
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
    // The daemon's log, line by line, so the test can wait for it to have
    // HEARD the signal — not only for the signal to have been sent.
    let stderr = run.daemon.stderr.take().unwrap();
    let (log_tx, log_rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if log_tx.send(line).is_err() {
                break;
            }
        }
    });
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    let port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
    let token = ready["token"].as_str().unwrap().to_owned();

    // Proxied, once the module has shaken hands.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some((200, body)) = get(port, &token, "/api/v1/remote/hi") {
            assert_eq!(body, "hello");
            break;
        }
        assert!(Instant::now() < deadline, "the package never answered");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pid: i32 = std::fs::read_to_string(home.path().join(".agent24/os/remote/pid"))
        .unwrap()
        .parse()
        .unwrap();
    run.module = Some(pid);

    let slow = {
        let token = token.clone();
        std::thread::spawn(move || get(port, &token, "/api/v1/remote/slow"))
    };
    // SIGTERM once the module is working on it — not after a guessed sleep.
    let entered = home.path().join(".agent24/os/remote/slow-entered");
    let by = Instant::now() + Duration::from_secs(10);
    while !entered.exists() {
        assert!(
            Instant::now() < by,
            "the slow request never reached the module"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let t0 = Instant::now();
    assert!(
        Command::new("kill")
            .args(["-TERM", &run.daemon.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    // Released only once the module's run is DRAINING — the supervisor logs
    // that after the generation refuses new work — with the request still held
    // by the module. Releasing it earlier would prove nothing.
    let heard_by = Instant::now() + Duration::from_secs(5);
    loop {
        let left = heard_by.saturating_duration_since(Instant::now());
        match log_rx.recv_timeout(left) {
            Ok(line) if line.contains("draining before the stop") => break,
            Ok(_) => {}
            Err(_) => panic!("the daemon never began draining the module"),
        }
    }
    std::fs::write(home.path().join(".agent24/os/remote/release"), b"").unwrap();

    assert_eq!(
        slow.join().unwrap(),
        Some((200, "slow done".to_owned())),
        "the request in flight at the shutdown was not drained"
    );
    let exited = loop {
        if let Some(status) = run.daemon.try_wait().unwrap() {
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
    let gone_by = Instant::now() + Duration::from_secs(2);
    while alive(pid) {
        assert!(Instant::now() < gone_by, "the module outlived the daemon");
        std::thread::sleep(Duration::from_millis(20));
    }
}
