use crate::output_io::{OutputWriteError, OutputWriter, PutFrameError, WriteStep};
use std::{
    io::{self, Write},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    thread,
    time::Duration,
};

type ResultFrame = Result<(), OutputWriteError>;

/// One detached worker is created per adapter instance; actor calls only poll channels.
/// A generic blocked `Write` may outlive `Drop`. This type is crate-private and unwired:
/// future runtime wiring must use one host-lifetime instance, never one per generation.
/// Replacement requires a global worker permit or cancellable OS I/O; this does not cancel.
pub(crate) struct OutputWorker {
    commands: SyncSender<Vec<u8>>,
    results: Receiver<ResultFrame>,
    in_flight: bool,
    terminal: bool,
}

impl OutputWorker {
    pub(crate) fn new<W: Write + Send + 'static>(writer: W) -> io::Result<Self> {
        let (command_tx, command_rx) = mpsc::sync_channel::<Vec<u8>>(1);
        let (result_tx, result_rx) = mpsc::sync_channel::<ResultFrame>(1);
        thread::Builder::new()
            .name("sidecar-stdout".into())
            .spawn(move || output_loop(OutputWriter::new(writer), command_rx, result_tx))?;
        Ok(Self {
            commands: command_tx,
            results: result_rx,
            in_flight: false,
            terminal: false,
        })
    }

    pub(crate) fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError> {
        if self.terminal {
            return Err(PutFrameError::Closed(frame));
        }
        if frame.len() > agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES {
            return Err(PutFrameError::TooLarge(frame));
        }
        if self.in_flight {
            return Err(PutFrameError::Busy(frame));
        }
        match self.commands.try_send(frame) {
            Ok(()) => {
                self.in_flight = true;
                Ok(())
            }
            Err(TrySendError::Full(frame)) => Err(PutFrameError::Busy(frame)),
            Err(TrySendError::Disconnected(frame)) => {
                self.terminal = true;
                Err(PutFrameError::Closed(frame))
            }
        }
    }

    pub(crate) fn step(&mut self) -> Result<WriteStep, OutputWriteError> {
        if self.terminal {
            return Err(OutputWriteError::Closed);
        }
        if !self.in_flight {
            return Ok(WriteStep::Idle);
        }
        match self.results.try_recv() {
            Ok(Ok(())) => {
                self.in_flight = false;
                Ok(WriteStep::Complete)
            }
            Ok(Err(error)) => {
                self.in_flight = false;
                self.terminal = true;
                Err(error)
            }
            Err(TryRecvError::Empty) => Ok(WriteStep::Pending),
            Err(TryRecvError::Disconnected) => {
                self.in_flight = false;
                self.terminal = true;
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

    fn worker(script: Arc<Mutex<Script>>) -> OutputWorker {
        OutputWorker::new(ScriptWriter(script)).unwrap()
    }

    fn wait_step(worker: &mut OutputWorker) -> Result<WriteStep, OutputWriteError> {
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let step = worker.step();
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
        worker.put(b"exact-frame\n".to_vec()).unwrap();
        assert_eq!(
            worker.put(b"second\n".to_vec()),
            Err(PutFrameError::Busy(b"second\n".to_vec()))
        );
        assert_eq!(wait_step(&mut worker), Ok(WriteStep::Complete));
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
        let too_large = vec![0; agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES + 1];
        assert_eq!(
            worker.put(too_large.clone()),
            Err(PutFrameError::TooLarge(too_large))
        );
        let frame = vec![b'x'; agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES];
        worker.put(frame.clone()).unwrap();
        assert_eq!(wait_step(&mut worker), Ok(WriteStep::Complete));
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
            worker.put(b"failure\n".to_vec()).unwrap();
            assert_eq!(wait_step(&mut worker), Err(expected));
            assert_eq!(
                worker.put(b"again\n".to_vec()),
                Err(PutFrameError::Closed(b"again\n".to_vec()))
            );
            assert_eq!(worker.step(), Err(OutputWriteError::Closed));
        }
    }

    #[test]
    fn result_channel_disconnect_is_terminal_without_blocking() {
        let script = Arc::new(Mutex::new(Script::default()));
        let mut worker = worker(script);
        let (_, disconnected) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut worker.results, disconnected);
        drop(old);
        worker.put(b"one-frame\n".to_vec()).unwrap();
        assert_eq!(worker.step(), Err(OutputWriteError::Closed));
        assert_eq!(
            worker.put(b"again\n".to_vec()),
            Err(PutFrameError::Closed(b"again\n".to_vec()))
        );
    }
}
