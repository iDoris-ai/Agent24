//! Dormant host-lifetime control and output ports.

use std::{
    io::{Read, Write},
    time::Duration,
};

use crate::{
    control_worker::ControlWorker,
    output_worker::OutputWorker,
    worker_slots::{WorkerSlotError, WorkerSlots},
};

/// Contains only its fixed stage and slot result, never an I/O endpoint/error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HostPortsBuildError {
    Output(WorkerSlotError),
    Control(WorkerSlotError),
}

/// The host-lifetime workers and their `WorkerSlots` provenance.
pub(crate) struct HostPorts {
    slots: &'static WorkerSlots,
    output: OutputWorker,
    control: ControlWorker,
}

impl HostPorts {
    /// Start output before control; no frames or credits are admitted here.
    pub(crate) fn new_in<R, W>(
        slots: &'static WorkerSlots,
        control: R,
        output: W,
        output_budget: Duration,
    ) -> Result<Self, HostPortsBuildError>
    where
        R: Read + Send + 'static,
        W: Write + Send + 'static,
    {
        let output = OutputWorker::new_in(slots, output, output_budget)
            .map_err(HostPortsBuildError::Output)?;
        let control =
            ControlWorker::new_in(slots, control, None).map_err(HostPortsBuildError::Control)?;
        Ok(Self {
            slots,
            output,
            control,
        })
    }

    /// Lend the complete port set to one generation assembly.
    pub(crate) fn borrow(
        &mut self,
    ) -> (&'static WorkerSlots, &mut OutputWorker, &mut ControlWorker) {
        (self.slots, &mut self.output, &mut self.control)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        control_io::IngressStep,
        control_worker::ControlStep,
        output_io::WriteStep,
        worker_slots::{WorkerRole, WorkerSlots},
    };
    use std::{
        io,
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, Instant},
    };

    const BUDGET: Duration = Duration::from_secs(2);

    struct Counting(Arc<AtomicUsize>);

    impl Read for Counting {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Err(io::ErrorKind::WouldBlock.into())
        }
    }

    impl Write for Counting {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct BlockingWrite {
        entered: mpsc::Sender<()>,
        release: mpsc::Receiver<()>,
    }

    impl Write for BlockingWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            let _ = self.release.recv();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn counted(
        slots: &'static WorkerSlots,
        reads: Arc<AtomicUsize>,
        writes: Arc<AtomicUsize>,
    ) -> Result<HostPorts, HostPortsBuildError> {
        HostPorts::new_in(slots, Counting(reads), Counting(writes), BUDGET)
    }

    fn wait_for_output_slot(slots: &'static WorkerSlots) {
        let deadline = Instant::now() + BUDGET;
        loop {
            if let Ok(permit) = slots.reserve(WorkerRole::Output) {
                drop(permit);
                return;
            }
            assert!(Instant::now() < deadline, "output worker did not retire");
            thread::yield_now();
        }
    }

    fn wait_for_output(worker: &mut OutputWorker, now: Instant) {
        let deadline = Instant::now() + BUDGET;
        loop {
            match worker.step(now).unwrap() {
                WriteStep::Complete => return,
                WriteStep::Pending if Instant::now() < deadline => thread::yield_now(),
                step => panic!("output did not complete: {step:?}"),
            }
        }
    }

    fn wait_for_control(worker: &mut ControlWorker, now: Instant) {
        let deadline = Instant::now() + BUDGET;
        loop {
            match worker.step(now).unwrap() {
                ControlStep::Complete(IngressStep::Pending) => return,
                ControlStep::Pending if Instant::now() < deadline => thread::yield_now(),
                step => panic!("control did not complete: {step:?}"),
            }
        }
    }

    #[test]
    fn construction_and_borrow_do_not_touch_io_and_ports_remain_usable() {
        let slots = WorkerSlots::isolated();
        let reads = Arc::new(AtomicUsize::new(0));
        let writes = Arc::new(AtomicUsize::new(0));
        let mut ports = counted(slots, Arc::clone(&reads), Arc::clone(&writes)).unwrap();

        {
            let (provenance, output, control) = ports.borrow();
            assert!(std::ptr::eq(provenance, slots));
            assert_eq!(output.step(Instant::now()), Ok(WriteStep::Idle));
            assert_eq!(control.step(Instant::now()), Ok(ControlStep::Idle));
        }
        assert_eq!(reads.load(Ordering::SeqCst), 0);
        assert_eq!(writes.load(Ordering::SeqCst), 0);

        let now = Instant::now();
        {
            let (_, output, control) = ports.borrow();
            output.put(b"host-frame\n".to_vec(), now).unwrap();
            wait_for_output(output, now);
            control.permit(now).unwrap();
            wait_for_control(control, now);
        }
        assert_eq!(reads.load(Ordering::SeqCst), 1);
        assert_eq!(writes.load(Ordering::SeqCst), 1);

        let (_, output, control) = ports.borrow();
        assert_eq!(output.step(Instant::now()), Ok(WriteStep::Idle));
        assert_eq!(control.step(Instant::now()), Ok(ControlStep::Idle));
    }

    #[test]
    fn output_slot_failure_does_not_start_control() {
        let slots = WorkerSlots::isolated();
        let held = slots.reserve(WorkerRole::Output).unwrap();
        let error = match HostPorts::new_in(slots, io::empty(), Vec::new(), BUDGET) {
            Ok(_) => panic!("busy output slot constructed host ports"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            HostPortsBuildError::Output(WorkerSlotError::Busy(WorkerRole::Output))
        );
        drop(slots.reserve(WorkerRole::Control).unwrap());
        drop(held);
    }

    #[test]
    fn control_slot_failure_retires_the_unused_output_worker() {
        let slots = WorkerSlots::isolated();
        let held = slots.reserve(WorkerRole::Control).unwrap();
        let error = match HostPorts::new_in(slots, io::empty(), Vec::new(), BUDGET) {
            Ok(_) => panic!("busy control slot constructed host ports"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            HostPortsBuildError::Control(WorkerSlotError::Busy(WorkerRole::Control))
        );
        wait_for_output_slot(slots);
        assert!(matches!(
            slots.reserve(WorkerRole::Control),
            Err(WorkerSlotError::Busy(WorkerRole::Control))
        ));
        drop(held);
    }

    #[test]
    fn dropping_ports_does_not_release_a_blocked_output_slot() {
        let slots = WorkerSlots::isolated();
        let (entered_tx, entered) = mpsc::channel();
        let (release_tx, release) = mpsc::channel();
        let mut ports = HostPorts::new_in(
            slots,
            Counting(Arc::new(AtomicUsize::new(0))),
            BlockingWrite {
                entered: entered_tx,
                release,
            },
            BUDGET,
        )
        .unwrap();
        let (_, output, _) = ports.borrow();
        output.put(b"blocked\n".to_vec(), Instant::now()).unwrap();
        entered.recv_timeout(BUDGET).unwrap();

        drop(ports);
        assert!(matches!(
            slots.reserve(WorkerRole::Output),
            Err(WorkerSlotError::Busy(WorkerRole::Output))
        ));
        release_tx.send(()).unwrap();
        wait_for_output_slot(slots);
    }
}
