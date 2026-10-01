//! Admission of the sidecar's first production control request.
//!
//! This seam only borrows the host-lifetime worker.  It never creates another
//! reader or sequence, starts a target, assembles a generation, or writes a
//! reply.

use std::time::{Duration, Instant};

use agent24_sidecar_host_protocol::Request;

use crate::{
    control_io::IngressStep,
    control_worker::{ControlPermitError, ControlStep, ControlWorkerError},
    host_ports::HostPorts,
    launch::LaunchIntent,
};

/// A bounded, sequence-validated first launch and its fixed deadline.
pub(crate) struct AcceptedLaunch {
    pub(crate) intent: LaunchIntent,
    pub(crate) request_id: u64,
    pub(crate) deadline: Instant,
}

impl std::fmt::Debug for AcceptedLaunch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AcceptedLaunch")
            .field("request_id", &self.request_id)
            .field("deadline", &self.deadline)
            .finish_non_exhaustive()
    }
}

/// A terminal first-request observation. No variant contains a fabricated ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FirstLaunchTerminal {
    Eof,
    NotLaunch,
    Fatal(ControlWorkerError),
    Accepted,
}

/// One non-blocking ingress turn.
#[derive(Debug)]
pub(crate) enum FirstLaunchStep {
    Pending,
    Accepted(AcceptedLaunch),
    Terminal(FirstLaunchTerminal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Open,
    Accepted,
    Terminal(FirstLaunchTerminal),
}

/// Accept exactly one valid `Request::Launch` from `HostPorts`' control worker.
///
/// `HostPorts` constructs its worker with no control timeout. Thus an idle
/// first launch is intentionally unbounded, while an accepted launch receives
/// one absolute, overflow-safe deadline.
pub(crate) struct FirstLaunchIngress {
    launch_budget: Duration,
    state: State,
}

impl FirstLaunchIngress {
    pub(crate) const fn new(launch_budget: Duration) -> Self {
        Self {
            launch_budget,
            state: State::Open,
        }
    }

    /// Poll once and, only while open, issue at most one worker credit.
    pub(crate) fn step(&mut self, ports: &mut HostPorts, now: Instant) -> FirstLaunchStep {
        match self.state {
            State::Accepted => return FirstLaunchStep::Terminal(FirstLaunchTerminal::Accepted),
            State::Terminal(terminal) => return FirstLaunchStep::Terminal(terminal),
            State::Open => {}
        }

        let (_, _, control) = ports.borrow();
        match control.step(now) {
            Ok(ControlStep::Complete(IngressStep::Request(request))) => self.accept(request, now),
            Ok(ControlStep::Complete(IngressStep::Eof)) => self.terminal(FirstLaunchTerminal::Eof),
            Err(error) => self.terminal(FirstLaunchTerminal::Fatal(error)),
            Ok(ControlStep::Idle | ControlStep::Pending)
            | Ok(ControlStep::Complete(IngressStep::Pending)) => match control.permit(now) {
                Ok(()) | Err(ControlPermitError::Busy) => FirstLaunchStep::Pending,
                Err(ControlPermitError::Closed) => {
                    self.terminal(FirstLaunchTerminal::Fatal(ControlWorkerError::Closed))
                }
            },
        }
    }

    fn accept(&mut self, request: Request, now: Instant) -> FirstLaunchStep {
        let Request::Launch { request_id, .. } = &request else {
            return self.terminal(FirstLaunchTerminal::NotLaunch);
        };
        let request_id = *request_id;
        // Overflow means expired now, never an accidental unlimited deadline.
        let deadline = now.checked_add(self.launch_budget).unwrap_or(now);
        let Ok(intent) = LaunchIntent::from_request(request) else {
            return self.terminal(FirstLaunchTerminal::NotLaunch);
        };
        self.state = State::Accepted;
        FirstLaunchStep::Accepted(AcceptedLaunch {
            intent,
            request_id,
            deadline,
        })
    }

