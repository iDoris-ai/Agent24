use crate::output_io::{OutputWriteError, OutputWriter, PutFrameError, WriteStep};
use crate::worker_slots::{WorkerRole, WorkerSlotError, WorkerSlots, hold_permit};
use std::{
    io::{self, Write},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::{Duration, Instant},
};

type ResultFrame = Result<(), OutputWriteError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    InFlight { deadline: Instant },
    Closed,
}

/// One detached worker is created per adapter instance; actor calls only poll channels.
/// A generic blocked `Write` may outlive `Drop`. This type is crate-private and unwired:
/// future runtime wiring must use one host-lifetime instance, never one per generation.
/// Replacement requires a global worker permit or cancellable OS I/O; this does not cancel.
pub(crate) struct OutputWorker {
    commands: SyncSender<Vec<u8>>,
    results: Receiver<ResultFrame>,
    budget: Duration,
    state: State,
}

impl OutputWorker {
    pub(crate) fn new_in<W: Write + Send + 'static>(
        slots: &'static WorkerSlots,
        writer: W,
        budget: Duration,
    ) -> Result<Self, WorkerSlotError> {
        let (command_tx, command_rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let (result_tx, result_rx) = mpsc::sync_channel::<ResultFrame>(1);
        slots.spawn(WorkerRole::Output, "sidecar-stdout", move |permit| {
            hold_permit(permit, || {
                output_loop(OutputWriter::new(writer), command_rx, result_tx)
            })
        })?;
        Ok(Self {
            commands: command_tx,
            results: result_rx,
            budget,
            state: State::Idle,
        })
    }

    #[cfg(test)]
    pub(crate) fn new<W: Write + Send + 'static>(
        writer: W,
        budget: Duration,
    ) -> Result<Self, WorkerSlotError> {
        Self::new_in(WorkerSlots::isolated(), writer, budget)
    }

    pub(crate) fn put(&mut self, frame: Vec<u8>, now: Instant) -> Result<(), PutFrameError> {
        if self.state == State::Closed {
            return Err(PutFrameError::Closed(frame));
        }
        if frame.len() > agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES {
            return Err(PutFrameError::TooLarge(frame));
        }
        if matches!(self.state, State::InFlight { .. }) {
            return Err(PutFrameError::Busy(frame));
        }
        match self.commands.try_send(frame) {
            Ok(()) => {
                // A deadline covers both admission and the actor observing a
                // completed flush. Overflow is fail-closed rather than a
                // panic or an unbounded write lease.
                let deadline = now.checked_add(self.budget).unwrap_or(now);
                self.state = State::InFlight { deadline };
                Ok(())
            }
            Err(TrySendError::Full(frame)) => Err(PutFrameError::Busy(frame)),
            Err(TrySendError::Disconnected(frame)) => {
                self.state = State::Closed;
                Err(PutFrameError::Closed(frame))
            }
        }
    }

    pub(crate) fn step(&mut self, now: Instant) -> Result<WriteStep, OutputWriteError> {
        let State::InFlight { deadline } = self.state else {
            return if self.state == State::Closed {
                Err(OutputWriteError::Closed)
            } else {
                Ok(WriteStep::Idle)
            };
        };
        // The deadline has priority over a result already waiting in the
        // channel: at the boundary, never claim a flush was observed in time.
        if now >= deadline {
            self.state = State::Closed;
            return Err(OutputWriteError::Io(io::ErrorKind::TimedOut));
        }
        match self.results.try_recv() {
            Ok(Ok(())) => {
                self.state = State::Idle;
                Ok(WriteStep::Complete)
            }
            Ok(Err(error)) => {
                self.state = State::Closed;
                Err(error)
            }
            Err(TryRecvError::Empty) => Ok(WriteStep::Pending),
            Err(TryRecvError::Disconnected) => {
                self.state = State::Closed;
                Err(OutputWriteError::Closed)
            }
        }
    }
}

