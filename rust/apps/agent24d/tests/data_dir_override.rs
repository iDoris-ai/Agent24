//! MAINT-03 — black-box proof that `AGENT24_DATA_DIR` relocates the whole
//! data root of a real (non-ephemeral) `agent24d serve`: `daemon.json` and
//! `agent24.db` land directly under the override and `$HOME/.agent24` is
//! never created; with the variable unset the daemon still uses
//! `$HOME/.agent24` exactly as before. Harness shape copied (trimmed) from
//! `a3_2b_attach_blackbox.rs`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

struct Running(Child);

impl Drop for Running {
    fn drop(&mut self) {
        #[allow(clippy::cast_possible_wrap)]
        if let Some(pid) = rustix::process::Pid::from_raw(self.0.id() as i32)
            && rustix::process::kill_process(pid, rustix::process::Signal::Term).is_ok()
        {
            let by = Instant::now() + Duration::from_secs(10);
            while self.0.try_wait().is_ok_and(|s| s.is_none()) && Instant::now() < by {
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn start(home: &Path, data_dir: Option<&Path>) -> Running {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home);
    if let Some(dir) = data_dir {
        cmd.env("AGENT24_DATA_DIR", dir);
    }
    let mut child = cmd
        .args(["serve", "--port", "0"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let run = Running(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let ready = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("no ready line within 30s");
    let _: serde_json::Value = serde_json::from_str(&ready).expect("the ready line");
    run
}

/// `daemon.json` is written right after the ready line — bounded poll.
fn wait_for(path: &Path) {
    let by = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < by, "{} never appeared", path.display());
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn data_dir_override_holds_daemon_json_and_db() {
    let home = tempfile::tempdir().unwrap();
    let data = tempfile::tempdir().unwrap();
    let run = start(home.path(), Some(data.path()));
    wait_for(&data.path().join("daemon.json"));
    assert!(
        data.path().join("agent24.db").exists(),
        "agent24.db not under AGENT24_DATA_DIR"
    );
    assert!(
        !data.path().join(".agent24").exists(),
        "override must be used as-is, not joined with .agent24"
    );
    assert!(
        !home.path().join(".agent24").exists(),
        "$HOME/.agent24 must stay untouched when AGENT24_DATA_DIR is set"
    );
    drop(run);
}

#[test]
fn without_override_the_daemon_still_uses_home_dot_agent24() {
    let home = tempfile::tempdir().unwrap();
    let run = start(home.path(), None);
    let dir = home.path().join(".agent24");
    wait_for(&dir.join("daemon.json"));
    assert!(
        dir.join("agent24.db").exists(),
        "agent24.db not under $HOME/.agent24"
    );
    drop(run);
}
