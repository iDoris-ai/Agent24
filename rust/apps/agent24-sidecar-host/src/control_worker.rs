use crate::control_io::{ControlIngress, IngressError, IngressStep};
use crate::worker_slots::{WorkerRole, WorkerSlotError, WorkerSlots, hold_permit};
use std::{
    io::Read,
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    time::{Duration, Instant},
};

type WorkerResult = Result<IngressStep, IngressError>;

/// The actor's bounded admission token for exactly one control read attempt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlPermitError {
    Busy,
    Closed,
}

/// A failure which leaves the control transport unusable.  The actor must
/// enter its cleanup path rather than waiting for control I/O to recover.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlWorkerError {
    Closed,
    TimedOut,
    Ingress(IngressError),
}

/// A non-blocking observation of the one worker admitted by a permit.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ControlStep {
    Idle,
    Pending,
    Complete(IngressStep),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    InFlight { deadline: Option<Instant> },
    Closed,
}

/// A single, host-lifetime control-I/O worker.  It is deliberately unwired:
/// construction moves only a reader, and no actor or platform pipe authority.
///
/// A permit is a credit, not a queued request.  While it is outstanding the
/// actor cannot admit another read.  The worker consumes at most one
/// `ControlIngress::next` result for that permit, then waits for another one.
/// Generic blocking `Read` cannot be cancelled by `Drop`; a timed out worker
/// is therefore terminal and must never be replaced by this primitive.
pub(crate) struct ControlWorker {
    credits: SyncSender<()>,
    results: Receiver<WorkerResult>,
    budget: Option<Duration>,
    state: State,
}

impl ControlWorker {
    pub(crate) fn new_in<R: Read + Send + 'static>(
        slots: &'static WorkerSlots,
        reader: R,
        budget: Option<Duration>,
    ) -> Result<Self, WorkerSlotError> {
        let (credit_tx, credit_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::sync_channel(1);
        slots.spawn(WorkerRole::Control, "sidecar-control", move |permit| {
            hold_permit(permit, || {
                control_loop(ControlIngress::new(reader), credit_rx, result_tx)
            })
        })?;
        Ok(Self {
            credits: credit_tx,
            results: result_rx,
            budget,
            state: State::Idle,
        })
    }

    #[cfg(test)]
    pub(crate) fn new<R: Read + Send + 'static>(
        reader: R,
        budget: Option<Duration>,
    ) -> Result<Self, WorkerSlotError> {
        Self::new_in(WorkerSlots::isolated(), reader, budget)
    }

    /// Give the one worker permission to perform one control read attempt.
    /// This only touches a bounded channel and never blocks the actor.
    pub(crate) fn permit(&mut self, now: Instant) -> Result<(), ControlPermitError> {
        if self.state == State::Closed {
            return Err(ControlPermitError::Closed);
        }
        if matches!(self.state, State::InFlight { .. }) {
            return Err(ControlPermitError::Busy);
        }
        match self.credits.try_send(()) {
            Ok(()) => {
                self.state = State::InFlight {
                    // An overflowing finite budget is an immediately expired
                    // deadline, never an accidental unlimited operation.
                    deadline: self
                        .budget
                        .map(|budget| now.checked_add(budget).unwrap_or(now)),
                };
                Ok(())
            }
            Err(TrySendError::Full(())) => Err(ControlPermitError::Busy),
            Err(TrySendError::Disconnected(())) => {
                self.state = State::Closed;
                Err(ControlPermitError::Closed)
            }
        }
    }

    /// Poll the worker without doing control I/O on the caller's thread.
    /// At the exact deadline, timeout wins over a queued result.
    pub(crate) fn step(&mut self, now: Instant) -> Result<ControlStep, ControlWorkerError> {
        let State::InFlight { deadline } = self.state else {
            return if self.state == State::Closed {
                Err(ControlWorkerError::Closed)
            } else {
                Ok(ControlStep::Idle)
            };
        };
        if deadline.is_some_and(|deadline| now >= deadline) {
            self.state = State::Closed;
            return Err(ControlWorkerError::TimedOut);
        }
        match self.results.try_recv() {
            Ok(Ok(step @ IngressStep::Eof)) => {
                // EOF ends the worker after this completion.  Do not admit a
                // racy extra credit while the thread is returning.
                self.state = State::Closed;
                Ok(ControlStep::Complete(step))
            }
            Ok(Ok(step)) => {
                self.state = State::Idle;
                Ok(ControlStep::Complete(step))
            }
            Ok(Err(error)) => {
                self.state = State::Closed;
                Err(ControlWorkerError::Ingress(error))
            }
            Err(TryRecvError::Empty) => Ok(ControlStep::Pending),
            Err(TryRecvError::Disconnected) => {
                self.state = State::Closed;
                Err(ControlWorkerError::Closed)
            }
        }
    }
}

