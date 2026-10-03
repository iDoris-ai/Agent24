#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::{Duration, Instant};
struct CapabilityDaemon {
    child: Child,
    stdin: Option<ChildStdin>,
}

impl Drop for CapabilityDaemon {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start(home: &std::path::Path) -> (CapabilityDaemon, u16, String) {
    let child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args([
            "serve",
            "--ephemeral",
            "--auth-mode",
            "capabilities",
            "--host-bootstrap-stdio",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("start capability daemon");
    let mut daemon = CapabilityDaemon { child, stdin: None };
    let stdout = daemon.child.stdout.take().expect("capability ready pipe");
    daemon.stdin = daemon.child.stdin.take();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("capability daemon did not become ready");
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("capability ready record");
    assert_eq!(ready["auth_mode"], "capabilities");
    let port = u16::try_from(ready["port"].as_u64().expect("ready port")).expect("u16 port");
    let host_bearer = ready["product_host_token"]
        .as_str()
        .expect("host bearer in private ready record")
        .to_owned();
    (daemon, port, host_bearer)
}

fn protected_status(port: u16, bearer: &str) -> u16 {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("capability listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("response timeout");
    write!(
        stream,
        "GET /api/v1/models HTTP/1.1\r\nhost: localhost\r\nauthorization: Bearer {bearer}\r\nconnection: close\r\n\r\n"
    )
    .expect("capability request");
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("capability response");
    response
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .expect("numeric HTTP status")
}

fn stop(mut daemon: CapabilityDaemon) {
    daemon.stdin.take();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = daemon.child.try_wait().expect("daemon status") {
            assert!(status.success(), "capability daemon exited unsuccessfully");
            return;
        }
        assert!(Instant::now() < deadline, "capability daemon did not stop");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn ready_is_emitted_only_after_the_capability_listener_authenticates() {
    let home = tempfile::tempdir().expect("test home");
    let (daemon, port, host_bearer) = start(home.path());

    assert_eq!(protected_status(port, &host_bearer), 200);
    assert_eq!(protected_status(port, ""), 401);
    stop(daemon);
}
