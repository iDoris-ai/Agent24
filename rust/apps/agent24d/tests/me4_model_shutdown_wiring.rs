//! ME4-4.2.2b2, H1 (Opus review round on top of `bb6fb0e`). The authoritative
//! test for the literal line `spawn_cancel_root(shutdown.modules_cut_off())`
//! in `server.rs::serve()` is the structural test
//! `server::tests::the_model_callback_cancel_root_is_spawned_from_modules_cut_off`
//! (`include_str!`), NOT this file — see that test's own doc comment for why:
//! empirically, `Shutdown::request()` ALSO revokes the module's `Generation`
//! as part of stopping it (design §3.3's own "代次撤销" row, pre-existing
//! `os-proto` machinery), which independently cancels an in-flight model call
//! within tens of milliseconds of the SAME cut-off — a real daemon, measured
//! with a real subprocess module and a real hung TCP provider, cannot
//! reliably tell "the cancel root fired" apart from "the module's connection
//! died for the other, unrelated reason it always eventually does during a
//! shutdown" (overlapping ranges: ~630-670ms for correct code vs ~670-760ms
//! with the cancel root wired to a token that never fires, across three
//! trials each). That overlap is exactly why the design doc's own review
//! round accepted an `include_str!` structural test as the deterministic
//! fallback here.
//!
//! This file stays anyway as real end-to-end coverage the structural test
//! CANNOT give: that a `models`-granted out-of-process module making a real
//! `_a24/model/complete` call against a real (hung) provider, during a real
//! `POST /api/v1/shutdown`, does not wedge the daemon — the provider's
//! connection eventually closes, the module sees `cancelled` or its
//! connection close, and the daemon still exits within its bound. Harness
//! shape copied from `daemon_modules.rs`'s `Running`/`Daemon`/`start`/`call`
//! (same reason that file gives: the parts that matter live in `serve`).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Requests `models` (default `model_access: local_only`), then makes ONE
/// `_a24/model/complete` call with no `request_id` over its callback
/// connection and records exactly what it got back — `cancelled`, or the
/// connection closing with no response at all (`readline()` returning empty).
const MODULE: &str = r#"import hashlib, json, os, socket, threading
name = "probe"
data_dir = os.environ["A24_DATA_DIR"]
with open(os.path.join(data_dir, "pid"), "w") as f:
    f.write(str(os.getpid()))
with open("domain-os.yml", "rb") as f:
    digest = "sha256:" + hashlib.sha256(f.read()).hexdigest()
listener = socket.socket(fileno=int(os.environ["A24_LISTEN_FD"]))
def serve():
    while True:
        conn, _ = listener.accept()
        conn.close()
threading.Thread(target=serve, daemon=True).start()
cb = socket.socket(socket.AF_UNIX)
cb.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": name,
    "manifest_digest": digest, "auth_token": os.environ["A24_HANDSHAKE_TOKEN"],
    "capabilities": []}}
cb.sendall((json.dumps(req) + "\n").encode())
f = cb.makefile("rb")
f.readline()
complete_req = {"jsonrpc": "2.0", "id": "2", "method": "_a24/model/complete",
                "params": {"messages": [{"role": "user", "content": "hi"}]}}
cb.sendall((json.dumps(complete_req) + "\n").encode())
line = f.readline()
with open(os.path.join(data_dir, "result.json"), "wb") as out:
    out.write(line if line else b'{"connection_closed": true}')
while f.readline():
    pass
"#;

fn install(home: &Path) {
    let dir = home.join(".agent24/packages/probe");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("domain-os.yml"),
        "name: probe\nversion: \"0.1.0\"\nroute_namespace: /api/v1/probe\n\
         event_module: probe\ndata_dir: ~/.agent24/os/probe/\n\
         kernel_capabilities: [models]\nimpl_kind: out_of_process_provider\n\
         spawn:\n  command: python3\n  args: [\"-I\", \"-S\", \"mod.py\"]\n",
    )
    .unwrap();
    std::fs::write(dir.join("mod.py"), MODULE).unwrap();
}

/// `method path` with the daemon's token and a JSON `body`: `(status, body)`.
fn call(port: u16, token: &str, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\n\
         content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw.split(' ').nth(1).unwrap().parse().unwrap();
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap();
    (status, body)
}

