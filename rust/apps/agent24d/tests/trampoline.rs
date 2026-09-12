//! The daemon binary is also every module's trampoline (ME3-SUP slice 1): a
//! module is started through it so that its fds are flagged close-on-exec in
//! the child, before the module's program runs. That only happens if `main`
//! hands over to `run_as_trampoline_if_asked` before anything else — which is
//! one line, easy to lose, and invisible when lost: modules would still start,
//! just without the fd isolation. This runs the real binary to pin it, with an
//! fd the test holds WITHOUT close-on-exec, which the binary inherits and must
//! keep from the module.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const PROBE: &str = r#"import os
def is_open(fd):
    try:
        os.fstat(fd)
        return True
    except OSError:
        return False
fds = [fd for fd in range(0, 1024) if is_open(fd)]
open(os.environ["OUT"], "w").write(" ".join(map(str, fds)) + "\n" + "\n".join(os.environ))
"#;

fn run(env: &[(&str, &str)], args: &[&str]) -> (std::process::ExitStatus, String) {
    let home = tempfile::tempdir().unwrap();
    let out = home.path().join("out");
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_agent24d"));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        // Should the hand-over be missing, the binary starts as a daemon: keep
        // anything it writes inside this directory, and stop it below.
        .env("HOME", home.path())
        .env("OUT", &out)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    for (k, v) in env {
        cmd.env(k, v);
    }
    let mut child = cmd.spawn().expect("the daemon binary");
    let deadline = Instant::now() + Duration::from_secs(20);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the binary ran as a daemon instead of becoming the module program");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    (status, std::fs::read_to_string(&out).unwrap_or_default())
}

#[test]
fn the_daemon_binary_becomes_the_module_and_keeps_its_own_fds_from_it() {
    // An fd of this test's, dup'd high and stripped of close-on-exec: the
    // binary inherits it, as a daemon inherits an fd from a shell.
    let file = std::fs::File::open("/dev/null").unwrap();
    let high = rustix::io::fcntl_dupfd_cloexec(&file, 700).unwrap();
    rustix::io::fcntl_setfd(&high, rustix::io::FdFlags::empty()).unwrap();

    let (status, out) = run(
        &[("A24_TRAMPOLINE", "1")],
        &[
            "--a24-exec-module",
            "/usr/bin/env",
            "python3",
            "-I",
            "-S",
            "-c",
            PROBE,
        ],
    );
    assert!(status.success(), "{status:?}");
    let mut lines = out.lines();
    // stdin, stdout, stderr — and not the inherited fd.
    assert_eq!(lines.next(), Some("0 1 2"), "{out}");
    // The trampoline's marker did not follow it.
    assert!(!lines.any(|v| v == "A24_TRAMPOLINE"), "{out}");
    drop(high);
}

/// Half the activation is an ordinary start: the variable without the argv
/// marker must leave the binary doing what it was asked — here, printing its
/// version — not exec anything.
#[test]
fn the_variable_alone_does_not_make_the_daemon_a_trampoline() {
    let out = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("A24_TRAMPOLINE", "1")
        .arg("--version")
        .output()
        .expect("the daemon binary");
    assert!(out.status.success(), "{:?}", out.status);
    assert!(
        String::from_utf8_lossy(&out.stdout).starts_with("agent24d "),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
}
