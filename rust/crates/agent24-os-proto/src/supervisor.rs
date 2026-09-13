//! ME3-SUP slice 3a — the supervisor loop: one module's whole life.
//!
//! ```text
//!   ┌──────────────────────────────────────────────────────────────────┐
//!   ▼                                                                  │
//! placeholder generation in `Current` (503 module_not_ready)           │
//!   → bind a fresh port (D4) → listen for the callback → spawn         │
//!   → accept + handshake before the startup deadline                   │
//!   → ready → serve the callback connection (methods bound to THIS     │
//!     generation, FU-49) until one of:                                 │
//!        the process exits · the connection ends (D1) · stop requested │
//!   → stop the process (revoke first, then the group)                  │
//!   → RestartPolicy: restart after a backoff ──────────────────────────┘
//!                    or give up (the breaker)
//! ```
//!
//! Every orderly end of a run goes through [`ModuleProcess::stop`], which
//! revokes the generation before anything is killed; an aborted or panicking
//! loop drops the process instead, and its `Drop` revokes and SIGKILLs. A run whose callback
//! connection ends is over even if the process is still alive (user decision
//! D1): the connection is the generation's lifeline, and a generation does not
//! reconnect.
//!
//! Library only: the daemon does not start supervisors until ME3-SUP slice 4.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent24_domain::SpawnCommand;
use tokio::sync::watch;

use crate::drain::{Current, Generation};
use crate::endpoint::{self, CallbackDir};
use crate::initialize::{Expectation, Offer};
use crate::launch::{self, LaunchSpec, Trampoline};
use crate::rpc::{self, Methods};
use crate::supervise::{Decision, ModuleProcess, RestartPolicy, STARTUP_TIMEOUT, Stopped};

/// What a supervisor starts: one installed module.
#[derive(Debug, Clone)]
pub struct ModuleSpec {
    /// The manifest's name — also what the handshake must claim.
    pub name: String,
    pub command: SpawnCommand,
    pub package_dir: PathBuf,
    pub data_dir: PathBuf,
    /// The digest of the manifest the kernel read; the handshake must report
    /// the same one.
    pub manifest_digest: String,
    pub trampoline: Trampoline,
}

/// How long the supervisor waits for things. Production uses [`Timings::default`];
/// tests shorten them so a crash loop runs in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timings {
    /// From spawn to a completed handshake. Past it the run counts as a crash
    /// (SPEC §8: *"spawn 后 N 秒无 initialize 成功 → 按崩溃处理"*).
    pub startup: Duration,
    /// SIGTERM to SIGKILL when a run is stopped.
    pub stop_grace: Duration,
    /// The first restart delay; doubles per consecutive failure.
    pub backoff_base: Duration,
}

/// How long a stopped module gets between SIGTERM and SIGKILL. ⚖️ Long enough
/// to flush a little state, short enough that a daemon shutdown does not hang
/// on a module that ignores it.
pub const STOP_GRACE: Duration = Duration::from_secs(3);

impl Default for Timings {
    fn default() -> Self {
        Self {
            startup: STARTUP_TIMEOUT,
            stop_grace: STOP_GRACE,
            backoff_base: crate::supervise::BASE_BACKOFF,
        }
    }
}

/// Where a supervised module is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    /// Run number `attempt` is starting: spawned or about to be, handshake not
    /// done yet.
    Starting { attempt: u32 },
    /// Handshaken and serving.
    Running,
    /// A run is being stopped (revoked already; the process is being
    /// terminated). Next: `Backoff`, `GaveUp`, `Stopped` or `StopFailed`.
    Stopping,
    /// The last run failed; the next starts after `delay`.
    Backoff { failures: u32, delay: Duration },
    /// The breaker tripped: no more runs. The loop waits for its stop and
    /// then says `Stopped`.
    GaveUp { failures: u32, within: Duration },
    /// Ended on request. Any process it ran is confirmed gone.
    Stopped,
    /// A run's process could not be confirmed gone: its stop failed, and the
    /// process was dropped — which sends SIGKILL once more while its leader is
    /// unreaped, and nothing once it was reaped (the id may be reused). No new
    /// run is started, and the slot stays held: no other supervisor can start
    /// the module while the old group may still hold its data directory.
    StopFailed { error: String },
    /// The supervisor itself panicked — a bug. A process it held, if any, had
    /// a SIGKILL attempted; the slot says `module_stopping`, and stays held if
    /// a process was not confirmed gone.
    Panicked,
    /// The loop was cancelled before reaching `Stopped` — its handle dropped,
    /// or its runtime shut down. A process not yet confirmed gone had a
    /// SIGKILL attempted on its group, not waited for, and then the slot stays
    /// held; otherwise the slot is released.
    Killed,
}

/// Builds the callback methods for one generation. Called once per run, with
/// that run's generation, so every method it returns is bound to the
/// generation whose handshake the connection completed — not to whatever
/// `Current` holds when a callback arrives (FU-49).
pub type MethodsFor = Arc<dyn Fn(&Arc<Generation>) -> Methods + Send + Sync>;

/// A running supervisor. Dropping it without [`SupervisorHandle::stop`] aborts
/// the loop; the module process it held is dropped with it, which revokes its
/// generation and SIGKILLs its group without grace. A kill not waited for is
/// not a confirmed end, so the slot is then kept held; only a loop caught
/// holding no process (none spawned yet, or all confirmed stopped) releases
/// it.
#[derive(Debug)]
pub struct SupervisorHandle {
    /// `None` while running; `Some(drain)` once a stop was asked for, with how
    /// long a run that is serving may take to finish the requests it has.
    stop: watch::Sender<Option<Duration>>,
    status: watch::Receiver<Status>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SupervisorHandle {
    /// Where the module is now.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// A receiver of the status. A `watch`: it sees the latest value, and a
    /// slow reader can miss short-lived ones in between.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Stop the module (revoke, SIGTERM, grace, SIGKILL) and the loop, and
    /// wait for both. `Ok` only when the module is confirmed gone and the slot
    /// is free for the next supervisor.
    ///
    /// # Errors
    ///
    /// [`SupervisorError::StopFailed`] when a run's process could not be
    /// confirmed gone — the slot stays held, so nothing else starts the module
    /// over it; [`SupervisorError::Panicked`] when the loop itself panicked;
    /// [`SupervisorError::Killed`] when the loop had been cancelled without a
    /// stop (its runtime shut down).
    pub async fn stop(self) -> Result<(), SupervisorError> {
        self.drain_and_stop(Duration::ZERO).await
    }

    /// [`SupervisorHandle::stop`], after DRAINING (SPEC §4): a run that is
    /// serving refuses new proxied requests and gets up to `drain` to finish
    /// the ones it has — its callback connection still served, since they may
    /// call back — then it is revoked and stopped. A run that is still
    /// starting, or between runs, is stopped without ever serving, and no run
    /// is started after the request: a module that crashes while draining is
    /// not restarted into a generation nobody drained (review of SUP-4,
    /// round 2).
    ///
    /// # Errors
    ///
    /// As [`SupervisorHandle::stop`].
    pub async fn drain_and_stop(mut self, drain: Duration) -> Result<(), SupervisorError> {
        let _ = self.stop.send(Some(drain));
        // Awaited in place, not taken out: a `stop` future dropped half-way —
        // a caller's deadline — drops `self` with the task still in it, and
        // `Drop` aborts it, which SIGKILLs the module now. Taking it out left
        // `Drop` nothing to abort, and the loop finished its graceful stop,
        // detached, past the caller's deadline (review of SUP-4, round 1).
        let joined = match self.task.as_mut() {
            Some(task) => Some(task.await),
            None => None,
        };
        self.task = None;
        if let Some(Err(e)) = joined
            && e.is_panic()
        {
            tracing::error!("the supervisor loop had panicked: {e}");
            return Err(SupervisorError::Panicked);
        }
        // Only `Stopped` is a clean stop. Anything else at this point — the
        // task cancelled by its runtime's shutdown (`Killed`), say — is not,
        // whatever the `JoinError` said (round 9).
        match self.status.borrow().clone() {
            Status::Stopped => Ok(()),
            Status::StopFailed { error } => Err(SupervisorError::StopFailed { error }),
            Status::Panicked => Err(SupervisorError::Panicked),
            _ => Err(SupervisorError::Killed),
        }
    }
}

/// Why [`SupervisorHandle::stop`] could not report a clean stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorError {
    /// A run's process could not be confirmed gone.
    StopFailed { error: String },
    /// The supervisor loop panicked (a bug); its module had a SIGKILL
    /// attempted.
    Panicked,
    /// The loop was cancelled before a clean stop — its runtime shut down,
    /// say. A process not yet confirmed gone, if any, had a SIGKILL attempted,
    /// not waited for (and then the slot stays held). Not a clean stop either
    /// way.
    Killed,
}

