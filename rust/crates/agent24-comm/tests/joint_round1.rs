//! Agent24 × Hyphae CLI joint round 1 (see `docs/comm/JOINT-ROUND1.md`).
//!
//! This is an interop harness, not a product-code test: it drives the real
//! Hyphae CLI binary from both sides of a conversation —
//! A via `agent24-comm`'s production `HyphaeRunner` (the embedded
//! `hyphae.lock.json`, no test override), B via a bare
//! `std::process::Command` standing in for an ordinary Hyphae CLI user —
//! against a locally spawned `hyphae-relay`, and records what actually
//! happens, including anywhere the real binary's behavior doesn't match
//! `docs/design/COMM-HYPHAE.md`'s claims.
//!
//! Requires two environment variables (and is `#[ignore]`d otherwise):
//! - `HYPHAE_JOINT_BIN`: path to a Hyphae CLI binary whose sha256 matches
//!   this crate's embedded `hyphae.lock.json` for the current platform. A
//!   mismatch here is a hard failure (`VerifiedBinary::install` rejects it)
//!   rather than a silent skip — the whole point of this harness is to
//!   exercise the production lock-verification path.
//! - `HYPHAE_JOINT_RELAY`: path to a `hyphae-relay` binary built from the
//!   same Hyphae source tree (its hash isn't lock-checked; COMM-HYPHAE.md
//!   §8.1 notes `hyphae-relay` is a joint-test tool, not a runtime
//!   dependency).
//!
//! Run with:
//! ```text
//! HYPHAE_JOINT_BIN=... HYPHAE_JOINT_RELAY=... \
//!   cargo test -p agent24-comm --test joint_round1 -- --ignored --nocapture
//! ```
//!
//! All state (both HOMEs, the relay's data dir, A's verified-binary install
//! dir) lives under one `tempfile::TempDir`, removed on drop; the real
//! `~/.hyphae` is never touched. Passwords are synthetic fixtures generated
//! for this run only, are never printed, and nsecs are never requested
//! (`identity export` is out of scope for this round).

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use agent24_comm::{
    Envelope, ExitClass, HyphaeLock, HyphaeRunner, Invocation, Password, VerifiedBinary,
    current_platform, parse_envelope,
};
use serde_json::Value;

const PASSWORD_A: &str = "joint-round1-a-synthetic-9f2c";
const PASSWORD_B: &str = "joint-round1-b-synthetic-7ae1";
const WRONG_PASSWORD: &str = "joint-round1-wrong-synthetic";

/// Picks a free TCP port on 127.0.0.1 by binding to port 0 and reading back
/// the OS-assigned port, then releasing the listener before the relay binds
/// it. Small race window, acceptable for a local, single-shot test.
fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local_addr").port()
}

