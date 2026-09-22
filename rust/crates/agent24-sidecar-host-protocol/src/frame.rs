//! Bounded byte framing for private sidecar NDJSON streams.

use std::mem;

pub const FRAME_READ_CHUNK_BYTES: usize = 4096;

pub enum FrameRead {
    NeedMore { consumed: usize },
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
        Ok(FrameRead::NeedMore { consumed: take })
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
    use super::super::{Request, RequestSequence, decode_request, encode_request};
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
        let input = b"one\nrest\n";
        let consumed = match reader.push(input).unwrap() {
            FrameRead::Complete { consumed, frame } => {
                assert_eq!(frame, b"one\n");
                consumed
            }
            _ => panic!("expected first frame"),
        };
        complete(reader.push(&input[consumed..]).unwrap(), b"rest\n", 5);
        complete(reader.push(b"\r\n").unwrap(), b"\r\n", 2);
        let mut bytewise = NdjsonFrameReader::with_limit(16);
        for byte in b"abc\n" {
            match bytewise.push(std::slice::from_ref(byte)).unwrap() {
                FrameRead::NeedMore { consumed: 1 } if *byte != b'\n' => (),
                FrameRead::Complete { consumed: 1, frame } if *byte == b'\n' => {
                    assert_eq!(frame, b"abc\n");
                }
                _ => panic!("unexpected framing result"),
            }
        }
    }

    #[test]
    fn crlf_is_preserved_as_two_framing_bytes() {
        let mut reader = NdjsonFrameReader::with_limit(4);
        complete(reader.push(b"\r\n").unwrap(), b"\r\n", 2);
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
        assert!(matches!(
            eof.push(b"part"),
            Ok(FrameRead::NeedMore { consumed: 4 })
        ));
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
                    FrameRead::NeedMore { .. } => (),
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

    #[test]
    fn push_reports_bounded_progress_and_empty_input() {
        let mut reader = NdjsonFrameReader::with_limit(8192);
        assert!(matches!(
            reader.push(b""),
            Ok(FrameRead::NeedMore { consumed: 0 })
        ));
        let mut input = vec![b'x'; FRAME_READ_CHUNK_BYTES];
        input.push(b'\n');
        assert!(matches!(
            reader.push(&input),
            Ok(FrameRead::NeedMore {
                consumed: FRAME_READ_CHUNK_BYTES
            })
        ));
        complete(
            reader.push(&input[FRAME_READ_CHUNK_BYTES..]).unwrap(),
            &input,
            1,
        );
    }

    #[test]
    fn raw_invalid_json_and_utf8_are_framed_before_decode_rejects_them() {
        for raw in [&b"\xff\n"[..], b"{broken}\n"] {
            let mut reader = NdjsonFrameReader::control();
            let frame = match reader.push(raw).unwrap() {
                FrameRead::Complete { consumed, frame } => {
                    assert_eq!(consumed, raw.len());
                    frame
                }
                _ => panic!("framing must not parse payload bytes"),
            };
            assert_eq!(
                decode_request(&frame, &mut RequestSequence::new()),
                Err(super::super::ProtocolError::InvalidJson)
            );
        }
    }

    #[test]
    fn crafted_growth_and_large_slices_remain_bounded_and_progress() {
        let mut small = NdjsonFrameReader::with_limit(5000);
        assert!(matches!(
            small.push(b"x"),
            Ok(FrameRead::NeedMore { consumed: 1 })
        ));
        assert!(matches!(
            small.push(&[b'x'; FRAME_READ_CHUNK_BYTES]),
            Ok(FrameRead::NeedMore {
                consumed: FRAME_READ_CHUNK_BYTES
            })
        ));
        assert!(small.buffer.capacity() <= small.limit);

        let mut input = vec![b'x'; FRAME_READ_CHUNK_BYTES * 3];
        input.push(b'\n');
        let mut reader = NdjsonFrameReader::control();
        let mut offset = 0;
        loop {
            match reader.push(&input[offset..]).unwrap() {
                FrameRead::NeedMore { consumed } => {
                    assert!(consumed > 0 && consumed <= FRAME_READ_CHUNK_BYTES);
                    offset += consumed;
                }
                FrameRead::Complete { consumed, frame } => {
                    assert!(consumed <= FRAME_READ_CHUNK_BYTES);
                    assert_eq!(offset + consumed, input.len());
                    assert_eq!(frame, input);
                    break;
                }
            }
        }
    }

    #[test]
    fn production_limits_reject_unterminated_control_and_ready_frames() {
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
            let input = vec![b'x'; limit + 1];
            let mut offset = 0;
            loop {
                match reader.push(&input[offset..]) {
                    Ok(FrameRead::NeedMore { consumed }) => offset += consumed,
                    Err(FrameReadError::TooLarge { limit: actual }) => {
                        assert_eq!(actual, limit);
                        break;
                    }
                    _ => panic!("unterminated frame must exceed its limit"),
                }
            }
        }
    }

    #[test]
    fn encoded_request_roundtrips_through_multichunk_framing() {
        #[cfg(windows)]
        let (executable, cwd) = (r"C:\agent\helper.exe", r"C:\agent");
        #[cfg(not(windows))]
        let (executable, cwd) = ("/agent/helper", "/agent");
        let request = Request::Launch {
            version: 1,
            request_id: 1,
            executable: executable.into(),
            cwd: cwd.into(),
            argv: vec!["x".repeat(4096); 2],
            env: Default::default(),
        };
        let encoded = encode_request(&request, &mut RequestSequence::new()).unwrap();
        let mut reader = NdjsonFrameReader::control();
        let mut offset = 0;
        let frame = loop {
            match reader.push(&encoded[offset..]).unwrap() {
                FrameRead::NeedMore { consumed } => {
                    assert!(consumed <= FRAME_READ_CHUNK_BYTES);
                    offset += consumed;
                }
                FrameRead::Complete { consumed, frame } => {
                    assert!(consumed <= FRAME_READ_CHUNK_BYTES);
                    break frame;
                }
            }
        };
        assert_eq!(
            decode_request(&frame, &mut RequestSequence::new()),
            Ok(request)
        );
    }
}