impl std::fmt::Display for SupervisorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StopFailed { error } => write!(f, "{error}"),
            Self::Panicked => f.write_str("the supervisor loop panicked"),
            Self::Killed => f.write_str(
                "the supervisor loop was cancelled before a clean stop; a module process not \
                 yet confirmed gone, if any, was SIGKILLed and the slot kept held",
            ),
        }
    }
}

impl std::error::Error for SupervisorError {}

/// [`supervise`] was refused: another supervisor holds the module's slot.
/// Stop that one first — [`SupervisorHandle::stop`] returning `Ok` frees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlotHeld;

impl std::fmt::Display for SlotHeld {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another supervisor holds this module's slot; stop it first")
    }
}

impl std::error::Error for SlotHeld {}

impl Drop for SupervisorHandle {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Start supervising `spec`. Must be called within a Tokio runtime.
///
/// `current` is the proxy's slot for this module: the supervisor puts each
/// run's generation there (and a placeholder between runs, so requests are
/// answered `503 module_not_ready` rather than reaching a dead generation).
///
/// A slot has at most one supervisor: until the one holding it has stopped —
/// its processes confirmed gone — another is refused, so two runs of a module
/// never share its data directory. A supervisor whose stop failed keeps the
/// slot for good.
///
/// # Errors
///
/// [`SlotHeld`] when another supervisor holds `current`.
pub fn supervise(
    spec: ModuleSpec,
    dir: Arc<CallbackDir>,
    current: Arc<Current>,
    methods: MethodsFor,
    timings: Timings,
) -> Result<SupervisorHandle, SlotHeld> {
    let slot = Slot::claim(current).ok_or(SlotHeld)?;
    let (stop_tx, stop_rx) = watch::channel(None);
    let (status_tx, status_rx) = watch::channel(Status::Starting { attempt: 1 });
    // Built here and moved into the task, not built inside it: a task aborted
    // before its first poll drops its future's captures — this guard — but
    // never runs a line of its body (review of ME3-SUP slice 3a, round 4).
    let exit = Exit {
        slot,
        status: status_tx,
    };
    let task = tokio::spawn(run_loop(spec, dir, exit, methods, timings, stop_rx));
    Ok(SupervisorHandle {
        stop: stop_tx,
        status: status_rx,
        task: Some(task),
    })
}

/// How one run ended.
enum Run {
    /// A stop was requested; the process has been stopped.
    StopRequested,
    /// The run is over: `why` for the policy, and when it became ready (if it
    /// did) and when it ended for [`RestartPolicy::ran`]. `ended_at` is taken
    /// when the run's outcome is known, before the process is stopped: the
    /// stop's grace is not running time, or a module that ignores SIGTERM
    /// would earn a healthy run by being slow to die (review of ME3-SUP
    /// slice 3a).
    Ended {
        why: Stopped,
        ready_at: Option<std::time::Instant>,
        ended_at: std::time::Instant,
    },
}

/// This supervisor's hold on the proxy's slot: it is the slot's only
/// supervisor (see [`supervise`]), and it remembers the generation it last put
/// there, which is the one it retires.
struct Slot {
    current: Arc<Current>,
    mine: std::sync::Mutex<Arc<Generation>>,
    /// This lease was given back. `Current::release` does not know who holds
    /// the slot, so a second release by the same supervisor — after a
    /// successor claimed it — would release the successor's claim (review of
    /// ME3-SUP slice 3a, round 7). A lease releases once.
    released: std::sync::atomic::AtomicBool,
    /// A process of this supervisor's exists and is not confirmed gone: set
    /// the moment `spawn` hands one over (no await in between), cleared once
    /// its stop succeeds. What `Exit` releases the slot by — not `Status`,
    /// which says `Starting` for a loop that never got to spawn anything
    /// (review of ME3-SUP slice 3a, round 8).
    unconfirmed: std::sync::atomic::AtomicBool,
}

impl Slot {
    /// Hold `current` with a fresh placeholder in it; `None` if another
    /// supervisor holds it.
    fn claim(current: Arc<Current>) -> Option<Self> {
        let mine = Generation::starting();
        current.claim(mine.clone()).then(|| Self {
            current,
            mine: std::sync::Mutex::new(mine),
            released: std::sync::atomic::AtomicBool::new(false),
            unconfirmed: std::sync::atomic::AtomicBool::new(false),
        })
    }

    fn mine(&self) -> std::sync::MutexGuard<'_, Arc<Generation>> {
        self.mine
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Put `next` in. Out goes this supervisor's previous generation: a
    /// placeholder, or a run's, revoked by its stop — nothing to do.
    fn install(&self, next: Arc<Generation>) {
        let mut mine = self.mine();
        let _previous = self.current.replace(next.clone());
        *mine = next;
    }

    /// The loop's end after a confirmed stop: retire, release the slot, and
    /// only then say `Stopped` — so whoever sees `Stopped` finds the slot free
    /// (review of ME3-SUP slice 3a, round 6).
    fn stopped(&self, status: &watch::Sender<Status>) {
        self.retire();
        self.release();
        status.send_replace(Status::Stopped);
    }

    /// Give the slot back — once: later calls do nothing, whoever holds the
    /// slot by then.
    fn release(&self) {
        if !self
            .released
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.current.release();
        }
    }

