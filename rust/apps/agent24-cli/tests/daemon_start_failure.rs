//! `agent24 daemon start` says why a daemon would not start (SPEC-002 §4's
//! `failed` line), at once; it waits for a concurrent winner only when the
//! daemon said `already_running`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn tmp_home() -> tempfile::TempDir {
    let home = tempfile::Builder::new()
        .prefix("a24start")
        .tempdir_in("/tmp")
        .unwrap();
    std::fs::create_dir(home.path().join("tmp")).unwrap();
    home
}

fn start(home: &Path, daemon_bin: &Path, env: &[(&str, &str)]) -> (std::process::Output, Duration) {
    let began = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_agent24"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("TMPDIR", home.join("tmp"))
        .env("AGENT24D_BIN", daemon_bin)
        .envs(env.iter().copied())
        .args(["daemon", "start"])
        .output()
        .unwrap();
    (out, began.elapsed())
}

fn real_daemon() -> PathBuf {
    Path::new(env!("CARGO_BIN_EXE_agent24")).with_file_name("agent24d")
}

#[test]
fn a_bad_setting_is_named_at_once() {
    let home = tmp_home();
    let (out, took) = start(
        home.path(),
        &real_daemon(),
        &[("A24_MODULE_GRANT_MAX_DAYS", "91")],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("agent24d did not start"), "{stderr}");
    assert!(stderr.contains("A24_MODULE_GRANT_MAX_DAYS"), "{stderr}");
    // No concurrent start to wait for: not the 3 s poll for a winner.
    assert!(took < Duration::from_secs(2), "took {took:?}");
}

#[test]
fn already_running_waits_for_the_winner_before_saying_so() {
    let home = tmp_home();
    let fake = home.path().join("fake-agent24d.sh");
    std::fs::write(
        &fake,
        "#!/bin/sh\nprintf '%s\\n' '{\"type\":\"failed\",\"code\":\"already_running\",\
         \"error\":\"another agent24d is already running (singleton lock held)\"}'\nexit 1\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (out, took) = start(home.path(), &fake, &[]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "{stderr}");
    assert!(stderr.contains("already running"), "{stderr}");
    // No winner ever shows up here, so the poll for one runs out first.
    assert!(took >= Duration::from_millis(2500), "took {took:?}");
}
