use agent24_sidecar_host_protocol::{Event, PROTOCOL_VERSION, Reply, encode_reply};

use crate::{
    launch::OwnedLaunch,
    output_io::{OutputWriteError, OutputWriter, PutFrameError, WriteStep},
    ready_io::ReadyGate,
};
use std::io::Write;

pub(crate) trait LaunchIdentity {
    fn request_id(&self) -> u64;
}

#[cfg(any(unix, windows))]
impl LaunchIdentity for OwnedLaunch {
    fn request_id(&self) -> u64 {
        self.request_id()
    }
}

pub(crate) trait FrameSink {
    fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError>;
    fn step(&mut self) -> Result<WriteStep, OutputWriteError>;
}

impl<W: Write> FrameSink for OutputWriter<W> {
    fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError> {
        OutputWriter::put(self, frame)
    }

    fn step(&mut self) -> Result<WriteStep, OutputWriteError> {
        OutputWriter::write_step(self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LaunchOrderStage {
    Contained,
    OwnedPending,
    AwaitReady,
    Ready,
    CleanupRequired,
}

pub(crate) struct LaunchOrder<L, S> {
    launch: L,
    sink: S,
    gate: ReadyGate,
    held_ready: Option<Event>,
    stage: LaunchOrderStage,
}

impl<L: LaunchIdentity, S: FrameSink> LaunchOrder<L, S> {
    pub(crate) fn new(launch: L, sink: S) -> Self {
        Self {
            launch,
            sink,
            gate: ReadyGate::new(),
            held_ready: None,
            stage: LaunchOrderStage::Contained,
        }
    }

    pub(crate) fn queue_owned(&mut self) -> Result<(), LaunchOrderStage> {
        if self.stage != LaunchOrderStage::Contained {
            return self.fail();
        }
        let reply = Reply::Owned {
            version: PROTOCOL_VERSION,
            request_id: self.launch.request_id(),
        };
        let frame = match encode_reply(&reply) {
            Ok(frame) => frame,
            Err(_) => return self.fail(),
        };
        if self.sink.put(frame).is_err() {
            return self.fail();
        }
        self.stage = LaunchOrderStage::OwnedPending;
        Ok(())
    }

    pub(crate) fn output_step(&mut self) -> Result<WriteStep, LaunchOrderStage> {
        if self.stage != LaunchOrderStage::OwnedPending {
            return self.fail();
        }
        match self.sink.step() {
            Ok(WriteStep::Pending) => Ok(WriteStep::Pending),
            Ok(WriteStep::Complete) => {
                self.stage = LaunchOrderStage::AwaitReady;
                Ok(WriteStep::Complete)
            }
            Ok(WriteStep::Idle) | Err(_) => self.fail(),
        }
    }

    pub(crate) fn ready(&mut self, input: &[u8]) -> Result<Option<Event>, LaunchOrderStage> {
        if self.stage == LaunchOrderStage::CleanupRequired {
            return Err(self.stage);
        }
        if self.stage == LaunchOrderStage::Contained {
            return self.fail();
        }
        let event = match self.gate.push(input) {
            Ok(Some(event)) => event,
            Ok(None) => return Ok(None),
            Err(_) => return self.fail(),
        };
        match self.stage {
            LaunchOrderStage::OwnedPending => {
                self.held_ready = Some(event);
                Ok(None)
            }
            LaunchOrderStage::AwaitReady => {
                self.stage = LaunchOrderStage::Ready;
                Ok(Some(event))
            }
            _ => self.fail(),
        }
    }

    pub(crate) fn take_ready(&mut self) -> Option<Event> {
        let event = (self.stage == LaunchOrderStage::AwaitReady)
            .then(|| self.held_ready.take())
            .flatten()?;
        self.stage = LaunchOrderStage::Ready;
        Some(event)
    }

    pub(crate) fn ready_eof(&mut self) -> Result<(), LaunchOrderStage> {
        if self.stage == LaunchOrderStage::CleanupRequired {
            return Err(self.stage);
        }
        match self.gate.finish() {
            Ok(()) => Ok(()),
            Err(_) => self.fail(),
        }
    }

    fn fail<T>(&mut self) -> Result<T, LaunchOrderStage> {
        self.stage = LaunchOrderStage::CleanupRequired;
        self.held_ready = None;
        Err(LaunchOrderStage::CleanupRequired)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use agent24_sidecar_host_protocol::decode_reply;

    struct FakeLaunch(u64);

    impl LaunchIdentity for FakeLaunch {
        fn request_id(&self) -> u64 {
            self.0
        }
    }

    struct FakeSink {
        frame: Option<Vec<u8>>,
        steps: Vec<Result<WriteStep, OutputWriteError>>,
    }

    impl FrameSink for FakeSink {
        fn put(&mut self, frame: Vec<u8>) -> Result<(), PutFrameError> {
            self.frame = Some(frame);
            Ok(())
        }

        fn step(&mut self) -> Result<WriteStep, OutputWriteError> {
            self.steps.remove(0)
        }
    }

    fn order(steps: Vec<Result<WriteStep, OutputWriteError>>) -> LaunchOrder<FakeLaunch, FakeSink> {
        LaunchOrder::new(FakeLaunch(7), FakeSink { frame: None, steps })
    }

    fn ready() -> &'static [u8] {
        br#"{"type":"ready","protocol":1,"port":1,"token":"tttttttttttttttttttttttttttttttt","version":"v"}
"#
    }

    #[test]
    fn owned_and_ready_are_ordered_and_released_once() {
        let mut owned = order(vec![Ok(WriteStep::Pending), Ok(WriteStep::Complete)]);
        assert!(owned.sink.frame.is_none());
        owned.queue_owned().unwrap();
        assert!(matches!(
            decode_reply(owned.sink.frame.as_ref().unwrap()).unwrap(),
            Reply::Owned { request_id: 7, .. }
        ));
        assert_eq!(owned.ready(ready()), Ok(None));
        assert_eq!(owned.output_step(), Ok(WriteStep::Pending));
        assert_eq!(owned.output_step(), Ok(WriteStep::Complete));
        assert!(owned.take_ready().is_some());
        assert!(owned.take_ready().is_none());

        let mut late = order(vec![Ok(WriteStep::Complete)]);
        late.queue_owned().unwrap();
        late.output_step().unwrap();
        assert!(late.ready(ready()).unwrap().is_some());
    }

    #[test]
    fn malformed_order_and_ready_streams_latch_cleanup() {
        let mut before_queue = order(vec![]);
        assert_eq!(
            before_queue.output_step(),
            Err(LaunchOrderStage::CleanupRequired)
        );
        assert_eq!(
            before_queue.ready(&[]),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut pre_ready_eof = order(vec![Ok(WriteStep::Complete)]);
        pre_ready_eof.queue_owned().unwrap();
        assert_eq!(
            pre_ready_eof.ready_eof(),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut post_ready_eof = order(vec![Ok(WriteStep::Complete)]);
        post_ready_eof.queue_owned().unwrap();
        post_ready_eof.output_step().unwrap();
        assert!(post_ready_eof.ready(ready()).unwrap().is_some());
        assert_eq!(post_ready_eof.ready_eof(), Ok(()));

        let mut trailing = order(vec![Ok(WriteStep::Complete)]);
        trailing.queue_owned().unwrap();
        trailing.output_step().unwrap();
        trailing.ready(ready()).unwrap();
        let mut bytes = ready().to_vec();
        bytes.extend_from_slice(ready());
        assert_eq!(
            trailing.ready(&bytes),
            Err(LaunchOrderStage::CleanupRequired)
        );

        let mut write_error = order(vec![Err(OutputWriteError::Closed)]);
        write_error.queue_owned().unwrap();
        assert_eq!(
            write_error.output_step(),
            Err(LaunchOrderStage::CleanupRequired)
        );
        assert_eq!(
            write_error.ready_eof(),
            Err(LaunchOrderStage::CleanupRequired)
        );
    }
}
