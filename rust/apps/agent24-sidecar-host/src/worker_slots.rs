use std::{
    fmt, io,
    io::ErrorKind,
    sync::{
        OnceLock,
        atomic::{AtomicU8, Ordering},
    },
    thread,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerRole {
    Control,
    Output,
    ReadyRead,
    StderrDrain,
}

impl WorkerRole {
    const fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerSlotError {
    Busy(WorkerRole),
    Spawn(ErrorKind),
}

impl fmt::Display for WorkerSlotError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Busy(role) => write!(formatter, "sidecar worker slot is busy: {role:?}"),
            Self::Spawn(kind) => write!(formatter, "sidecar worker spawn failed: {kind:?}"),
        }
    }
}

pub(crate) struct WorkerSlots {
    occupied: AtomicU8,
}

impl WorkerSlots {
    const fn new() -> Self {
        Self {
            occupied: AtomicU8::new(0),
        }
    }

    pub(crate) fn host() -> &'static Self {
        static HOST: OnceLock<WorkerSlots> = OnceLock::new();
        HOST.get_or_init(Self::new)
    }

    pub(crate) fn reserve(
        &'static self,
        role: WorkerRole,
    ) -> Result<WorkerPermit, WorkerSlotError> {
        let bit = role.bit();
        let mut occupied = self.occupied.load(Ordering::Acquire);
        loop {
            if occupied & bit != 0 {
                return Err(WorkerSlotError::Busy(role));
            }
            match self.occupied.compare_exchange_weak(
                occupied,
                occupied | bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(WorkerPermit { slots: self, role }),
                Err(current) => occupied = current,
            }
        }
    }

    pub(crate) fn spawn<F>(
        &'static self,
        role: WorkerRole,
        name: &'static str,
        worker: F,
    ) -> Result<(), WorkerSlotError>
    where
        F: FnOnce(WorkerPermit) + Send + 'static,
    {
        self.spawn_with(role, name, worker, |builder, job| {
            builder.spawn(job).map(|_| ())
        })
    }

    fn spawn_with<F, S>(
        &'static self,
        role: WorkerRole,
        name: &'static str,
        worker: F,
        start: S,
    ) -> Result<(), WorkerSlotError>
    where
        F: FnOnce(WorkerPermit) + Send + 'static,
        S: FnOnce(thread::Builder, Box<dyn FnOnce() + Send>) -> io::Result<()>,
    {
        let permit = self.reserve(role)?;
        let job = Box::new(move || worker(permit));
        start(thread::Builder::new().name(name.into()), job)
            .map_err(|error| WorkerSlotError::Spawn(error.kind()))
    }

    #[cfg(test)]
    pub(crate) fn isolated() -> &'static Self {
        Box::leak(Box::new(Self::new()))
    }
}

pub(crate) struct WorkerPermit {
    slots: &'static WorkerSlots,
    role: WorkerRole,
}

impl Drop for WorkerPermit {
    fn drop(&mut self) {
        self.slots
            .occupied
            .fetch_and(!self.role.bit(), Ordering::Release);
    }
}

