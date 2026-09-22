use agent24_sidecar_host_protocol::MAX_CONTROL_FRAME_BYTES;
use std::io::{ErrorKind, Write};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PutFrameError {
    Busy(Vec<u8>),
    TooLarge(Vec<u8>),
    Closed(Vec<u8>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WriteStep {
    Idle,
    Pending,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OutputWriteError {
    Io(ErrorKind),
    Closed,
}

pub(crate) struct OutputWriter<W> {
    writer: W,
    frame: Option<Vec<u8>>,
    offset: usize,
    terminal: bool,
}

impl<W: Write> OutputWriter<W> {
    pub(crate) fn new(writer: W) -> Self {
        Self {
            writer,
            frame: None,
            offset: 0,
            terminal: false,
        }
    }

    pub(crate) fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError> {
        if self.terminal {
            return Err(PutFrameError::Closed(frame));
        }
        if frame.len() > MAX_CONTROL_FRAME_BYTES {
            return Err(PutFrameError::TooLarge(frame));
        }
        if self.frame.is_some() {
            return Err(PutFrameError::Busy(frame));
        }
        self.frame = Some(frame);
        self.offset = 0;
        Ok(())
    }

    pub(crate) fn write_step(&mut self) -> Result<WriteStep, OutputWriteError> {
        if self.terminal {
            return Err(OutputWriteError::Closed);
        }
        let Some(frame) = self.frame.as_ref() else {
            return Ok(WriteStep::Idle);
        };
        if self.offset < frame.len() {
            match self.writer.write(&frame[self.offset..]) {
                Ok(0) => return self.fail(ErrorKind::WriteZero),
                Ok(written) => {
                    self.offset += written;
                    if self.offset < frame.len() {
                        return Ok(WriteStep::Pending);
                    }
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) =>
                {
                    return Ok(WriteStep::Pending);
                }
                Err(error) => return self.fail(error.kind()),
            }
        }
        match self.writer.flush() {
            Ok(()) => {
                self.frame = None;
                self.offset = 0;
                Ok(WriteStep::Complete)
            }
            Err(error)
                if matches!(error.kind(), ErrorKind::Interrupted | ErrorKind::WouldBlock) =>
            {
                Ok(WriteStep::Pending)
            }
            Err(error) => self.fail(error.kind()),
        }
    }

    fn fail<T>(&mut self, kind: ErrorKind) -> Result<T, OutputWriteError> {
        self.terminal = true;
        self.frame = None;
        self.offset = 0;
        Err(OutputWriteError::Io(kind))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    struct ScriptedWrite {
        writes: Vec<Result<usize, ErrorKind>>,
        flushes: Vec<Result<(), ErrorKind>>,
        bytes: Vec<u8>,
    }
    impl Write for ScriptedWrite {
        fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
            let limit = self.writes.remove(0).map_err(std::io::Error::from)?;
            let written = limit.min(input.len());
            self.bytes.extend_from_slice(&input[..written]);
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            self.flushes.remove(0).map_err(std::io::Error::from)
        }
    }
    fn output(
        writes: Vec<Result<usize, ErrorKind>>,
        flushes: Vec<Result<(), ErrorKind>>,
    ) -> OutputWriter<ScriptedWrite> {
        OutputWriter::new(ScriptedWrite {
            writes,
            flushes,
            bytes: Vec::new(),
        })
    }

    #[test]
    fn short_writes_preserve_exact_bytes_and_busy_frame() {
        let mut output = output(vec![Ok(2), Ok(8)], vec![Ok(())]);
        output.put(b"abcd\n".to_vec()).unwrap();
        let busy = b"next\n".to_vec();
        assert_eq!(output.put(busy.clone()), Err(PutFrameError::Busy(busy)));
        assert_eq!(output.write_step().unwrap(), WriteStep::Pending);
        assert_eq!(output.write_step().unwrap(), WriteStep::Complete);
        assert_eq!(output.writer.bytes, b"abcd\n");
        assert_eq!(output.write_step().unwrap(), WriteStep::Idle);
    }

    #[test]
    fn interrupted_would_block_and_flush_pending_resume_same_frame() {
        let mut output = output(
            vec![
                Err(ErrorKind::Interrupted),
                Err(ErrorKind::WouldBlock),
                Ok(8),
            ],
            vec![Err(ErrorKind::WouldBlock), Ok(())],
        );
        output.put(b"frame\n".to_vec()).unwrap();
        for _ in 0..3 {
            assert_eq!(output.write_step().unwrap(), WriteStep::Pending);
        }
        assert_eq!(output.writer.bytes, b"frame\n");
        assert_eq!(output.write_step().unwrap(), WriteStep::Complete);
        assert_eq!(output.writer.bytes, b"frame\n");
    }

    #[test]
    fn limit_and_terminal_failures_return_or_reject_frames() {
        let mut writer = output(vec![Ok(0)], vec![]);
        let large = vec![0; MAX_CONTROL_FRAME_BYTES + 1];
        assert_eq!(
            writer.put(large.clone()),
            Err(PutFrameError::TooLarge(large))
        );
        writer.put(b"frame\n".to_vec()).unwrap();
        assert_eq!(
            writer.write_step(),
            Err(OutputWriteError::Io(ErrorKind::WriteZero))
        );
        let rejected = b"again\n".to_vec();
        assert_eq!(
            writer.put(rejected.clone()),
            Err(PutFrameError::Closed(rejected))
        );
        assert_eq!(writer.write_step(), Err(OutputWriteError::Closed));

        let mut broken = output(vec![Err(ErrorKind::BrokenPipe)], vec![]);
        broken.put(b"frame\n".to_vec()).unwrap();
        assert_eq!(
            broken.write_step(),
            Err(OutputWriteError::Io(ErrorKind::BrokenPipe))
        );
    }
}
