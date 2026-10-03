use agent24_sidecar_host_protocol::frame::{
    FRAME_READ_CHUNK_BYTES, FrameRead, FrameReadError, NdjsonFrameReader,
};
use std::io::{ErrorKind, Read};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NextFrame {
    Pending,
    Frame(Vec<u8>),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ControlReadError {
    Frame(FrameReadError),
    Io(ErrorKind),
    Closed,
}

enum Terminal {
    Open,
    Eof,
    Failed,
}

pub(crate) struct ControlReader<R> {
    reader: R,
    framing: NdjsonFrameReader,
    input: [u8; FRAME_READ_CHUNK_BYTES],
    start: usize,
    end: usize,
    terminal: Terminal,
}

impl<R: Read> ControlReader<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader,
            framing: NdjsonFrameReader::control(),
            input: [0; FRAME_READ_CHUNK_BYTES],
            start: 0,
            end: 0,
            terminal: Terminal::Open,
        }
    }

    pub(crate) fn next_frame(&mut self) -> Result<NextFrame, ControlReadError> {
        if matches!(self.terminal, Terminal::Failed) {
            return Err(ControlReadError::Closed);
        }
        if matches!(self.terminal, Terminal::Eof) {
            return Ok(NextFrame::Eof);
        }
        loop {
            if self.start < self.end {
                match self.framing.push(&self.input[self.start..self.end]) {
                    Ok(FrameRead::NeedMore { consumed }) => {
                        self.start += consumed;
                    }
                    Ok(FrameRead::Complete { consumed, frame }) => {
                        self.start += consumed;
                        return Ok(NextFrame::Frame(frame));
                    }
                    Err(error) => {
                        self.terminal = Terminal::Failed;
                        return Err(ControlReadError::Frame(error));
                    }
                }
                continue;
            }
            let read = match self.reader.read(&mut self.input) {
                Ok(0) => {
                    self.terminal = match self.framing.finish() {
                        Ok(()) => Terminal::Eof,
                        Err(error) => {
                            self.terminal = Terminal::Failed;
                            return Err(ControlReadError::Frame(error));
                        }
                    };
                    return Ok(NextFrame::Eof);
                }
                Ok(read) => read,
                Err(error) if error.kind() == ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return Ok(NextFrame::Pending);
                }
                Err(error) => {
                    self.terminal = Terminal::Failed;
                    return Err(ControlReadError::Io(error.kind()));
                }
            };
            self.start = 0;
            self.end = read;
        }
    }
}

use agent24_sidecar_host_protocol::{ProtocolError, Request, RequestSequence, decode_request};
use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum IngressStep {
    Pending,
    Request(Request),
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum IngressError {
    Framing(FrameReadError),
    Io(ErrorKind),
    Protocol(ProtocolError),
    Closed,
}

impl fmt::Display for IngressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Framing(_) => "control frame rejected",
            Self::Io(_) => "control input failed",
            Self::Protocol(_) => "control request rejected",
            Self::Closed => "control ingress is closed",
        })
    }
}
impl std::error::Error for IngressError {}

enum IngressState {
    Open,
    Eof,
    Poisoned,
}

pub(crate) struct ControlIngress<R> {
    reader: ControlReader<R>,
    sequence: RequestSequence,
    state: IngressState,
}

impl<R: Read> ControlIngress<R> {
    pub(crate) fn new(reader: R) -> Self {
        Self {
            reader: ControlReader::new(reader),
            sequence: RequestSequence::new(),
            state: IngressState::Open,
        }
    }

    pub(crate) fn next(&mut self) -> Result<IngressStep, IngressError> {
        match self.state {
            IngressState::Eof => return Ok(IngressStep::Eof),
            IngressState::Poisoned => return Err(IngressError::Closed),
            IngressState::Open => (),
        }
        match self.reader.next_frame() {
            Ok(NextFrame::Pending) => Ok(IngressStep::Pending),
            Ok(NextFrame::Eof) => {
                self.state = IngressState::Eof;
                Ok(IngressStep::Eof)
            }
            Ok(NextFrame::Frame(frame)) => match decode_request(&frame, &mut self.sequence) {
                Ok(request) => Ok(IngressStep::Request(request)),
                Err(error) => Err(self.poison(IngressError::Protocol(error))),
            },
            Err(ControlReadError::Frame(error)) => Err(self.poison(IngressError::Framing(error))),
            Err(ControlReadError::Io(error)) => Err(self.poison(IngressError::Io(error))),
            Err(ControlReadError::Closed) => Err(self.poison(IngressError::Closed)),
        }
    }