    /// Leave this supervisor's generation saying `module_stopping`: the module
    /// is not coming back. A run's (revoked already by its stop: a no-op) or a
    /// never-started placeholder (revoking abandons nothing).
    fn retire(&self) {
        drop(self.mine().revoke());
    }
}

/// The loop's slot and status, owned. Every way out — return, panic, the task
/// aborted, even before its first poll — drops this, which retires this
/// supervisor's generation, so no path leaves the slot promising a module that
/// will not come back. It releases the slot only when no process of this
/// supervisor's can be left (`Slot::unconfirmed` clear): after `Stopped`
/// (released already, before `Stopped` was published), or when the loop was
/// aborted with nothing spawned or everything confirmed stopped. Aborted or
/// panicking with a process — or after `StopFailed` — it keeps the slot: the
/// process had at most a SIGKILL attempted, never confirmed (review of
/// ME3-SUP slice 3a, rounds 6 and 8).
struct Exit {
    slot: Slot,
    status: watch::Sender<Status>,
}

impl Drop for Exit {
    fn drop(&mut self) {
        self.slot.retire();
        let holds_no_process = !self
            .slot
            .unconfirmed
            .load(std::sync::atomic::Ordering::SeqCst);
        if std::thread::panicking() {
            tracing::error!("the supervisor loop panicked");
            self.status.send_replace(Status::Panicked);
        } else if !matches!(
            *self.status.borrow(),
            Status::Stopped | Status::StopFailed { .. }
        ) {
            // Aborted (the handle was dropped): the process went with the task,
            // SIGKILLed, not waited for — so not `Stopped`.
            self.status.send_replace(Status::Killed);
        }
        if holds_no_process {
            self.slot.release();
        }
    }
}

async fn run_loop(
    spec: ModuleSpec,
    dir: Arc<CallbackDir>,
    exit: Exit,
    methods: MethodsFor,
    timings: Timings,
    mut stop: watch::Receiver<Option<Duration>>,
) {
    let (slot, status) = (&exit.slot, &exit.status);
    let mut policy = RestartPolicy::with_base(timings.backoff_base);
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        status.send_replace(Status::Starting { attempt });
        let (why, ready_at, ended_at) =
            match run_once(&spec, &dir, slot, &methods, &timings, &mut stop, status).await {
                Err(Unconfirmed(error)) => {
                    // No restart over a group that may still be there.
                    tracing::error!(module = %spec.name, "not restarting: {error}");
                    slot.retire();
                    status.send_replace(Status::StopFailed { error });
                    stop_requested(&mut stop).await;
                    return;
                }
                Ok(Run::StopRequested) => {
                    slot.stopped(status);
                    return;
                }
                Ok(Run::Ended {
                    why,
                    ready_at,
                    ended_at,
                }) => (why, ready_at, ended_at),
            };
        if let Some(ready_at) = ready_at {
            policy.ran(ready_at, ended_at);
        }
        match policy.failed(why, ended_at) {
            Decision::RestartAfter(delay) => {
                // Until the next run is handshaken the slot holds a generation
                // that was never started: requests get `503 module_not_ready`
                // (coming back), not the ended run's `module_stopping`. The one
                // taken out was revoked by its run's stop: nothing to do.
                // After a stop or a give-up the revoked generation stays — the
                // module is not coming back, and `module_stopping` says so.
                slot.install(Generation::starting());
                tracing::warn!(module = %spec.name, ?why, delay_ms = delay.as_millis(), "module run ended; restarting");
                status.send_replace(Status::Backoff {
                    failures: policy.consecutive_failures(),
                    delay,
                });
                tokio::select! {
                    biased;
                    () = stop_requested(&mut stop) => {
                        // The placeholder put in above must not outlive the
                        // module: `module_not_ready` would promise a return.
                        slot.stopped(status);
                        return;
                    }
                    () = tokio::time::sleep(delay) => {}
                }
            }
            Decision::GiveUp { after, within } => {
                tracing::error!(module = %spec.name, "{}", Decision::GiveUp { after, within });
                // Also when no run got as far as a process (every spawn
                // failed): the slot may hold a placeholder, never revoked.
                slot.retire();
                status.send_replace(Status::GaveUp {
                    failures: after,
                    within,
                });
                stop_requested(&mut stop).await;
                slot.stopped(status);
                return;
            }
        }
    }
}

/// A run's process could not be confirmed gone: its stop failed.
struct Unconfirmed(String);

/// One run: spawn, handshake, serve, and stop. Returns how it ended; the
/// process is always stopped by the time it returns — or, if that could not
/// be confirmed, dropped (one more SIGKILL) and reported as [`Unconfirmed`],
/// after which the loop starts nothing new (review of ME3-SUP slice 3a,
/// round 4).
async fn run_once(
    spec: &ModuleSpec,
    dir: &CallbackDir,
    slot: &Slot,
    methods: &MethodsFor,
    timings: &Timings,
    stop: &mut watch::Receiver<Option<Duration>>,
    status: &watch::Sender<Status>,
) -> Result<Run, Unconfirmed> {
    let failed = |why: Stopped| Run::Ended {
        why,
        ready_at: None,
        ended_at: std::time::Instant::now(),
    };
    // D4: a fresh port for every generation, so a connection pooled or queued
    // for the previous one can never reach this one.
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(module = %spec.name, "could not bind the module's port: {e}");
            return Ok(failed(Stopped::Exited));
        }
    };
    let callback = match dir.listen_next() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(module = %spec.name, "could not listen for the callback: {e}");
            return Ok(failed(Stopped::Exited));
        }
    };
    // Raced against a stop: the package check before the spawn walks a whole
    // tree and can be slow. Dropping `spawn` there is safe — its only await
    // comes before the child exists. (The blocking walk itself runs on to its
    // end on its own thread, bounded by the entry cap.)
    let spawned = tokio::select! {
        biased;
        () = stop_requested(stop) => return Ok(Run::StopRequested),
        spawned = launch::spawn(LaunchSpec {
            name: &spec.name,
            command: &spec.command,
            package_dir: &spec.package_dir,
            data_dir: &spec.data_dir,
            callback_sock: callback.path(),
            trampoline: &spec.trampoline,
            listener,
        }) => spawned,
    };
    let mut process = match spawned {
        Ok(p) => {
            slot.unconfirmed
                .store(true, std::sync::atomic::Ordering::SeqCst);
            p
        }
        Err(e) => {
            tracing::error!(module = %spec.name, "could not start the module: {e}");
            return Ok(failed(Stopped::Exited));
        }
    };
    let generation = process.generation().clone();
    // Out goes this supervisor's placeholder, which was never started.
    slot.install(generation.clone());
    let expect = Expectation {
        module: spec.name.clone(),
        manifest_digest: spec.manifest_digest.clone(),
        auth_token: process.token().to_owned(),
        kernel_versions: crate::version::kernel_range(),
        offer: Offer::none(),
    };
    // The same deadline for the accept and the handshake (see `accept_one`).
    let deadline = tokio::time::Instant::now() + timings.startup;
    let handshaken = tokio::select! {
        biased;
        () = stop_requested(stop) => {
            finish(process, timings, &spec.name, status, slot).await?;
            return Ok(Run::StopRequested);
        }
        exited = process.exited() => {
            tracing::warn!(module = %spec.name, ?exited, "the module exited before its handshake");
            let run = failed(Stopped::Exited);
            finish(process, timings, &spec.name, status, slot).await?;
            return Ok(run);
        }
        result = async {
            let stream = callback.accept_one(deadline).await.map_err(|e| e.to_string())?;
            endpoint::handshake(stream, &expect, deadline).await.map_err(|e| e.to_string())
        } => result,
    };
    let handshaken = match handshaken {
        Ok(h) => h,
        Err(why) => {
            tracing::warn!(module = %spec.name, "the module did not complete its handshake: {why}");
            let run = failed(Stopped::StartupTimeout);
            finish(process, timings, &spec.name, status, slot).await?;
            return Ok(run);
        }
    };
    if !generation.ready() {
        // Only a revocation moves a generation out of Starting, and only this
        // run stops its process — so this is not expected. It is still not a
        // ready module.
        tracing::error!(module = %spec.name, "the generation was revoked during its handshake");
        let run = failed(Stopped::StartupTimeout);
        finish(process, timings, &spec.name, status, slot).await?;
        return Ok(run);
    }
    // Built before `Running` is published: a caller's `MethodsFor` that
    // panics must not leave the status saying a module is being served.
    let methods = methods(&generation);
    let ready_at = std::time::Instant::now();
    status.send_replace(Status::Running);
    let serve = rpc::serve_until(
        handshaken.reader,
        handshaken.writer,
        methods,
        rpc::Limits::default(),
        {
            let generation = generation.clone();
            async move { generation.revoked().await }
        },
    );
    let mut serve = std::pin::pin!(serve);
    let outcome = tokio::select! {
        biased;
        () = stop_requested(stop) => None,
        ended = serve.as_mut() => Some(format!("the callback connection ended: {ended:?}")),
        exited = process.exited() => Some(format!("the module exited: {exited:?}")),
    };
    if outcome.is_none() {
        let drain = (*stop.borrow()).unwrap_or_default();
        drain_run(&generation, drain, &mut serve, &mut process).await;
    }
    let ended_at = std::time::Instant::now();
    finish(process, timings, &spec.name, status, slot).await?;
    Ok(match outcome {
        None => Run::StopRequested,
        Some(why) => {
            tracing::warn!(module = %spec.name, "{why}");
            Run::Ended {
                why: Stopped::Exited,
                ready_at: Some(ready_at),
                ended_at,
            }
        }
    })
}

