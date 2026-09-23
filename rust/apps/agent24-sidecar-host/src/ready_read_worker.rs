use crate::pipe_access::NativeStdout;
use crate::worker_slots::{WorkerRole, WorkerSlotError, WorkerSlots, hold_permit};
use std::{
    io::{ErrorKind, Read},
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
};

/// The maximum data returned for one actor credit.  This worker owns no
/// framing state, so callers may safely feed each chunk to their own parser.
pub(crate) const READ_CHUNK_BYTES: usize = 4096;

type WorkerResult = Result<ReadyRead, ReadyReadError>;

/// The actor's bounded admission token for one stdout read attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyReadPermitError {
    Busy,
    Closed,
}

/// A terminal transport failure.  Policy, parsing, and readiness deadlines
/// deliberately live above this worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyReadError {
    Closed,
    Io(ErrorKind),
}

/// The outcome of exactly one admitted bounded read.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadyRead {
    Pending,
    Chunk {
        bytes: [u8; READ_CHUNK_BYTES],
        len: usize,
    },
    Eof,
}

/// One non-blocking observation of the worker admitted by a permit.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ReadyReadStep {
    Idle,
    Pending,
    Complete(ReadyRead),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    InFlight,
    Closed,
}

/// A single detached stdout reader for a target generation.
///
/// This is intentionally transport-only and dormant: it neither owns a
/// `ReadyGate` nor parses tokens or protocol frames.  A permit is a credit,
/// not a queued operation.  It authorizes exactly one bounded `Read`; the
/// actor only calls `try_send` and `try_recv` through `permit` and `step`.
/// The result is retained as busy until the actor consumes it.
///
/// Dropping this object never joins the thread.  Generic blocking `Read`
/// cannot be cancelled by `Drop`, so a blocked read may outlive the worker.
/// Future actor wiring must retain one fixed worker per moved stdout pipe; it
/// must not pre-read, replace a blocked worker, or spawn per chunk.
pub(crate) struct ReadyReadWorker {
    credits: SyncSender<()>,
    results: Receiver<WorkerResult>,
    state: State,
}

impl ReadyReadWorker {
    /// Move a native target stdout into the one synchronous reading thread.
    ///
    /// Unix stdout is already a synchronous `Read`.  On Windows this consumes
    /// Tokio's async handle before any `AsyncRead` poll and converts the owned
    /// handle to a `std::fs::File`; no raw-handle clone or competing reader is
    /// created.
    #[cfg(unix)]
    pub(crate) fn from_native_stdout(stdout: NativeStdout) -> Result<Self, WorkerSlotError> {
        Self::new_in(WorkerSlots::host(), stdout)
    }

    #[cfg(windows)]
    pub(crate) fn from_native_stdout(stdout: NativeStdout) -> Result<Self, WorkerSlotError> {
        let file = std::fs::File::from(
            stdout
                .into_owned_handle()
                .map_err(|error| WorkerSlotError::Spawn(error.kind()))?,
        );
        Self::new_in(WorkerSlots::host(), file)
    }

    pub(crate) fn new_in<R: Read + Send + 'static>(
        slots: &'static WorkerSlots,
        reader: R,
    ) -> Result<Self, WorkerSlotError> {
        let (credit_tx, credit_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        slots.spawn(WorkerRole::ReadyRead, "sidecar-ready-read", move |permit| {
            hold_permit(permit, || ready_read_loop(reader, credit_rx, result_tx))
        })?;
        Ok(Self {
            credits: credit_tx,
            results: result_rx,
            state: State::Idle,
        })
    }

    #[cfg(test)]
    pub(crate) fn new<R: Read + Send + 'static>(reader: R) -> Result<Self, WorkerSlotError> {
        Self::new_in(WorkerSlots::isolated(), reader)
    }

    /// Admit exactly one bounded read without blocking the actor.
    pub(crate) fn permit(&mut self) -> Result<(), ReadyReadPermitError> {
        if self.state == State::Closed {
            return Err(ReadyReadPermitError::Closed);
        }
        if self.state == State::InFlight {
            return Err(ReadyReadPermitError::Busy);
        }
        match self.credits.try_send(()) {
            Ok(()) => {
                self.state = State::InFlight;
                Ok(())
            }
            Err(TrySendError::Full(())) => Err(ReadyReadPermitError::Busy),
            Err(TrySendError::Disconnected(())) => {
                self.state = State::Closed;
                Err(ReadyReadPermitError::Closed)
            }
        }
    }

    /// Observe a result without doing I/O on the caller's thread.
    pub(crate) fn step(&mut self) -> Result<ReadyReadStep, ReadyReadError> {
        if self.state == State::Closed {
            return Err(ReadyReadError::Closed);
        }
        if self.state == State::Idle {
            return Ok(ReadyReadStep::Idle);
        }
        match self.results.try_recv() {
            Ok(Ok(ReadyRead::Eof)) => {
                self.state = State::Closed;
                Ok(ReadyReadStep::Complete(ReadyRead::Eof))
            }
            Ok(Ok(read)) => {
                self.state = State::Idle;
                Ok(ReadyReadStep::Complete(read))
            }
            Ok(Err(error)) => {
                self.state = State::Closed;
                Err(error)
            }
            Err(TryRecvError::Empty) => Ok(ReadyReadStep::Pending),
            Err(TryRecvError::Disconnected) => {
                self.state = State::Closed;
                Err(ReadyReadError::Closed)
            }
        }
    }
}

