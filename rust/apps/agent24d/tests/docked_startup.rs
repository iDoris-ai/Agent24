//! CORE-5 5f-3 black box: the real `serve` driven into the docked path.
//! Needs the debug-only `test-enrollment` feature, whose
//! `AGENT24_TEST_MIGRATION_GUARD=1` switches the guard on; production
//! (`migration_coordinator::ENABLED = false`) has no such switch.

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(all(unix, feature = "test-enrollment"))]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    _stdin: Option<ChildStdin>,
    line: serde_json::Value,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn data_dir(with_marker: bool) -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("d");
    std::fs::create_dir(&data).unwrap();
    if with_marker {
        let mark = serde_json::json!({"existed": true, "version": 7});
        let marker = serde_json::json!({
            "format_version": 1, "attempt_id": "a1", "snapshot_id": null,
            "created_at": "2026-11-02T00:00:00Z",
            "failed_binary": {"version": "0.6.0", "sha256": "h"},
            "previous_binary": null, "database": null, "migration_version": null,
            "phase": "migrating", "error_code": "migration_failed",
            "error_summary": "migration failed; databases restored from snapshot",
            "rollback": "restored",
            "before_migrations": {"store": mark, "memory": mark},
            "observed_migrations": {"store": mark, "memory": mark},
        });
        std::fs::write(data.join("migration_failed"), marker.to_string()).unwrap();
    }
    (root, data)
}

fn spawn(data: &Path, guard: bool, args: &[&str]) -> Child {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", data.parent().unwrap())
        .env("AGENT24_DATA_DIR", data);
    if guard {
        cmd.env("AGENT24_TEST_MIGRATION_GUARD", "1");
    }
    cmd.args(["serve"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap()
}

fn start(data: &Path, guard: bool, args: &[&str]) -> Daemon {
    let mut child = spawn(data, guard, args);
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(30)).expect("no line");
    Daemon {
        child,
        _stdin: stdin,
        line: serde_json::from_str(&line).unwrap(),
    }
}

fn get(port: u64, path: &str) -> String {
    let mut s = TcpStream::connect(("127.0.0.1", u16::try_from(port).unwrap())).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

fn wait_exit(d: &mut Daemon) -> std::process::ExitStatus {
    let by = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(s) = d.child.try_wait().unwrap() {
            return s;
        }
        assert!(Instant::now() < by, "daemon did not exit");
        std::thread::sleep(Duration::from_millis(20));
    }
}

const SERVICE: &[&str] = &["--auth-mode", "capabilities", "--enroll-endpoint"];
const CHILD: &[&str] = &["--auth-mode", "capabilities", "--host-bootstrap-stdio"];

/// Nothing a business daemon creates may exist: no database, no workspace,
/// no hardware snapshot, no OS tree.
fn assert_no_business_start(data: &Path) {
    for name in [
        "agent24.db",
        "memory.db",
        "workspace",
        "hardware.json",
        "os",
    ] {
        assert!(
            !data.join(name).exists(),
            "{name} must not exist when docked"
        );
    }
}

#[test]
fn service_mode_docks_serves_and_exits_cleanly_on_sigterm() {
    let (_root, data) = data_dir(true);
    let mut d = start(&data, true, SERVICE);
    assert_eq!(d.line["type"], "docked");
    assert!(!d.line.to_string().contains("token"));
    let port = d.line["port"].as_u64().unwrap();
    let rec: serde_json::Value =
        serde_json::from_slice(&std::fs::read(data.join("daemon.json")).unwrap()).unwrap();
    assert_eq!(rec["startup_state"], "docked");
    assert_eq!(rec["generation"], d.line["generation"]);
    assert!(rec["enroll_endpoint"].is_string() && data.join("enroll.sock").exists());
    assert!(get(port, "/api/v1/health").starts_with("HTTP/1.1 503"));
    assert!(get(port, "/api/v1/runs").starts_with("HTTP/1.1 404"));
    assert_no_business_start(&data);

    // A second instance cannot take the singleton lock.
    let mut second = start(&data, true, SERVICE);
    assert_eq!(second.line["type"], "failed");
    assert_eq!(second.line["code"], "already_running");
    assert!(!wait_exit(&mut second).success());

    // The real SIGTERM path: clean exit (so `SuccessfulExit=false` is quiet).
    Command::new("/bin/kill")
        .args(["-TERM", &d.child.id().to_string()])
        .status()
        .unwrap();
    assert!(wait_exit(&mut d).success(), "SIGTERM must exit 0");
    assert!(!data.join("daemon.json").exists());
}

#[test]
fn host_child_mode_sends_docked_on_the_pipe_without_a_token() {
    let (_root, data) = data_dir(true);
    let d = start(&data, true, CHILD);
    assert_eq!(d.line["type"], "docked");
    assert!(!d.line.to_string().contains("token"));
    let rec = std::fs::read_to_string(data.join("daemon.json")).unwrap();
    assert!(rec.contains("\"docked\"") && !rec.contains("token"));
    assert!(
        !data.join("enroll.sock").exists() || d.line["enroll_endpoint"].is_string(),
        "host child opens the endpoint by itself and says so"
    );
    assert_no_business_start(&data);
}

#[test]
fn legacy_mode_docks_without_an_enrolment_endpoint() {
    let (_root, data) = data_dir(true);
    let d = start(&data, true, &[]);
    assert_eq!(d.line["type"], "docked");
    assert!(d.line["enroll_endpoint"].is_null());
    assert!(!data.join("enroll.sock").exists());
    assert!(!d.line.to_string().contains("\"token\""));
    assert_no_business_start(&data);
}

/// Without the switch (the production configuration) a marker on disk changes
/// nothing: the daemon starts normally and says `ready`.
#[test]
fn guard_off_starts_normally_even_with_a_marker() {
    let (_root, data) = data_dir(true);
    let d = start(&data, false, &[]);
    assert_eq!(d.line["type"], "ready");
    assert!(data.join("agent24.db").exists());
}