    fn terminal(&mut self, terminal: FirstLaunchTerminal) -> FirstLaunchStep {
        self.state = State::Terminal(terminal);
        FirstLaunchStep::Terminal(terminal)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{control_io::IngressError, host_ports::HostPorts, worker_slots::WorkerSlots};
    use agent24_sidecar_host_protocol::{
        PROTOCOL_VERSION, ProtocolError, Request, RequestSequence, encode_request,
    };
    use std::{
        collections::{BTreeMap, VecDeque},
        io::{self, Read, Write},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        thread,
    };

    const WAIT: Duration = Duration::from_secs(3);
    const LAUNCH: Duration = Duration::from_secs(7);

    struct ScriptedRead {
        chunks: VecDeque<Vec<u8>>,
        reads: Arc<AtomicUsize>,
    }
    impl Read for ScriptedRead {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.reads.fetch_add(1, Ordering::SeqCst);
            let Some(mut chunk) = self.chunks.pop_front() else {
                return Ok(0);
            };
            let count = output.len().min(chunk.len());
            output[..count].copy_from_slice(&chunk[..count]);
            if count < chunk.len() {
                chunk.drain(..count);
                self.chunks.push_front(chunk);
            }
            Ok(count)
        }
    }
    struct CountingWrite(Arc<AtomicUsize>);
    impl Write for CountingWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn request(id: u64) -> Request {
        #[cfg(windows)]
        let (executable, cwd) = (r"C:\\agent\\helper.exe", r"C:\\agent");
        #[cfg(not(windows))]
        let (executable, cwd) = ("/bin/true", "/tmp");
        Request::Launch {
            version: PROTOCOL_VERSION,
            request_id: id,
            executable: executable.into(),
            cwd: cwd.into(),
            argv: vec![],
            env: BTreeMap::new(),
        }
    }
    fn wire(request: Request) -> Vec<u8> {
        encode_request(&request, &mut RequestSequence::new()).unwrap()
    }
    fn replace_id(frame: Vec<u8>, id: u64) -> Vec<u8> {
        let frame = String::from_utf8(frame).unwrap();
        frame
            .replacen("\"request_id\":1", &format!("\"request_id\":{id}"), 1)
            .into_bytes()
    }
    fn duplicate_id(mut frame: Vec<u8>) -> Vec<u8> {
        let newline = frame.pop();
        assert_eq!(newline, Some(b'\n'));
        assert_eq!(frame.pop(), Some(b'}'));
        frame.extend(b",\"request_id\":1}\n");
        frame
    }
    fn ports(chunks: Vec<Vec<u8>>) -> (HostPorts, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let ports = HostPorts::new_in(
            WorkerSlots::isolated(),
            ScriptedRead {
                chunks: chunks.into(),
                reads: reads.clone(),
            },
            CountingWrite(writes.clone()),
            WAIT,
        )
        .unwrap();
        (ports, reads, writes)
    }
    fn complete(
        ingress: &mut FirstLaunchIngress,
        ports: &mut HostPorts,
        now: Instant,
    ) -> FirstLaunchStep {
        let deadline = Instant::now() + WAIT;
        loop {
            let step = ingress.step(ports, now);
            if !matches!(step, FirstLaunchStep::Pending) || Instant::now() >= deadline {
                return step;
            }
            thread::yield_now();
        }
    }
    fn terminal(chunks: Vec<Vec<u8>>) -> (FirstLaunchTerminal, Arc<AtomicUsize>) {
        let (mut ports, _, writes) = ports(chunks);
        let step = complete(
            &mut FirstLaunchIngress::new(LAUNCH),
            &mut ports,
            Instant::now(),
        );
        let FirstLaunchStep::Terminal(terminal) = step else {
            panic!("terminal result expected")
        };
        (terminal, writes)
    }
    fn accepted(chunks: Vec<Vec<u8>>) -> AcceptedLaunch {
        let (mut ports, _, _) = ports(chunks);
        let FirstLaunchStep::Accepted(accepted) = complete(
            &mut FirstLaunchIngress::new(LAUNCH),
            &mut ports,
            Instant::now(),
        ) else {
            panic!("launch expected")
        };
        accepted
    }

