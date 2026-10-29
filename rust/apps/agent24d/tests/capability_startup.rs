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
    start_with_mode(home, true)
}

fn start_with_mode(home: &std::path::Path, ephemeral: bool) -> (CapabilityDaemon, u16, String) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home);
    command.args([
        "serve",
        "--port",
        "0",
        "--auth-mode",
        "capabilities",
        "--host-bootstrap-stdio",
    ]);
    if ephemeral {
        command.arg("--ephemeral");
    }
    let child = command
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

fn status(port: u16, method: &str, path: &str, bearer: Option<&str>, body: Option<&str>) -> u16 {
    response(port, method, path, bearer, body)
        .split_whitespace()
        .nth(1)
        .expect("HTTP status")
        .parse()
        .expect("numeric HTTP status")
}

fn response(
    port: u16,
    method: &str,
    path: &str,
    bearer: Option<&str>,
    body: Option<&str>,
) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("capability listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("response timeout");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: localhost:{port}\r\n"
    )
    .expect("capability request");
    if let Some(bearer) = bearer {
        write!(stream, "authorization: Bearer {bearer}\r\n").expect("capability request");
    }
    if let Some(body) = body {
        write!(
            stream,
            "content-type: application/json\r\ncontent-length: {}\r\n",
            body.len()
        )
        .expect("capability request");
    }
    write!(stream, "connection: close\r\n\r\n").expect("capability request");
    if let Some(body) = body {
        stream
            .write_all(body.as_bytes())
            .expect("capability request");
    }
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("capability response");
    response
}

fn upgrade_status(port: u16, bearer: Option<&str>) -> u16 {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("capability listener");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("response timeout");
    write!(
        stream,
        "GET /api/v1/events HTTP/1.1\r\n\
         host: localhost:{port}\r\n\
         Upgrade: websocket\r\n\
         Connection: Upgrade\r\n\
         Sec-WebSocket-Version: 13\r\n\
         Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n"
    )
    .expect("capability request");
    if let Some(bearer) = bearer {
        write!(stream, "authorization: Bearer {bearer}\r\n").expect("capability request");
    }
    write!(stream, "\r\n").expect("capability request");
    let mut status_line = String::new();
    BufReader::new(stream)
        .read_line(&mut status_line)
        .expect("capability response");
    status_line
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

    assert_eq!(
        status(port, "GET", "/api/v1/models", Some(&host_bearer), None),
        200
    );
    assert_eq!(status(port, "GET", "/api/v1/models", Some(""), None), 401);
    stop(daemon);
}

#[test]
fn desktop_routes_accept_the_host_bearer_in_capability_mode() {
    let home = tempfile::tempdir().expect("test home");
    let (daemon, port, host_bearer) = start(home.path());
    let routes = [
        ("GET", "/api/v1/models", None),
        ("GET", "/api/v1/sessions", None),
        ("GET", "/api/v1/runs", None),
        ("GET", "/api/v1/approvals?status=pending", None),
        ("GET", "/api/v1/schedules", None),
        ("GET", "/api/v1/os", None),
        ("GET", "/api/v1/attached", None),
        ("GET", "/api/v1/usage", None),
        ("GET", "/api/v1/memory/settings", None),
        ("GET", "/api/v1/memory/assertions", None),
        // A domain-module route (agent24-comm), merged in front of auth like
        // every module. `/api/v1/documents/*` is not listed: that module is
        // not mounted in a fresh home, so the request would only reach the
        // fallback.
        ("GET", "/api/v1/comm/identity", None),
        ("POST", "/api/v1/chat", Some("{}")),
    ];
    for (method, path, body) in routes {
        let with_host = status(port, method, path, Some(&host_bearer), body);
        let without = status(port, method, path, None, body);
        assert!(
            !matches!(with_host, 401 | 403),
            "{method} {path} with host bearer returned {with_host}"
        );
        assert_eq!(
            without, 401,
            "{method} {path} without bearer returned {without}"
        );
    }
    stop(daemon);
}

#[test]
fn desktop_event_stream_accepts_the_host_bearer_in_capability_mode() {
    let home = tempfile::tempdir().expect("test home");
    let (daemon, port, host_bearer) = start(home.path());
    let with_host = upgrade_status(port, Some(&host_bearer));
    let without = upgrade_status(port, None);
    assert_eq!(
        with_host, 101,
        "GET /api/v1/events with host bearer returned {with_host}"
    );
    assert_eq!(
        without, 401,
        "GET /api/v1/events without bearer returned {without}"
    );
    stop(daemon);
}

#[test]
fn idoris_corrupt_operations_do_not_prevent_daemon_startup() {
    let home = tempfile::tempdir().unwrap();
    let root = home
        .path()
        .join("Library/Application Support/Agent24/idoris");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("operations.json"), "invalid json").unwrap();
    let (daemon, port, bearer) = start_with_mode(home.path(), false);
    assert_eq!(
        status(port, "GET", "/api/v1/models", Some(&bearer), None),
        200
    );
    assert_eq!(
        status(port, "POST", "/api/v1/idoris/start", Some(&bearer), None),
        500
    );
    let reply = response(port, "GET", "/api/v1/idoris/status", Some(&bearer), None);
    let (_, body) = reply.split_once("\r\n\r\n").unwrap();
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["state"], "failed");
    assert_eq!(body["last_error"], "persistence_failed");
    stop(daemon);
}

#[test]
fn idoris_restore_persistence_failure_does_not_prevent_daemon_startup() {
    use std::os::unix::fs::PermissionsExt;
    let home = tempfile::tempdir().unwrap();
    let root = home
        .path()
        .join("Library/Application Support/Agent24/idoris");
    std::fs::create_dir_all(&root).unwrap();
    let record = agent24d::idoris_host::Installation {
        version: "0.4.0".into(),
        release_dir: root.join("release"),
        archive_path: root.join("archive.tar.gz"),
        components_dir: Some(root.join("components")),
        key_id: Some("vk_fixture".into()),
        data_port: 8740,
        admin_port: 8741,
        roles: vec![],
        core_reserved_bytes: Some(0),
        omlx_url: None,
        omlx_key_id: None,
        desired_running: true,
        verification: None,
    };
    std::fs::write(
        root.join("installation.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (daemon, port, bearer) = start_with_mode(home.path(), false);
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(
        status(port, "GET", "/api/v1/models", Some(&bearer), None),
        200
    );
    assert_eq!(
        status(port, "POST", "/api/v1/idoris/start", Some(&bearer), None),
        500
    );
    let reply = response(port, "GET", "/api/v1/idoris/status", Some(&bearer), None);
    let (_, body) = reply.split_once("\r\n\r\n").unwrap();
    let body: serde_json::Value = serde_json::from_str(body).unwrap();
    assert_eq!(body["state"], "failed");
    assert_eq!(body["last_error"], "persistence_failed");
    stop(daemon);
}