fn ready_read_loop<R: Read>(
    mut reader: R,
    credits: Receiver<()>,
    results: SyncSender<WorkerResult>,
) {
    while credits.recv().is_ok() {
        let result = read_once(&mut reader);
        let terminal = matches!(&result, Ok(ReadyRead::Eof) | Err(_));
        if results.send(result).is_err() || terminal {
            return;
        }
    }
}

fn read_once<R: Read>(reader: &mut R) -> WorkerResult {
    let mut bytes = [0; READ_CHUNK_BYTES];
    match reader.read(&mut bytes) {
        Ok(0) => Ok(ReadyRead::Eof),
        Ok(len) => Ok(ReadyRead::Chunk { bytes, len }),
        Err(error) if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) => {
            Ok(ReadyRead::Pending)
        }
        Err(error) => Err(ReadyReadError::Io(error.kind())),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{
        collections::{HashSet, VecDeque},
        io,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };

    const WAIT: Duration = Duration::from_secs(2);

    struct ScriptedRead {
        steps: VecDeque<Result<Vec<u8>, ErrorKind>>,
        pending: Vec<u8>,
        calls: Arc<AtomicUsize>,
        threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
    }

    impl Read for ScriptedRead {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.threads.lock().unwrap().insert(thread::current().id());
            if self.pending.is_empty() {
                match self.steps.pop_front().unwrap_or(Err(ErrorKind::WouldBlock)) {
                    Ok(bytes) => self.pending = bytes,
                    Err(kind) => return Err(io::Error::from(kind)),
                }
            }
            let len = self.pending.len().min(output.len());
            output[..len].copy_from_slice(&self.pending[..len]);
            self.pending.drain(..len);
            Ok(len)
        }
    }

    fn reader(
        steps: Vec<Result<Vec<u8>, ErrorKind>>,
    ) -> (
        ScriptedRead,
        Arc<AtomicUsize>,
        Arc<Mutex<HashSet<thread::ThreadId>>>,
    ) {
        let calls = Arc::new(AtomicUsize::new(0));
        let threads = Arc::new(Mutex::new(HashSet::new()));
        (
            ScriptedRead {
                steps: steps.into(),
                pending: Vec::new(),
                calls: calls.clone(),
                threads: threads.clone(),
            },
            calls,
            threads,
        )
    }

    fn complete(worker: &mut ReadyReadWorker) -> Result<ReadyReadStep, ReadyReadError> {
        let deadline = Instant::now() + WAIT;
        loop {
            let step = worker.step();
            if step != Ok(ReadyReadStep::Pending) || Instant::now() >= deadline {
                return step;
            }
            thread::yield_now();
        }
    }

    fn chunk(step: ReadyReadStep) -> Vec<u8> {
        match step {
            ReadyReadStep::Complete(ReadyRead::Chunk { bytes, len }) => bytes[..len].to_vec(),
            other => panic!("expected chunk, got {other:?}"),
        }
    }

    #[test]
    fn no_credit_means_no_read_and_busy_lasts_until_result_consumption() {
        let (input, calls, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut worker = ReadyReadWorker::new(input).unwrap();
        assert_eq!(worker.step(), Ok(ReadyReadStep::Idle));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        worker.permit().unwrap();
        assert_eq!(worker.permit(), Err(ReadyReadPermitError::Busy));
        assert_eq!(
            complete(&mut worker),
            Ok(ReadyReadStep::Complete(ReadyRead::Pending))
        );
        worker.permit().unwrap();
    }

    #[test]
    fn each_credit_yields_one_bounded_partial_chunk_or_pending() {
        let oversized = vec![b'x'; READ_CHUNK_BYTES + 7];
        let (input, calls, _) = reader(vec![
            Ok(b"partial".to_vec()),
            Err(ErrorKind::Interrupted),
            Err(ErrorKind::WouldBlock),
            Ok(oversized.clone()),
        ]);
        let mut worker = ReadyReadWorker::new(input).unwrap();

        worker.permit().unwrap();
        assert_eq!(chunk(complete(&mut worker).unwrap()), b"partial");
        worker.permit().unwrap();
        assert_eq!(
            complete(&mut worker),
            Ok(ReadyReadStep::Complete(ReadyRead::Pending))
        );
        worker.permit().unwrap();
        assert_eq!(
            complete(&mut worker),
            Ok(ReadyReadStep::Complete(ReadyRead::Pending))
        );
        worker.permit().unwrap();
        let first = chunk(complete(&mut worker).unwrap());
        assert_eq!(first.len(), READ_CHUNK_BYTES);
        assert_eq!(first, oversized[..READ_CHUNK_BYTES]);
        worker.permit().unwrap();
        assert_eq!(
            chunk(complete(&mut worker).unwrap()),
            oversized[READ_CHUNK_BYTES..]
        );
        assert_eq!(calls.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn continuous_chunks_use_the_same_single_worker_thread() {
        let (input, _, threads) = reader(vec![Ok(b"one".to_vec()), Ok(b"two".to_vec())]);
        let mut worker = ReadyReadWorker::new(input).unwrap();
        worker.permit().unwrap();
        assert_eq!(chunk(complete(&mut worker).unwrap()), b"one");
        worker.permit().unwrap();
        assert_eq!(chunk(complete(&mut worker).unwrap()), b"two");
        assert_eq!(threads.lock().unwrap().len(), 1);
    }

    #[test]
    fn eof_and_errors_are_terminal() {
        let (input, _, _) = reader(vec![Ok(Vec::new())]);
        let mut eof = ReadyReadWorker::new(input).unwrap();
        eof.permit().unwrap();
        assert_eq!(
            complete(&mut eof),
            Ok(ReadyReadStep::Complete(ReadyRead::Eof))
        );
        assert_eq!(eof.permit(), Err(ReadyReadPermitError::Closed));
        assert_eq!(eof.step(), Err(ReadyReadError::Closed));

        let (input, _, _) = reader(vec![Err(ErrorKind::BrokenPipe)]);
        let mut failed = ReadyReadWorker::new(input).unwrap();
        failed.permit().unwrap();
        assert_eq!(
            complete(&mut failed),
            Err(ReadyReadError::Io(ErrorKind::BrokenPipe))
        );
        assert_eq!(failed.permit(), Err(ReadyReadPermitError::Closed));

        let (input, _, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut disconnected = ReadyReadWorker::new(input).unwrap();
        let (_, replacement) = mpsc::sync_channel(1);
        let original = std::mem::replace(&mut disconnected.results, replacement);
        drop(original);
        disconnected.permit().unwrap();
        assert_eq!(disconnected.step(), Err(ReadyReadError::Closed));
        assert_eq!(disconnected.permit(), Err(ReadyReadPermitError::Closed));
    }

    struct BlockingRead {
        started: SyncSender<()>,
        release: Receiver<()>,
    }

    impl Read for BlockingRead {
        fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
            let _ = self.started.send(());
            let _ = self.release.recv();
            Ok(0)
        }
    }

    #[test]
    fn silent_pipe_never_blocks_actor_or_drop() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let mut worker = ReadyReadWorker::new(BlockingRead {
            started: started_tx,
            release: release_rx,
        })
        .unwrap();
        worker.permit().unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        assert_eq!(worker.step(), Ok(ReadyReadStep::Pending));

        let began = Instant::now();
        drop(worker);
        assert!(began.elapsed() < Duration::from_millis(100));
        release_tx.send(()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_stdout_moves_directly_into_the_sync_worker() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf native"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut worker = ReadyReadWorker::from_native_stdout(stdout).unwrap();
        worker.permit().unwrap();
        assert_eq!(chunk(complete(&mut worker).unwrap()), b"native");
        child.wait().unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_stdout_is_converted_before_sync_worker_reads() {
        let mut child = tokio::process::Command::new("cmd.exe")
            .args(["/C", "<nul set /p =native"])
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let mut worker = ReadyReadWorker::from_native_stdout(stdout).unwrap();
        worker.permit().unwrap();
        assert_eq!(chunk(complete(&mut worker).unwrap()), b"native");
        child.wait().await.unwrap();
    }
}
