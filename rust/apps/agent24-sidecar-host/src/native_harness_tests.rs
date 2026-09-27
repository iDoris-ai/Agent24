#![cfg(any(unix, windows))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::{
    actor::{Deadlines, Phase},
    first_launch_dispatch::{self, FirstLaunchDispatch},
    first_launch_ingress::AcceptedLaunch,
    generation_harness::{GenerationHarness, TurnIntent, TurnReport},
    host_ports::HostPorts,
    launch::LaunchIntent,
    native_generation::NativeGeneration,
    worker_slots::WorkerSlots,
};
use agent24_sidecar_host_protocol::{
    Event, MAX_CONTROL_FRAME_BYTES, PROTOCOL_VERSION, Reply, Request, decode_event, decode_reply,
};
#[cfg(unix)]
use std::collections::BTreeMap;
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const LIMITS: Deadlines = Deadlines {
    launch: Duration::from_secs(4),
    ready: Duration::from_secs(4),
    graceful: Duration::from_millis(100),
    force: Duration::from_secs(4),
    drain: Duration::from_secs(2),
};
const DESCENDANT_LIMITS: Deadlines = Deadlines {
    launch: Duration::from_secs(15),
    ready: Duration::from_secs(30),
    graceful: Duration::from_millis(100),
    force: Duration::from_secs(8),
    drain: Duration::from_secs(2),
};
const OUTPUT_CAPACITY: usize = 2 * MAX_CONTROL_FRAME_BYTES;

struct ParentInput(Arc<AtomicBool>);

impl Read for ParentInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        if self.0.load(Ordering::SeqCst) {
            Ok(0)
        } else {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
}

struct BoundedOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
    capacity: usize,
}