    #[test]
    fn launch_yields_intent_id_and_absolute_deadline() {
        let (mut first_ports, _, _) = ports(vec![wire(request(9))]);
        let t0 = Instant::now();
        let mut ingress = FirstLaunchIngress::new(LAUNCH);
        assert!(matches!(
            ingress.step(&mut first_ports, t0),
            FirstLaunchStep::Pending
        ));
        let t1 = t0 + Duration::from_millis(1);
        let FirstLaunchStep::Accepted(accepted) = complete(&mut ingress, &mut first_ports, t1)
        else {
            panic!("launch expected")
        };
        assert_eq!((accepted.request_id, accepted.deadline), (9, t1 + LAUNCH));
        assert!(matches!(accepted.intent, LaunchIntent { .. }));

        let (mut ports, _, _) = ports(vec![wire(request(10))]);
        let mut ingress = FirstLaunchIngress::new(Duration::MAX);
        assert!(matches!(
            ingress.step(&mut ports, t0),
            FirstLaunchStep::Pending
        ));
        let FirstLaunchStep::Accepted(accepted) = complete(&mut ingress, &mut ports, t1) else {
            panic!("overflow launch expected")
        };
        assert_eq!(accepted.deadline, t1);
    }

    #[test]
    fn eof_partial_eof_and_non_launch_are_terminal() {
        assert_eq!(terminal(vec![]).0, FirstLaunchTerminal::Eof);
        let partial = terminal(vec![b"{\"type\":\"launch\"".to_vec()]).0;
        assert!(matches!(
            partial,
            FirstLaunchTerminal::Fatal(ControlWorkerError::Ingress(IngressError::Framing(_)))
        ));
        for non_launch in [
            b"{\"type\":\"signal\",\"version\":1,\"request_id\":1,\"force\":false}\n".to_vec(),
            b"{\"type\":\"is_empty\",\"version\":1,\"request_id\":1}\n".to_vec(),
        ] {
            assert_eq!(
                terminal(vec![non_launch]).0,
                FirstLaunchTerminal::Fatal(ControlWorkerError::Ingress(IngressError::Protocol(
                    ProtocolError::WrongSequence
                )))
            );
        }
    }

    #[test]
    fn sequence_boundaries_and_fatal_decode_do_not_reply() {
        assert_eq!(
            terminal(vec![replace_id(wire(request(1)), 0)]).0,
            FirstLaunchTerminal::Fatal(ControlWorkerError::Ingress(IngressError::Protocol(
                ProtocolError::InvalidMessage
            )))
        );
        assert_eq!(accepted(vec![wire(request(u64::MAX))]).request_id, u64::MAX);
        let (malformed, writes) = terminal(vec![duplicate_id(wire(request(1)))]);
        assert_eq!(
            malformed,
            FirstLaunchTerminal::Fatal(ControlWorkerError::Ingress(IngressError::Protocol(
                ProtocolError::InvalidJson
            )))
        );
        assert_eq!(writes.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn accepted_launch_never_credits_or_consumes_second_request() {
        let (mut ports, reads, _) = ports(vec![wire(request(7)), wire(request(8))]);
        let now = Instant::now();
        let mut ingress = FirstLaunchIngress::new(LAUNCH);
        assert!(matches!(
            complete(&mut ingress, &mut ports, now),
            FirstLaunchStep::Accepted(AcceptedLaunch { request_id: 7, .. })
        ));
        let reads_before_terminal = reads.load(Ordering::SeqCst);
        assert!(matches!(
            ingress.step(&mut ports, now),
            FirstLaunchStep::Terminal(FirstLaunchTerminal::Accepted)
        ));
        assert_eq!(reads.load(Ordering::SeqCst), reads_before_terminal);
        let (_, _, control) = ports.borrow();
        assert_eq!(control.step(now), Ok(ControlStep::Idle));
    }
}
