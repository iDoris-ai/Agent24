//! Bounded byte framing for private sidecar NDJSON streams.

use std::mem;

pub const FRAME_READ_CHUNK_BYTES: usize = 4096;

pub enum FrameRead {
    NeedMore,
    Complete { consumed: usize, frame: Vec<u8> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameReadError {
    TooLarge { limit: usize },
    UnexpectedEof,
    AllocationFailed,
    Closed,
}

pub struct NdjsonFrameReader {
    buffer: Vec<u8>,
    limit: usize,
    closed: bool,
}

impl NdjsonFrameReader {
    pub fn control() -> Self {
        Self::new(super::MAX_CONTROL_FRAME_BYTES)
    }

    pub fn target_ready() -> Self {
        Self::new(super::MAX_TARGET_READY_FRAME_BYTES)
    }

    fn new(limit: usize) -> Self {
        Self {
            buffer: Vec::new(),
            limit,
            closed: false,
        }
    }

    #[cfg(test)]
    fn with_limit(limit: usize) -> Self {
        Self::new(limit)
    }

    fn fail(&mut self, error: FrameReadError) -> FrameReadError {
        self.buffer.clear();
        self.closed = true;
        error
    }

    pub fn push(&mut self, input: &[u8]) -> Result<FrameRead, FrameReadError> {
        if self.closed {
            return Err(FrameReadError::Closed);
        }
        let scan = &input[..input.len().min(FRAME_READ_CHUNK_BYTES)];
        let (take, complete) = match scan.iter().position(|byte| *byte == b'\n') {
            Some(index) => (index + 1, true),
            None => (scan.len(), false),
        };
        let Some(total) = self.buffer.len().checked_add(take) else {
            return Err(self.fail(FrameReadError::TooLarge { limit: self.limit }));
        };
        if total > self.limit || (!complete && total == self.limit) {
            return Err(self.fail(FrameReadError::TooLarge { limit: self.limit }));
        }
        if take > 0 && self.buffer.try_reserve_exact(take).is_err() {
            return Err(self.fail(FrameReadError::AllocationFailed));
        }
        self.buffer.extend_from_slice(&scan[..take]);
        if complete {
            return Ok(FrameRead::Complete {
                consumed: take,
                frame: mem::take(&mut self.buffer),
            });
        }
        Ok(FrameRead::NeedMore)
    }

    pub fn finish(&mut self) -> Result<(), FrameReadError> {
        if self.closed {
            return Err(FrameReadError::Closed);
        }
        if self.buffer.is_empty() {
            self.closed = true;
            return Ok(());
        }
        Err(self.fail(FrameReadError::UnexpectedEof))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn complete(result: FrameRead, bytes: &[u8], consumed: usize) {
        match result {
            FrameRead::Complete { consumed: n, frame } => {
                assert_eq!(n, consumed);
                assert_eq!(frame, bytes);
            }
            _ => panic!("expected complete frame"),
        }
    }

    #[test]
    fn frames_across_push_shapes_and_leaves_next_frame_unconsumed() {
        let mut reader = NdjsonFrameReader::with_limit(16);
        complete(reader.push(b"one\nrest").unwrap(), b"one\n", 4);
        complete(reader.push(b"\r\n").unwrap(), b"\r\n", 2);
        let mut bytewise = NdjsonFrameReader::with_limit(16);
        for byte in b"abc\n" {
            match bytewise.push(std::slice::from_ref(byte)).unwrap() {
                FrameRead::NeedMore if *byte != b'\n' => (),
                FrameRead::Complete { consumed: 1, frame } if *byte == b'\n' => {
                    assert_eq!(frame, b"abc\n");
                }
                _ => panic!("unexpected framing result"),
            }
        }
    }

    #[test]
    fn exact_limit_and_over_limit_poison_without_resynchronizing() {
        let mut exact = NdjsonFrameReader::with_limit(4);
        complete(exact.push(b"abc\n").unwrap(), b"abc\n", 4);
        let mut over = NdjsonFrameReader::with_limit(4);
        assert!(matches!(
            over.push(b"abcd"),
            Err(FrameReadError::TooLarge { limit: 4 })
        ));
        assert!(matches!(over.push(b"\n"), Err(FrameReadError::Closed)));
    }

    #[test]
    fn eof_and_constructor_limits_are_fail_closed() {
        let mut eof = NdjsonFrameReader::with_limit(8);
        assert!(matches!(eof.push(b"part"), Ok(FrameRead::NeedMore)));
        assert_eq!(eof.finish(), Err(FrameReadError::UnexpectedEof));
        assert!(matches!(eof.push(b"\n"), Err(FrameReadError::Closed)));
        assert_eq!(
            NdjsonFrameReader::control().limit,
            super::super::MAX_CONTROL_FRAME_BYTES
        );
        assert_eq!(
            NdjsonFrameReader::target_ready().limit,
            super::super::MAX_TARGET_READY_FRAME_BYTES
        );
        for (mut reader, limit) in [
            (
                NdjsonFrameReader::control(),
                super::super::MAX_CONTROL_FRAME_BYTES,
            ),
            (
                NdjsonFrameReader::target_ready(),
                super::super::MAX_TARGET_READY_FRAME_BYTES,
            ),
        ] {
            let mut frame = vec![b'x'; limit];
            frame[limit - 1] = b'\n';
            let mut result = None;
            for chunk in frame.chunks(FRAME_READ_CHUNK_BYTES) {
                match reader.push(chunk).unwrap() {
                    FrameRead::NeedMore => (),
                    FrameRead::Complete {
                        consumed,
                        frame: bytes,
                    } => {
                        assert!(consumed <= FRAME_READ_CHUNK_BYTES);
                        result = Some(bytes);
                    }
                }
            }
            assert_eq!(result.as_deref(), Some(frame.as_slice()));
        }
    }
}
