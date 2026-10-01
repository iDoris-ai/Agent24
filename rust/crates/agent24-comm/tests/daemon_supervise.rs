//! COMM-4a: daemon supervision, proven against a fake "hyphae" binary (a
//! shell script) rather than the real one — same pattern as
//! `tests/router_lifecycle.rs`. The real-binary end-to-end path lives in
//! `daemon_joint.rs`, `#[ignore]`d.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::os::unix::process::CommandExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use agent24_comm::binary::sha256_of;
use agent24_comm::{
    Account, DaemonCtx, DaemonStartError, HyphaeDaemonSupervisor, HyphaeRunner,
    MemoryPasswordStore, OrphanOutcome, Password, PasswordStore, VerifiedBinary, reap_orphan,
};

/// `identity list` / `relay list` answer fixed, valid data; `daemon ...`
/// reads (and discards) the password on stdin, then either exits
/// immediately with the code in `$HOME/.hyphae/exit_code` (if a test wrote
/// one) or `exec`s into `sleep 9999` — becoming a single long-running
/// process with no child of its own, so a plain `kill -9 <pid>` (no process
/// group needed) leaves nothing behind.
const DAEMON_SCRIPT: &str = r#"#!/bin/sh
case "$1" in
  identity)
    echo '{"ok":true,"data":[{"nickname":"alice","npub":"npub1x","default":true,"encrypted":true}]}'
    exit 0
    ;;
  relay)
    echo '{"ok":true,"data":{"relays":["wss://relay.example"],"source":"config"}}'
    exit 0
    ;;
  daemon)
    cat > /dev/null
    if [ -f "$HOME/.hyphae/exit_code" ]; then
      exit "$(cat "$HOME/.hyphae/exit_code")"
    fi
    exec sleep 9999
    ;;
esac
"#;

async fn install(dir: &Path, script: &str) -> VerifiedBinary {
    let source = dir.join("hyphae-fake.sh");
    tokio::fs::write(&source, script).await.unwrap();
    let bytes = tokio::fs::read(&source).await.unwrap();
    let expected = sha256_of(&bytes);
    VerifiedBinary::install(&source, expected, &dir.join("bin"))
        .await
        .unwrap()
}

async fn make_ctx(dir: &Path) -> DaemonCtx {
    let home = dir.join("hyphae-home");
    tokio::fs::create_dir_all(home.join(".hyphae"))
        .await
        .unwrap();
    tokio::fs::write(
        home.join(".hyphae").join("keystore.json"),
        br#"{"salt":"dGVzdHNhbHQ="}"#,
    )
    .await
    .unwrap();
    let bin = install(dir, DAEMON_SCRIPT).await;
    let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
    let store = Arc::new(MemoryPasswordStore::new());
    store
        .put(
            &Account::from_salt("dGVzdHNhbHQ="),
            &Password::new(b"testpass".to_vec()).unwrap(),
        )
        .await
        .unwrap();
    DaemonCtx {
        runner,
        password_store: store,
        home,
        pid_path: dir.join("hyphae-daemon.pid"),
        log_path: dir.join("logs").join("hyphae-daemon.log"),
    }
}

async fn read_pid(path: &Path) -> u32 {
    let bytes = tokio::fs::read(path).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["pid"].as_u64().unwrap() as u32
}

async fn wait_until<F: FnMut() -> bool>(mut f: F, timeout: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if f() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn kill_minus_9(pid: u32) {
    let _ = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status();
}

fn is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

// ---------------------------------------------------------------------
// exit-code classification (COMM-HYPHAE.md §6.1: only the exit code)
// ---------------------------------------------------------------------

#[tokio::test]
async fn exit_code_3_is_locked_and_never_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    tokio::fs::write(ctx.home.join(".hyphae").join("exit_code"), b"3")
        .await
        .unwrap();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "locked", Duration::from_secs(2)).await);
    assert_eq!(sup.status().reason.as_deref(), Some("password_rejected"));
    assert_eq!(
        sup.status().consecutive_failures,
        0,
        "a locked exit must not feed the restart policy"
    );

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(sup.status().state, "locked", "no retry after exit code 3");
    assert_eq!(sup.status().generation, 1);
}

