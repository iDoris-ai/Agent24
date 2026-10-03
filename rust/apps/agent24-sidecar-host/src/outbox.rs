//! Private fixed-slot output admission.  It has no process or pipe authority.
use agent24_sidecar_host_protocol::{
    Event, MAX_CONTROL_FRAME_BYTES, ProtocolError, Reply, encode_event, encode_reply,
};

use crate::launch_order::FrameSink;
use crate::output_io::{OutputWriteError, PutFrameError, WriteStep};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FrameKind {
    Reply,
    Ready,
    Exit,
    InFlight,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum QueueError {
    Occupied(FrameKind),
    WrongVariant(FrameKind),
    Encode(ProtocolError),
    TooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DriveStep {
    Idle,
    Pending,
    Complete(FrameKind),
    Admitted(FrameKind),
}

/// Four slots, each holding at most one bounded frame.  Encoding happens
/// before a slot is inspected, so a bad frame can never replace a fact.
#[derive(Default)]
pub(crate) struct Outbox {
    pending_reply: Option<Vec<u8>>,
    ready: Option<Vec<u8>>,
    exit: Option<Vec<u8>>,
    in_flight: Option<FrameKind>,
}

impl Outbox {
    pub(crate) fn reply(&mut self, reply: &Reply) -> Result<(), QueueError> {
        self.queue(FrameKind::Reply, encode_reply(reply))
    }

    /// Compatibility seam for a reply encoded by the protocol boundary.
    pub(crate) fn encoded_reply(&mut self, bytes: Vec<u8>) -> Result<(), QueueError> {
        self.queue(FrameKind::Reply, Ok(bytes))
    }

    pub(crate) fn ready(&mut self, event: &Event) -> Result<(), QueueError> {
        if !matches!(event, Event::Ready { .. }) {
            return Err(QueueError::WrongVariant(FrameKind::Ready));
        }
        self.queue(FrameKind::Ready, encode_event(event))
    }

    pub(crate) fn exit(&mut self, event: &Event) -> Result<(), QueueError> {
        if !matches!(event, Event::Exit { .. }) {
            return Err(QueueError::WrongVariant(FrameKind::Exit));
        }
        self.queue(FrameKind::Exit, encode_event(event))
    }

    fn queue(
        &mut self,
        kind: FrameKind,
        encoded: Result<Vec<u8>, ProtocolError>,
    ) -> Result<(), QueueError> {
        let bytes = encoded.map_err(QueueError::Encode)?;
        if bytes.len() > MAX_CONTROL_FRAME_BYTES {
            return Err(QueueError::TooLarge);
        }
        let slot = match kind {
            FrameKind::Reply => &mut self.pending_reply,
            FrameKind::Ready => &mut self.ready,
            FrameKind::Exit => &mut self.exit,
            FrameKind::InFlight => return Err(QueueError::Occupied(kind)),
        };
        if slot.is_some() {
            return Err(QueueError::Occupied(kind));
        }
        *slot = Some(bytes);
        Ok(())
    }

    /// Execute exactly one sink operation.  A failed admission restores its
    /// original slot, including `Busy`, so a later drive cannot reorder it.
    pub(crate) fn drive<S: FrameSink>(
        &mut self,
        sink: &mut S,
        now: Instant,
    ) -> Result<DriveStep, PutFrameErrorOrWrite> {
        if let Some(kind) = self.in_flight {
            return match sink.step(now).map_err(PutFrameErrorOrWrite::Write)? {
                WriteStep::Idle => Ok(DriveStep::Pending),
                WriteStep::Pending => Ok(DriveStep::Pending),
                WriteStep::Complete => {
                    self.in_flight = None;
                    Ok(DriveStep::Complete(kind))
                }
            };
        }
        let kind = if self.pending_reply.is_some() {
            FrameKind::Reply
        } else if self.ready.is_some() {
            FrameKind::Ready
        } else if self.exit.is_some() {
            FrameKind::Exit
        } else {
            return Ok(DriveStep::Idle);
        };
        let Some(bytes) = self.slot(kind).take() else {
            return Ok(DriveStep::Idle);
        };
        match sink.put(bytes, now) {
            Ok(()) => {
                self.in_flight = Some(kind);
                Ok(DriveStep::Admitted(kind))
            }
            Err(error) => {
                let (failure, bytes) = match error {
                    PutFrameError::Busy(bytes) => (PutFailure::Busy, bytes),
                    PutFrameError::TooLarge(bytes) => (PutFailure::TooLarge, bytes),
                    PutFrameError::Closed(bytes) => (PutFailure::Closed, bytes),
                };
                *self.slot(kind) = Some(bytes);
                Err(PutFrameErrorOrWrite::Put(failure))
            }
        }
    }

    fn slot(&mut self, kind: FrameKind) -> &mut Option<Vec<u8>> {
        match kind {
            FrameKind::Reply => &mut self.pending_reply,
            FrameKind::Ready => &mut self.ready,
            FrameKind::Exit => &mut self.exit,
            FrameKind::InFlight => unreachable!(),
        }
    }

    pub(crate) fn exit_retained(&self) -> bool {
        self.exit.is_some() || self.in_flight == Some(FrameKind::Exit)
    }

    pub(crate) fn has_work(&self) -> bool {
        self.pending_reply.is_some()
            || self.ready.is_some()
            || self.exit_retained()
            || self.in_flight.is_some()
    }
}

/// `PutFrameError` returns its frame, but consuming it in a match keeps the
/// restoration path explicit and prevents a future error variant from losing it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PutFailure {
    Busy,
    TooLarge,
    Closed,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PutFrameErrorOrWrite {
    Put(PutFailure),
    Write(OutputWriteError),
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::output_io::OutputWriter;
    use std::io::Write;

    struct Sink(Vec<Vec<u8>>);
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.push(bytes.to_vec());
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn ready() -> Event {
        Event::Ready {
            protocol: 1,
            port: 1,
            token: "t".repeat(32),
            version: "v".into(),
        }
    }
    fn exit() -> Event {
        Event::Exit {
            protocol: 1,
            code: None,
        }
    }
    fn reply(id: u64) -> Reply {
        Reply::Result {
            version: 1,
            request_id: id,
        }
    }

    #[test]
    fn bounded_order_never_discards_admitted_ready() {
        let now = Instant::now();
        let mut outbox = Outbox::default();
        assert_eq!(outbox.reply(&reply(1)), Ok(()));
        assert_eq!(
            outbox.reply(&reply(2)),
            Err(QueueError::Occupied(FrameKind::Reply))
        );
        assert_eq!(outbox.ready(&ready()), Ok(()));
        assert_eq!(outbox.exit(&exit()), Ok(()));
        let mut sink = OutputWriter::new(Sink(vec![]));
        let expected = [FrameKind::Reply, FrameKind::Ready, FrameKind::Exit];
        for kind in expected {
            assert_eq!(outbox.drive(&mut sink, now), Ok(DriveStep::Admitted(kind)));
            assert_eq!(outbox.drive(&mut sink, now), Ok(DriveStep::Complete(kind)));
        }
    }

    #[test]
    fn wrong_variant_and_encoding_do_not_consume_exit_slot() {
        let mut outbox = Outbox::default();
        assert_eq!(
            outbox.exit(&ready()),
            Err(QueueError::WrongVariant(FrameKind::Exit))
        );
        assert!(!outbox.exit_retained());
        assert_eq!(outbox.exit(&exit()), Ok(()));
        assert_eq!(
            outbox.exit(&exit()),
            Err(QueueError::Occupied(FrameKind::Exit))
        );
    }
}