impl Write for BoundedOutput {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let mut bytes = self
            .bytes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if bytes.len().saturating_add(input.len()) > self.capacity {
            return Err(io::ErrorKind::WriteZero.into());
        }
        bytes.extend_from_slice(input);
        Ok(input.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn request(id: u64, ready: bool) -> Request {
    #[cfg(unix)]
    let (executable, cwd, argv, env) = {
        let ready_line = r#"printf '{"type":"ready","protocol":1,"port":4312,"token":"tttttttttttttttttttttttttttttttt","version":"native-harness"}\n'; "#;
        let script = if ready {
            format!("trap '' TERM; {ready_line} cat >/dev/null; exec sleep 30")
        } else {
            "trap '' TERM; exec sleep 30".to_owned()
        };
        (
            "/bin/sh".to_owned(),
            "/".to_owned(),
            vec!["-c".to_owned(), script],
            BTreeMap::new(),
        )
    };
    #[cfg(windows)]
    let (executable, cwd, argv, env) = {
        let root =
            std::env::var_os("SystemRoot").expect("SystemRoot is required for Windows smoke");
        let executable = std::path::PathBuf::from(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        assert!(executable.is_file(), "Windows PowerShell is required");
        let script = if ready {
            "[Console]::Out.WriteLine('{\"type\":\"ready\",\"protocol\":1,\"port\":4312,\"token\":\"tttttttttttttttttttttttttttttttt\",\"version\":\"native-harness\"}'); [Console]::In.ReadToEnd() | Out-Null; Start-Sleep -Seconds 30"
        } else {
            "Start-Sleep -Seconds 30"
        };
        let env = ["SystemRoot", "WINDIR", "PATH", "TEMP", "TMP", "USERPROFILE"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        (
            executable.to_string_lossy().into_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
            vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-Command".to_owned(),
                script.to_owned(),
            ],
            env,
        )
    };
    Request::Launch {
        version: PROTOCOL_VERSION,
        request_id: id,
        executable,
        cwd,
        argv,
        env,
    }
}

fn with_generation<R>(
    id: u64,
    ready: bool,
    run: impl FnOnce(
        &mut GenerationHarness<NativeGeneration<'_>>,
        &Arc<Mutex<Vec<u8>>>,
        &Arc<AtomicBool>,
    ) -> R,
) -> R {
    with_request_limits(id, request(id, ready), LIMITS, run)
}

fn with_request_limits<R>(
    id: u64,
    request: Request,
    limits: Deadlines,
    run: impl FnOnce(
        &mut GenerationHarness<NativeGeneration<'_>>,
        &Arc<Mutex<Vec<u8>>>,
        &Arc<AtomicBool>,
    ) -> R,
) -> R {
    let slots = WorkerSlots::isolated();
    let bytes = Arc::new(Mutex::new(Vec::new()));
    let parent_eof = Arc::new(AtomicBool::new(false));
    let mut ports = HostPorts::new_in(
        slots,
        ParentInput(Arc::clone(&parent_eof)),
        BoundedOutput {
            bytes: Arc::clone(&bytes),
            capacity: OUTPUT_CAPACITY,
        },
        Duration::from_secs(3),
    )
    .expect("host ports");
    let now = Instant::now();
    let accepted = AcceptedLaunch {
        intent: LaunchIntent::from_request(request).expect("launch request"),
        request_id: id,
        deadline: now + limits.launch,
    };
    let FirstLaunchDispatch::Generation(generation) =
        first_launch_dispatch::dispatch(accepted, &mut ports, limits, || now)
    else {
        panic!("test launch must dispatch to NativeGeneration")
    };
    let mut harness = GenerationHarness::new(generation);
    let result = run(&mut harness, &bytes, &parent_eof);
    drop(harness);
    drop(ports);
    #[cfg(unix)]
    crate::posix::tests::wait_for_reaper_idle();
    result
}

#[derive(Clone, Copy, Debug)]
enum WaitFor {
    AwaitReady,
    GracefulStopping,
    Running,
    Empty,
}

fn drive_until(
    harness: &mut GenerationHarness<NativeGeneration<'_>>,
    target: WaitFor,
    intent: TurnIntent,
    seconds: u64,
) -> TurnReport {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let report = harness.turn(intent, Instant::now());
        let reached = match target {
            WaitFor::AwaitReady => matches!(report.state.phase, Phase::AwaitReady(_)),
            WaitFor::GracefulStopping => {
                matches!(report.state.phase, Phase::GracefulStopping(_))
            }
            WaitFor::Running => report.state.phase == Phase::Running,
            WaitFor::Empty => report.state.phase == Phase::Empty,
        };
        if reached && !report.state.output_pending {
            return report;
        }
        assert!(
            Instant::now() < deadline,
            "phase did not reach {target:?}; got {:?}, error={:?}, first_error={:?}",
            report.state.phase,
            report.error,
            report.first_error
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn assert_frames(bytes: &[u8], request_id: u64, expected: &[u8], allow_exit: bool) {
    let mut kinds = Vec::with_capacity(expected.len());
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        match decode_reply(line) {
            Ok(Reply::Owned {
                version,
                request_id: actual_id,
            }) => {
                assert_eq!(version, PROTOCOL_VERSION, "Owned version mismatch");
                assert_eq!(actual_id, request_id, "Owned request id mismatch");
                kinds.push(b'O');
            }
            Ok(_) => panic!("unexpected non-Owned reply frame"),
            Err(_) => match decode_event(line) {
                Ok(Event::Ready {
                    protocol,
                    port,
                    token,
                    version,
                }) => {
                    assert_eq!(protocol, PROTOCOL_VERSION, "Ready protocol mismatch");
                    assert_eq!(port, 4312, "Ready port mismatch");
                    assert!(token == "t".repeat(32), "Ready token mismatch");
                    assert_eq!(version, "native-harness", "Ready version mismatch");
                    kinds.push(b'R');
                }
                Ok(Event::Exit { .. }) if allow_exit => kinds.push(b'X'),
                Ok(Event::Exit { .. }) => panic!("unexpected Exit event frame"),
                Err(_) => panic!("invalid output frame"),
            },
        }
    }
    let observed = kinds.as_slice();
    assert!(
        observed == expected || (allow_exit && observed.strip_suffix(b"X") == Some(expected)),
        "output frame order/count mismatch"
    );
}

struct MarkerDirectory(PathBuf);

impl MarkerDirectory {
    fn new(request_id: u64) -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "agent24-native-descendant-{}-{request_id}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).expect("create unique descendant marker directory");
        Self(path)
    }

    fn marker(&self) -> PathBuf {
        self.0.join("descendant.pid")
    }

    fn gate(&self) -> PathBuf {
        self.0.join("leader.exit")
    }
}

impl Drop for MarkerDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn descendant_request(id: u64, marker: &Path, ready_gate: &Path, gate: &Path) -> Request {
    #[cfg(unix)]
    let (executable, cwd, argv, env) = {
        let marker = shell_quote(&marker.display().to_string());
        let ready_gate = shell_quote(&ready_gate.display().to_string());
        let gate = shell_quote(&gate.display().to_string());
        let child = format!(
            "sleep 120 & descendant=$!; printf '%s\\n' \"$descendant\" > {marker}; wait \"$descendant\""
        );
        let ready = r#"{"type":"ready","protocol":1,"port":4312,"token":"tttttttttttttttttttttttttttttttt","version":"native-harness"}"#;
        let script = format!(
            "/bin/sh -c {} & while [ ! -s {marker} ]; do sleep 0.01; done; while [ ! -e {ready_gate} ]; do sleep 0.01; done; printf '%s\\n' '{}'; while [ ! -e {gate} ]; do sleep 0.01; done; exit 17",
            shell_quote(&child),
            ready
        );
        (
            "/bin/sh".to_owned(),
            "/".to_owned(),
            vec!["-c".to_owned(), script],
            BTreeMap::new(),
        )
    };
    #[cfg(windows)]
    let (executable, cwd, argv, env) = {
        let root =
            std::env::var_os("SystemRoot").expect("SystemRoot is required for Windows smoke");
        let executable = std::path::PathBuf::from(&root)
            .join("System32")
            .join("WindowsPowerShell")
            .join("v1.0")
            .join("powershell.exe");
        assert!(executable.is_file(), "Windows PowerShell is required");
        let script_path = marker
            .parent()
            .expect("marker has a parent directory")
            .join("leader.ps1");
        let marker = powershell_quote(&marker.display().to_string());
        let ready_gate = powershell_quote(&ready_gate.display().to_string());
        let gate = powershell_quote(&gate.display().to_string());
        let ready = r#"[Console]::Out.WriteLine('{"type":"ready","protocol":1,"port":4312,"token":"tttttttttttttttttttttttttttttttt","version":"native-harness"}'); [Console]::Out.Flush(); "#;
        let script = format!(
            "[System.IO.File]::WriteAllText('{marker}', 'leader-started'); $start = [System.Diagnostics.ProcessStartInfo]::new(); $start.FileName = \"$PSHOME\\powershell.exe\"; $start.Arguments = '-NoLogo -NoProfile -NonInteractive -Command \"Start-Sleep -Seconds 120\"'; $start.UseShellExecute = $false; $child = [System.Diagnostics.Process]::Start($start); [System.IO.File]::WriteAllText('{marker}', [string]$child.Id); while (-not [System.IO.File]::Exists('{ready_gate}')) {{ [System.Threading.Thread]::Sleep(10) }}; {ready} while (-not [System.IO.File]::Exists('{gate}')) {{ [System.Threading.Thread]::Sleep(10) }}; exit 17"
        );
        std::fs::write(&script_path, script).expect("write Windows descendant fixture");
        let env = ["SystemRoot", "WINDIR", "PATH", "TEMP", "TMP", "USERPROFILE"]
            .into_iter()
            .filter_map(|key| std::env::var(key).ok().map(|value| (key.to_owned(), value)))
            .collect();
        (
            executable.to_string_lossy().into_owned(),
            std::env::temp_dir().to_string_lossy().into_owned(),
            vec![
                "-NoLogo".to_owned(),
                "-NoProfile".to_owned(),
                "-NonInteractive".to_owned(),
                "-ExecutionPolicy".to_owned(),
                "Bypass".to_owned(),
                "-File".to_owned(),
                script_path.to_string_lossy().into_owned(),
            ],
            env,
        )
    };
    Request::Launch {
        version: PROTOCOL_VERSION,
        request_id: id,
        executable,
        cwd,
        argv,
        env,
    }
}

#[cfg(unix)]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(windows)]
fn powershell_quote(value: &str) -> String {
    value.replace('\'', "''")
}

fn wait_for_descendant_pid(marker: &Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(raw) = std::fs::read_to_string(marker)
            && let Ok(pid) = raw.trim().parse::<u32>()
        {
            return pid;
        }
        assert!(
            Instant::now() < deadline,
            "descendant PID marker was not published; leader_started={}",
            marker.exists()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
fn descendant_is_alive(pid: u32) -> io::Result<bool> {
    use nix::{errno::Errno, sys::signal::kill, unistd::Pid};

    let pid =
        i32::try_from(pid).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    match kill(Pid::from_raw(pid), None) {
        Ok(()) => Ok(true),
        Err(Errno::ESRCH) => Ok(false),
        Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
    }
}

#[cfg(windows)]
fn descendant_is_alive(pid: u32) -> io::Result<bool> {
    let output = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "tasklist failed with status {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
}

fn wait_until_descendant_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if !descendant_is_alive(pid).expect("confirm descendant PID absence") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "descendant {pid} did not disappear"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn assert_descendant_frames(bytes: &[u8], request_id: u64) {
    let mut kinds = Vec::new();
    let mut exit_count = 0;
    for line in bytes.split_inclusive(|byte| *byte == b'\n') {
        match decode_reply(line) {
            Ok(Reply::Owned {
                version,
                request_id: actual_id,
            }) => {
                assert_eq!(version, PROTOCOL_VERSION);
                assert_eq!(actual_id, request_id);
                kinds.push(b'O');
            }
            Ok(_) => panic!("unexpected reply frame in descendant lifecycle"),
            Err(_) => match decode_event(line) {
                Ok(Event::Ready {
                    protocol,
                    port,
                    token,
                    version,
                }) => {
                    assert_eq!(protocol, PROTOCOL_VERSION);
                    assert_eq!(port, 4312);
                    assert_eq!(token, "t".repeat(32));
                    assert_eq!(version, "native-harness");
                    kinds.push(b'R');
                }
                Ok(Event::Exit { protocol, code }) => {
                    assert_eq!(protocol, PROTOCOL_VERSION);
                    assert_eq!(code, Some(17), "leader exit code must be retained");
                    exit_count += 1;
                    kinds.push(b'X');
                }
                Err(_) => panic!("invalid output frame in descendant lifecycle"),
            },
        }
    }
    assert_eq!(kinds, b"ORX", "Owned, Ready, Exit ordering/count mismatch");
    assert_eq!(exit_count, 1, "leader Exit must be emitted exactly once");
}

#[test]
fn ready_force_cancel_then_continue_reaps_same_native_generation() {
    #[cfg(unix)]
    let _guard = crate::posix::tests::test_lock();
    with_generation(7401, true, |harness, bytes, _| {
        drive_until(harness, WaitFor::Running, TurnIntent::Continue, 6);
        let first = harness.turn(TurnIntent::ForceCancel, Instant::now());
        assert_ne!(first.state.phase, Phase::Empty);
        drive_until(harness, WaitFor::Empty, TurnIntent::Continue, 8);
        let before_noop = bytes.lock().unwrap().len();
        let first_error = first.first_error;
        for _ in 0..2 {
            let report = harness.turn(TurnIntent::Continue, Instant::now());
            assert_eq!(report.state.phase, Phase::Empty);
            assert_eq!(report.error, None);
            assert_eq!(report.first_error, first_error);
        }
        let output = bytes.lock().unwrap();
        assert_eq!(
            output.len(),
            before_noop,
            "Empty turns must not emit output"
        );
        assert!(output.len() <= OUTPUT_CAPACITY);
        assert_frames(&output, 7401, b"OR", false);
    });
}

#[test]
fn parent_eof_after_await_ready_or_running_cleans_up() {
    #[cfg(unix)]
    let _guard = crate::posix::tests::test_lock();
    for (id, ready, reached, expected) in [
        (7402, false, WaitFor::AwaitReady, b"O".as_slice()),
        (7403, true, WaitFor::Running, b"OR".as_slice()),
    ] {
        with_generation(id, ready, |harness, bytes, parent_eof| {
            drive_until(harness, reached, TurnIntent::Continue, 6);
            parent_eof.store(true, Ordering::SeqCst);
            let eof_turn = harness.turn(TurnIntent::Continue, Instant::now());
            assert_ne!(eof_turn.state.phase, Phase::Empty);
            if ready {
                let graceful =
                    drive_until(harness, WaitFor::GracefulStopping, TurnIntent::Continue, 3);
                let Phase::GracefulStopping(deadline) = graceful.state.phase else {
                    unreachable!("wait target guarantees GracefulStopping")
                };
                harness.turn(TurnIntent::Continue, deadline);
            }
            drive_until(harness, WaitFor::Empty, TurnIntent::Continue, 8);
            let output = bytes.lock().unwrap();
            assert!(output.len() <= OUTPUT_CAPACITY);
            assert_frames(&output, id, expected, true);
        });
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "runs separately on Windows to avoid process-tree timing interference"
)]
fn leader_exit_with_live_descendant_forces_tree_to_confirmed_empty() {
    #[cfg(unix)]
    let _guard = crate::posix::tests::test_lock();
    const REQUEST_ID: u64 = 7404;
    let markers = MarkerDirectory::new(REQUEST_ID);
    let marker = markers.marker();
    let ready_gate = markers.0.join("ready.emit");
    let gate = markers.gate();
    with_request_limits(
        REQUEST_ID,
        descendant_request(REQUEST_ID, &marker, &ready_gate, &gate),
        DESCENDANT_LIMITS,
        |harness, bytes, _| {
            drive_until(harness, WaitFor::AwaitReady, TurnIntent::Continue, 3);
            let descendant = wait_for_descendant_pid(&marker);
            assert!(
                descendant_is_alive(descendant).expect("observe live descendant"),
                "descendant must be alive before leader is released"
            );
            std::fs::write(&ready_gate, b"ready\n").expect("release leader ready gate");
            let running = drive_until(harness, WaitFor::Running, TurnIntent::Continue, 35);
            assert_ne!(running.state.phase, Phase::Empty);

            std::fs::write(&gate, b"exit\n").expect("release leader exit gate");
            let empty = drive_until(harness, WaitFor::Empty, TurnIntent::Continue, 20);
            assert_eq!(empty.state.phase, Phase::Empty);
            wait_until_descendant_gone(descendant);

            let before_tombstone = bytes.lock().unwrap().clone();
            assert_descendant_frames(&before_tombstone, REQUEST_ID);
            for _ in 0..2 {
                let tombstone = harness.turn(TurnIntent::Continue, Instant::now());
                assert_eq!(tombstone.state.phase, Phase::Empty);
                assert_eq!(tombstone.error, None);
            }
            let after_tombstone = bytes.lock().unwrap();
            assert_eq!(
                *after_tombstone, before_tombstone,
                "Empty tombstone turns must not emit another Exit"
            );
            assert_descendant_frames(&after_tombstone, REQUEST_ID);
        },
    );
}
