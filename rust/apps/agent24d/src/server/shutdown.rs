use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

// A shutdown's budgets and deadlines — the HTTP drain's fixed 1.5s, the
// out-of-process modules' drain and stop grace (tunable: `A24_MODULE_DRAIN_MS`,
// `A24_COMPONENT_STOP_GRACE_MS`), the time to put the summary on disk, and the
// watchdog after all of them — live in `crate::lifecycle` (SHUT-1b). At the
// defaults `kill -TERM` ends the process within 16.5s, or about 50.6s while a managed iDoris holds the summary open (ADR-038 D5 raised the stop grace to 15s; TASKS B2's 2s now holds only with a lowered grace).

/// A shutdown request, from anything that can make one — a signal, `POST
/// /api/v1/shutdown`, the server ending. [`Shutdown::request`] is synchronous
/// and does no I/O: it fixes the deadline, arms the watchdog, and cancels, in
/// that order — so nothing stuck, a stalled stderr say, keeps a shutdown from
/// starting or from being bounded (review of SUP-4, rounds 4 and 5). Clones
/// share one deadline and one watchdog.
#[derive(Clone)]
pub struct Shutdown {
    token: CancellationToken,
    /// When the shutdown began: fixed once, by whoever gets there first.
    began: Arc<std::sync::OnceLock<tokio::time::Instant>>,
    /// The modules' budgets its deadlines are derived from (SHUT-1b).
    params: crate::lifecycle::Params,
    armed: Arc<std::sync::atomic::AtomicBool>,
    /// `STARTING`, `READY` or `STOPPING`: readiness and a shutdown request
    /// decide, once and atomically, which came first (see
    /// [`Shutdown::commit_ready`]).
    pub(super) phase: Arc<std::sync::atomic::AtomicU8>,
    pub(super) idoris_budget: Arc<std::sync::atomic::AtomicBool>,
}

const STARTING: u8 = 0;
const READY: u8 = 1;
pub(super) const STOPPING: u8 = 2;

impl Shutdown {
    #[cfg(test)]
    pub fn new(token: CancellationToken) -> Self {
        Self::with_params(token, crate::lifecycle::Params::default())
    }

    /// A controller whose deadlines follow `params` (SHUT-1b).
    #[must_use]
    pub fn with_params(token: CancellationToken, params: crate::lifecycle::Params) -> Self {
        Self {
            token,
            idoris_budget: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            began: Arc::new(std::sync::OnceLock::new()),
            params,
            armed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            phase: Arc::new(std::sync::atomic::AtomicU8::new(STARTING)),
        }
    }

    /// Begin the shutdown (idempotent).
    pub fn request(&self) {
        self.phase
            .store(STOPPING, std::sync::atomic::Ordering::SeqCst);
        arm_watchdog(self.deadlines().watchdog, &self.armed);
        self.token.cancel();
    }

    /// Declare the daemon ready — unless a shutdown was requested first.
    /// `false`: it was, and nothing may say this daemon is ready. One atomic
    /// step against [`Shutdown::request`], so the two cannot both win: a
    /// check followed by the ready line let a shutdown land in between and the
    /// CLI report a daemon started that was already exiting (review of SUP-4,
    /// rounds 5 and 7).
    #[must_use]
    pub fn commit_ready(&self) -> bool {
        self.phase
            .compare_exchange(
                STARTING,
                READY,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }

    /// Every deadline of this shutdown, from the moment it began — fixed by
    /// the first [`Shutdown::request`], or, for a token cancelled some other
    /// way, by the first look after it. Every path that sees the cancel calls
    /// `request` before reading these, so the first to arrive fixes it, once
    /// (SHUT-1b).
    #[must_use]
    pub fn deadlines(&self) -> crate::lifecycle::Deadlines {
        let began = *self.began.get_or_init(tokio::time::Instant::now);
        let mut deadlines = self.params.deadlines(began);
        if self.idoris_budget.load(std::sync::atomic::Ordering::SeqCst) {
            deadlines.persist = deadlines
                .persist
                .max(began + agent24_idoris::idoris_host::SHUTDOWN_BUDGET);
            deadlines.watchdog =
                deadlines.http.max(deadlines.persist) + crate::lifecycle::WATCHDOG_MARGIN;
        }
        deadlines
    }

    /// When the HTTP drain must be done.
    #[must_use]
    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadlines().http
    }