pub(crate) fn hold_permit(permit: WorkerPermit, work: impl FnOnce()) {
    work();
    drop(permit);
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::{
        control_worker::ControlWorker,
        output_worker::OutputWorker,
        ready_read_worker::ReadyReadWorker,
        stderr_drain_worker::{StderrDrainStatus, StderrDrainWorker},
    };
    use std::{
        io::{Read, Write},
        sync::{Arc, Condvar, Mutex, mpsc},
        time::{Duration, Instant},
    };

    #[derive(Clone)]
    struct Gate(Arc<(Mutex<bool>, Condvar)>);

    impl Gate {
        fn new() -> Self {
            Self(Arc::new((Mutex::new(false), Condvar::new())))
        }

        fn wait(&self) {
            let (open, wake) = &*self.0;
            let mut open = open.lock().unwrap();
            while !*open {
                open = wake.wait(open).unwrap();
            }
        }

        fn open(&self) {
            let (open, wake) = &*self.0;
            *open.lock().unwrap() = true;
            wake.notify_all();
        }
    }

    struct BlockingRead {
        gate: Gate,
        entered: mpsc::Sender<()>,
    }

    impl Read for BlockingRead {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            let _ = self.entered.send(());
            self.gate.wait();
            Ok(0)
        }
    }

    struct BlockingWrite(BlockingRead);

    impl Write for BlockingWrite {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let _ = self.0.entered.send(());
            self.0.gate.wait();
            Ok(bytes.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn wait_for(entered: &mpsc::Receiver<()>, count: usize) {
        for _ in 0..count {
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
        }
    }

    fn release_then_reserve(slots: &'static WorkerSlots, gate: Gate, roles: &[WorkerRole]) {
        gate.open();
        let deadline = Instant::now() + Duration::from_secs(2);
        for &role in roles {
            loop {
                if let Ok(permit) = slots.reserve(role) {
                    drop(permit);
                    break;
                }
                assert!(Instant::now() < deadline, "{role:?} never released");
                thread::yield_now();
            }
        }
    }

    #[test]
    fn roles_are_independent_and_each_role_is_exclusive() {
        let slots = WorkerSlots::isolated();
        let permits = [
            slots.reserve(WorkerRole::Control).unwrap(),
            slots.reserve(WorkerRole::Output).unwrap(),
            slots.reserve(WorkerRole::ReadyRead).unwrap(),
            slots.reserve(WorkerRole::StderrDrain).unwrap(),
        ];
        for role in [
            WorkerRole::Control,
            WorkerRole::Output,
            WorkerRole::ReadyRead,
            WorkerRole::StderrDrain,
        ] {
            assert!(matches!(
                slots.reserve(role),
                Err(WorkerSlotError::Busy(actual)) if actual == role
            ));
        }
        drop(permits);
        for role in [
            WorkerRole::Control,
            WorkerRole::Output,
            WorkerRole::ReadyRead,
            WorkerRole::StderrDrain,
        ] {
            drop(slots.reserve(role).unwrap());
        }
    }

    #[test]
    fn failed_spawn_returns_the_reservation() {
        let slots = WorkerSlots::isolated();
        let error = slots.spawn_with(
            WorkerRole::Control,
            "test",
            |_| {},
            |_, _| Err(io::Error::from(ErrorKind::Other)),
        );
        assert_eq!(error, Err(WorkerSlotError::Spawn(ErrorKind::Other)));
        drop(slots.reserve(WorkerRole::Control).unwrap());
    }

    #[test]
    fn blocking_adapters_keep_all_roles_busy_after_drop() {
        let slots = WorkerSlots::isolated();
        let gate = Gate::new();
        let (entered_tx, entered) = mpsc::channel();
        let read = || BlockingRead {
            gate: gate.clone(),
            entered: entered_tx.clone(),
        };
        let now = Instant::now();
        let mut control = ControlWorker::new_in(slots, read(), Duration::from_secs(1)).unwrap();
        let mut output =
            OutputWorker::new_in(slots, BlockingWrite(read()), Duration::from_secs(1)).unwrap();
        let mut ready = ReadyReadWorker::new_in(slots, read()).unwrap();
        let stderr = StderrDrainWorker::new_in(slots, read()).unwrap();
        control.permit(now).unwrap();
        output.put(b"frame\n".to_vec(), now).unwrap();
        ready.permit().unwrap();
        wait_for(&entered, 4);
        drop((control, output, ready, stderr));
        let roles = [
            WorkerRole::Control,
            WorkerRole::Output,
            WorkerRole::ReadyRead,
            WorkerRole::StderrDrain,
        ];
        for role in roles {
            assert!(
                matches!(slots.reserve(role), Err(WorkerSlotError::Busy(actual)) if actual == role)
            );
        }
        release_then_reserve(slots, gate, &roles);
    }

    #[test]
    fn timeout_does_not_release_a_blocked_worker_slot() {
        let slots = WorkerSlots::isolated();
        let gate = Gate::new();
        let (entered_tx, entered) = mpsc::channel();
        let read = || BlockingRead {
            gate: gate.clone(),
            entered: entered_tx.clone(),
        };
        let now = Instant::now();
        let mut control = ControlWorker::new_in(slots, read(), Duration::ZERO).unwrap();
        let mut output =
            OutputWorker::new_in(slots, BlockingWrite(read()), Duration::ZERO).unwrap();
        control.permit(now).unwrap();
        output.put(b"frame\n".to_vec(), now).unwrap();
        wait_for(&entered, 2);
        assert!(control.step(now).is_err());
        assert!(output.step(now).is_err());
        for role in [WorkerRole::Control, WorkerRole::Output] {
            assert!(
                matches!(slots.reserve(role), Err(WorkerSlotError::Busy(actual)) if actual == role)
            );
        }
        drop((control, output));
        release_then_reserve(slots, gate, &[WorkerRole::Control, WorkerRole::Output]);
    }

    #[test]
    fn stderr_terminal_summary_does_not_release_before_reader_drop() {
        struct EofThenDrop {
            gate: Gate,
            drop_started: mpsc::Sender<()>,
        }
        impl Read for EofThenDrop {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Ok(0)
            }
        }
        impl Drop for EofThenDrop {
            fn drop(&mut self) {
                let _ = self.drop_started.send(());
                self.gate.wait();
            }
        }

        let slots = WorkerSlots::isolated();
        let gate = Gate::new();
        let (drop_started_tx, drop_started) = mpsc::channel();
        let mut worker = StderrDrainWorker::new_in(
            slots,
            EofThenDrop {
                gate: gate.clone(),
                drop_started: drop_started_tx,
            },
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while worker.snapshot().status == StderrDrainStatus::Running {
            assert!(
                Instant::now() < deadline,
                "terminal summary was not delivered"
            );
            thread::yield_now();
        }
        drop_started.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            slots.reserve(WorkerRole::StderrDrain),
            Err(WorkerSlotError::Busy(WorkerRole::StderrDrain))
        ));
        drop(worker);
        release_then_reserve(slots, gate, &[WorkerRole::StderrDrain]);
    }
}