fn control_loop<R: Read>(
    mut ingress: ControlIngress<R>,
    credits: Receiver<()>,
    results: SyncSender<WorkerResult>,
) {
    while credits.recv().is_ok() {
        let result = ingress.next();
        let terminal = matches!(&result, Ok(IngressStep::Eof) | Err(_));
        if results.send(result).is_err() || terminal {
            return;
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use agent24_sidecar_host_protocol::{Request, RequestSequence, encode_request};
    use std::{
        collections::{HashSet, VecDeque},
        io,
        io::ErrorKind,
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    const BUDGET: Duration = Duration::from_secs(2);

    struct ScriptedRead {
        steps: VecDeque<Result<Vec<u8>, ErrorKind>>,
        pending: Vec<u8>,
        calls: Arc<AtomicUsize>,
        threads: Arc<Mutex<HashSet<thread::ThreadId>>>,
    }

    impl Read for ScriptedRead {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.threads.lock().unwrap().insert(thread::current().id());
            if self.pending.is_empty() {
                match self.steps.pop_front().unwrap_or(Err(ErrorKind::WouldBlock)) {
                    Ok(bytes) => self.pending = bytes,
                    Err(kind) => return Err(io::Error::from(kind)),
                }
            }
            let count = self.pending.len().min(out.len());
            out[..count].copy_from_slice(&self.pending[..count]);
            self.pending.drain(..count);
            Ok(count)
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

    fn launch(id: u64) -> Request {
        #[cfg(windows)]
        let (executable, cwd) = (r"C:\\agent\\helper.exe", r"C:\\agent");
        #[cfg(not(windows))]
        let (executable, cwd) = ("/bin/true", "/tmp");
        Request::Launch {
            version: 1,
            request_id: id,
            executable: executable.into(),
            cwd: cwd.into(),
            argv: vec![],
            env: Default::default(),
        }
    }

    fn signal(id: u64) -> Request {
        Request::Signal {
            version: 1,
            request_id: id,
            force: false,
        }
    }

    fn wire(request: Request) -> Vec<u8> {
        encode_request(&request, &mut RequestSequence::new()).unwrap()
    }

    fn complete(
        worker: &mut ControlWorker,
        now: Instant,
    ) -> Result<ControlStep, ControlWorkerError> {
        let deadline = Instant::now() + BUDGET;
        loop {
            let step = worker.step(now);
            if step != Ok(ControlStep::Pending) || Instant::now() >= deadline {
                return step;
            }
            thread::yield_now();
        }
    }

    #[test]
    fn no_permit_does_not_read_and_second_permit_is_busy() {
        let (input, calls, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut worker = ControlWorker::new(input, Some(BUDGET)).unwrap();
        let now = Instant::now();
        assert_eq!(worker.step(now), Ok(ControlStep::Idle));
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        worker.permit(now).unwrap();
        assert_eq!(worker.permit(now), Err(ControlPermitError::Busy));
        assert_eq!(
            complete(&mut worker, now),
            Ok(ControlStep::Complete(IngressStep::Pending))
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn one_credit_returns_at_most_one_request_and_reuses_one_worker() {
        let mut sequence = RequestSequence::new();
        let mut input = encode_request(&launch(1), &mut sequence).unwrap();
        input.extend(encode_request(&signal(2), &mut sequence).unwrap());
        let (input, _, threads) = reader(vec![Ok(input)]);
        let mut worker = ControlWorker::new(input, Some(BUDGET)).unwrap();
        let now = Instant::now();

        worker.permit(now).unwrap();
        assert_eq!(
            complete(&mut worker, now),
            Ok(ControlStep::Complete(IngressStep::Request(launch(1))))
        );
        assert_eq!(threads.lock().unwrap().len(), 1);

        worker.permit(now).unwrap();
        assert_eq!(
            complete(&mut worker, now),
            Ok(ControlStep::Complete(IngressStep::Request(signal(2))))
        );
        assert_eq!(threads.lock().unwrap().len(), 1);
    }

    #[test]
    fn partial_input_needs_a_second_permit_and_eof_is_a_completion() {
        let frame = wire(launch(7));
        let split = frame.len() / 2;
        let (input, _, _) = reader(vec![
            Ok(frame[..split].to_vec()),
            Err(ErrorKind::WouldBlock),
            Ok(frame[split..].to_vec()),
            Ok(Vec::new()),
        ]);
        let mut worker = ControlWorker::new(input, Some(BUDGET)).unwrap();
        let now = Instant::now();

        worker.permit(now).unwrap();
        assert_eq!(
            complete(&mut worker, now),
            Ok(ControlStep::Complete(IngressStep::Pending))
        );
        worker.permit(now).unwrap();
        assert_eq!(
            complete(&mut worker, now),
            Ok(ControlStep::Complete(IngressStep::Request(launch(7))))
        );
        worker.permit(now).unwrap();
        assert_eq!(
            complete(&mut worker, now),
            Ok(ControlStep::Complete(IngressStep::Eof))
        );
        assert_eq!(worker.permit(now), Err(ControlPermitError::Closed));
    }

    #[test]
    fn disconnected_result_worker_and_ingress_failure_are_terminal() {
        let (input, _, _) = reader(vec![Err(ErrorKind::PermissionDenied)]);
        let mut worker = ControlWorker::new(input, Some(BUDGET)).unwrap();
        let now = Instant::now();
        worker.permit(now).unwrap();
        assert_eq!(
            complete(&mut worker, now),
            Err(ControlWorkerError::Ingress(IngressError::Io(
                ErrorKind::PermissionDenied
            )))
        );
        assert_eq!(worker.permit(now), Err(ControlPermitError::Closed));

        let (input, _, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut disconnected = ControlWorker::new(input, Some(BUDGET)).unwrap();
        let (_, receiver) = mpsc::sync_channel(1);
        let old = std::mem::replace(&mut disconnected.results, receiver);
        drop(old);
        disconnected.permit(now).unwrap();
        assert_eq!(disconnected.step(now), Err(ControlWorkerError::Closed));
    }

    struct BlockingRead {
        started: SyncSender<()>,
        release: Receiver<()>,
    }

    impl Read for BlockingRead {
        fn read(&mut self, _out: &mut [u8]) -> io::Result<usize> {
            let _ = self.started.send(());
            let _ = self.release.recv();
            Ok(0)
        }
    }

    #[test]
    fn blocked_read_times_out_and_drop_never_waits_for_the_actor() {
        let (started_tx, started_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let now = Instant::now();
        let mut worker = ControlWorker::new(
            BlockingRead {
                started: started_tx,
                release: release_rx,
            },
            Some(Duration::from_millis(1)),
        )
        .unwrap();
        worker.permit(now).unwrap();
        started_rx.recv_timeout(BUDGET).unwrap();
        assert_eq!(
            worker.step(now + Duration::from_millis(1)),
            Err(ControlWorkerError::TimedOut)
        );

        let began = Instant::now();
        drop(worker);
        assert!(began.elapsed() < Duration::from_millis(100));
        release_tx.send(()).unwrap();
    }

    #[test]
    fn no_budget_completes_even_when_polled_far_after_admission() {
        let (input, _, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut worker = ControlWorker::new(input, None).unwrap();
        let now = Instant::now();
        worker.permit(now).unwrap();
        let far_future = now
            .checked_add(Duration::from_secs(100 * 365 * 24 * 60 * 60))
            .unwrap();
        assert_eq!(
            complete(&mut worker, far_future),
            Ok(ControlStep::Complete(IngressStep::Pending))
        );
    }

    #[test]
    fn finite_budget_is_inclusive_and_overflow_is_terminal() {
        let (input, _, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let now = Instant::now();
        let mut zero = ControlWorker::new(input, Some(Duration::ZERO)).unwrap();
        zero.permit(now).unwrap();
        assert_eq!(zero.step(now), Err(ControlWorkerError::TimedOut));

        let (input, _, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let mut overflow = ControlWorker::new(input, Some(Duration::MAX)).unwrap();
        overflow.permit(now).unwrap();
        assert_eq!(overflow.step(now), Err(ControlWorkerError::TimedOut));
    }

    #[test]
    fn queued_completion_loses_to_an_expired_deadline() {
        let (input, calls, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let now = Instant::now();
        let mut worker = ControlWorker::new(input, Some(Duration::from_secs(1))).unwrap();
        worker.permit(now).unwrap();
        while calls.load(Ordering::SeqCst) == 0 {
            thread::yield_now();
        }
        assert_eq!(
            worker.step(now + Duration::from_secs(1)),
            Err(ControlWorkerError::TimedOut)
        );
    }

    #[test]
    fn busy_permit_does_not_renew_the_original_deadline() {
        let (input, _, _) = reader(vec![Err(ErrorKind::WouldBlock)]);
        let now = Instant::now();
        let mut worker = ControlWorker::new(input, Some(Duration::from_secs(1))).unwrap();
        worker.permit(now).unwrap();
        assert_eq!(
            worker.permit(now + Duration::from_millis(500)),
            Err(ControlPermitError::Busy)
        );
        assert_eq!(
            worker.step(now + Duration::from_secs(1)),
            Err(ControlWorkerError::TimedOut)
        );
    }
}
