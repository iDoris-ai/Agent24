use crate::pipe_access::NativeStderr;
use crate::worker_slots::{WorkerRole, WorkerSlotError, WorkerSlots, hold_permit};
use std::{
    io::{ErrorKind, Read},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError},
    },
    thread,
    time::Duration,
};

/// Every synchronous read uses this one fixed-size stack buffer.
pub(crate) const STDERR_DRAIN_BUFFER_BYTES: usize = 4096;
const WOULD_BLOCK_BACKOFF: Duration = Duration::from_millis(10);

/// The one terminal outcome retained by the drain worker.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum StderrDrainStatus {
    Running,
    Eof,
    Io(ErrorKind),
    Closed,
}

/// A non-blocking, allocation-free observation of stderr draining.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StderrDrainSnapshot {
    pub(crate) bytes_drained: u64,
    pub(crate) status: StderrDrainStatus,
}

/// A detached, continuous stderr drainer for one moved target pipe.
///
/// Construction immediately starts exactly one fixed thread. It never exposes
/// chunks, stores bytes, hashes, strings, or a ring buffer: only a saturating
/// byte total and one terminal summary are retained. `Drop` only requests a
/// stop and never joins. A generic blocked `Read` cannot be cancelled, so the
/// thread may outlive this object; callers must not replace it with another
/// reader for the same pipe.
pub(crate) struct StderrDrainWorker {
    bytes: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    terminal: Receiver<StderrDrainSnapshot>,
    cached_terminal: Option<StderrDrainSnapshot>,
}

impl StderrDrainWorker {
    /// Move native stderr into the synchronous drain thread.
    ///
    /// Unix stderr is already synchronous. On Windows, this consumes Tokio's
    /// still-unpolled async handle immediately and turns its owned handle into
    /// a `File`; it does not clone a raw handle or create a competing reader.
    #[cfg(unix)]
    pub(crate) fn from_native_stderr(stderr: NativeStderr) -> Result<Self, WorkerSlotError> {
        Self::new_in(WorkerSlots::host(), stderr)
    }

    #[cfg(windows)]
    pub(crate) fn from_native_stderr(stderr: NativeStderr) -> Result<Self, WorkerSlotError> {
        let file = std::fs::File::from(
            stderr
                .into_owned_handle()
                .map_err(|error| WorkerSlotError::Spawn(error.kind()))?,
        );
        Self::new_in(WorkerSlots::host(), file)
    }

    /// Start the one drain thread immediately.
    pub(crate) fn new_in<R: Read + Send + 'static>(
        slots: &'static WorkerSlots,
        reader: R,
    ) -> Result<Self, WorkerSlotError> {
        let bytes = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (terminal_tx, terminal) = mpsc::sync_channel(1);
        slots.spawn(WorkerRole::StderrDrain, "sidecar-stderr-drain", {
            let bytes = Arc::clone(&bytes);
            let stop = Arc::clone(&stop);
            move |permit| hold_permit(permit, || drain_loop(reader, bytes, stop, terminal_tx))
        })?;
        Ok(Self {
            bytes,
            stop,
            terminal,
            cached_terminal: None,
        })
    }

    #[cfg(test)]
    pub(crate) fn new<R: Read + Send + 'static>(reader: R) -> Result<Self, WorkerSlotError> {
        Self::new_in(WorkerSlots::isolated(), reader)
    }

    /// Observe progress without blocking or performing caller-thread I/O.
    /// Terminal observations are cached so a consumed channel cannot turn an
    /// already observed EOF or I/O failure into `Closed`.
    pub(crate) fn snapshot(&mut self) -> StderrDrainSnapshot {
        if self.cached_terminal.is_none() {
            match self.terminal.try_recv() {
                Ok(summary) => self.cached_terminal = Some(summary),
                Err(TryRecvError::Disconnected) => {
                    self.cached_terminal = Some(StderrDrainSnapshot {
                        bytes_drained: self.bytes.load(Ordering::Acquire),
                        status: StderrDrainStatus::Closed,
                    });
                }
                Err(TryRecvError::Empty) => {}
            }
        }
        self.cached_terminal.unwrap_or(StderrDrainSnapshot {
            bytes_drained: self.bytes.load(Ordering::Acquire),
            status: StderrDrainStatus::Running,
        })
    }
}

