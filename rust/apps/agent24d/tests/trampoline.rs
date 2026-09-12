//! The daemon binary is also every module's trampoline (ME3-SUP slice 1): a
//! module is started through it so that its fds are flagged close-on-exec in
//! the child, before the module's program runs. That only happens if `main`
//! hands over to `run_as_trampoline_if_asked` before anything else — which is
//! one line, easy to lose, and invisible when lost: modules would still start,
//! just without the fd isolation. This runs the real binary to pin it.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[test]
fn the_daemon_binary_becomes_the_module_program_when_started_as_a_trampoline() {
    let home = tempfile::tempdir().unwrap();
    let out = home.path().join("out");
    let mut child = Command::new(env!("CARGO_BIN_EXE_agent24d"))
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        // Should the hand-over be missing, the binary starts as a daemon: keep
        // anything it writes inside this directory, and stop it below.
        .env("HOME", home.path())
        .env("A24_TRAMPOLINE_PROGRAM", "/bin/sh")
        .env(
            "A24_TRAMPOLINE_ARGS",
            format!(r#"["-c", "env > '{}'"]"#, out.display()),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the daemon binary");

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

    assert!(status.success(), "{status:?}");
    let env = std::fs::read_to_string(&out).expect("the module program ran");
    // It ran, and the trampoline's own variables did not follow it.
    assert!(env.contains("PATH=/usr/bin:/bin"), "{env}");
    assert!(!env.contains("A24_TRAMPOLINE_"), "{env}");
}
