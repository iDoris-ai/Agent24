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
use std::process::{Command, Stdio};
use std::time::Duration;

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

#[test]
fn comm_routes_are_mounted_and_report_not_configured_without_a_binary() {
    let home = tempfile::Builder::new()
        .prefix("a24-comm-smoke")
        .tempdir_in("/tmp")
        .unwrap();
    // `env_clear` is the point: no `A24_HYPHAE_BIN`/`A24_SPEAKER_BIN`, the
    // same shape a real operator who has never set up comm would run in.
    let mut daemon = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home.path())
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
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let ready: serde_json::Value = serde_json::from_str(&ready).expect("the ready line is json");
    let port = u16::try_from(ready["port"].as_u64().unwrap()).unwrap();
    let token = ready["token"].as_str().unwrap().to_owned();

    // The daemon itself must be healthy — comm being unconfigured must not
    // take the rest of the daemon down with it.
    let (status, _) = get(port, &token, "/api/v1/health");
    assert_eq!(status, 200, "the daemon must be healthy regardless of comm");

    let (status, body) = get(port, &token, "/api/v1/comm/identity");
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("not_configured"), "{body}");

    let _ = daemon.kill();
    let _ = daemon.wait();
}
