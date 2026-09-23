use agent24_sidecar_host_protocol::frame::{FrameRead, FrameReadError, NdjsonFrameReader};
use agent24_sidecar_host_protocol::{Event, ProtocolError, decode_event};
use std::fmt;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReadyGateError {
    Closed,
    Framing(FrameReadError),
    Decode(ProtocolError),
    UnexpectedEvent,
    TrailingData,
    MissingReady,
    UnexpectedEof,
}

impl fmt::Display for ReadyGateError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Closed => "ready gate is closed",
            Self::Framing(_) => "ready frame rejected",
            Self::Decode(_) => "ready event rejected",
            Self::UnexpectedEvent => "unexpected ready-stream event",
            Self::TrailingData => "trailing data after ready",
            Self::MissingReady => "ready event missing",
            Self::UnexpectedEof => "incomplete ready event",
        })
    }
}
impl std::error::Error for ReadyGateError {}
enum State {
    Awaiting,
    Ready,
    Eof,
    Closed,
}
pub(crate) struct ReadyGate {
    framing: NdjsonFrameReader,
    state: State,
    consumed: usize,
}
impl ReadyGate {
    pub(crate) fn new() -> Self {
        Self {
            framing: NdjsonFrameReader::target_ready(),
            state: State::Awaiting,
            consumed: 0,
        }
    }
    pub(crate) fn push(&mut self, input: &[u8]) -> Result<Option<Event>, ReadyGateError> {
        if matches!(self.state, State::Closed) {
            return Err(ReadyGateError::Closed);
        }
        if matches!(self.state, State::Ready | State::Eof) {
            if !input.is_empty() {
                self.consumed = self.consumed.saturating_add(input.len());
                return Err(self.fail(ReadyGateError::TrailingData));
            }
            return Ok(None);
        }
        let mut offset = 0;
        while offset < input.len() {
            let result = self.framing.push(&input[offset..]);
            match result {
                Ok(FrameRead::NeedMore { consumed }) => {
                    offset += consumed;
                    self.consumed = self.consumed.saturating_add(consumed);
                }
                Ok(FrameRead::Complete { consumed, frame }) => {
                    offset += consumed;
                    self.consumed = self.consumed.saturating_add(consumed);
                    let event = decode_event(&frame)
                        .map_err(|error| self.fail(ReadyGateError::Decode(error)))?;
                    match event {
                        Event::Ready { .. } => {
                            self.state = State::Ready;
                            if offset < input.len() {
                                self.consumed = self
                                    .consumed
                                    .saturating_add(input.len().saturating_sub(offset));
                                return Err(self.fail(ReadyGateError::TrailingData));
                            }
                            return Ok(Some(event));
                        }
                        Event::Exit { .. } => {
                            return Err(self.fail(ReadyGateError::UnexpectedEvent));
                        }
                    }
                }
                Err(error) => return Err(self.fail(ReadyGateError::Framing(error))),
            }
        }
        Ok(None)
    }
    pub(crate) fn finish(&mut self) -> Result<(), ReadyGateError> {
        match self.state {
            State::Closed => Err(ReadyGateError::Closed),
            State::Ready => {
                self.state = State::Eof;
                Ok(())
            }
            State::Eof => Ok(()),
            State::Awaiting => match self.framing.finish() {
                Ok(()) => Err(self.fail(ReadyGateError::MissingReady)),
                Err(FrameReadError::UnexpectedEof) => Err(self.fail(ReadyGateError::UnexpectedEof)),
                Err(error) => Err(self.fail(ReadyGateError::Framing(error))),
            },
        }
    }
    pub(crate) fn consumed(&self) -> usize {
        self.consumed
    }
    fn fail(&mut self, error: ReadyGateError) -> ReadyGateError {
        self.state = State::Closed;
        error
    }
}
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use agent24_sidecar_host_protocol::{MAX_TARGET_READY_FRAME_BYTES, encode_event};
    fn ready() -> Event {
        Event::Ready {
            protocol: 1,
            port: 4312,
            token: "t".repeat(32),
            version: "sidecar-test".into(),
        }
    }
    fn feed(bytes: &[u8], width: usize) -> Option<Event> {
        let mut gate = ReadyGate::new();
        bytes
            .chunks(width)
            .find_map(|chunk| gate.push(chunk).unwrap())
    }
    #[test]
    fn accepts_chunked_and_bytewise_ready_without_changing_fields() {
        let bytes = encode_event(&ready()).unwrap();
        assert_eq!(feed(&bytes, 1), Some(ready()));
        assert_eq!(feed(&bytes, 3), Some(ready()));
    }
    #[test]
    fn rejects_forged_exit_event_and_poisoning_is_terminal() {
        let bytes = b"{\"type\":\"exit\",\"protocol\":1}\n";
        let mut gate = ReadyGate::new();
        assert_eq!(gate.push(bytes), Err(ReadyGateError::UnexpectedEvent));
        assert_eq!(gate.push(b"anything"), Err(ReadyGateError::Closed));
    }
    #[test]
    fn rejects_same_buffer_and_later_trailing_data() {
        let first = encode_event(&ready()).unwrap();
        let second = encode_event(&ready()).unwrap();
        let mut same = ReadyGate::new();
        let mut input = first.clone();
        input.extend_from_slice(b" \n");
        assert_eq!(same.push(&input), Err(ReadyGateError::TrailingData));
        assert_eq!(same.consumed(), input.len());
        let mut later = ReadyGate::new();
        assert_eq!(later.push(&first), Ok(Some(ready())));
        assert_eq!(later.push(&second), Err(ReadyGateError::TrailingData));
    }
    #[test]
    fn distinguishes_clean_partial_and_post_ready_eof() {
        let mut clean = ReadyGate::new();
        assert_eq!(clean.finish(), Err(ReadyGateError::MissingReady));
        let mut partial = ReadyGate::new();
        partial.push(b"{\"type\":").unwrap();
        assert_eq!(partial.finish(), Err(ReadyGateError::UnexpectedEof));
        let mut ready_gate = ReadyGate::new();
        ready_gate.push(&encode_event(&ready()).unwrap()).unwrap();
        assert_eq!(ready_gate.finish(), Ok(()));
        assert_eq!(ready_gate.finish(), Ok(()));
    }
    #[test]
    fn limit_boundary_and_overlimit_are_rejected_without_leaking_input() {
        let mut exact = ReadyGate::new();
        let mut at_limit = vec![b'x'; MAX_TARGET_READY_FRAME_BYTES - 1];
        at_limit.push(b'\n');
        assert_eq!(
            exact.push(&at_limit),
            Err(ReadyGateError::Decode(ProtocolError::InvalidJson))
        );
        let mut over = ReadyGate::new();
        let mut too_large = vec![b'x'; MAX_TARGET_READY_FRAME_BYTES];
        too_large.push(b'\n');
        assert!(matches!(
            over.push(&too_large),
            Err(ReadyGateError::Framing(FrameReadError::TooLarge { .. }))
        ));
        let secret = "secret-token-that-must-not-appear-0123456789";
        let mut bad = ReadyGate::new();
        let error = bad
            .push(format!("{{broken:{secret}}}\n").as_bytes())
            .unwrap_err();
        assert!(!error.to_string().contains(secret));
    }
}
