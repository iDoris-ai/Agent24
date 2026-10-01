use std::{
    fmt,
    time::{Duration, Instant},
};

use agent24_sidecar_host_protocol::{Reply, encode_reply};

use crate::{
    actor::{Deadlines, Phase},
    first_launch_dispatch::{self, FirstLaunchDispatch},
    first_launch_ingress::{FirstLaunchIngress, FirstLaunchStep, FirstLaunchTerminal},
    generation_driver::SessionEnd,
    generation_harness::{GenerationHarness, TurnIntent},
    host_ports::HostPorts,
    native_generation::NativeGeneration,
    output_io::{PutFrameError, WriteStep},
    pre_owned_cleanup::{CleanupTarget, PreOwnedCleanup},
    target::TreeObservation,
};

const PARK: Duration = Duration::from_millis(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct HostSessionError {
    stage: &'static str,
    reason: &'static str,
    kind: Option<std::io::ErrorKind>,
}

impl HostSessionError {
    const fn new(stage: &'static str, reason: &'static str) -> Self {
        Self {
            stage,
            reason,
            kind: None,
        }
    }
}

impl fmt::Display for HostSessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.stage, self.reason)?;
        if let Some(kind) = self.kind {
            write!(f, " ({kind:?})")?;
        }
        Ok(())
    }
}

impl std::error::Error for HostSessionError {}

/// Run one dormant session. Native spawn remains synchronous.
pub(crate) fn run_session(
    ports: &mut HostPorts,
    launch_budget: Duration,
    limits: Deadlines,
    mut clock: impl FnMut() -> Instant,
    mut park: impl FnMut(Duration),
    mut cancelled: impl FnMut() -> bool,
) -> Result<(), HostSessionError> {
    let mut ingress = FirstLaunchIngress::new(launch_budget);
    let accepted = loop {
        let now = clock();
        if cancelled() {
            return Err(HostSessionError::new("ingress", "cancelled"));
        }
        match ingress.step(ports, now) {
            FirstLaunchStep::Pending => park(PARK),
            FirstLaunchStep::Accepted(accepted) => break accepted,
            FirstLaunchStep::Terminal(FirstLaunchTerminal::Eof) => return Ok(()),
            FirstLaunchStep::Terminal(FirstLaunchTerminal::NotLaunch) => {
                return Err(HostSessionError::new("ingress", "launch_required"));
            }
            FirstLaunchStep::Terminal(FirstLaunchTerminal::Fatal(error)) => {
                let _ = error;
                return Err(HostSessionError::new("ingress", "control_failed"));
            }
            FirstLaunchStep::Terminal(FirstLaunchTerminal::Accepted) => {
                return Err(HostSessionError::new("ingress", "already_accepted"));
            }
        }
    };

    let dispatched = first_launch_dispatch::dispatch(accepted, ports, limits, &mut clock);
    match dispatched {
        FirstLaunchDispatch::Generation(generation) => run_generation(
            GenerationHarness::new(generation),
            &mut clock,
            &mut park,
            &mut cancelled,
        ),
        FirstLaunchDispatch::Rejected { reply, cleanup } => {
            run_rejected(ports, reply, cleanup, &mut clock, &mut park)
        }
        FirstLaunchDispatch::Cleanup(cleanup) => run_cleanup(cleanup, &mut clock, &mut park),
    }
}

fn run_generation(
    mut harness: GenerationHarness<NativeGeneration<'_>>,
    clock: &mut impl FnMut() -> Instant,
    park: &mut impl FnMut(Duration),
    cancelled: &mut impl FnMut() -> bool,
) -> Result<(), HostSessionError> {
    let mut force = false;
    loop {
        let now = clock();
        force |= cancelled();
        let report = harness.turn(
            if force {
                TurnIntent::ForceCancel
            } else {
                TurnIntent::Continue
            },
            now,
        );
        // Retryable observe/reap errors do not change cancellation intent.
        if generation_done(
            report.state.phase,
            report.session_end,
            report.state.output_pending,
            report.state.exit_retained,
            force,
        ) {
            if force {
                return Err(HostSessionError::new("generation", "cancelled"));
            }
            if report.session_end == Some(SessionEnd::Failed) {
                return Err(HostSessionError::new("generation", "session_failed"));
            }
            return Ok(());
        }
        park(PARK);
    }
}

fn generation_done(
    phase: Phase,
    end: Option<SessionEnd>,
    output_pending: bool,
    exit_retained: bool,
    cancelled: bool,
) -> bool {
    phase == Phase::Empty
        && (cancelled
            || end == Some(SessionEnd::Failed)
            || (end == Some(SessionEnd::ParentEof) && !output_pending && !exit_retained))
}