/// Resolves once a stop has been requested. The `watch::Ref` that `wait_for`
/// yields holds a read guard, which must not live across an await in a task
/// that is spawned: it is dropped here, and only `()` comes out.
///
/// A closed channel is not a stop: the handle closes it only by being
/// dropped, and that aborts this task — which must end as `Killed`, not run
/// a stop path to `Stopped` in the meantime (review of ME3-SUP slice 3a,
/// round 8).
async fn stop_requested(stop: &mut watch::Receiver<Option<Duration>>) {
    if stop.wait_for(Option::is_some).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Let a serving run finish what it has, for up to `drain`: DRAINING refuses
/// new proxied requests; this waits until nothing is in flight, the deadline,
/// the callback connection ending, or the process exiting — whichever is
/// first. The callback connection is served throughout (`serve` is polled
/// here), because a request finishing may call back.
async fn drain_run(
    generation: &Generation,
    drain: Duration,
    serve: &mut std::pin::Pin<&mut impl std::future::Future<Output = rpc::Ended>>,
    process: &mut ModuleProcess,
) {
    if drain.is_zero() || !generation.begin_drain(std::time::Instant::now(), drain) {
        return;
    }
    let deadline = tokio::time::Instant::now() + drain;
    let idle = async {
        while generation.in_flight() > 0 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    tokio::select! {
        () = idle => {}
        () = tokio::time::sleep_until(deadline) => {}
        _ = serve.as_mut() => {}
        _ = process.exited() => {}
    }
}

/// Stop a run's process, with the status saying so while it happens. A stop
/// that fails is reported: the process is dropped, which SIGKILLs its group
/// once more, but its end is not confirmed.
async fn finish(
    mut process: ModuleProcess,
    timings: &Timings,
    name: &str,
    status: &watch::Sender<Status>,
    slot: &Slot,
) -> Result<(), Unconfirmed> {
    // Revoked before `Stopping` is published: whoever sees `Stopping` must
    // find new work already refused (review of ME3-SUP slice 3a, round 2).
    // A `false` here — revoked by someone else — is reported by `stop`.
    let _ = process.begin_stop();
    status.send_replace(Status::Stopping);
    // Recorded the moment the group is confirmed empty — before the wait for
    // the output drains and before anything that could panic (a log line
    // included), so neither a cancellation nor a panic past that point leaves
    // a stopped supervisor holding its slot (rounds 9 and 10).
    let gone = || {
        slot.unconfirmed
            .store(false, std::sync::atomic::Ordering::SeqCst);
    };
    match process.stop_then(timings.stop_grace, gone).await {
        Ok(report) => {
            if !report.abandoned.is_empty() {
                tracing::warn!(
                    module = name,
                    abandoned = ?report.abandoned,
                    "requests in flight when the module stopped: outcome unknown"
                );
            }
            Ok(())
        }
        Err(failed) => {
            let error = format!("the module could not be confirmed stopped: {failed}");
            tracing::error!(module = name, "{error}");
            Err(Unconfirmed(error))
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use std::sync::Mutex;

    /// A module in Python. `mode`: `normal` (serve until the callback ends —
    /// D1's module side), `crash` (exit right after the handshake), `silent`
    /// (never connect), `hangup` (close the callback, keep running). Every
    /// `escape`: like `normal`, after starting a `sleep` in a session of its
    /// own (out of the module's group) that holds its stdout, and writing that
    /// pid to `<data>/escaped` — so a stop empties the group quickly but waits
    /// out the output drains. Every
    /// start appends `<pid> <token> <CLOCK_MONOTONIC>` to `<data>/starts` —
    /// system-wide, unlike Python's `time.monotonic()` on macOS. `early`:
    /// exit without ever connecting. `stubborn`: like `hangup`, but ignoring
    /// SIGTERM, so its stop lasts the whole grace.
    const MOCK: &str = r#"import json, os, socket, sys, time
mode, name, digest = sys.argv[1], sys.argv[2], sys.argv[3]
if mode == "stubborn_serving":
    # Before the handshake: the kernel can stop it the moment it is ready.
    import signal
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
data = os.environ["A24_DATA_DIR"]
token = os.environ["A24_HANDSHAKE_TOKEN"]
with open(os.path.join(data, "starts"), "a") as f:
    f.write("%d %s %f\n" % (os.getpid(), token, time.clock_gettime(time.CLOCK_MONOTONIC)))
if mode == "silent":
    time.sleep(60)
    sys.exit(0)
if mode == "early":
    sys.exit(4)
if mode == "escape":
    import subprocess
    p = subprocess.Popen(["sleep", "30"], start_new_session=True)
    with open(os.path.join(data, "escaped"), "w") as e:
        e.write(str(p.pid))
s = socket.socket(socket.AF_UNIX)
s.connect(os.environ["A24_CALLBACK_SOCK"])
req = {"jsonrpc": "2.0", "id": "1", "method": "initialize", "params": {
    "protocol_versions": {"min": 1, "max": 1000}, "module": name,
    "manifest_digest": digest, "auth_token": token, "capabilities": []}}
s.sendall((json.dumps(req) + "\n").encode())
f = s.makefile("rb")
f.readline()
if mode == "crash":
    sys.exit(3)
if mode == "stubborn_serving":
    while f.readline():
        pass
    time.sleep(60)
    sys.exit(0)
if mode == "stubborn":
    import signal
    signal.signal(signal.SIGTERM, signal.SIG_IGN)
    f.close()
    s.close()
    time.sleep(60)
    sys.exit(0)
if mode == "hangup":
    f.close()
    s.close()
    time.sleep(60)
    sys.exit(0)
while f.readline():
    pass
sys.exit(0)
"#;

    struct Fixture {
        _state: tempfile::TempDir,
        _package: tempfile::TempDir,
        data: tempfile::TempDir,
        dir: Arc<CallbackDir>,
        spec: ModuleSpec,
    }

    fn fixture(mode: &str) -> Fixture {
        // A short state path: a socket address holds ~104 bytes.
        let state = tempfile::Builder::new()
            .prefix("a24")
            .tempdir_in("/tmp")
            .unwrap();
        let package = tempfile::tempdir().unwrap();
        let data = tempfile::tempdir().unwrap();
        std::fs::write(package.path().join("mock.py"), MOCK).unwrap();
        let dir = Arc::new(CallbackDir::create(state.path()).unwrap());
        let spec = ModuleSpec {
            name: "mock".to_owned(),
            command: SpawnCommand {
                command: "python3".to_owned(),
                args: ["-I", "-S", "mock.py", mode, "mock", "sha256:x"]
                    .map(str::to_owned)
                    .to_vec(),
            },
            package_dir: package.path().to_owned(),
            data_dir: data.path().to_owned(),
            manifest_digest: "sha256:x".to_owned(),
            trampoline: crate::launch::test_trampoline(),
        };
        Fixture {
            _state: state,
            _package: package,
            data,
            dir,
            spec,
        }
    }

    fn fast() -> Timings {
        Timings {
            startup: Duration::from_secs(30),
            stop_grace: Duration::from_secs(1),
            backoff_base: Duration::from_millis(10),
        }
    }

    fn no_methods() -> MethodsFor {
        Arc::new(|_| Methods::none())
    }

    /// Wait (bounded) until the status satisfies `pred`.
    async fn until(
        rx: &mut watch::Receiver<Status>,
        what: &str,
        pred: impl Fn(&Status) -> bool,
    ) -> Status {
        let got = tokio::time::timeout(Duration::from_secs(60), rx.wait_for(|s| pred(s)))
            .await
            .map(|r| r.map(|s| s.clone()));
        match got {
            Ok(Ok(s)) => s,
            Ok(Err(_)) => panic!(
                "never reached: {what}; the loop ended at {:?}",
                *rx.borrow()
            ),
            Err(_) => panic!("never reached: {what}; last seen {:?}", *rx.borrow()),
        }
    }

    /// The `<pid> <token>` of every start, in order. A missing file is no
    /// start yet.
    fn starts(data: &std::path::Path) -> Vec<(i32, String)> {
        start_lines(data)
            .into_iter()
            .map(|(p, t, _)| (p, t))
            .collect()
    }

    fn start_lines(data: &std::path::Path) -> Vec<(i32, String, f64)> {
        let text = match std::fs::read_to_string(data.join("starts")) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => panic!("reading the starts: {e}"),
        };
        text.lines()
            .map(|l| {
                let mut w = l.split(' ');
                let pid = w.next().unwrap().parse().unwrap();
                let token = w.next().unwrap().to_owned();
                let at = w.next().unwrap().parse().unwrap();
                (pid, token, at)
            })
            .collect()
    }

    /// Wait (bounded) until at least `n` starts were recorded.
    async fn started(data: &std::path::Path, n: usize) -> Vec<(i32, String)> {
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        loop {
            let s = starts(data);
            if s.len() >= n {
                return s;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "only {} starts",
                s.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Start a supervisor on a slot the test knows is free.
    fn sup(
        spec: ModuleSpec,
        dir: Arc<CallbackDir>,
        current: Arc<Current>,
        methods: MethodsFor,
        timings: Timings,
    ) -> SupervisorHandle {
        supervise(spec, dir, current, methods, timings).expect("the slot is free")
    }

    /// Stop, bounded: a stop that hangs fails the test instead of the run.
    /// A clean stop is expected; a test of a failing one calls `stop` itself.
    async fn stop(handle: SupervisorHandle) {
        tokio::time::timeout(Duration::from_secs(30), handle.stop())
            .await
            .expect("the supervisor did not stop")
            .expect("the stop was not clean");
    }

    /// Wait (bounded) until `pid` is gone.
    async fn gone(pid: i32) -> bool {
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if rustix::process::test_kill_process(pid) == Err(rustix::io::Errno::SRCH) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        false
    }

    /// The whole happy path: the module handshakes and runs, the slot holds
    /// its generation Running, and a stop leaves nothing behind.
    #[tokio::test]
    async fn a_module_is_run_to_ready_and_stopped_cleanly() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "Running", |s| *s == Status::Running).await;
        assert_eq!(current.get().state(), crate::drain::DrainState::Running);
        let (pid, _) = started(f.data.path(), 1).await[0].clone();

        stop(handle).await;
        assert_eq!(*rx.borrow(), Status::Stopped);
        assert!(gone(pid).await, "the module outlived the stop");
        assert_eq!(current.get().state(), crate::drain::DrainState::Revoked);
    }

    /// A module that dies after every handshake is restarted with backoff
    /// until the breaker trips; every start had a new token (FU-44, end to
    /// end), and no process is left.
    #[tokio::test]
    async fn a_crash_loop_restarts_with_fresh_tokens_until_the_breaker_trips() {
        let f = fixture("crash");
        let current = Current::new(Generation::starting());
        let handle = sup(f.spec.clone(), f.dir.clone(), current, no_methods(), fast());
        let mut rx = handle.subscribe();
        let gave_up = until(&mut rx, "GaveUp", |s| matches!(s, Status::GaveUp { .. })).await;
        assert!(
            matches!(gave_up, Status::GaveUp { failures, .. } if failures == crate::supervise::BREAKER_THRESHOLD),
            "{gave_up:?}"
        );
        let starts = starts(f.data.path());
        assert_eq!(
            starts.len(),
            crate::supervise::BREAKER_THRESHOLD as usize,
            "{starts:?}"
        );
        let tokens: std::collections::BTreeSet<&String> = starts.iter().map(|(_, t)| t).collect();
        assert_eq!(
            tokens.len(),
            starts.len(),
            "a token was reused across restarts"
        );
        for (pid, _) in &starts {
            assert!(gone(*pid).await, "pid {pid} outlived its run");
        }
        stop(handle).await;
    }

    /// Restarts wait out the backoff: the gap before restart `i` is at least
    /// `base · 2^(i-1)`. (Launching Python alone takes longer than a small
    /// base, so the base here is large enough that only a real sleep covers
    /// the later gaps.)
    #[tokio::test]
    async fn restarts_wait_out_a_doubling_backoff() {
        let f = fixture("crash");
        let current = Current::new(Generation::starting());
        let base = Duration::from_millis(300);
        let timings = Timings {
            backoff_base: base,
            ..fast()
        };
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current,
            no_methods(),
            timings,
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "GaveUp", |s| matches!(s, Status::GaveUp { .. })).await;
        let at: Vec<f64> = start_lines(f.data.path()).iter().map(|l| l.2).collect();
        assert_eq!(
            at.len(),
            crate::supervise::BREAKER_THRESHOLD as usize,
            "{at:?}"
        );
        for (i, pair) in at.windows(2).enumerate() {
            let want = base.as_secs_f64() * f64::from(1u32 << i);
            assert!(
                pair[1] - pair[0] >= want,
                "gap {i}: {:.3}s < {want:.3}s ({at:?})",
                pair[1] - pair[0]
            );
        }
        stop(handle).await;
    }

    /// While a run is being stopped the status says `Stopping`, not `Running`,
    /// and the slot already refuses: revoked before `Stopping` is published, so
    /// a watcher on another worker thread cannot see `Stopping` over a
    /// generation still admitting work.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_run_being_stopped_is_reported_as_stopping() {
        let f = fixture("stubborn");
        let current = Current::new(Generation::starting());
        let timings = Timings {
            stop_grace: Duration::from_secs(3),
            ..fast()
        };
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            timings,
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "Stopping", |s| *s == Status::Stopping).await;
        assert_eq!(current.get().state(), crate::drain::DrainState::Revoked);
        stop(handle).await;
    }

    /// A `MethodsFor` that panics ends supervision visibly: the status says
    /// `Panicked` (not `Running`), the module is killed, and the slot says
    /// `module_stopping`.
    #[tokio::test]
    async fn a_panic_in_the_loop_is_reported_and_kills_the_module() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let methods: MethodsFor = Arc::new(|_| panic!("a broken MethodsFor"));
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            methods,
            fast(),
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "Panicked", |s| *s == Status::Panicked).await;
        let (pid, _) = starts(f.data.path())[0].clone();
        assert!(gone(pid).await, "the module outlived a panicked supervisor");
        assert_eq!(current.get().state(), crate::drain::DrainState::Revoked);
        // And `stop` says so rather than reporting a clean stop.
        assert_eq!(handle.stop().await, Err(SupervisorError::Panicked));
    }

    /// A slot has one supervisor at a time: a second is refused while the
    /// first runs, and the slot it holds is untouched; once the first stops
    /// cleanly the second starts (review of ME3-SUP slice 3a, round 5).
    #[tokio::test]
    async fn a_slot_is_supervised_by_one_supervisor_at_a_time() {
        let current = Current::new(Generation::starting());
        let a = fixture("normal");
        let first = sup(
            a.spec.clone(),
            a.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        until(&mut first.subscribe(), "the first Running", |s| {
            *s == Status::Running
        })
        .await;
        let theirs = current.get();
        let b = fixture("normal");
        let refused = supervise(
            b.spec.clone(),
            b.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        assert_eq!(refused.err(), Some(SlotHeld));
        assert!(
            Arc::ptr_eq(&current.get(), &theirs),
            "a refused supervisor touched the slot"
        );
        assert_eq!(theirs.state(), crate::drain::DrainState::Running);
        assert!(
            starts(b.data.path()).is_empty(),
            "a refused supervisor started a module"
        );

        stop(first).await;
        let second = sup(
            b.spec.clone(),
            b.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        until(&mut second.subscribe(), "the second Running", |s| {
            *s == Status::Running
        })
        .await;
        stop(second).await;
    }

    /// A lease releases once: a supervisor that released its slot and
    /// releases again (its exit guard after `stopped`) must not release a
    /// successor's claim — or a third supervisor could start beside the
    /// second (review of ME3-SUP slice 3a, round 7).
    #[test]
    fn a_lease_released_twice_does_not_release_a_successors_claim() {
        let current = Current::new(Generation::starting());
        let first = Slot::claim(current.clone()).expect("free");
        first.release();
        let _second = Slot::claim(current.clone()).expect("released");
        first.release();
        assert!(
            Slot::claim(current).is_none(),
            "a third claim beside the second"
        );
    }

    /// A loop cancelled by its runtime's shutdown is not a clean stop:
    /// `stop` called afterwards, on another runtime, says so, and the slot —
    /// its module SIGKILLed, not confirmed gone — stays held (round 9).
    #[test]
    fn a_supervisor_cancelled_by_its_runtime_does_not_report_a_clean_stop() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let first = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let handle = first.block_on(async {
            let h = sup(
                f.spec.clone(),
                f.dir.clone(),
                current.clone(),
                no_methods(),
                fast(),
            );
            until(&mut h.subscribe(), "Running", |s| *s == Status::Running).await;
            h
        });
        first.shutdown_timeout(Duration::from_secs(10));
        let second = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        assert_eq!(second.block_on(handle.stop()), Err(SupervisorError::Killed));
        let g = fixture("normal");
        assert_eq!(
            supervise(g.spec.clone(), g.dir.clone(), current, no_methods(), fast()).err(),
            Some(SlotHeld),
            "the slot was released over a module not confirmed gone"
        );
    }

    /// A stop cancelled after the group was confirmed empty — while it waits
    /// for the output drains, which a process that left the group keeps open
    /// (≈1 s) — releases the slot: nothing of the module's group is left
    /// (round 10). Cancelled by shutting its runtime down: cancelling a
    /// `stop()` future only detaches the loop, which then finishes the stop.
    #[test]
    fn a_stop_cancelled_while_draining_output_still_releases_the_slot() {
        let f = fixture("escape");
        let current = Current::new(Generation::starting());
        let first = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let handle = first.block_on(async {
            let h = sup(
                f.spec.clone(),
                f.dir.clone(),
                current.clone(),
                no_methods(),
                fast(),
            );
            until(&mut h.subscribe(), "Running", |s| *s == Status::Running).await;
            h
        });
        let (pid, _) = starts(f.data.path())[0].clone();
        let escaped: i32 = std::fs::read_to_string(f.data.path().join("escaped"))
            .unwrap()
            .parse()
            .unwrap();
        let _stopping = first.spawn(handle.stop());
        let leader = rustix::process::Pid::from_raw(pid).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while rustix::process::test_kill_process(leader) != Err(rustix::io::Errno::SRCH) {
            assert!(
                std::time::Instant::now() < deadline,
                "the module outlived its stop"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        // The group is empty; the stop now waits for the drains. Cancel it there.
        std::thread::sleep(Duration::from_millis(300));
        first.shutdown_timeout(Duration::from_secs(10));
        let g = fixture("normal");
        let second = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        second.block_on(async {
            let next = supervise(
                g.spec.clone(),
                g.dir.clone(),
                current.clone(),
                no_methods(),
                fast(),
            )
            .expect("a stop cancelled after the group was confirmed empty kept the slot");
            stop(next).await;
        });
        let _ = rustix::process::kill_process(
            rustix::process::Pid::from_raw(escaped).unwrap(),
            rustix::process::Signal::Kill,
        );
    }

    /// `drain_and_stop` drains a serving run before stopping it (SPEC §4):
    /// the request in flight holds the stop, new ones are refused as
    /// `Draining` meanwhile, the module keeps running — and once the request
    /// finishes, the stop goes on at once rather than waiting out the drain
    /// budget (review of SUP-4, round 2).
    #[tokio::test]
    async fn a_drain_lets_the_request_in_flight_finish_then_stops() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        until(&mut handle.subscribe(), "Running", |s| {
            *s == Status::Running
        })
        .await;
        let (pid, _) = starts(f.data.path())[0].clone();
        let generation = current.get();
        let in_flight = generation.admit_request("r-1".into()).unwrap();
        let stopping = tokio::spawn(handle.drain_and_stop(Duration::from_secs(10)));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while generation.state() != crate::drain::DrainState::Draining {
            assert!(std::time::Instant::now() < deadline, "never drained");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(
            generation.admit_request("r-2".into()).unwrap_err(),
            crate::drain::RequestRefused::Draining
        );
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !stopping.is_finished(),
            "stopped with a request still in flight"
        );
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_ok(),
            "the module was stopped while draining"
        );
        drop(in_flight);
        let stopped = tokio::time::timeout(Duration::from_secs(3), stopping)
            .await
            .expect("the stop waited out the drain budget after the request finished")
            .unwrap();
        assert_eq!(stopped, Ok(()));
        assert!(gone(pid).await);
    }

    /// A `stop` whose caller gives up — its future dropped at a deadline —
    /// kills the module now: the handle, and the loop in it, go with the
    /// future, rather than the loop finishing its graceful stop detached, past
    /// the deadline (review of SUP-4, round 1). The module ignores SIGTERM and
    /// the grace is long, so only a kill ends it within the bound.
    #[tokio::test]
    async fn a_stop_given_up_on_kills_the_module_at_once() {
        let f = fixture("stubborn_serving");
        let current = Current::new(Generation::starting());
        let timings = Timings {
            stop_grace: Duration::from_secs(10),
            ..fast()
        };
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current,
            no_methods(),
            timings,
        );
        until(&mut handle.subscribe(), "Running", |s| {
            *s == Status::Running
        })
        .await;
        let (pid, _) = starts(f.data.path())[0].clone();
        let gave_up = tokio::time::timeout(Duration::from_millis(300), handle.stop()).await;
        assert!(
            gave_up.is_err(),
            "the stop finished inside a SIGTERM-ignoring grace: {gave_up:?}"
        );
        let pid = rustix::process::Pid::from_raw(pid).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while rustix::process::test_kill_process(pid) != Err(rustix::io::Errno::SRCH) {
            assert!(
                std::time::Instant::now() < deadline,
                "the module outlived a stop its caller gave up on"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// A handle dropped while its module runs keeps the slot: the SIGKILL
    /// its drop sends is not waited for, so the module is not confirmed gone
    /// and no successor may start over it. Dropped after giving up — no
    /// process left — it releases the slot, and says `Killed` (round 6).
    #[tokio::test]
    async fn a_dropped_handle_releases_the_slot_only_when_it_held_no_process() {
        let current = Current::new(Generation::starting());
        let a = fixture("normal");
        let running = sup(
            a.spec.clone(),
            a.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        until(&mut running.subscribe(), "Running", |s| {
            *s == Status::Running
        })
        .await;
        let (pid, _) = starts(a.data.path())[0].clone();
        drop(running);
        assert!(gone(pid).await, "the module outlived a dropped supervisor");
        let b = fixture("normal");
        assert_eq!(
            supervise(
                b.spec.clone(),
                b.dir.clone(),
                current.clone(),
                no_methods(),
                fast()
            )
            .err(),
            Some(SlotHeld),
            "a kill not waited for released the slot"
        );

        let current = Current::new(Generation::starting());
        let c = fixture("crash");
        let gave_up = sup(
            c.spec.clone(),
            c.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        let mut rx = gave_up.subscribe();
        until(&mut rx, "GaveUp", |s| matches!(s, Status::GaveUp { .. })).await;
        drop(gave_up);
        let killed = tokio::time::timeout(
            Duration::from_secs(10),
            rx.wait_for(|s| *s == Status::Killed),
        )
        .await
        .expect("the status never said Killed")
        .is_ok();
        assert!(
            killed,
            "a supervisor dropped after giving up ended as {:?}",
            *rx.borrow()
        );
        let d = fixture("normal");
        let next = sup(
            d.spec.clone(),
            d.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        stop(next).await;
    }

    /// A run whose process cannot be confirmed gone — here a process of this
    /// test's joined the module's group, so after the SIGKILL it is a zombie
    /// only the test can reap — ends in `StopFailed`: no restart, `stop`
    /// reports it, and the slot stays held so no other supervisor starts the
    /// module over the old group (review of ME3-SUP slice 3a, round 5).
    #[tokio::test]
    async fn a_stop_that_cannot_be_confirmed_ends_supervision_and_keeps_the_slot() {
        use std::os::unix::process::CommandExt;
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "Running", |s| *s == Status::Running).await;
        let (pid, _) = starts(f.data.path())[0].clone();
        let mut joiner = std::process::Command::new("sleep")
            .arg("30")
            .process_group(pid)
            .spawn()
            .expect("a process joining the module's group");
        // The module dies by itself; stopping that run cannot empty the group.
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(pid).unwrap(),
            rustix::process::Signal::Kill,
        )
        .unwrap();
        until(&mut rx, "StopFailed", |s| {
            matches!(s, Status::StopFailed { .. })
        })
        .await;
        // Well past a backoff the fast timings (10 ms) would have restarted
        // within.
        tokio::time::sleep(Duration::from_secs(1)).await;
        assert_eq!(
            starts(f.data.path()).len(),
            1,
            "restarted over an unconfirmed group"
        );

        let err = tokio::time::timeout(Duration::from_secs(30), handle.stop())
            .await
            .expect("the supervisor did not stop")
            .expect_err("a failed stop was reported as clean");
        assert!(matches!(err, SupervisorError::StopFailed { .. }), "{err}");
        let g = fixture("normal");
        assert_eq!(
            supervise(
                g.spec.clone(),
                g.dir.clone(),
                current.clone(),
                no_methods(),
                fast()
            )
            .err(),
            Some(SlotHeld),
            "the slot was released over a group that may still be there"
        );
        joiner.wait().unwrap(); // reap the zombie
    }

    /// While a restart is pending the slot holds a never-started placeholder
    /// (`503 module_not_ready`: coming back), not the ended run's revoked
    /// generation (`module_stopping`); once the breaker trips, the revoked one
    /// stays.
    #[tokio::test]
    async fn between_runs_the_slot_says_not_ready_and_after_giving_up_stopping() {
        let f = fixture("crash");
        let current = Current::new(Generation::starting());
        let timings = Timings {
            backoff_base: Duration::from_secs(2),
            ..fast()
        };
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            timings,
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "the first backoff", |s| {
            matches!(s, Status::Backoff { failures: 1, .. })
        })
        .await;
        assert_eq!(current.get().state(), crate::drain::DrainState::Starting);
        // Stopped during the backoff: the placeholder must not outlive it.
        stop(handle).await;
        assert_eq!(current.get().state(), crate::drain::DrainState::Revoked);
        let f = fixture("crash");
        let current = Current::new(Generation::starting());
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "GaveUp", |s| matches!(s, Status::GaveUp { .. })).await;
        assert_eq!(current.get().state(), crate::drain::DrainState::Revoked);
        stop(handle).await;
    }

    /// When no run gets as far as a process — every spawn is refused, here
    /// for a package directory others can write — the breaker still trips, and
    /// the slot's never-started placeholder is revoked with it: it says
    /// `module_stopping`, not a `module_not_ready` that promises a return.
    #[tokio::test]
    async fn giving_up_without_ever_spawning_retires_the_placeholder() {
        use std::os::unix::fs::PermissionsExt;
        let f = fixture("normal");
        std::fs::set_permissions(&f.spec.package_dir, std::fs::Permissions::from_mode(0o777))
            .unwrap();
        let current = Current::new(Generation::starting());
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "GaveUp", |s| matches!(s, Status::GaveUp { .. })).await;
        assert!(
            starts(f.data.path()).is_empty(),
            "a refused package was run"
        );
        assert_eq!(current.get().state(), crate::drain::DrainState::Revoked);
        stop(handle).await;
    }

    /// A module that exits before its handshake is a failed run at once — not
    /// after the whole startup deadline, which here is far longer than the
    /// test waits.
    #[tokio::test]
    async fn a_module_that_exits_before_its_handshake_fails_at_once() {
        let f = fixture("early");
        let current = Current::new(Generation::starting());
        let timings = Timings {
            startup: Duration::from_secs(600),
            ..fast()
        };
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current,
            no_methods(),
            timings,
        );
        let mut rx = handle.subscribe();
        tokio::time::timeout(
            Duration::from_secs(20),
            rx.wait_for(|s| matches!(s, Status::Backoff { failures: 1, .. })),
        )
        .await
        .expect("an exit before the handshake waited for the startup deadline")
        .unwrap();
        stop(handle).await;
    }

    /// A module that never handshakes is stopped at the startup deadline and
    /// counted as a failure.
    #[tokio::test]
    async fn a_module_that_never_handshakes_is_stopped_at_the_deadline() {
        let f = fixture("silent");
        let current = Current::new(Generation::starting());
        let timings = Timings {
            startup: Duration::from_millis(500),
            ..fast()
        };
        let handle = sup(
            f.spec.clone(),
            f.dir.clone(),
            current,
            no_methods(),
            timings,
        );
        let mut rx = handle.subscribe();
        until(&mut rx, "Backoff after the first run", |s| {
            matches!(s, Status::Backoff { failures: 1, .. })
        })
        .await;
        let (pid, _) = starts(f.data.path())[0].clone();
        assert!(
            gone(pid).await,
            "the silent module outlived its startup deadline"
        );
        stop(handle).await;
    }

    /// D1: when the callback connection ends, the generation is over — the
    /// process is killed even though it would have kept running, and a new
    /// run begins.
    #[tokio::test]
    async fn when_the_callback_connection_ends_the_run_ends() {
        let f = fixture("hangup");
        let current = Current::new(Generation::starting());
        let handle = sup(f.spec.clone(), f.dir.clone(), current, no_methods(), fast());
        let starts = started(f.data.path(), 2).await;
        assert!(
            gone(starts[0].0).await,
            "a module that hung up its callback was left running"
        );
        stop(handle).await;
    }

    /// FU-49: the methods serving a connection are built for the generation
    /// that completed its handshake — the one in the slot at that moment — and
    /// a later run's methods for its own. When a run ends, its generation is
    /// revoked, whatever the next one does.
    #[tokio::test]
    async fn each_run_is_served_by_methods_bound_to_its_own_generation() {
        let f = fixture("hangup");
        let current = Current::new(Generation::starting());
        let seen: Arc<Mutex<Vec<Arc<Generation>>>> = Arc::new(Mutex::new(Vec::new()));
        let methods: MethodsFor = {
            let (seen, current) = (seen.clone(), current.clone());
            Arc::new(move |g: &Arc<Generation>| {
                assert!(
                    Arc::ptr_eq(g, &current.get()),
                    "methods built for a generation not in the slot"
                );
                seen.lock().unwrap().push(g.clone());
                Methods::none()
            })
        };
        let handle = sup(f.spec.clone(), f.dir.clone(), current, methods, fast());
        started(f.data.path(), 2).await;
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        while seen.lock().unwrap().len() < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "the second run was never served"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = seen.lock().unwrap().clone();
        assert!(
            !Arc::ptr_eq(&seen[0], &seen[1]),
            "two runs shared a generation"
        );
        assert_eq!(seen[0].state(), crate::drain::DrainState::Revoked);
        stop(handle).await;
    }

    /// A handle dropped before its task was ever polled still retires the
    /// slot: the exit guard is built before the spawn and moved into the task,
    /// so it runs even when not one line of the loop did (review of ME3-SUP
    /// slice 3a, round 4). On this single-threaded runtime nothing is polled
    /// until the test awaits.
    #[tokio::test]
    async fn a_handle_dropped_before_the_first_poll_still_retires_the_slot() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        drop(sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        ));
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while current.get().state() != crate::drain::DrainState::Revoked {
            assert!(
                std::time::Instant::now() < deadline,
                "the placeholder was left promising a module"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(
            starts(f.data.path()).is_empty(),
            "an aborted supervisor started a module"
        );
        // It never had a process, so the slot is free again (round 8).
        let next = sup(
            f.spec.clone(),
            f.dir.clone(),
            current.clone(),
            no_methods(),
            fast(),
        );
        stop(next).await;
    }

    /// Dropping the handle without stopping is still a kill path: the loop is
    /// aborted, the process it held is dropped, and dropping it kills the group.
    #[tokio::test]
    async fn dropping_the_handle_kills_the_module() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let handle = sup(f.spec.clone(), f.dir.clone(), current, no_methods(), fast());
        let mut rx = handle.subscribe();
        until(&mut rx, "Running", |s| *s == Status::Running).await;
        let (pid, _) = starts(f.data.path())[0].clone();
        drop(handle);
        assert!(gone(pid).await, "the module outlived a dropped supervisor");
        // `Killed`, not `Stopped`: the kill was sent, its end not waited for.
        let killed = tokio::time::timeout(
            Duration::from_secs(10),
            rx.wait_for(|s| *s == Status::Killed),
        )
        .await
        .expect("the status never said Killed")
        .is_ok();
        assert!(killed, "the status ended as {:?}, not Killed", *rx.borrow());
    }
}