    /// The budgets this shutdown gives modules.
    #[must_use]
    pub fn params(&self) -> crate::lifecycle::Params {
        self.params
    }

    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }

    /// A token cancelled with the shutdown.
    #[must_use]
    pub fn child_token(&self) -> CancellationToken {
        self.token.child_token()
    }

    /// Resolves when in-flight model calls of modules and attached modules must be
    /// cancelled: the drain plus the per-call cancel grace
    /// ([`crate::lifecycle::CALL_CANCEL_GRACE`], ADR-038 D6), capped by the stop grace when that
    /// is shorter and never stretched by it. Attached modules get no SIGTERM, so a 15s stop
    /// grace must not keep their calls alive. Stored
    /// instant, like [`Shutdown::modules_cut_off`].
    pub fn calls_cut_off(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let shutdown = self.clone();
        async move {
            shutdown.token.cancelled().await;
            tokio::time::sleep_until(shutdown.deadlines().calls).await;
        }
    }

    /// Resolves when a module still being stopped by `os disable` must be
    /// killed: the budget any module gets once the shutdown begins — its
    /// drain and its stop grace — so a disable's longer drain neither holds
    /// the shutdown past its bound nor gets less than a module the shutdown
    /// stops itself (SUP-5). An absolute instant after the moment the
    /// shutdown began, which is stored, not derived: measured from whenever
    /// this was first polled, a late poll put it past the shutdown's own
    /// deadline, which then stopped waiting for it (review of SUP-5, rounds
    /// 2 and 3; SHUT-1b).
    pub fn modules_cut_off(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let shutdown = self.clone();
        async move {
            shutdown.token.cancelled().await;
            let began = shutdown.deadlines().began;
            tokio::time::sleep_until(began + shutdown.params.drain + shutdown.params.stop_grace)
                .await;
        }
    }
}

/// Arm the hard bound, once: at `at` — the watchdog instant of `crate::lifecycle` — the process
/// ends, whatever is stuck — a module's directory on a stalled filesystem
/// during startup, a blocking task. A native thread, so no scheduling of the
/// runtime can delay it; an absolute instant, so arming it late does not move
/// it; no I/O when it fires, so a stalled stderr cannot hold it; and exit
/// status 0, because a shutdown was asked for — a supervisor such as launchd
/// must not read it as a crash and start the daemon again (review of SUP-4,
/// round 4). A thread the OS refuses to create ends the process at once,
/// rather than losing the shutdown (round 5). Modules it did not get to stop
/// read EOF on their callback connections, which ends their runs (D1).
fn arm_watchdog(at: tokio::time::Instant, armed: &std::sync::atomic::AtomicBool) {
    if armed
        .compare_exchange(
            false,
            true,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return;
    }
    let at = at.into_std();
    let spawned = std::thread::Builder::new()
        .name("shutdown-watchdog".to_owned())
        .spawn(move || {
            let now = std::time::Instant::now();
            if at > now {
                std::thread::sleep(at - now);
            }
            std::process::exit(0);
        });
    if spawned.is_err() {
        std::process::exit(0);
    }
}

/// Stop every supervised module, the way SPEC §4 stops one, concurrently for
/// all: DRAINING first — new proxied requests refused, the ones in flight left
/// to finish, for up to `drain` — then REVOKING and the process stop.
/// Each supervisor does both itself (`drain_and_stop`), so a module that was
/// starting is stopped without ever serving and none is restarted into a
/// generation nobody drained (review of SUP-4, round 2). Logs any stop that
/// was not clean. The caller bounds the whole of it.
///
/// Draining first is what keeps a shutdown from answering a request a module
/// is in the middle of `request_abandoned` — and a client that then retries a
/// write the module did complete (review of SUP-4, round 1).
pub(super) async fn stop_supervisors(closed: crate::domain::Closed, drain: Duration) {
    let mut stops = tokio::task::JoinSet::new();
    for s in closed.running {
        stops.spawn(async move { (s.name, s.handle.drain_and_stop(drain).await) });
    }
    while let Some(done) = stops.join_next().await {
        match done {
            Ok((_, Ok(()))) => {}
            Ok((name, Err(e))) => tracing::error!("domain OS {name:?} did not stop cleanly: {e}"),
            Err(e) => tracing::error!("stopping a domain OS failed: {e}"),
        }
    }
    // Stops `os disable` began: each ends by `Shutdown::modules_cut_off` at
    // the latest, with a SIGKILL of its module attempted by then — attempted,
    // not confirmed (SUP-5).
    for stop in closed.disabling {
        let _ = stop.task.await;
    }
}
