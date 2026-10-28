//! CORE-2e black-box: service mode (`--auth-mode capabilities
//! --enroll-endpoint`), the enrolment endpoint placeholder and the startup
//! matrix (docs/agent/CORE-2-ENROLL-DESIGN.md D5, D8, T4, T5).

#![allow(clippy::unwrap_used, clippy::expect_used)]
#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Daemon {
    child: Child,
    stdin: Option<ChildStdin>,
    ready: Option<serde_json::Value>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

const SERVICE: &[&str] = &["--auth-mode", "capabilities", "--enroll-endpoint"];
const CHILD: &[&str] = &["--auth-mode", "capabilities", "--host-bootstrap-stdio"];

fn data_dir() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("d");
    std::fs::create_dir(&data).unwrap();
    // Looser than required: the endpoint must tighten it.
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o755)).unwrap();
    (root, data)
}

fn spawn(data: &Path, args: &[&str]) -> Daemon {
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", data.parent().unwrap())
        .env("AGENT24_DATA_DIR", data)
        .args(["serve"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take();
    Daemon {
        child,
        stdin,
        ready: None,
    }
}

/// First stdout line, or `None` if the process ended without one.
fn first_line(d: &mut Daemon, wait: Duration) -> Option<String> {
    let stdout = d.child.stdout.take()?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    rx.recv_timeout(wait).ok().filter(|l| !l.is_empty())
}

fn start_ok(data: &Path, args: &[&str]) -> Daemon {
    let mut d = spawn(data, args);
    let line = first_line(&mut d, Duration::from_secs(30)).expect("no ready line");
    d.ready = Some(serde_json::from_str(&line).unwrap());
    d
}

fn exit_code(d: &mut Daemon, wait: Duration) -> Option<std::process::ExitStatus> {
    let by = Instant::now() + wait;
    while Instant::now() < by {
        if let Some(s) = d.child.try_wait().unwrap() {
            return Some(s);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn assert_fails_without_ready(data: &Path, args: &[&str]) {
    let mut d = spawn(data, args);
    assert!(
        first_line(&mut d, Duration::from_secs(30)).is_none(),
        "a refused start must print no ready line"
    );
    let status = exit_code(&mut d, Duration::from_secs(15)).expect("process must exit");
    assert!(!status.success(), "refused start must exit non-zero");
}

fn sock(data: &Path) -> PathBuf {
    data.join("enroll.sock")
}

fn ask(path: &Path, request: &[u8]) -> String {
    let mut s = UnixStream::connect(path).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    // The daemon may close mid-write on an over-long frame.
    let _ = s.write_all(request);
    let mut out = String::new();
    let _ = s.read_to_string(&mut out);
    out
}

fn wait_gone(path: &Path) {
    let by = Instant::now() + Duration::from_secs(10);
    while path.exists() {
        assert!(Instant::now() < by, "{} was not removed", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn term(d: &mut Daemon) {
    #[allow(clippy::cast_possible_wrap)]
    let pid = rustix::process::Pid::from_raw(d.child.id() as i32).unwrap();
    rustix::process::kill_process(pid, rustix::process::Signal::Term).unwrap();
}

#[test]
fn rejected_matrix_rows_do_not_start() {
    let (_root, data) = data_dir();
    // legacy + enroll
    assert_fails_without_ready(&data, &["--enroll-endpoint"]);
    // capabilities with neither bootstrap nor enroll
    assert_fails_without_ready(&data, &["--auth-mode", "capabilities"]);
    // capabilities + bootstrap + enroll (ambiguous, Q5)
    assert_fails_without_ready(
        &data,
        &[
            "--auth-mode",
            "capabilities",
            "--host-bootstrap-stdio",
            "--enroll-endpoint",
        ],
    );
    // service mode cannot be ephemeral
    assert_fails_without_ready(&data, &[SERVICE, &["--ephemeral"]].concat());
    assert!(!sock(&data).exists());
}

#[test]
fn service_mode_publishes_endpoint_and_no_secret() {
    let (_root, data) = data_dir();
    let mut d = start_ok(&data, SERVICE);
    let ready = d.ready.clone().unwrap();
    assert_eq!(ready["auth_mode"], "capabilities");
    assert_eq!(ready["enroll_endpoint"], sock(&data).to_str().unwrap());
    let raw = serde_json::to_string(&ready).unwrap();
    assert!(!raw.contains("token"), "ready line leaked a secret: {raw}");

    let state = std::fs::read_to_string(data.join("daemon.json")).unwrap();
    let state: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(state["enroll_endpoint"], sock(&data).to_str().unwrap());
    assert_eq!(state["auth_mode"], "capabilities");
    assert!(state.get("token").is_none(), "daemon.json carries a token");

    let sock_mode = std::fs::symlink_metadata(sock(&data)).unwrap().mode() & 0o7777;
    assert_eq!(sock_mode, 0o600);
    let dir_mode = std::fs::metadata(&data).unwrap().mode() & 0o7777;
    assert_eq!(dir_mode, 0o700);

    // A graceful stop takes its own socket and discovery record with it.
    term(&mut d);
    assert!(
        exit_code(&mut d, Duration::from_secs(15))
            .unwrap()
            .success()
    );
    wait_gone(&sock(&data));
    assert!(!data.join("daemon.json").exists());
}

#[test]
fn second_instance_fails() {
    let (_root, data) = data_dir();
    let _first = start_ok(&data, SERVICE);
    let inode = std::fs::symlink_metadata(sock(&data)).unwrap().ino();
    assert_fails_without_ready(&data, SERVICE);
    // The loser must not have touched the winner's socket.
    assert_eq!(std::fs::symlink_metadata(sock(&data)).unwrap().ino(), inode);
    assert!(ask(&sock(&data), b"{}\n").contains("bad_frame"));
}

#[test]
fn new_generation_replaces_a_stale_socket() {
    let (_root, data) = data_dir();
    let mut first = start_ok(&data, SERVICE);
    let old = std::fs::symlink_metadata(sock(&data)).unwrap().ino();
    // Crash: SIGKILL leaves the socket node (and daemon.json) behind.
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    assert!(sock(&data).exists(), "SIGKILL should leave a stale socket");
    // Pin the old inode with a second link: filesystems (tmpfs/ext4 on
    // Linux) recycle a freed inode number at once, so without this a genuine
    // unlink + re-create can show the same number.
    std::fs::hard_link(sock(&data), data.join("stale.pin")).unwrap();
    assert_eq!(
        std::fs::symlink_metadata(data.join("stale.pin"))
            .unwrap()
            .ino(),
        old
    );

    let second = start_ok(&data, SERVICE);
    let meta = std::fs::symlink_metadata(sock(&data)).unwrap();
    assert_ne!(meta.ino(), old, "the stale node must have been replaced");
    assert_eq!(meta.mode() & 0o7777, 0o600);
    assert_ne!(
        second.ready.as_ref().unwrap()["generation"],
        first.ready.as_ref().unwrap()["generation"]
    );
    assert!(ask(&sock(&data), b"{}\n").contains("bad_frame"));
}

#[test]
fn bind_failure_means_no_ready_and_non_zero_exit() {
    // A non-socket squatting on the path is never overwritten.
    let (_root, data) = data_dir();
    std::fs::write(sock(&data), b"mine").unwrap();
    assert_fails_without_ready(&data, SERVICE);
    assert_eq!(std::fs::read(sock(&data)).unwrap(), b"mine");
    assert!(!data.join("daemon.json").exists(), "half-ready discovery");

    // A live foreign listener is never displaced.
    let (_root2, data2) = data_dir();
    let _live = std::os::unix::net::UnixListener::bind(sock(&data2)).unwrap();
    assert_fails_without_ready(&data2, SERVICE);
    assert!(!data2.join("daemon.json").exists());

    // A discovery record that cannot be published fails the service start
    // (revises facts 8.3 for the service path), and leaves no socket behind.
    let (_root4, data4) = data_dir();
    std::fs::create_dir(data4.join("daemon.json")).unwrap();
    assert_fails_without_ready(&data4, SERVICE);
    wait_gone(&sock(&data4));

    // A symlinked data directory is refused.
    let (_root3, real) = data_dir();
    let link = real.parent().unwrap().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    assert_fails_without_ready(&link, SERVICE);
    assert!(!sock(&real).exists());
}

#[test]
fn service_mode_ignores_stdin_eof_but_host_child_still_exits_on_it() {
    let (_root, data) = data_dir();
    let mut service = start_ok(&data, SERVICE);
    service.stdin.take();
    assert!(
        exit_code(&mut service, Duration::from_millis(1500)).is_none(),
        "closing stdin must not shut a service down"
    );
    term(&mut service);
    assert!(exit_code(&mut service, Duration::from_secs(15)).is_some());
    drop(service);

    // Regression: the host-child mode keeps its parent-EOF lease and opens
    // the endpoint by itself.
    let (_root2, data2) = data_dir();
    let mut child = start_ok(&data2, CHILD);
    assert!(child.ready.as_ref().unwrap()["product_host_token"].is_string());
    assert!(ask(&sock(&data2), b"{}\n").contains("bad_frame"));
    child.stdin.take();
    assert!(
        exit_code(&mut child, Duration::from_secs(15))
            .expect("parent EOF must stop the child-mode daemon")
            .success()
    );
    wait_gone(&sock(&data2));
}

#[test]
fn endpoint_rejects_invalid_frames_and_never_a_token() {
    let (_root, data) = data_dir();
    let _d = start_ok(&data, SERVICE);
    for request in [
        &b"{\"v\":1,\"hello\":true}\n"[..],
        b"{\"v\":999}\n",
        b"not json\n",
    ] {
        let reply = ask(&sock(&data), request);
        let reply: serde_json::Value = serde_json::from_str(reply.trim()).unwrap();
        assert_eq!(
            reply,
            serde_json::json!({"v":1,"op":"error","code": if request == b"{\"v\":999}\n" {"unsupported_version"} else {"bad_frame"},"message":"Enrollment rejected"})
        );
    }
    // An idle peer gets the protocol timeout. An actual early EOF closes silently.
    assert!(ask(&sock(&data), b"").contains("timeout"));
    let mut eof = UnixStream::connect(sock(&data)).unwrap();
    eof.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    eof.shutdown(std::net::Shutdown::Write).unwrap();
    let mut reply = String::new();
    eof.read_to_string(&mut reply).unwrap();
    assert!(reply.is_empty());
    let mut big = vec![b'x'; 20 * 1024];
    big.push(b'\n');
    assert!(ask(&sock(&data), &big).contains("bad_frame"));
}

#[test]
fn endpoint_caps_concurrent_connections_and_recovers() {
    let (_root, data) = data_dir();
    let _d = start_ok(&data, SERVICE);
    // Eight idle peers fill the cap (they send nothing and just sit there).
    let idle: Vec<UnixStream> = (0..8)
        .map(|_| UnixStream::connect(sock(&data)).unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(300));
    // The ninth is closed unanswered, promptly (not after the 5 s deadline).
    let started = Instant::now();
    let mut over = UnixStream::connect(sock(&data)).unwrap();
    over.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let _ = over.write_all(b"{}\n");
    let mut out = String::new();
    let _ = over.read_to_string(&mut out);
    assert_eq!(out, "", "over-cap connection must get no reply");
    assert!(started.elapsed() < Duration::from_secs(3));
    // Freeing the slots restores service.
    drop(idle);
    std::thread::sleep(Duration::from_millis(300));
    assert!(ask(&sock(&data), b"{}\n").contains("bad_frame"));
}