impl Drop for StderrDrainWorker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

fn drain_loop<R: Read>(
    mut reader: R,
    bytes: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    terminal: SyncSender<StderrDrainSnapshot>,
) {
    let mut buffer = [0_u8; STDERR_DRAIN_BUFFER_BYTES];
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        match reader.read(&mut buffer) {
            Ok(0) => return send_terminal(&terminal, &bytes, StderrDrainStatus::Eof),
            Ok(len) => saturating_add(&bytes, len),
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                thread::sleep(WOULD_BLOCK_BACKOFF);
            }
            Err(error) => {
                return send_terminal(&terminal, &bytes, StderrDrainStatus::Io(error.kind()));
            }
        }
    }
}

fn saturating_add(bytes: &AtomicU64, len: usize) {
    let increment = u64::try_from(len).unwrap_or(u64::MAX);
    let _ = bytes.fetch_update(Ordering::AcqRel, Ordering::Acquire, |current| {
        Some(current.saturating_add(increment))
    });
}

fn send_terminal(
    terminal: &SyncSender<StderrDrainSnapshot>,
    bytes: &AtomicU64,
    status: StderrDrainStatus,
) {
    let _ = terminal.send(StderrDrainSnapshot {
        bytes_drained: bytes.load(Ordering::Acquire),
        status,
    });
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::{
        collections::{HashSet, VecDeque},
        io,
        sync::{Arc, Mutex},
        thread,
        time::{Duration, Instant},
    };

    const WAIT: Duration = Duration::from_secs(2);

    struct ScriptedRead {
        steps: VecDeque<Result<Vec<u8>, ErrorKind>>,
        pending: Vec<u8>,
        calls: Arc<AtomicU64>,
        buffers: Arc<Mutex<Vec<usize>>>,
        threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
    }

    impl Read for ScriptedRead {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.buffers.lock().unwrap().push(output.len());
            self.threads.lock().unwrap().insert(thread::current().id());
            if self.pending.is_empty() {
                match self.steps.pop_front().unwrap_or(Ok(Vec::new())) {
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

    type ReaderFacts = (
        Arc<AtomicU64>,
        Arc<Mutex<Vec<usize>>>,
        Arc<Mutex<HashSet<thread::ThreadId>>>,
    );

    fn reader(steps: Vec<Result<Vec<u8>, ErrorKind>>) -> (ScriptedRead, ReaderFacts) {
        let calls = Arc::new(AtomicU64::new(0));
        let buffers = Arc::new(Mutex::new(Vec::new()));
        let threads = Arc::new(Mutex::new(HashSet::new()));
        (
            ScriptedRead {
                steps: steps.into(),
                pending: Vec::new(),
                calls: Arc::clone(&calls),
                buffers: Arc::clone(&buffers),
                threads: Arc::clone(&threads),
            },
            (calls, buffers, threads),
        )
    }

    fn terminal(worker: &mut StderrDrainWorker) -> StderrDrainSnapshot {
        let deadline = Instant::now() + WAIT;
        loop {
            let snapshot = worker.snapshot();
            if snapshot.status != StderrDrainStatus::Running || Instant::now() >= deadline {
                return snapshot;
            }
            thread::yield_now();
        }
    }

    #[test]
    fn continuously_drains_multiple_chunks_without_a_consumer() {
        let first = vec![b'a'; STDERR_DRAIN_BUFFER_BYTES + 9];
        let second = b"tail".to_vec();
        let (input, facts) = reader(vec![Ok(first), Ok(second), Ok(Vec::new())]);
        let mut worker = StderrDrainWorker::new(input).unwrap();
        let snapshot = terminal(&mut worker);
        assert_eq!(snapshot.status, StderrDrainStatus::Eof);
        assert_eq!(
            snapshot.bytes_drained,
            (STDERR_DRAIN_BUFFER_BYTES + 9 + 4) as u64
        );
        assert_eq!(facts.0.load(Ordering::SeqCst), 4);
    }

    #[test]
    fn one_fixed_thread_uses_only_the_fixed_buffer() {
        let (input, facts) = reader(vec![
            Ok(b"one".to_vec()),
            Ok(b"two".to_vec()),
            Ok(Vec::new()),
        ]);
        let mut worker = StderrDrainWorker::new(input).unwrap();
        assert_eq!(terminal(&mut worker).status, StderrDrainStatus::Eof);
        assert_eq!(facts.2.lock().unwrap().len(), 1);
        assert!(
            facts
                .1
                .lock()
                .unwrap()
                .iter()
                .all(|&size| size == STDERR_DRAIN_BUFFER_BYTES)
        );
    }

    #[test]
    fn partial_interrupted_and_would_block_reads_continue() {
        let (input, facts) = reader(vec![
            Ok(b"part".to_vec()),
            Err(ErrorKind::Interrupted),
            Err(ErrorKind::WouldBlock),
            Ok(b"ial".to_vec()),
            Ok(Vec::new()),
        ]);
        let mut worker = StderrDrainWorker::new(input).unwrap();
        assert_eq!(
            terminal(&mut worker),
            StderrDrainSnapshot {
                bytes_drained: 7,
                status: StderrDrainStatus::Eof
            }
        );
        assert_eq!(facts.0.load(Ordering::SeqCst), 5);
    }

    #[test]
    fn eof_and_fatal_io_are_cached_terminal_states() {
        let (input, _) = reader(vec![Ok(Vec::new())]);
        let mut eof = StderrDrainWorker::new(input).unwrap();
        assert_eq!(terminal(&mut eof).status, StderrDrainStatus::Eof);
        assert_eq!(eof.snapshot().status, StderrDrainStatus::Eof);

        let (input, _) = reader(vec![Err(ErrorKind::BrokenPipe)]);
        let mut failed = StderrDrainWorker::new(input).unwrap();
        assert_eq!(
            terminal(&mut failed).status,
            StderrDrainStatus::Io(ErrorKind::BrokenPipe)
        );
        assert_eq!(
            failed.snapshot().status,
            StderrDrainStatus::Io(ErrorKind::BrokenPipe)
        );
    }

    #[test]
    fn byte_count_saturates_at_u64_max() {
        let bytes = AtomicU64::new(u64::MAX - 2);
        saturating_add(&bytes, 3);
        assert_eq!(bytes.load(Ordering::Acquire), u64::MAX);
    }

    #[test]
    fn disconnected_terminal_channel_is_closed() {
        let (input, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut worker = StderrDrainWorker::new(input).unwrap();
        let (_, replacement) = mpsc::sync_channel(1);
        let original = std::mem::replace(&mut worker.terminal, replacement);
        drop(original);
        assert_eq!(worker.snapshot().status, StderrDrainStatus::Closed);
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
    fn blocked_read_does_not_make_drop_wait() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker = StderrDrainWorker::new(BlockingRead {
            started: started_tx,
            release: release_rx,
        })
        .unwrap();
        started_rx.recv_timeout(WAIT).unwrap();
        let began = Instant::now();
        drop(worker);
        assert!(began.elapsed() < Duration::from_millis(100));
        release_tx.send(()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn native_stderr_moves_directly_into_the_sync_worker() {
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "printf native >&2"])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut worker = StderrDrainWorker::from_native_stderr(stderr).unwrap();
        assert_eq!(
            terminal(&mut worker),
            StderrDrainSnapshot {
                bytes_drained: 6,
                status: StderrDrainStatus::Eof
            }
        );
        child.wait().unwrap();
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn native_stderr_is_converted_before_sync_worker_reads() {
        let mut child = tokio::process::Command::new("cmd.exe")
            .args(["/C", "<nul 1>&2 set /p =native"])
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let mut worker = StderrDrainWorker::from_native_stderr(stderr).unwrap();
        assert_eq!(
            terminal(&mut worker),
            StderrDrainSnapshot {
                bytes_drained: 6,
                status: StderrDrainStatus::Eof
            }
        );
        child.wait().await.unwrap();
    }
}