#[tokio::test]
async fn exit_code_1_is_gave_up_and_never_retried() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    tokio::fs::write(ctx.home.join(".hyphae").join("exit_code"), b"1")
        .await
        .unwrap();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "gave_up", Duration::from_secs(2)).await);
    assert_eq!(sup.status().consecutive_failures, 0);

    tokio::time::sleep(Duration::from_millis(700)).await;
    assert_eq!(sup.status().state, "gave_up", "no retry after exit code 1");
}

#[tokio::test]
async fn relay_not_configured_is_rejected_before_any_spawn() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    let home = dir.join("hyphae-home");
    tokio::fs::create_dir_all(home.join(".hyphae"))
        .await
        .unwrap();
    tokio::fs::write(
        home.join(".hyphae").join("keystore.json"),
        br#"{"salt":"dGVzdHNhbHQ="}"#,
    )
    .await
    .unwrap();
    let script = r#"#!/bin/sh
case "$1" in
  identity) echo '{"ok":true,"data":[{"nickname":"a","default":true}]}'; exit 0 ;;
  relay) echo '{"ok":true,"data":{"relays":[],"source":"default"}}'; exit 0 ;;
  daemon) echo 'should never run' >&2; exit 4 ;;
esac
"#;
    let bin = install(dir, script).await;
    let runner = Arc::new(HyphaeRunner::new(bin, home.clone(), Duration::from_secs(5)));
    let ctx = DaemonCtx {
        runner,
        password_store: Arc::new(MemoryPasswordStore::new()),
        home,
        pid_path: dir.join("hyphae-daemon.pid"),
        log_path: dir.join("logs").join("hyphae-daemon.log"),
    };
    let pid_path = dir.join("hyphae-daemon.pid");
    let sup = HyphaeDaemonSupervisor::spawn(ctx);
    let err = sup.start().await.unwrap_err();
    assert!(matches!(err, DaemonStartError::NotConfigured(_)), "{err:?}");
    assert_eq!(sup.status().state, "not_configured");
    assert!(
        !pid_path.exists(),
        "a rejected precondition must never spawn anything"
    );
}

// ---------------------------------------------------------------------
// config-change restart (COMM-HYPHAE.md §6.3)
// ---------------------------------------------------------------------

#[tokio::test]
async fn config_change_bumps_generation_without_touching_failures() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    let pid_path = ctx.pid_path.clone();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);
    assert_eq!(sup.status().generation, 1);
    let first_pid = read_pid(&pid_path).await;

    sup.on_config_changed().await;
    assert_eq!(
        sup.status().generation,
        2,
        "a config-change restart bumps generation"
    );
    assert_eq!(
        sup.status().consecutive_failures,
        0,
        "a config-change restart must not count as a failure"
    );
    assert_eq!(sup.status().state, "running");
    let second_pid = read_pid(&pid_path).await;
    assert_ne!(
        first_pid, second_pid,
        "the old process must actually have been replaced"
    );
    assert!(
        !is_alive(first_pid),
        "the old process must have been stopped, not leaked"
    );

    sup.stop().await;
    assert_eq!(sup.status().state, "stopped");
    assert!(!pid_path.exists());
    assert!(!is_alive(second_pid));
}

// ---------------------------------------------------------------------
// crash-restart backoff + breaker (COMM-HYPHAE.md §6.3)
// ---------------------------------------------------------------------

#[tokio::test]
async fn kill_minus_9_backs_off_then_trips_the_breaker_at_five() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    let pid_path = ctx.pid_path.clone();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);

    let mut since_last_restart = std::time::Instant::now();
    for failures in 1..=4u32 {
        let pid = read_pid(&pid_path).await;
        kill_minus_9(pid);
        assert!(
            wait_until(
                || sup.status().state == "backoff" && sup.status().consecutive_failures == failures,
                Duration::from_secs(2)
            )
            .await,
            "expected backoff with {failures} consecutive failures"
        );
        assert!(
            wait_until(
                || sup.status().state == "running"
                    && sup.status().generation == u64::from(failures) + 1,
                Duration::from_secs(10)
            )
            .await,
            "expected a restart after failure {failures}"
        );
        // 500ms * 2^(failures-1), with generous slack against scheduling jitter.
        let min_expected = Duration::from_millis(350 * (1u64 << (failures - 1)));
        let elapsed = since_last_restart.elapsed();
        assert!(
            elapsed >= min_expected,
            "restart {failures} came back too fast: {elapsed:?} < {min_expected:?}"
        );
        since_last_restart = std::time::Instant::now();
    }

    // The 5th failure trips the breaker: `gave_up`, and no further restart.
    let pid = read_pid(&pid_path).await;
    kill_minus_9(pid);
    assert!(
        wait_until(
            || sup.status().state == "gave_up" && sup.status().consecutive_failures == 5,
            Duration::from_secs(2)
        )
        .await
    );
    let generation_at_giveup = sup.status().generation;
    tokio::time::sleep(Duration::from_secs(2)).await;
    assert_eq!(
        sup.status().generation,
        generation_at_giveup,
        "no spawn after the breaker trips"
    );
    assert_eq!(sup.status().state, "gave_up");
}

