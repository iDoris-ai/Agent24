#![cfg(any(unix, windows))]
#![allow(clippy::expect_used, clippy::unwrap_used)]

use crate::{
    actor::{Deadlines, Phase},
    first_launch_dispatch::{self, FirstLaunchDispatch},
    first_launch_ingress::AcceptedLaunch,
    generation_harness::{GenerationHarness, TurnIntent},
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
        assert!(
            executable.is_file(),
            "Windows PowerShell is required: {executable:?}"
        );
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
    let request = request(id, ready);
    let accepted = AcceptedLaunch {
        intent: LaunchIntent::from_request(request).expect("launch request"),
        request_id: id,
        deadline: now + LIMITS.launch,
    };
    let FirstLaunchDispatch::Generation(generation) =
        first_launch_dispatch::dispatch(accepted, &mut ports, LIMITS, || now)
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
) {
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
            return;
        }
        assert!(
            Instant::now() < deadline,
            "phase did not reach {target:?}; got {:?}",
            report.state.phase
        );
        thread::sleep(Duration::from_millis(2));
    }
}

fn assert_frames(bytes: &[u8], request_id: u64, expected: &[u8]) {
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
                Ok(Event::Exit { .. }) => panic!("unexpected Exit event frame"),
                Err(_) => panic!("invalid output frame"),
            },
        }
    }
    assert_eq!(kinds, expected, "output frame order/count mismatch");
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
        assert_frames(&output, 7401, b"OR");
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
            drive_until(harness, WaitFor::GracefulStopping, TurnIntent::Continue, 3);
            let graceful_deadline = match harness
                .turn(TurnIntent::Continue, Instant::now())
                .state
                .phase
            {
                Phase::GracefulStopping(deadline) => deadline,
                phase => panic!("parent EOF did not enter graceful cleanup: {phase:?}"),
            };
            harness.turn(TurnIntent::Continue, graceful_deadline);
            drive_until(harness, WaitFor::Empty, TurnIntent::Continue, 8);
            let output = bytes.lock().unwrap();
            assert!(output.len() <= OUTPUT_CAPACITY);
            assert_frames(&output, id, expected);
        });
    }
}
