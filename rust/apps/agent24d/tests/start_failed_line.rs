//! SPEC-002 §4: a start that fails before its ready line says why on stdout,
//! `{"type":"failed","code":…,"error":…}`. A second daemon on the same data
//! dir says `already_running`, so a launcher can look for the winner instead
//! of reporting an error.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn daemon(home: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    command
}

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn a_second_daemon_says_it_is_already_running() {
    let home = tempfile::tempdir().unwrap();
    let mut first = Running(daemon(home.path()).spawn().unwrap());
    let mut ready = String::new();
    BufReader::new(first.0.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    let ready: serde_json::Value = serde_json::from_str(&ready).unwrap();
    assert_eq!(ready["type"], "ready", "{ready}");

    let mut second = daemon(home.path()).spawn().unwrap();
    let by = Instant::now() + Duration::from_secs(30);
    while second.try_wait().unwrap().is_none() {
        if Instant::now() > by {
            let _ = second.kill();
            let _ = second.wait();
            panic!("a second daemon started on a held singleton lock");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let out = second.wait_with_output().unwrap();
    assert!(!out.status.success());
    let stdout = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    let failed: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(failed["type"], "failed", "{failed}");
    assert_eq!(failed["code"], "already_running", "{failed}");
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("already running"),
        "{failed}"
    );
}