// ---------------------------------------------------------------------
// shutdown (SHUT-1b integration point)
// ---------------------------------------------------------------------

#[tokio::test]
async fn shutdown_stops_the_whole_group_and_clears_the_pid_file() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    let pid_path = ctx.pid_path.clone();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);
    let pid = read_pid(&pid_path).await;

    let outcome = sup.shutdown().await;
    assert!(outcome.had_process);
    assert!(outcome.leader.is_some());
    assert!(!pid_path.exists());
    assert!(
        !is_alive(pid),
        "the daemon process must not survive agent24d's shutdown"
    );
}

// ---------------------------------------------------------------------
// orphan cleanup (COMM-HYPHAE.md §6.1 "孤儿识别"): pid alive AND start time
// unchanged, never by name.
// ---------------------------------------------------------------------

#[tokio::test]
async fn reap_orphan_reports_no_pid_file_when_none_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let outcome = reap_orphan(&tmp.path().join("missing.pid"), Duration::from_millis(200)).await;
    assert_eq!(outcome, OrphanOutcome::NoPidFile);
}

#[tokio::test]
async fn reap_orphan_removes_a_stale_pid_file() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_path = tmp.path().join("hyphae-daemon.pid");
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let pid = child.id();
    child.wait().unwrap(); // now definitely dead
    let body = serde_json::json!({"pid": pid, "pgid": pid, "start_marker": "whatever", "generation": 1u64, "bin_sha256": "x"});
    tokio::fs::write(&pid_path, serde_json::to_vec(&body).unwrap())
        .await
        .unwrap();

    let outcome = reap_orphan(&pid_path, Duration::from_millis(200)).await;
    assert_eq!(outcome, OrphanOutcome::Stale);
    assert!(!pid_path.exists());
}

#[tokio::test]
async fn reap_orphan_leaves_a_live_pid_alone_when_its_start_time_does_not_match() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_path = tmp.path().join("hyphae-daemon.pid");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exec sleep 9999")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    let body = serde_json::json!({"pid": pid, "pgid": pid, "start_marker": "definitely-not-the-real-start-time", "generation": 1u64, "bin_sha256": "x"});
    tokio::fs::write(&pid_path, serde_json::to_vec(&body).unwrap())
        .await
        .unwrap();

    let outcome = reap_orphan(&pid_path, Duration::from_millis(200)).await;
    assert_eq!(
        outcome,
        OrphanOutcome::LeftAlone,
        "a mismatched start time must never be killed"
    );
    assert!(is_alive(pid));

    let _ = child.kill();
    let _ = child.wait();
}

#[tokio::test]
async fn reap_orphan_kills_a_live_pid_whose_start_time_matches() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_path = tmp.path().join("hyphae-daemon.pid");
    let mut child = std::process::Command::new("sh")
        .arg("-c")
        .arg("exec sleep 9999")
        .process_group(0)
        .spawn()
        .unwrap();
    let pid = child.id();
    tokio::time::sleep(Duration::from_millis(100)).await; // let `ps` observe it
    let out = std::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .unwrap();
    let marker = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let body = serde_json::json!({"pid": pid, "pgid": pid, "start_marker": marker, "generation": 1u64, "bin_sha256": "x"});
    tokio::fs::write(&pid_path, serde_json::to_vec(&body).unwrap())
        .await
        .unwrap();

    let outcome = reap_orphan(&pid_path, Duration::from_millis(500)).await;
    assert_eq!(outcome, OrphanOutcome::Killed);
    // `reap_orphan` is not this child's parent (the test is, via `std::process`,
    // not tokio's auto-reaping `Command`), so SIGKILL alone leaves a zombie
    // until the actual parent reaps it — do that before checking liveness.
    let _ = child.wait();
    assert!(!is_alive(pid));
    assert!(!pid_path.exists());
}