fn output_loop<W: Write>(
    mut writer: OutputWriter<W>,
    commands: Receiver<Vec<u8>>,
    results: SyncSender<ResultFrame>,
) {
    while let Ok(frame) = commands.recv() {
        let result = match writer.put(frame) {
            Ok(()) => loop {
                match writer.write_step() {
                    Ok(WriteStep::Complete) => break Ok(()),
                    Ok(WriteStep::Pending) => thread::sleep(Duration::from_millis(1)),
                    Ok(WriteStep::Idle) => break Err(OutputWriteError::Closed),
                    Err(error) => break Err(error),
                }
            },
            Err(_) => Err(OutputWriteError::Closed),
        };
        let failed = result.is_err();
        if results.send(result).is_err() {
            return;
        }
        if failed {
            return;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{
        collections::VecDeque,
        sync::{Arc, Mutex},
        time::{Duration, Instant},
    };

    #[derive(Default)]
    struct Script {
        writes: VecDeque<Result<usize, io::ErrorKind>>,
        flushes: VecDeque<Result<(), io::ErrorKind>>,
        bytes: Vec<u8>,
        flushed: usize,
    }

    struct ScriptWriter(Arc<Mutex<Script>>);

    impl Write for ScriptWriter {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let mut script = self.0.lock().unwrap();
            let next = script.writes.pop_front().unwrap_or(Ok(input.len()));
            let count = next.map_err(io::Error::from)?.min(input.len());
            script.bytes.extend_from_slice(&input[..count]);
            Ok(count)
        }

        fn flush(&mut self) -> io::Result<()> {
            let mut script = self.0.lock().unwrap();
            script.flushed += 1;
            script
                .flushes
                .pop_front()
                .unwrap_or(Ok(()))
                .map_err(io::Error::from)
        }
    }

    const BUDGET: Duration = Duration::from_secs(3);

    fn worker(script: Arc<Mutex<Script>>) -> OutputWorker {
        OutputWorker::new(ScriptWriter(script), BUDGET).unwrap()
    }

    fn worker_with_budget(script: Arc<Mutex<Script>>, budget: Duration) -> OutputWorker {
        OutputWorker::new(ScriptWriter(script), budget).unwrap()
    }

    fn wait_step(worker: &mut OutputWorker, now: Instant) -> Result<WriteStep, OutputWriteError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let step = worker.step(now);
            if step != Ok(WriteStep::Pending) || Instant::now() >= deadline {
                return step;
            }
            thread::yield_now();
        }
    }

    #[test]
    fn short_interrupted_and_would_block_writes_preserve_one_exact_frame() {
        let script = Arc::new(Mutex::new(Script {
            writes: [
                Err(io::ErrorKind::Interrupted),
                Err(io::ErrorKind::WouldBlock),
                Err(io::ErrorKind::WouldBlock),
                Err(io::ErrorKind::WouldBlock),
                Ok(2),
            ]
            .into(),
            flushes: [Err(io::ErrorKind::WouldBlock), Ok(())].into(),
            ..Script::default()
        }));
        let mut worker = worker(script.clone());
        let now = Instant::now();
        worker.put(b"exact-frame\n".to_vec(), now).unwrap();
        assert_eq!(
            worker.put(b"second\n".to_vec(), now),
            Err(PutFrameError::Busy(b"second\n".to_vec()))
        );
        assert_eq!(wait_step(&mut worker, now), Ok(WriteStep::Complete));
        let state = script.lock().unwrap();
        assert_eq!(state.bytes, b"exact-frame\n");
        assert_eq!(state.flushed, 2);
        assert!(
            state.writes.is_empty(),
            "sustained WouldBlock retries were skipped"
        );
    }

    #[test]
    fn frame_limit_is_enforced_and_boundary_frame_is_moved_once() {
        let script = Arc::new(Mutex::new(Script::default()));
        let mut worker = worker(script.clone());
        let now = Instant::now();
        let too_large = vec![0; agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES + 1];
        assert_eq!(
            worker.put(too_large.clone(), now),
            Err(PutFrameError::TooLarge(too_large))
        );
        let frame = vec![b'x'; agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES];
        worker.put(frame.clone(), now).unwrap();
        assert_eq!(wait_step(&mut worker, now), Ok(WriteStep::Complete));
        assert_eq!(script.lock().unwrap().bytes, frame);
    }

    #[test]
    fn write_zero_broken_pipe_and_flush_failure_are_terminal() {
        for (writes, flushes, expected) in [
            (
                vec![Ok(0)],
                vec![],
                OutputWriteError::Io(io::ErrorKind::WriteZero),
            ),
            (
                vec![Err(io::ErrorKind::BrokenPipe)],
                vec![],
                OutputWriteError::Io(io::ErrorKind::BrokenPipe),
            ),
            (
                vec![Ok(8)],
                vec![Err(io::ErrorKind::Other)],
                OutputWriteError::Io(io::ErrorKind::Other),
            ),
        ] {
            let script = Arc::new(Mutex::new(Script {
                writes: writes.into(),
                flushes: flushes.into(),
                ..Script::default()
            }));
            let mut worker = worker(script);
            let now = Instant::now();
            worker.put(b"failure\n".to_vec(), now).unwrap();
            assert_eq!(wait_step(&mut worker, now), Err(expected));
            assert_eq!(
                worker.put(b"again\n".to_vec(), now),
                Err(PutFrameError::Closed(b"again\n".to_vec()))
            );
            assert_eq!(worker.step(now), Err(OutputWriteError::Closed));
        }
    }

    #[test]
    fn result_channel_disconnect_is_terminal_without_blocking() {
        let script = Arc::new(Mutex::new(Script::default()));
        let mut worker = worker(script);
        let (_, disconnected) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut worker.results, disconnected);
        drop(old);
        let now = Instant::now();
        worker.put(b"one-frame\n".to_vec(), now).unwrap();
        assert_eq!(worker.step(now), Err(OutputWriteError::Closed));
        assert_eq!(
            worker.put(b"again\n".to_vec(), now),
            Err(PutFrameError::Closed(b"again\n".to_vec()))
        );
    }

    #[test]
    fn admission_starts_one_fixed_deadline_and_completion_returns_to_idle() {
        let now = Instant::now();
        let budget = Duration::from_secs(1);
        let mut worker = worker_with_budget(Arc::new(Mutex::new(Script::default())), budget);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut worker.results, result_rx);
        drop(old);

        worker.put(b"one\n".to_vec(), now).unwrap();
        result_tx.send(Ok(())).unwrap();
        assert_eq!(
            worker.step(now + budget - Duration::from_nanos(1)),
            Ok(WriteStep::Complete)
        );
        assert_eq!(worker.step(now + budget), Ok(WriteStep::Idle));
    }

    #[test]
    fn failed_admission_does_not_create_or_refresh_a_deadline() {
        let now = Instant::now();
        let budget = Duration::from_secs(1);
        let mut worker = worker_with_budget(Arc::new(Mutex::new(Script::default())), budget);
        let (command_tx, command_rx) = mpsc::sync_channel(1);
        command_tx.send(vec![0]).unwrap();
        worker.commands = command_tx;
        assert_eq!(
            worker.put(b"full\n".to_vec(), now),
            Err(PutFrameError::Busy(b"full\n".to_vec()))
        );
        assert_eq!(worker.step(now + budget), Ok(WriteStep::Idle));
        drop(command_rx);

        let too_large = vec![0; agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES + 1];
        assert_eq!(
            worker.put(too_large.clone(), now + budget),
            Err(PutFrameError::TooLarge(too_large))
        );
        assert_eq!(worker.step(now + budget), Ok(WriteStep::Idle));
    }

    #[test]
    fn pending_before_deadline_and_busy_admission_do_not_extend_it() {
        let now = Instant::now();
        let budget = Duration::from_secs(1);
        let mut worker = worker_with_budget(Arc::new(Mutex::new(Script::default())), budget);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut worker.results, result_rx);
        drop(old);

        worker.put(b"held\n".to_vec(), now).unwrap();
        assert_eq!(
            worker.step(now + budget - Duration::from_nanos(1)),
            Ok(WriteStep::Pending)
        );
        assert_eq!(
            worker.put(b"second\n".to_vec(), now + budget - Duration::from_nanos(1)),
            Err(PutFrameError::Busy(b"second\n".to_vec()))
        );
        assert_eq!(
            worker.step(now + budget),
            Err(OutputWriteError::Io(io::ErrorKind::TimedOut))
        );
        drop(result_tx);
    }

    #[test]
    fn command_channel_disconnect_is_terminal_without_creating_a_lease() {
        let now = Instant::now();
        let mut worker = worker(Arc::new(Mutex::new(Script::default())));
        let (commands, receiver) = mpsc::sync_channel(1);
        drop(receiver);
        worker.commands = commands;

        assert_eq!(
            worker.put(b"closed\n".to_vec(), now),
            Err(PutFrameError::Closed(b"closed\n".to_vec()))
        );
        assert_eq!(worker.step(now), Err(OutputWriteError::Closed));
    }

    #[test]
    fn deadline_is_inclusive_and_has_priority_over_a_queued_completion() {
        let now = Instant::now();
        let budget = Duration::from_secs(1);
        let mut worker = worker_with_budget(Arc::new(Mutex::new(Script::default())), budget);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut worker.results, result_rx);
        drop(old);

        worker.put(b"late\n".to_vec(), now).unwrap();
        result_tx.send(Ok(())).unwrap();
        assert_eq!(
            worker.step(now + budget),
            Err(OutputWriteError::Io(io::ErrorKind::TimedOut))
        );
        assert_eq!(worker.step(now), Err(OutputWriteError::Closed));
        assert_eq!(
            worker.put(b"again\n".to_vec(), now),
            Err(PutFrameError::Closed(b"again\n".to_vec()))
        );
    }

    #[test]
    fn zero_and_overflow_budgets_expire_without_panicking() {
        let now = Instant::now();
        for budget in [Duration::ZERO, Duration::MAX] {
            let mut worker = worker_with_budget(Arc::new(Mutex::new(Script::default())), budget);
            worker.put(b"deadline\n".to_vec(), now).unwrap();
            assert_eq!(
                worker.step(now),
                Err(OutputWriteError::Io(io::ErrorKind::TimedOut))
            );
        }
    }
}