fn run_cleanup<L: CleanupTarget>(
    mut cleanup: PreOwnedCleanup<L>,
    clock: &mut impl FnMut() -> Instant,
    park: &mut impl FnMut(Duration),
) -> Result<(), HostSessionError> {
    let mut first_error = None;
    loop {
        let now = clock();
        match cleanup.step(now) {
            Ok(TreeObservation::ConfirmedEmpty) => {
                let reason = if first_error.is_some() {
                    "cleanup_failed"
                } else {
                    "generation_failed"
                };
                return Err(HostSessionError::new("cleanup", reason));
            }
            Ok(TreeObservation::Present | TreeObservation::Unconfirmed) => {}
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
        park(PARK);
    }
}

fn run_rejected<L: CleanupTarget>(
    ports: &mut HostPorts,
    reply: Reply,
    cleanup: Option<PreOwnedCleanup<L>>,
    clock: &mut impl FnMut() -> Instant,
    park: &mut impl FnMut(Duration),
) -> Result<(), HostSessionError> {
    let (frame, encode_failed) = match encode_reply(&reply) {
        Ok(frame) => (Some(frame), false),
        Err(_) => (None, true),
    };
    let mut cleanup = cleanup;
    let mut queued = false;
    let mut output_done = false;
    let mut output_failed = encode_failed;
    let mut first_cleanup_error = None;
    loop {
        let now = clock();
        let mut tree_empty = cleanup.is_none();
        if let Some(owner) = cleanup.as_mut() {
            match owner.step(now) {
                Ok(TreeObservation::ConfirmedEmpty) => tree_empty = true,
                Ok(TreeObservation::Present | TreeObservation::Unconfirmed) => {}
                Err(error) => {
                    if first_cleanup_error.is_none() {
                        first_cleanup_error = Some(error);
                    }
                }
            }
        }
        if !queued && !output_failed {
            let (_, output, _) = ports.borrow();
            if let Some(frame) = frame.as_ref() {
                match output.put(frame.clone(), now) {
                    Ok(()) => queued = true,
                    Err(PutFrameError::Busy(_)) => {}
                    Err(PutFrameError::TooLarge(_) | PutFrameError::Closed(_)) => {
                        output_failed = true
                    }
                }
            } else {
                output_failed = true;
            }
        }
        if queued && !output_done && !output_failed {
            let (_, output, _) = ports.borrow();
            match output.step(now) {
                Ok(WriteStep::Complete) => output_done = true,
                Ok(WriteStep::Idle | WriteStep::Pending) => {}
                Err(_) => output_failed = true,
            }
        }
        let transport_done = output_done || output_failed;
        if tree_empty && transport_done {
            return Err(HostSessionError::new(
                if encode_failed && !queued {
                    "reply"
                } else {
                    "launch"
                },
                if encode_failed && !queued {
                    "encode_failed"
                } else if first_cleanup_error.is_some() {
                    "rejected_cleanup_failed"
                } else {
                    "launch_rejected"
                },
            ));
        }
        park(PARK);
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        actor::Phase,
        pre_owned_cleanup::CleanupTarget,
        target::{ExitObservation, TreeObservation},
        worker_slots::WorkerSlots,
    };
    use agent24_sidecar_host_protocol::{ErrorCode, PROTOCOL_VERSION, decode_reply};
    use std::{
        collections::VecDeque,
        io::{self, Read, Write},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct BadRead;
    impl Read for BadRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::InvalidData.into())
        }
    }
    struct Sink(Option<Arc<Mutex<Vec<u8>>>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if let Some(out) = &self.0 {
                out.lock().unwrap().extend_from_slice(bytes);
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::ErrorKind::BrokenPipe.into())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct Fake {
        results: VecDeque<TreeObservation>,
        calls: Arc<AtomicUsize>,
    }
    impl CleanupTarget for Fake {
        fn observe_exit(&mut self) -> io::Result<ExitObservation> {
            Ok(ExitObservation::Running)
        }
        fn stop(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn reap(
            &mut self,
            _: &mut Phase,
        ) -> Result<TreeObservation, crate::cleanup::CleanupStepError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let tree = self.results.pop_front().expect("scripted result");
            Ok(tree)
        }
    }
    fn cleanup(
        results: impl IntoIterator<Item = TreeObservation>,
        now: Instant,
    ) -> (PreOwnedCleanup<Fake>, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        (
            PreOwnedCleanup::new(
                Fake {
                    results: results.into_iter().collect(),
                    calls: calls.clone(),
                },
                Phase::ForceStopping(now + Duration::from_secs(30)),
                limits(),
                true,
            ),
            calls,
        )
    }
    fn ports(captured: Option<Arc<Mutex<Vec<u8>>>>) -> HostPorts {
        HostPorts::new_in(
            WorkerSlots::isolated(),
            io::empty(),
            Sink(captured),
            Duration::from_secs(1),
        )
        .unwrap()
    }
    fn reply(id: u64) -> Reply {
        Reply::Error {
            version: PROTOCOL_VERSION,
            request_id: id,
            code: ErrorCode::LaunchFailed,
        }
    }

    fn limits() -> Deadlines {
        Deadlines {
            launch: Duration::from_secs(1),
            ready: Duration::from_secs(1),
            graceful: Duration::from_millis(10),
            force: Duration::from_secs(1),
            drain: Duration::from_secs(1),
        }
    }

    #[test]
    fn ingress_eof_and_fatal_are_the_only_prelaunch_terminals() {
        let mut empty = ports(None);
        assert_eq!(
            run_session(
                &mut empty,
                Duration::from_secs(1),
                limits(),
                Instant::now,
                |_| std::thread::sleep(Duration::from_millis(1)),
                || false
            ),
            Ok(())
        );
        let mut bad = HostPorts::new_in(
            WorkerSlots::isolated(),
            BadRead,
            Sink(None),
            Duration::from_secs(1),
        )
        .unwrap();
        let error = run_session(
            &mut bad,
            Duration::from_secs(1),
            limits(),
            Instant::now,
            |_| std::thread::sleep(Duration::from_millis(1)),
            || false,
        )
        .unwrap_err();
        assert_eq!((error.stage, error.reason), ("ingress", "control_failed"));
    }

    #[test]
    fn generation_end_requires_empty_and_session_flush() {
        let cases = [
            (None, false, false, false, false),
            (Some(SessionEnd::ParentEof), true, false, false, false),
            (Some(SessionEnd::ParentEof), false, true, false, false),
            (Some(SessionEnd::Failed), true, true, false, true),
            (None, true, true, true, true),
            (Some(SessionEnd::ParentEof), false, false, false, true),
        ];
        for (end, output, exit, cancel, expected) in cases {
            assert_eq!(
                generation_done(Phase::Empty, end, output, exit, cancel),
                expected
            );
        }
        assert!(!generation_done(
            Phase::Running,
            Some(SessionEnd::ParentEof),
            false,
            false,
            false
        ));
        assert!(!generation_done(
            Phase::Running,
            Some(SessionEnd::Failed),
            false,
            false,
            false
        ));
    }

    #[test]
    fn rejected_reply_is_once_and_cleanup_survives_broken_output() {
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let mut output = ports(Some(bytes.clone()));
        let now = Instant::now();
        let (owner, _) = cleanup(
            [
                TreeObservation::Unconfirmed,
                TreeObservation::ConfirmedEmpty,
            ],
            now,
        );
        let result = run_rejected(
            &mut output,
            reply(77),
            Some(owner),
            &mut || now,
            &mut |wait| std::thread::sleep(wait),
        );
        assert_eq!(result.unwrap_err().reason, "launch_rejected");
        assert_eq!(decode_reply(&bytes.lock().unwrap()).unwrap(), reply(77));
        assert_eq!(
            bytes
                .lock()
                .unwrap()
                .iter()
                .filter(|b| **b == b'\n')
                .count(),
            1
        );

        let mut broken = HostPorts::new_in(
            WorkerSlots::isolated(),
            io::empty(),
            Broken,
            Duration::from_millis(20),
        )
        .unwrap();
        let (owner, calls) = cleanup(
            [
                TreeObservation::Unconfirmed,
                TreeObservation::ConfirmedEmpty,
            ],
            now,
        );
        let result = run_rejected(
            &mut broken,
            reply(91),
            Some(owner),
            &mut Instant::now,
            &mut |wait| std::thread::sleep(wait),
        );
        assert_eq!(result.unwrap_err().reason, "launch_rejected");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn cleanup_dispatch_path_has_no_launch_failed_reply() {
        let now = Instant::now();
        let (owner, calls) = cleanup([TreeObservation::ConfirmedEmpty], now);
        assert_eq!(
            run_cleanup(owner, &mut || now, &mut |_| {})
                .unwrap_err()
                .reason,
            "generation_failed"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
