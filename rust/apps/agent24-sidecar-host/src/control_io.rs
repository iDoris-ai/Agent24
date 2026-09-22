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

pub(crate) struct ControlReader<R> {
    reader: R,
    framing: NdjsonFrameReader,
    input: [u8; FRAME_READ_CHUNK_BYTES],
    start: usize,
    end: usize,
    terminal: Terminal,
}

enum Terminal {
    Open,
    Eof,
    Failed,
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct ScriptedRead {
        steps: Vec<Result<Vec<u8>, ErrorKind>>,
        pending: Vec<u8>,
    }
    impl Read for ScriptedRead {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
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
    fn reader(steps: Vec<Result<Vec<u8>, ErrorKind>>) -> ControlReader<ScriptedRead> {
        ControlReader::new(ScriptedRead {
            steps,
            pending: Vec::new(),
        })
    }
    fn frame(reader: &mut ControlReader<ScriptedRead>) -> Vec<u8> {
        let NextFrame::Frame(frame) = reader.next_frame().unwrap() else {
            panic!("expected frame")
        };
        frame
    }
    #[test]
    fn preserves_two_frames_from_one_read() {
        let mut reader = reader(vec![Ok(b"one\ntwo\n".to_vec()), Ok(Vec::new())]);
        assert_eq!(frame(&mut reader), b"one\n");
        assert_eq!(frame(&mut reader), b"two\n");
        assert_eq!(reader.next_frame().unwrap(), NextFrame::Eof);
    }
    #[test]
    fn spans_reads_and_handles_partial_consumption() {
        let mut reader = reader(vec![
            Ok(b"one\npa".to_vec()),
            Ok(b"rt\n".to_vec()),
            Ok(Vec::new()),
        ]);
        assert_eq!(frame(&mut reader), b"one\n");
        assert_eq!(frame(&mut reader), b"part\n");
    }

    #[test]
    fn accepts_frame_at_limit_and_rejects_overflow_terminally() {
        let mut at_limit = reader(vec![Ok(vec![b'x'; 65_535]), Ok(b"\n".to_vec())]);
        assert_eq!(frame(&mut at_limit).len(), 65_536);
        let mut over = reader(vec![Ok(vec![b'x'; 65_536]), Ok(b"\n".to_vec())]);
        assert!(matches!(
            over.next_frame(),
            Err(ControlReadError::Frame(FrameReadError::TooLarge { .. }))
        ));
        assert_eq!(over.next_frame(), Err(ControlReadError::Closed));
    }

    #[test]
    fn eof_distinguishes_clean_and_partial_input() {
        let mut clean = reader(vec![Ok(Vec::new())]);
        assert_eq!(clean.next_frame().unwrap(), NextFrame::Eof);
        assert_eq!(clean.next_frame().unwrap(), NextFrame::Eof);
        let mut partial = reader(vec![Ok(b"partial".to_vec()), Ok(Vec::new())]);
        assert_eq!(
            partial.next_frame(),
            Err(ControlReadError::Frame(FrameReadError::UnexpectedEof))
        );
        assert_eq!(partial.next_frame(), Err(ControlReadError::Closed));
    }

    #[test]
    fn retries_interrupted_and_keeps_cursor_on_would_block() {
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
    fn io_failure_is_fixed_and_terminal() {
        let mut reader = reader(vec![Err(ErrorKind::PermissionDenied)]);
        assert_eq!(
            reader.next_frame(),
            Err(ControlReadError::Io(ErrorKind::PermissionDenied))
        );
        assert_eq!(reader.next_frame(), Err(ControlReadError::Closed));
    }
}
