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

/// Same `identity`/`relay` answers as [`DAEMON_SCRIPT`], but the `daemon`
/// leader forks a helper that **ignores SIGTERM** and keeps touching a
/// marker file, then itself `exec`s into `sleep 9999` and stays up — for
/// proving a `stop`/`shutdown` cleans up the whole process group, not just
/// the leader (PR #626 review, High #1: reaping the leader before every
/// real signal has gone out could free its pid for reuse and let the final
/// SIGKILL miss this group entirely).
const DAEMON_SCRIPT_HELPER_STAYS: &str = r#"#!/bin/sh
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
    ( trap '' TERM ; while : ; do touch "$HOME/.hyphae/helper-alive" ; sleep 0.05 ; done ) &
    exec sleep 9999
    ;;
esac
"#;

/// Same helper as [`DAEMON_SCRIPT_HELPER_STAYS`], but the leader does NOT
/// stay up: it exits on its own (not killed by us) a third of a second in —
/// for proving a descendant is cleaned up on a NATURAL leader exit too, not
/// only on an explicit `stop` (PR #626 review, Medium #3: the previous
/// version reaped the leader as the very mechanism used to detect this exit
/// and went straight to backoff, leaving the helper unsupervised).
///
/// Exits 1 (`gave_up`, never retried, COMM-HYPHAE.md §6.1), deliberately:
/// an exit code that DID retry would spawn a SECOND generation — with its
/// own helper touching the exact same marker file — within roughly
/// `grace + BASE_BACKOFF` of the first, which raced the test's own
/// "did it stay dead" check against a brand new, unrelated helper.
const DAEMON_SCRIPT_HELPER_THEN_EXIT: &str = r#"#!/bin/sh
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
    ( trap '' TERM ; while : ; do touch "$HOME/.hyphae/helper-alive" ; sleep 0.05 ; done ) &
    sleep 0.3
    exit 1
    ;;
esac
"#;

/// Same `identity`/`relay` answers again; the `daemon` leader sleeps for a
/// third of a second — comfortably longer than a shortened `ready_after` —
/// and then exits with a code that is neither 1 nor 3, for proving an exit
/// during `starting` is classified exactly like one during `running`, and
/// that `running` is never reported first (PR #626 review, Medium #6).
const DAEMON_SCRIPT_SLOW_DEATH: &str = r#"#!/bin/sh
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
    sleep 0.3
    exit 7
    ;;
esac
"#;

/// Short enough that `wait_until(.., Duration::from_secs(2))` below still has
/// margin after the `starting` -> `running` promotion (COMM-HYPHAE.md §6.1's
/// real 3s would blow every existing timeout in this file).
const TEST_READY_AFTER: Duration = Duration::from_millis(150);
/// Short SIGTERM-to-SIGKILL grace for these fixtures: none of them ignore
/// TERM, so this only bounds how long a stop/shutdown call can take.
const TEST_GRACE: Duration = Duration::from_millis(500);

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
    make_ctx_with_script(dir, DAEMON_SCRIPT).await
}

async fn make_ctx_with_script(dir: &Path, script: &str) -> DaemonCtx {
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
    let bin = install(dir, script).await;
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
        autostart_path: dir.join("daemon-autostart.json"),
        grace: TEST_GRACE,
        ready_after: TEST_READY_AFTER,
    }
}

async fn read_pid(path: &Path) -> u32 {
    let bytes = tokio::fs::read(path).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    value["pid"].as_u64().unwrap() as u32
}

/// The helper fixtures above stop touching their marker file — measured as
/// "the mtime stops advancing", since the file itself remains. Same shape as
/// `agent24-os-proto`'s `supervise.rs` test helper of the same purpose.
async fn marker_stopped_advancing(path: &Path) -> bool {
    tokio::time::sleep(Duration::from_millis(150)).await;
    let settle = std::time::SystemTime::now();
    tokio::time::sleep(Duration::from_millis(600)).await;
    !std::fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|t| t > settle)
        .unwrap_or(true)
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
        autostart_path: dir.join("daemon-autostart.json"),
        grace: TEST_GRACE,
        ready_after: TEST_READY_AFTER,
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
    // COMM-HYPHAE.md §6.1: the restart re-enters `starting` first, same as
    // any other spawn; it only reaches `running` after `ready_after`.
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);
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
    // Must match `ps_lstart`'s own normalization (PR #626 review, blocking
    // Medium: `env_clear` + `LC_ALL=C`/`TZ=UTC`) — otherwise this test's own
    // locale/timezone would just as easily fail to match what `reap_orphan`
    // computes, for the exact same reason the bug existed in the first
    // place. `daemon::tests::ps_lstart_marker_is_stable_across_different_caller_environments`
    // covers the normalization itself; this test only needs a marker that
    // genuinely matches what the real implementation would record.
    let out = std::process::Command::new("ps")
        .env_clear()
        .env("LC_ALL", "C")
        .env("TZ", "UTC")
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

