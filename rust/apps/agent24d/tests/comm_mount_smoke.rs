//! COMM-2a's own acceptance: `/api/v1/comm/*` is mounted, reachable, and —
//! with no `A24_HYPHAE_BIN` configured (nor its `A24_SPEAKER_BIN` fallback,
//! nor a sibling `hyphae` binary next to the test binary, which never
//! exists) — answers `not_configured` rather than stopping the daemon from
//! starting at all (`comm_routes.rs`'s own brief). A real blackbox test
//! (spawns the actual `agent24d` binary), same pattern as
//! `tests/daemon_modules.rs`; everything else (the actual comm routes
//! against a real Hyphae binary) is covered in-process in
//! `agent24-comm`'s own `tests/router_lifecycle.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Kills and reaps the wrapped `agent24d` child on drop, including when a
/// test `panic!`s/`assert!`-fails partway through — a bare `daemon.kill()`
/// placed after the assertions never runs in that case, which is exactly how
/// a previous version of this test leaked an orphaned `agent24d` that ran
/// for nearly two hours. The `$HOME` tempdir is held alongside the child so
/// it outlives the daemon using it and is cleaned up only once the daemon is
/// actually dead (field drop order: `child` first via our own `kill`/`wait`,
/// then `_home` via its own `Drop`).
struct DaemonGuard {
    child: Child,
    _home: tempfile::TempDir,
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn get(port: u16, token: &str, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: x\r\nauthorization: Bearer {token}\r\nconnection: close\r\n\r\n"
    )
    .unwrap();
    let mut raw = String::new();
    s.read_to_string(&mut raw).unwrap();
    let status = raw.split(' ').nth(1).unwrap().parse().unwrap();
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_, b)| b.to_owned())
        .unwrap_or_default();
    (status, body)
}

/// Spawns `agent24d serve --port 0` with a fresh `$HOME` and `env_clear`'d
/// otherwise, plus whatever `extra_env` adds on top. Returns the guard (kill
/// it by dropping it), the bound port, the bearer token, and a background
/// handle that accumulates every stderr line so a test can assert on log
/// output without racing the daemon for it.
fn spawn_daemon(
    extra_env: &[(&str, &str)],
) -> (
    DaemonGuard,
    u16,
    String,
    std::sync::Arc<std::sync::Mutex<String>>,
) {
    let home = tempfile::Builder::new()
        .prefix("a24-comm-smoke")
        .tempdir_in("/tmp")
        .unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    // `env_clear` is the point: no `A24_HYPHAE_BIN`/`A24_SPEAKER_BIN` unless
    // `extra_env` adds them back, the same shape a real operator who has
    // never set up comm would run in.
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path());
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let mut daemon = cmd
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    let stdout = daemon.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });

    let stderr = daemon.stderr.take().unwrap();
    let stderr_log = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let stderr_log_writer = stderr_log.clone();
    std::thread::spawn(move || {
        let reader = BufReader::new(stderr);
        for line in reader.lines().map_while(Result::ok) {
            if let Ok(mut buf) = stderr_log_writer.lock() {
                buf.push_str(&line);
                buf.push('\n');
            }
        }
    });

    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line is json");
    let port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
    let token = ready["token"].as_str().unwrap().to_owned();

    (
        DaemonGuard {
            child: daemon,
            _home: home,
        },
        port,
        token,
        stderr_log,
    )
}

#[test]
fn comm_routes_are_mounted_and_report_not_configured_without_a_binary() {
    let (_daemon, port, token, _stderr) = spawn_daemon(&[]);

    // The daemon itself must be healthy — comm being unconfigured must not
    // take the rest of the daemon down with it.
    let (status, _) = get(port, &token, "/api/v1/health");
    assert_eq!(status, 200, "the daemon must be healthy regardless of comm");

    let (status, body) = get(port, &token, "/api/v1/comm/identity");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("not_configured"), "{body}");
}

/// `A24_COMM_PASSWORD_STORE=memory`: the daemon stays healthy, comm still
/// mounts (routes past the backend, even though it's `not_configured` here
/// for the unrelated reason that no Hyphae binary was named), and the
/// startup warn about the in-memory, non-persistent password store is
/// printed. This does not need to inspect the real keychain to prove memory
/// mode never touches it — `comm_routes::select_password_store` never
/// constructs a `KeyringPasswordStore` on this branch at all (see its unit
/// tests in `comm_routes.rs`).
#[test]
fn memory_password_store_warns_and_still_mounts_comm() {
    let (_daemon, port, token, stderr) = spawn_daemon(&[("A24_COMM_PASSWORD_STORE", "memory")]);

    let (status, _) = get(port, &token, "/api/v1/health");
    assert_eq!(status, 200, "the daemon must be healthy under memory mode");

    let (status, body) = get(port, &token, "/api/v1/comm/identity");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("not_configured"), "{body}");

    // Give the stderr-draining thread a moment to catch up with startup.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let log = stderr.lock().unwrap().clone();
        if log.contains("A24_COMM_PASSWORD_STORE=memory") {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "no memory-password-store warning in stderr within 10s; log so far:\n{log}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// An unrecognized `A24_COMM_PASSWORD_STORE` value must not be silently
/// treated as `keyring` or `memory`: the daemon stays healthy, and comm
/// reports a configuration error carrying the bad value as its reason.
#[test]
fn invalid_password_store_value_reports_a_configuration_error() {
    let (_daemon, port, token, _stderr) =
        spawn_daemon(&[("A24_COMM_PASSWORD_STORE", "not-a-real-backend")]);

    let (status, _) = get(port, &token, "/api/v1/health");
    assert_eq!(
        status, 200,
        "the daemon must be healthy even with a bad password-store value"
    );

    let (status, body) = get(port, &token, "/api/v1/comm/identity");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("not_configured"), "{body}");
    assert!(
        body.contains("not-a-real-backend"),
        "the error should name the offending value: {body}"
    );
}