/// The daemon, killed if the test fails before it stops it — and the module
/// with it, once its pid is known — so a failing test leaves no process
/// behind (copied from `daemon_modules.rs`).
struct Running {
    daemon: std::process::Child,
    module: Option<i32>,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(pid) = self.module {
            let _ = Command::new("kill")
                .args(["-KILL", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

struct Daemon {
    run: Running,
    port: u16,
    token: String,
}

fn start(home: &Path, omlx_port: u16) -> Daemon {
    let daemon = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("OMLX_URL", format!("http://127.0.0.1:{omlx_port}"))
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
    // Drain stderr in the background so a full pipe never blocks the daemon.
    let stderr = run.daemon.stderr.take().unwrap();
    std::thread::spawn(
        move || {
            for _line in BufReader::new(stderr).lines().map_while(Result::ok) {}
        },
    );
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    Daemon {
        run,
        port: u16::try_from(ready["port"].as_u64().unwrap()).unwrap(),
        token: ready["token"].as_str().unwrap().to_owned(),
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

fn tmp_home() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("a24")
        .tempdir_in("/tmp")
        .unwrap()
}

/// A provider stub: accepts ONE connection, signals `saw_request` once it
/// has read at least one byte of the request (proof the daemon's own HTTP
/// client really dialled it), then never answers — only reads, so it can
/// tell whether the peer later closes the connection (`closed`).
struct StubProvider {
    port: u16,
    saw_request: mpsc::Receiver<()>,
    closed: Arc<AtomicBool>,
}

fn start_hanging_provider() -> StubProvider {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let closed2 = closed.clone();
    std::thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        let mut buf = [0u8; 8192];
        if stream.read(&mut buf).unwrap_or(0) > 0 {
            let _ = tx.send(());
        }
        // Never write a response — just keep reading so a later close (the
        // peer's FIN) is observable as `Ok(0)`.
        let mut trailing = [0u8; 16];
        loop {
            match stream.read(&mut trailing) {
                Ok(0) | Err(_) => {
                    closed2.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(_) => {}
            }
        }
    });
    StubProvider {
        port,
        saw_request: rx,
        closed,
    }
}

/// Real end-to-end coverage of "a `models`-granted module's in-flight call
/// does not wedge a real shutdown" — see the module doc comment above for
/// why this is NOT the authoritative test for H1's specific mutations.
#[test]
fn a_models_granted_module_does_not_block_a_real_shutdown() {
    let home = tmp_home();
    install(home.path());
    let provider = start_hanging_provider();
    let mut d = start(home.path(), provider.port);

    provider
        .saw_request
        .recv_timeout(Duration::from_secs(30))
        .expect(
            "the module's _a24/model/complete call never reached the provider \
         (the module may not have mounted/handshaken)",
        );

    // Now that the provider has the bytes, the module's pid file must exist
    // (it is written before the handshake even starts) — record it so a
    // failing assertion below still cleans the process up.
    let pid: i32 = std::fs::read_to_string(home.path().join(".agent24/os/probe/pid"))
        .unwrap()
        .parse()
        .unwrap();
    d.run.module = Some(pid);

    // The production path: `agent24 daemon stop` posts here too.
    let (status, _) = call(d.port, &d.token, "POST", "/api/v1/shutdown", "");
    assert_eq!(status, 202);

    let by = Instant::now() + Duration::from_secs(10);
    while !provider.closed.load(Ordering::SeqCst) {
        assert!(
            Instant::now() < by,
            "the provider never observed the connection close — the in-flight \
             model call outlived the daemon's shutdown"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    // The module's own record of what its call got back — "cancelled" OR
    // the connection closing are both correct (design §3.3: the revoke and
    // the cut-off race, and in practice the module's own idle drain usually
    // wins that race — see the module doc comment above). When the module is
    // torn down before it ever writes `result.json`, its process being
    // confirmed gone IS the "connection closed" outcome (its end of the
    // socket died with the process) — not a thing to keep waiting for.
    let result_path = home.path().join(".agent24/os/probe/result.json");
    loop {
        if let Ok(bytes) = std::fs::read(&result_path)
            && let Ok(result) = serde_json::from_slice::<serde_json::Value>(&bytes)
        {
            let connection_closed = result
                .get("connection_closed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false);
            let cancelled = result["error"]["data"]["kind"].as_str() == Some("cancelled");
            assert!(
                connection_closed || cancelled,
                "the module must see either `cancelled` or its connection close: {result}"
            );
            break;
        }
        if !alive(pid) {
            break;
        }
        assert!(
            Instant::now() < by,
            "the module neither recorded a result nor exited"
        );
        std::thread::sleep(Duration::from_millis(20));
    }

    let exited = loop {
        if let Some(status) = d.run.daemon.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < by,
            "the daemon did not exit within its bound — an in-flight model \
             call must not be able to wedge shutdown"
        );
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(exited.success(), "{exited:?}");
}