// ---------------------------------------------------------------------
// PR #626 review, High #1: stop/shutdown must clean up the WHOLE group,
// never reaping the leader before every real signal has gone out.
// ---------------------------------------------------------------------

#[tokio::test]
async fn stop_kills_a_helper_the_leader_left_behind_in_its_group() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx_with_script(tmp.path(), DAEMON_SCRIPT_HELPER_STAYS).await;
    let marker = ctx.home.join(".hyphae").join("helper-alive");
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(
        wait_until(|| marker.exists(), Duration::from_secs(2)).await,
        "the fixture's helper never started"
    );

    sup.stop().await;
    assert_eq!(sup.status().state, "stopped");
    assert!(
        marker_stopped_advancing(&marker).await,
        "a helper that ignores SIGTERM must not outlive `stop`: reaping the leader before \
         every real signal has gone out could let its pid be reused and the final SIGKILL \
         miss this group entirely (PR #626 review, High #1)"
    );
}

#[tokio::test]
async fn shutdown_kills_a_helper_the_leader_left_behind_in_its_group() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx_with_script(tmp.path(), DAEMON_SCRIPT_HELPER_STAYS).await;
    let marker = ctx.home.join(".hyphae").join("helper-alive");
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(
        wait_until(|| marker.exists(), Duration::from_secs(2)).await,
        "the fixture's helper never started"
    );

    let outcome = sup.shutdown().await;
    assert!(outcome.had_process);
    assert!(
        marker_stopped_advancing(&marker).await,
        "agent24d's SHUT-1b shutdown path must clean up a straggling helper too, same as a \
         plain `stop` (PR #626 review, High #1)"
    );
}

// ---------------------------------------------------------------------
// PR #626 review, Medium #3: a NATURAL leader exit must clean up whatever
// the leader left running in its group before the restart loop proceeds.
// ---------------------------------------------------------------------

#[tokio::test]
async fn a_natural_leader_exit_still_cleans_up_a_descendant_it_left_running() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx_with_script(tmp.path(), DAEMON_SCRIPT_HELPER_THEN_EXIT).await;
    let pid_path = ctx.pid_path.clone();
    let marker = ctx.home.join(".hyphae").join("helper-alive");
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(
        wait_until(|| marker.exists(), Duration::from_secs(2)).await,
        "the fixture's helper never started"
    );

    // The leader exits ON ITS OWN, ~0.3s in — nobody here signals it. Exit
    // code 1 is never retried (COMM-HYPHAE.md §6.1), so no second
    // generation's helper can start touching the same marker while this
    // test is still checking the first one (see the fixture's own comment).
    assert!(
        wait_until(|| sup.status().state == "gave_up", Duration::from_secs(3)).await,
        "the supervisor never noticed the leader's natural exit"
    );
    assert!(
        marker_stopped_advancing(&marker).await,
        "a descendant the leader left running must not survive its NATURAL exit either — \
         left unsupervised, it collides with the next restart's generation (PR #626 review, \
         Medium #3)"
    );
    assert!(
        !pid_path.exists(),
        "the pid file must be cleared once the leader's exit (and group cleanup) is handled"
    );
}

// ---------------------------------------------------------------------
// PR #626 review, Medium #6: `starting` must last at least `ready_after`,
// and an exit before that must never be reported as `running`.
// ---------------------------------------------------------------------

