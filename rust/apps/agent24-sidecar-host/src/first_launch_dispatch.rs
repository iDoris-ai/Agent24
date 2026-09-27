use std::time::Instant;

use agent24_sidecar_host_protocol::{ErrorCode, PROTOCOL_VERSION, Reply};

use crate::{
    actor::{Deadlines, Phase},
    first_launch_ingress::AcceptedLaunch,
    host_ports::HostPorts,
    launch::{LaunchFailure, OwnedLaunch},
    native_generation::NativeGeneration,
    pre_owned_cleanup::{IntoCleanupOwner, PreOwnedCleanup},
};

#[allow(clippy::large_enum_variant)]
pub(crate) enum FirstLaunchDispatch<'host> {
    Generation(NativeGeneration<'host>),
    Rejected {
        reply: Reply,
        cleanup: Option<PreOwnedCleanup>,
    },
    Cleanup(PreOwnedCleanup),
}

impl FirstLaunchDispatch<'_> {
    fn rejected(request_id: u64, cleanup: Option<PreOwnedCleanup>) -> Self {
        Self::Rejected {
            reply: Reply::Error {
                version: PROTOCOL_VERSION,
                request_id,
                code: ErrorCode::LaunchFailed,
            },
            cleanup,
        }
    }
}

pub(crate) fn dispatch<'host>(
    accepted: AcceptedLaunch,
    ports: &'host mut HostPorts,
    limits: Deadlines,
    mut clock: impl FnMut() -> Instant,
) -> FirstLaunchDispatch<'host> {
    let request_id = accepted.request_id;
    let deadline = accepted.deadline;
    if clock() >= deadline {
        return FirstLaunchDispatch::rejected(request_id, None);
    }

    let launch = match OwnedLaunch::start(accepted.intent) {
        Ok(launch) => launch,
        Err(failure @ LaunchFailure::Pipes { .. }) => {
            match failure.into_pre_owned_cleanup(Phase::Launching(deadline), limits) {
                Ok(cleanup) => {
                    return FirstLaunchDispatch::rejected(request_id, Some(cleanup));
                }
                Err(_) => unreachable!("pipe failure must transfer its target owner"),
            }
        }
        Err(_) => return FirstLaunchDispatch::rejected(request_id, None),
    };

    let after_start = clock();
    if after_start >= deadline {
        return FirstLaunchDispatch::rejected(
            request_id,
            Some(PreOwnedCleanup::new(
                launch.into_cleanup_owner(),
                Phase::Launching(deadline),
                limits,
                false,
            )),
        );
    }

    let (slots, output, control) = ports.borrow();
    let assembled =
        NativeGeneration::assemble_in(slots, launch, output, control, limits, deadline, clock);
    let generation = match assembled {
        Ok(generation) => generation,
        Err(error) => {
            return FirstLaunchDispatch::rejected(
                request_id,
                Some(error.into_pre_owned_cleanup(Phase::Launching(deadline), limits)),
            );
        }
    };

    if generation.schedule_state().terminal {
        return FirstLaunchDispatch::Cleanup(generation.into_pre_owned_cleanup());
    }

    FirstLaunchDispatch::Generation(generation)
}