    fn poison(&mut self, error: IngressError) -> IngressError {
        self.state = IngressState::Poisoned;
        error
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use agent24_sidecar_host_protocol::{Request, encode_request};

    struct ScriptedRead {
        steps: Vec<Result<Vec<u8>, ErrorKind>>,
        pending: Vec<u8>,
        calls: usize,
    }
    impl Read for ScriptedRead {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            self.calls += 1;
            if self.pending.is_empty() {
                match self.steps.remove(0) {
                    Ok(bytes) => self.pending = bytes,
                    Err(kind) => return Err(std::io::Error::from(kind)),
                }
            }
            let count = self.pending.len().min(out.len());
            out[..count].copy_from_slice(&self.pending[..count]);
            self.pending.drain(..count);
            Ok(count)
        }
    }
    fn scripted(steps: Vec<Result<Vec<u8>, ErrorKind>>) -> ScriptedRead {
        ScriptedRead {
            steps,
            pending: Vec::new(),
            calls: 0,
        }
    }
    fn reader(steps: Vec<Result<Vec<u8>, ErrorKind>>) -> ControlReader<ScriptedRead> {
        ControlReader::new(scripted(steps))
    }
    fn ingress(steps: Vec<Result<Vec<u8>, ErrorKind>>) -> ControlIngress<ScriptedRead> {
        ControlIngress::new(scripted(steps))
    }
    fn launch(id: u64) -> Request {
        #[cfg(windows)]
        let (executable, cwd) = (r"C:\agent\helper.exe", r"C:\agent");
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
    fn wire(request: Request) -> Vec<u8> {
        encode_request(&request, &mut RequestSequence::new()).unwrap()
    }
    fn request(ingress: &mut ControlIngress<ScriptedRead>) -> Request {
        let IngressStep::Request(request) = ingress.next().unwrap() else {
            panic!("request expected")
        };
        request
    }
    fn frame(reader: &mut ControlReader<ScriptedRead>) -> Vec<u8> {
        let NextFrame::Frame(frame) = reader.next_frame().unwrap() else {
            panic!("expected frame")
        };
        frame
    }
    fn closed_after_once(input: &mut ControlIngress<ScriptedRead>, error: IngressError) {
        assert_eq!(input.next(), Err(error));
        let calls = input.reader.reader.calls;
        assert_eq!(input.next(), Err(IngressError::Closed));
        assert_eq!(input.reader.reader.calls, calls);
    }

    #[test]
    fn reader_preserves_frames_spans_reads_and_retries() {
        let mut input = reader(vec![Ok(b"one\ntwo\n".to_vec()), Ok(Vec::new())]);
        assert_eq!(frame(&mut input), b"one\n");
        assert_eq!(frame(&mut input), b"two\n");
        assert_eq!(input.next_frame().unwrap(), NextFrame::Eof);
        let mut split = reader(vec![Ok(b"one\npa".to_vec()), Ok(b"rt\n".to_vec())]);
        assert_eq!(frame(&mut split), b"one\n");
        assert_eq!(frame(&mut split), b"part\n");
        let mut interrupted = reader(vec![Err(ErrorKind::Interrupted), Ok(b"ok\n".to_vec())]);
        assert_eq!(frame(&mut interrupted), b"ok\n");
        let mut blocked = reader(vec![
            Ok(b"part".to_vec()),
            Err(ErrorKind::WouldBlock),
            Ok(b"\n".to_vec()),
        ]);
        assert_eq!(blocked.next_frame().unwrap(), NextFrame::Pending);
        assert_eq!(frame(&mut blocked), b"part\n");
    }

    #[test]
    fn reader_enforces_frame_limit_eof_and_terminal_errors() {
        let mut exact = reader(vec![Ok(vec![b'x'; 65_535]), Ok(b"\n".to_vec())]);
        assert_eq!(frame(&mut exact).len(), 65_536);
        let mut over = reader(vec![Ok(vec![b'x'; 65_536])]);
        assert!(matches!(
            over.next_frame(),
            Err(ControlReadError::Frame(FrameReadError::TooLarge { .. }))
        ));
        assert_eq!(over.next_frame(), Err(ControlReadError::Closed));
        let mut clean = reader(vec![Ok(Vec::new())]);
        assert_eq!(clean.next_frame().unwrap(), NextFrame::Eof);
        assert_eq!(clean.next_frame().unwrap(), NextFrame::Eof);
        let mut partial = reader(vec![Ok(b"partial".to_vec()), Ok(Vec::new())]);
        assert_eq!(
            partial.next_frame(),
            Err(ControlReadError::Frame(FrameReadError::UnexpectedEof))
        );
        let mut io = reader(vec![Err(ErrorKind::PermissionDenied)]);
        assert_eq!(
            io.next_frame(),
            Err(ControlReadError::Io(ErrorKind::PermissionDenied))
        );
        assert_eq!(io.next_frame(), Err(ControlReadError::Closed));
    }

    #[test]
    fn one_request_per_call_retains_coalesced_frames_and_advances_sequence() {
        let first = wire(launch(4));
        let second = agent24_sidecar_host_protocol::encode_request(
            &Request::IsEmpty {
                version: 1,
                request_id: 5,
            },
            &mut {
                let mut s = RequestSequence::new();
                s.accept(&launch(4)).unwrap();
                s
            },
        )
        .unwrap();
        let mut bytes = first.clone();
        bytes.extend(second);
        let mut input = ingress(vec![Ok(bytes)]);
        assert_eq!(request(&mut input), launch(4));
        assert_eq!(
            input.next().unwrap(),
            IngressStep::Request(Request::IsEmpty {
                version: 1,
                request_id: 5
            })
        );
    }

    #[test]
    fn would_block_keeps_partial_frame_open_and_clean_eof_is_sticky() {
        let bytes = wire(launch(1));
        let mut input = ingress(vec![
            Ok(bytes[..8].to_vec()),
            Err(ErrorKind::WouldBlock),
            Ok(bytes[8..].to_vec()),
        ]);
        assert_eq!(input.next().unwrap(), IngressStep::Pending);
        assert_eq!(request(&mut input), launch(1));
        let mut clean = ingress(vec![Ok(Vec::new())]);
        assert_eq!(clean.next().unwrap(), IngressStep::Eof);
        let calls = clean.reader.reader.calls;
        assert_eq!(clean.next().unwrap(), IngressStep::Eof);
        assert_eq!(clean.reader.reader.calls, calls);
    }

    #[test]
    fn partial_eof_poison_overflow_and_io_failure_are_terminal() {
        let mut partial = ingress(vec![Ok(b"{\"type\":".to_vec()), Ok(Vec::new())]);
        closed_after_once(
            &mut partial,
            IngressError::Framing(FrameReadError::UnexpectedEof),
        );
        let mut over = ingress(vec![Ok(vec![b'x'; 65_536])]);
        closed_after_once(
            &mut over,
            IngressError::Framing(FrameReadError::TooLarge { limit: 65_536 }),
        );
        let mut io = ingress(vec![Err(ErrorKind::PermissionDenied)]);
        closed_after_once(&mut io, IngressError::Io(ErrorKind::PermissionDenied));
    }

    #[test]
    fn json_message_and_sequence_errors_poison_without_leaking_input() {
        for (bytes, expected) in [
            (b"{bad secret}\n".to_vec(), ProtocolError::InvalidJson),
            (b"{\"type\":\"signal\",\"version\":1,\"request_id\":1,\"force\":false}\n".to_vec(), ProtocolError::WrongSequence),
            (b"{\"type\":\"launch\",\"version\":1,\"request_id\":1,\"executable\":\"relative\",\"cwd\":\"/tmp\",\"argv\":[],\"env\":{}}\n".to_vec(), ProtocolError::InvalidMessage),
        ] {
            let mut input = ingress(vec![Ok(bytes)]);
            let error = input.next().unwrap_err();
            assert_eq!(error, IngressError::Protocol(expected));
            assert!(!error.to_string().contains("secret"));
            let calls = input.reader.reader.calls;
            assert_eq!(input.next(), Err(IngressError::Closed));
            assert_eq!(input.reader.reader.calls, calls);
        }
    }

    #[test]
    fn accepts_u64_max_without_arithmetic_overflow() {
        let mut input = ingress(vec![Ok(wire(launch(u64::MAX)))]);
        assert_eq!(request(&mut input), launch(u64::MAX));
    }
}