#[tokio::test]
async fn an_exit_before_ready_after_is_classified_but_never_reported_as_running() {
    let tmp = tempfile::tempdir().unwrap();
    let mut ctx = make_ctx_with_script(tmp.path(), DAEMON_SCRIPT_SLOW_DEATH).await;
    // Comfortably longer than the fixture's 0.3s sleep: the exit must land
    // well inside `starting`.
    ctx.ready_after = Duration::from_millis(1500);
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert_eq!(
        sup.status().state,
        "starting",
        "a freshly spawned daemon must answer `starting`, not `running`, before it has \
         survived `ready_after`"
    );

    let mut saw_running = false;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < deadline {
        match sup.status().state {
            "running" => saw_running = true,
            "backoff" | "gave_up" => break,
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(
        !saw_running,
        "an exit before `ready_after` must never have been reported as `running` in between"
    );
    assert_eq!(
        sup.status().state,
        "backoff",
        "exit code 7 (neither 1 nor 3) during `starting` must be classified exactly like \
         one during `running`"
    );
}

#[tokio::test]
async fn surviving_ready_after_promotes_starting_to_running() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert_eq!(sup.status().state, "starting");
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);
}

// ---------------------------------------------------------------------
// PR #626 review, Medium #4: a manual `start` resets the breaker.
// ---------------------------------------------------------------------

#[tokio::test]
async fn manual_start_resets_the_breaker_after_it_trips() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    let pid_path = ctx.pid_path.clone();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);

    for failures in 1..=4u32 {
        let pid = read_pid(&pid_path).await;
        kill_minus_9(pid);
        // Wait for `backoff` FIRST: `running` is also this loop's starting
        // state, so waiting only for `running | gave_up` could return
        // immediately on the stale pre-kill status before the supervisor
        // has even noticed the kill.
        assert!(
            wait_until(
                || sup.status().state == "backoff" && sup.status().consecutive_failures == failures,
                Duration::from_secs(2)
            )
            .await
        );
        assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(10)).await);
    }
    let pid = read_pid(&pid_path).await;
    kill_minus_9(pid);
    assert!(wait_until(|| sup.status().state == "gave_up", Duration::from_secs(2)).await);
    assert_eq!(sup.status().consecutive_failures, 5);

    // A manual start after the breaker trips.
    sup.start().await.unwrap();
    assert!(wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await);
    assert_eq!(
        sup.status().consecutive_failures,
        0,
        "a manual start must reset the breaker, not inherit a count tripped by an earlier, \
         unrelated crash loop (PR #626 review, Medium #4)"
    );

    // One failure after the reset must back off ~500ms like a fresh policy
    // would, not immediately re-trip the breaker as it would if the count
    // from before the reset had carried over (it was already at 5).
    let pid = read_pid(&pid_path).await;
    let since_failure = std::time::Instant::now();
    kill_minus_9(pid);
    assert!(wait_until(|| sup.status().state == "backoff", Duration::from_secs(2)).await);
    assert_eq!(sup.status().consecutive_failures, 1);
    assert!(
        wait_until(|| sup.status().state == "running", Duration::from_secs(2)).await,
        "expected a restart after the single post-reset failure"
    );
    assert!(
        since_failure.elapsed() >= Duration::from_millis(350),
        "{:?}",
        since_failure.elapsed()
    );
}

// ---------------------------------------------------------------------
// PR #626 review, Medium #5: `daemon.autostart` persistence (COMM-HYPHAE.md
// §6.2). `comm_routes.rs` (agent24d) is what actually reads this file again
// on the NEXT `agent24d` launch and decides whether to auto-start; this
// crate owns writing it on manual start/stop, which is what is tested here.
// ---------------------------------------------------------------------

#[tokio::test]
async fn manual_start_persists_autostart_true_and_stop_persists_false() {
    let tmp = tempfile::tempdir().unwrap();
    let ctx = make_ctx(tmp.path()).await;
    let autostart_path = ctx.autostart_path.clone();
    let sup = HyphaeDaemonSupervisor::spawn(ctx);

    assert!(
        !agent24_comm::read_autostart(&autostart_path).await,
        "nothing has started yet; autostart must default to false"
    );

    sup.start().await.unwrap();
    assert!(
        agent24_comm::read_autostart(&autostart_path).await,
        "a successful manual start must persist autostart=true (COMM-HYPHAE.md §6.2)"
    );

    sup.stop().await;
    assert!(
        !agent24_comm::read_autostart(&autostart_path).await,
        "a manual stop must persist autostart=false (COMM-HYPHAE.md §6.2)"
    );
}