#[cfg(all(test, unix))]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        host_ports::HostPorts,
        launch::LaunchIntent,
        output_io::WriteStep,
        target::TreeObservation,
        worker_slots::{WorkerRole, WorkerSlots},
    };
    use agent24_sidecar_host_protocol::Request;
    use std::{
        collections::{BTreeMap, VecDeque},
        io::{self, Read, Write},
        sync::{Arc, Mutex},
        thread,
        time::Duration,
    };

    const LIMITS: Deadlines = Deadlines {
        launch: Duration::from_secs(2),
        ready: Duration::from_secs(2),
        graceful: Duration::from_secs(1),
        force: Duration::from_secs(1),
        drain: Duration::from_secs(1),
    };

    struct PendingRead;
    impl Read for PendingRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    struct Capture(Arc<Mutex<Vec<u8>>>);
    impl Write for Capture {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct BrokenSink;
    impl Write for BrokenSink {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn request(id: u64, executable: &str, argv: &[&str]) -> Request {
        Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: id,
            executable: executable.into(),
            cwd: "/".into(),
            argv: argv.iter().map(|arg| (*arg).into()).collect(),
            env: BTreeMap::new(),
        }
    }
    fn accepted(id: u64, executable: &str, argv: &[&str], deadline: Instant) -> AcceptedLaunch {
        AcceptedLaunch {
            intent: LaunchIntent::from_request(request(id, executable, argv)).unwrap(),
            request_id: id,
            deadline,
        }
    }
    fn ports() -> (HostPorts, &'static WorkerSlots, Arc<Mutex<Vec<u8>>>) {
        let slots = WorkerSlots::isolated();
        let bytes = Arc::new(Mutex::new(Vec::new()));
        (
            HostPorts::new_in(
                slots,
                PendingRead,
                Capture(Arc::clone(&bytes)),
                Duration::from_secs(2),
            )
            .unwrap(),
            slots,
            bytes,
        )
    }
    fn rejected_cleanup(result: FirstLaunchDispatch<'_>) -> PreOwnedCleanup {
        match result {
            FirstLaunchDispatch::Rejected {
                cleanup: Some(cleanup),
                ..
            } => cleanup,
            _ => panic!("expected rejected launch with cleanup authority"),
        }
    }
    fn expire(mut cleanup: PreOwnedCleanup) {
        let timeout = Instant::now() + Duration::from_secs(3);
        loop {
            match cleanup.step(Instant::now()).unwrap() {
                TreeObservation::ConfirmedEmpty => break,
                TreeObservation::Present | TreeObservation::Unconfirmed
                    if Instant::now() < timeout =>
                {
                    thread::yield_now()
                }
                tree => panic!("cleanup did not confirm empty: {tree:?}"),
            }
        }
    }

    #[test]
    fn deadline_before_start_rejects_with_the_original_request_id() {
        let _test_guard = crate::posix::tests::test_lock();
        let (mut ports, _, _) = ports();
        let now = Instant::now();
        let result = dispatch(
            accepted(31, "/path/secret-helper", &[], now),
            &mut ports,
            LIMITS,
            || now,
        );
        let FirstLaunchDispatch::Rejected { reply, cleanup } = result else {
            panic!("expired launch must be rejected")
        };
        assert!(cleanup.is_none());
        assert!(matches!(
            reply,
            Reply::Error {
                request_id: 31,
                code: ErrorCode::LaunchFailed,
                ..
            }
        ));
    }

    #[test]
    fn start_failure_is_static_and_keeps_the_request_id() {
        let _test_guard = crate::posix::tests::test_lock();
        let (mut ports, _, _) = ports();
        let now = Instant::now();
        let result = dispatch(
            accepted(32, "/path/private-helper", &[], now + LIMITS.launch),
            &mut ports,
            LIMITS,
            || now,
        );
        let FirstLaunchDispatch::Rejected { reply, cleanup } = result else {
            panic!("spawn failure must be a rejection")
        };
        assert!(cleanup.is_none());
        assert!(matches!(
            reply,
            Reply::Error {
                request_id: 32,
                code: ErrorCode::LaunchFailed,
                ..
            }
        ));
        assert!(!format!("{reply:?}").contains("private-helper"));
    }

    #[test]
    fn deadline_at_or_after_spawn_and_after_assembly_returns_cleanup() {
        let _test_guard = crate::posix::tests::test_lock();
        for (id, samples_after_spawn) in [(33, 0), (34, 1), (35, 2)] {
            let (mut ports, _, bytes) = ports();
            let start = Instant::now();
            let deadline = start + Duration::from_secs(10);
            let mut samples = match samples_after_spawn {
                0 => VecDeque::from([start, deadline]),
                1 => VecDeque::from([start, start + Duration::from_millis(1), deadline]),
                _ => VecDeque::from([
                    start,
                    start + Duration::from_millis(1),
                    deadline + Duration::from_nanos(1),
                ]),
            };
            let result = dispatch(
                accepted(id, "/bin/sh", &["-c", "exit 0"], deadline),
                &mut ports,
                LIMITS,
                || samples.pop_front().unwrap_or(deadline),
            );
            let FirstLaunchDispatch::Rejected {
                reply,
                cleanup: Some(cleanup),
            } = result
            else {
                panic!("deadline expiry must reject with cleanup ownership")
            };
            let (_, output, _) = ports.borrow();
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match output.step(Instant::now()) {
                    Ok(WriteStep::Idle | WriteStep::Complete) => break,
                    Ok(WriteStep::Pending) if Instant::now() < until => thread::yield_now(),
                    step => panic!("output did not settle: {step:?}"),
                }
            }
            assert!(
                matches!(reply, Reply::Error { request_id, code: ErrorCode::LaunchFailed, .. } if request_id == id)
            );
            assert!(
                bytes.lock().unwrap().is_empty(),
                "Owned output escaped before deadline check"
            );
            expire(cleanup);
        }
        crate::posix::tests::wait_for_reaper_idle();
    }

    #[test]
    fn owned_pending_is_not_acknowledged_until_the_owned_frame_flushes() {
        let _test_guard = crate::posix::tests::test_lock();
        let (mut ports, _, _) = ports();
        let now = Instant::now();
        let result = dispatch(
            accepted(35, "/bin/sh", &["-c", "exec sleep 30"], now + LIMITS.launch),
            &mut ports,
            LIMITS,
            || now,
        );
        let FirstLaunchDispatch::Generation(mut generation) = result else {
            panic!("valid target should assemble")
        };
        let initial = generation.schedule_state();
        assert!(matches!(initial.phase, Phase::AwaitReady(_)));
        assert!(!initial.owned_acknowledged);
        let deadline = Instant::now() + Duration::from_secs(2);
        while !generation.schedule_state().owned_acknowledged {
            let _ = generation.step(Instant::now());
            assert!(Instant::now() < deadline, "Owned frame did not flush");
            thread::yield_now();
        }
        assert!(generation.schedule_state().owned_acknowledged);
        expire(generation.into_pre_owned_cleanup());
        crate::posix::tests::wait_for_reaper_idle();
    }

    fn busy_slot(id: u64, role: WorkerRole) {
        let (mut ports, slots, _) = ports();
        let permit = slots.reserve(role).unwrap();
        let now = Instant::now();
        expire(rejected_cleanup(dispatch(
            accepted(id, "/bin/sh", &["-c", "exit 0"], now + LIMITS.launch),
            &mut ports,
            LIMITS,
            || now,
        )));
        drop(permit);
    }
    #[test]
    fn ready_read_busy_slot_returns_cleanup_authority() {
        let _test_guard = crate::posix::tests::test_lock();
        busy_slot(36, WorkerRole::ReadyRead);
        crate::posix::tests::wait_for_reaper_idle();
    }
    #[test]
    fn stderr_drain_busy_slot_returns_cleanup_authority() {
        let _test_guard = crate::posix::tests::test_lock();
        busy_slot(38, WorkerRole::StderrDrain);
        crate::posix::tests::wait_for_reaper_idle();
    }

    #[test]
    fn owned_queue_failure_returns_cleanup_that_survives_broken_writer() {
        let _test_guard = crate::posix::tests::test_lock();
        let slots = WorkerSlots::isolated();
        let mut ports =
            HostPorts::new_in(slots, PendingRead, BrokenSink, Duration::from_secs(2)).unwrap();
        let now = Instant::now();
        {
            let (_, output, _) = ports.borrow();
            output.put(vec![b'x'], now).unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                match output.step(Instant::now()) {
                    Err(_) => break,
                    Ok(WriteStep::Pending) if Instant::now() < deadline => thread::yield_now(),
                    step => panic!("broken writer did not close: {step:?}"),
                }
            }
        }
        let result = dispatch(
            accepted(39, "/bin/sh", &["-c", "exec sleep 30"], now + LIMITS.launch),
            &mut ports,
            LIMITS,
            || now,
        );
        let cleanup = match result {
            FirstLaunchDispatch::Cleanup(cleanup) => cleanup,
            FirstLaunchDispatch::Rejected { reply, cleanup } => {
                panic!(
                    "unexpected rejection {reply:?}, owner retained: {}",
                    cleanup.is_some()
                )
            }
            FirstLaunchDispatch::Generation(generation) => {
                panic!(
                    "failed Owned queue returned generation: {:?}",
                    generation.schedule_state()
                )
            }
        };
        expire(cleanup);
        crate::posix::tests::wait_for_reaper_idle();
    }
}