fn wait_for_port(port: u16, timeout: Duration) {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!("relay did not start listening on 127.0.0.1:{port} within {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn spawn_relay(relay_bin: &Path, port: u16, data_dir: &Path) -> Child {
    Command::new(relay_bin)
        .args([
            "-listen",
            "127.0.0.1",
            "-port",
            &port.to_string(),
            "-data-dir",
            data_dir.to_str().expect("utf8 data dir"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn hyphae-relay")
}

/// Stops a child process by its specific pid (never by name/pattern) and
/// reaps it so it doesn't linger as a zombie.
fn stop_child(mut child: Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// B side: an ordinary Hyphae CLI user driving the binary directly with its
/// own throwaway `HOME`. Uses the crate's own `parse_envelope` so both sides
/// of this harness are checked against exactly the contract the production
/// runner enforces.
struct BSide {
    bin: PathBuf,
    home: PathBuf,
}

impl BSide {
    fn run(&self, args: &[&str], password: Option<&str>) -> (i32, Envelope) {
        let mut cmd = Command::new(&self.bin);
        cmd.args(args);
        if password.is_some() {
            cmd.arg("--password-stdin");
        }
        cmd.env("HOME", &self.home);
        cmd.env("HYPHAE_OUTPUT", "json");
        cmd.current_dir(&self.home);
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.stdin(if password.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        let mut child = cmd.spawn().expect("spawn hyphae (B side)");
        if let Some(password) = password {
            let mut stdin = child.stdin.take().expect("stdin pipe");
            stdin
                .write_all(password.as_bytes())
                .expect("write password to stdin");
            drop(stdin);
        }
        let output = child.wait_with_output().expect("wait for hyphae (B side)");
        let code = output
            .status
            .code()
            .expect("hyphae (B side) exited via signal, not a status code");
        let envelope = parse_envelope(code, &output.stdout, &output.stderr)
            .unwrap_or_else(|e| panic!("B side envelope parse failed for {args:?}: {e}"));
        (code, envelope)
    }
}

fn exit_code_of(envelope: &Envelope) -> i32 {
    match envelope {
        Envelope::Ok { .. } => 0,
        Envelope::Failed { exit, .. } => *exit as i32,
    }
}

fn classify(envelope: &Envelope) -> Option<(ExitClass, String)> {
    match envelope {
        Envelope::Failed { exit, error, .. } => Some((*exit, error.clone())),
        Envelope::Ok { .. } => None,
    }
}

fn envelope_data(envelope: &Envelope) -> Option<&Value> {
    match envelope {
        Envelope::Ok { data } => Some(data),
        Envelope::Failed { data, .. } => data.as_ref(),
    }
}

fn redact_args(args: &[&str], has_password: bool) -> String {
    let mut shown = args.join(" ");
    if has_password {
        shown.push_str(" --password-stdin <password redacted, synthetic test fixture>");
    }
    shown
}

/// Prints one step's command (password redacted), exit code, and envelope —
/// the raw material for `docs/comm/JOINT-ROUND1.md`. npubs are left as-is;
/// no nsec ever appears here.
fn log_step(who: &str, args: &[&str], has_password: bool, envelope: &Envelope) {
    println!("---- {who}: hyphae {}", redact_args(args, has_password));
    println!("     exit = {}", exit_code_of(envelope));
    match envelope {
        Envelope::Ok { data } => println!("     ok:true data = {data}"),
        Envelope::Failed {
            exit,
            error,
            message,
            data,
        } => {
            println!("     ok:false exit_class={exit:?} error={error} message={message}");
            if let Some(data) = data {
                println!("     data = {data}");
            }
        }
    }
}

async fn a_run(runner: &HyphaeRunner, args: &[&str], password: Option<&str>) -> Envelope {
    let inv = Invocation {
        args: args.iter().map(std::ffi::OsString::from).collect(),
        password: password.map(|p| Password::new(p.as_bytes().to_vec()).expect("valid password")),
        timeout: None,
    };
    runner.run(inv).await.unwrap_or_else(|e| {
        panic!("A side run failed to even produce an envelope for {args:?}: {e}")
    })
}

#[tokio::test]
#[ignore]
async fn joint_round1() {
    let Ok(bin_path) = std::env::var("HYPHAE_JOINT_BIN") else {
        eprintln!(
            "skipping joint_round1: HYPHAE_JOINT_BIN is not set (point it at a Hyphae binary \
             matching this crate's hyphae.lock.json for the current platform)"
        );
        return;
    };
    let Ok(relay_path) = std::env::var("HYPHAE_JOINT_RELAY") else {
        eprintln!(
            "skipping joint_round1: HYPHAE_JOINT_RELAY is not set (point it at a hyphae-relay binary)"
        );
        return;
    };

    let bin_path = PathBuf::from(&bin_path)
        .canonicalize()
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_BIN={bin_path:?} is not readable: {e}"));
    let relay_path = PathBuf::from(&relay_path)
        .canonicalize()
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_RELAY={relay_path:?} is not readable: {e}"));

    let tmp = tempfile::tempdir().expect("tempdir");
    let home_a = tmp.path().join("home-a");
    let home_b = tmp.path().join("home-b");
    let install_dir = tmp.path().join("install-a");
    let relay_data = tmp.path().join("relay-data");
    for dir in [&home_a, &home_b, &install_dir, &relay_data] {
        std::fs::create_dir_all(dir).expect("create tmp subdir");
    }

    // ---- A side: the production lock-verification path, no test override. ----
    let platform = current_platform();
    let lock = HyphaeLock::embedded().expect("embedded hyphae.lock.json parses");
    let expected = lock
        .expected_for(&platform)
        .unwrap_or_else(|e| panic!("no hyphae.lock.json entry for platform {platform}: {e}"));
    let verified = VerifiedBinary::install(&bin_path, expected, &install_dir)
        .await
        .unwrap_or_else(|e| panic!("HYPHAE_JOINT_BIN must match hyphae.lock.json: {e}"));
    println!(
        "baseline: platform={platform} sha256={} (matched hyphae.lock.json via the production path, no test-lock-override)",
        verified.sha256().to_hex()
    );
    let runner = HyphaeRunner::new(verified, home_a.clone(), Duration::from_secs(20));

    let b = BSide {
        bin: bin_path.clone(),
        home: home_b.clone(),
    };

    let port = free_port();
    let relay_child = spawn_relay(&relay_path, port, &relay_data);
    wait_for_port(port, Duration::from_secs(5));
    let relay_url = format!("ws://127.0.0.1:{port}");
    println!(
        "relay: listening on {relay_url}, data_dir={}",
        relay_data.display()
    );

    // ======================================================================
    // Step 1: identity create on both sides.
    // ======================================================================
    let envelope = a_run(
        &runner,
        &["identity", "create", "--nickname", "a"],
        Some(PASSWORD_A),
    )
    .await;
    log_step(
        "A",
        &["identity", "create", "--nickname", "a"],
        true,
        &envelope,
    );
    let Envelope::Ok { data: a_data } = &envelope else {
        panic!("A identity create failed: {envelope:?}");
    };
    assert_eq!(
        a_data.get("encrypted").and_then(Value::as_bool),
        Some(true),
        "A identity should report an encrypted keystore"
    );
    let npub_a = a_data
        .get("npub")
        .and_then(Value::as_str)
        .expect("A npub present")
        .to_string();

    let (code, envelope) = b.run(&["identity", "create", "--nickname", "b"], Some(PASSWORD_B));
    log_step(
        "B",
        &["identity", "create", "--nickname", "b"],
        true,
        &envelope,
    );
    assert_eq!(code, 0, "B identity create should succeed");
    let Envelope::Ok { data: b_data } = &envelope else {
        panic!("B identity create failed: {envelope:?}");
    };
    let npub_b = b_data
        .get("npub")
        .and_then(Value::as_str)
        .expect("B npub present")
        .to_string();

    println!("npub_a = {npub_a}");
    println!("npub_b = {npub_b}");

    // ======================================================================
    // Step 2: mutual contact add, relay set, relay list (source==config).
    // ======================================================================
    let envelope = a_run(
        &runner,
        &["contact", "add", "--nickname", "b", "--npub", &npub_b],
        None,
    )
    .await;
    log_step(
        "A",
        &["contact", "add", "--nickname", "b", "--npub", &npub_b],
        false,
        &envelope,
    );
    assert!(
        matches!(envelope, Envelope::Ok { .. }),
        "A contact add b should succeed"
    );

    let (code, envelope) = b.run(
        &["contact", "add", "--nickname", "a", "--npub", &npub_a],
        None,
    );
    log_step(
        "B",
        &["contact", "add", "--nickname", "a", "--npub", &npub_a],
        false,
        &envelope,
    );
    assert_eq!(code, 0, "B contact add a should succeed");

    let envelope = a_run(&runner, &["relay", "set", "--relay", &relay_url], None).await;
    log_step(
        "A",
        &["relay", "set", "--relay", &relay_url],
        false,
        &envelope,
    );
    assert!(
        matches!(envelope, Envelope::Ok { .. }),
        "A relay set should succeed"
    );

    let (code, envelope) = b.run(&["relay", "set", "--relay", &relay_url], None);
    log_step(
        "B",
        &["relay", "set", "--relay", &relay_url],
        false,
        &envelope,
    );
    assert_eq!(code, 0, "B relay set should succeed");

    let envelope = a_run(&runner, &["relay", "list"], None).await;
    log_step("A", &["relay", "list"], false, &envelope);
    let source = envelope_data(&envelope)
        .and_then(|d| d.get("source"))
        .and_then(Value::as_str);
    assert_eq!(
        source,
        Some("config"),
        "A relay source should be config, got {source:?}"
    );

    let (code, envelope) = b.run(&["relay", "list"], None);
    log_step("B", &["relay", "list"], false, &envelope);
    assert_eq!(code, 0);
    let source = envelope_data(&envelope)
        .and_then(|d| d.get("source"))
        .and_then(Value::as_str);
    assert_eq!(
        source,
        Some("config"),
        "B relay source should be config, got {source:?}"
    );

    // ======================================================================
    // Step 3: A -> B ("joint-1"), B reads it back.
    // ======================================================================
    let envelope = a_run(
        &runner,
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            &npub_b,
            "--content",
            "joint-1",
        ],
        Some(PASSWORD_A),
    )
    .await;
    log_step(
        "A",
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            &npub_b,
            "--content",
            "joint-1",
        ],
        true,
        &envelope,
    );
    let Envelope::Ok { data } = &envelope else {
        panic!("A send joint-1 failed: {envelope:?}");
    };
    let published_to = data
        .get("published_to")
        .and_then(Value::as_i64)
        .expect("published_to present");
    assert!(
        published_to >= 1,
        "expected published_to>=1, got {published_to}"
    );
    let event_id_1 = data
        .get("event_id")
        .and_then(Value::as_str)
        .expect("event_id present")
        .to_string();
    println!("event_id (joint-1) = {event_id_1}");

    // B's history inbox is pull-based, not push-based: check what it looks
    // like before any `agent inbox` pull.
    let (code, envelope) = b.run(&["history", "inbox", "--as", "b", "--limit", "10"], None);
    log_step(
        "B",
        &["history", "inbox", "--as", "b", "--limit", "10"],
        false,
        &envelope,
    );
    assert_eq!(code, 0);
    let before_pull_data = envelope_data(&envelope).cloned();
    let before_pull_empty = before_pull_data
        .as_ref()
        .and_then(Value::as_array)
        .is_some_and(|a| a.is_empty());
    assert!(
        before_pull_empty,
        "B's `history inbox` should be empty before any `agent inbox` pull (history is \
         populated by pulling, not pushed automatically), got data = {before_pull_data:?}"
    );
    println!("confirmed: B's history inbox is empty before pulling (pull-based, not push-based)");

    let (code, envelope) = b.run(&["agent", "inbox", "--as", "b"], Some(PASSWORD_B));
    log_step("B", &["agent", "inbox", "--as", "b"], true, &envelope);
    assert_eq!(code, 0, "B agent inbox pull should succeed");

    let (code, envelope) = b.run(&["history", "inbox", "--as", "b", "--limit", "10"], None);
    log_step(
        "B",
        &["history", "inbox", "--as", "b", "--limit", "10"],
        false,
        &envelope,
    );
    assert_eq!(code, 0);
    let ids: Vec<String> = envelope_data(&envelope)
        .and_then(Value::as_array)
        .expect("history inbox data is an array")
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(
        ids.contains(&event_id_1),
        "B history inbox should contain event_id {event_id_1}, got {ids:?}"
    );

    // ======================================================================
    // Step 4: disconnect / retry.
    // ======================================================================
    stop_child(relay_child);
    std::thread::sleep(Duration::from_millis(300));

    let envelope = a_run(
        &runner,
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            &npub_b,
            "--content",
            "joint-2",
        ],
        Some(PASSWORD_A),
    )
    .await;
    log_step(
        "A",
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            &npub_b,
            "--content",
            "joint-2",
        ],
        true,
        &envelope,
    );
    let Envelope::Ok { data } = &envelope else {
        panic!("A send joint-2 (relay down) failed: {envelope:?}");
    };
    assert_eq!(
        data.get("published_to").and_then(Value::as_i64),
        Some(0),
        "expected published_to:0 while the relay is down"
    );
    assert_eq!(
        data.get("queued_for_retry").and_then(Value::as_bool),
        Some(true),
        "expected queued_for_retry:true while the relay is down"
    );
    let event_id_2 = data
        .get("event_id")
        .and_then(Value::as_str)
        .expect("event_id present")
        .to_string();
    println!("event_id (joint-2, E2) = {event_id_2}");

    let envelope = a_run(&runner, &["storage", "outbox", "list"], None).await;
    log_step("A", &["storage", "outbox", "list"], false, &envelope);
    let outbox_ids: Vec<String> = envelope_data(&envelope)
        .and_then(Value::as_array)
        .expect("outbox list data is an array")
        .iter()
        .filter_map(|e| e.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(
        outbox_ids.contains(&event_id_2),
        "A outbox should contain E2={event_id_2}, got {outbox_ids:?}"
    );

    let relay_child = spawn_relay(&relay_path, port, &relay_data);
    wait_for_port(port, Duration::from_secs(5));
    println!("relay: restarted on {relay_url} (same port + data_dir)");

    let envelope = a_run(
        &runner,
        &["storage", "outbox", "retry", "--id", &event_id_2],
        None,
    )
    .await;
    log_step(
        "A",
        &["storage", "outbox", "retry", "--id", &event_id_2],
        false,
        &envelope,
    );
    assert!(
        matches!(envelope, Envelope::Ok { .. }),
        "outbox retry should succeed"
    );

    let (code, envelope) = b.run(&["agent", "inbox", "--as", "b"], Some(PASSWORD_B));
    log_step("B", &["agent", "inbox", "--as", "b"], true, &envelope);
    assert_eq!(code, 0);

    let (code, envelope) = b.run(&["history", "inbox", "--as", "b", "--limit", "10"], None);
    log_step(
        "B",
        &["history", "inbox", "--as", "b", "--limit", "10"],
        false,
        &envelope,
    );
    assert_eq!(code, 0);
    let ids_after_retry: Vec<String> = envelope_data(&envelope)
        .and_then(Value::as_array)
        .expect("history inbox data is an array")
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    assert!(
        ids_after_retry.contains(&event_id_2),
        "B history inbox should contain E2={event_id_2} after retry, got {ids_after_retry:?}"
    );

    // Pull once more: the same event_id must not be duplicated.
    let (code, envelope) = b.run(&["agent", "inbox", "--as", "b"], Some(PASSWORD_B));
    log_step("B", &["agent", "inbox", "--as", "b"], true, &envelope);
    assert_eq!(code, 0);
    let (code, envelope) = b.run(&["history", "inbox", "--as", "b", "--limit", "10"], None);
    log_step(
        "B",
        &["history", "inbox", "--as", "b", "--limit", "10"],
        false,
        &envelope,
    );
    assert_eq!(code, 0);
    let ids_second_pull: Vec<String> = envelope_data(&envelope)
        .and_then(Value::as_array)
        .expect("history inbox data is an array")
        .iter()
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_string))
        .collect();
    let e2_count = ids_second_pull
        .iter()
        .filter(|id| id.as_str() == event_id_2)
        .count();
    assert_eq!(
        e2_count, 1,
        "E2 should appear exactly once after a second pull, got {e2_count} in {ids_second_pull:?}"
    );

    // ======================================================================
    // Step 5: error paths.
    // ======================================================================
    let envelope = a_run(
        &runner,
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            &npub_b,
            "--content",
            "should-not-send",
        ],
        Some(WRONG_PASSWORD),
    )
    .await;
    log_step(
        "A",
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            &npub_b,
            "--content",
            "should-not-send",
        ],
        true,
        &envelope,
    );
    let (exit, error) = classify(&envelope).unwrap_or_else(|| {
        panic!("expected a Failed envelope for the wrong password, got Ok: {envelope:?}")
    });
    assert_eq!(
        exit,
        ExitClass::AuthError,
        "wrong password should be auth_error"
    );
    assert_eq!(error, "auth_error");

    let envelope = a_run(
        &runner,
        &[
            "contact",
            "add",
            "--nickname",
            "badguy",
            "--npub",
            "not-a-valid-npub",
        ],
        None,
    )
    .await;
    log_step(
        "A",
        &[
            "contact",
            "add",
            "--nickname",
            "badguy",
            "--npub",
            "not-a-valid-npub",
        ],
        false,
        &envelope,
    );
    let (exit, error) = classify(&envelope).unwrap_or_else(|| {
        panic!("expected a Failed envelope for an invalid npub, got Ok: {envelope:?}")
    });
    assert_eq!(
        exit,
        ExitClass::OtherError,
        "invalid npub in contact add should be other_error"
    );
    assert_eq!(error, "other_error");

    let envelope = a_run(
        &runner,
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            "not-a-valid-npub",
            "--content",
            "bad-to",
        ],
        Some(PASSWORD_A),
    )
    .await;
    log_step(
        "A",
        &[
            "agent",
            "msg",
            "--from",
            "a",
            "--to",
            "not-a-valid-npub",
            "--content",
            "bad-to",
        ],
        true,
        &envelope,
    );
    let (exit, error) = classify(&envelope).unwrap_or_else(|| {
        panic!("expected a Failed envelope for an invalid --to npub, got Ok: {envelope:?}")
    });
    assert_eq!(
        exit,
        ExitClass::UserError,
        "`agent msg --to <invalid npub>` should be user_error (exit 1), matching Hyphae's \
         documented claim for the send path (distinct from contact add's other_error)"
    );
    assert_eq!(error, "user_error");
    println!(
        "confirmed: `agent msg --to <invalid npub>` returns user_error (exit 1), \
         matching Hyphae's documented claim (distinct from contact add's other_error)"
    );

    // ======================================================================
    // Cleanup: stop the relay by its own pid (never pkill -f), then let
    // `tmp`'s Drop remove both HOMEs, the install dir, and the relay data.
    // ======================================================================
    stop_child(relay_child);

    println!("==== all findings are asserted above; every step matched the expected behavior ====");

    drop(tmp);
}
