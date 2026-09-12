//! ME-3b-3 — keeping a module process alive, and deciding when to stop trying.
//!
//! Split in two on purpose:
//!
//! - [`RestartPolicy`] — a **pure** state machine over "it exited / it failed to
//!   become ready", answering `restart after D` or `give up`. No process, no
//!   clock of its own, no I/O. Table-testable, so the numbers are arguable
//!   before anything runs.
//! - [`ModuleProcess`] — the only holder of a running child and of its
//!   generation's kill path: it stops the process only after revoking that
//!   generation, and kills the process **group**. The loop that applies the
//!   policy to it (spawn → handshake → ready → restart or give up) is ME3-SUP's
//!   third slice.
//!
//! The split is not tidiness. A restart policy tested through a real process is
//! tested by waiting, and a test that waits is a test that gets its timings
//! loosened until it passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use rustix::process::{Pid, Signal};

use crate::drain::{Generation, Revocation};

/// After this long without a successful handshake, a freshly spawned module is
/// treated as having crashed.
///
/// SPEC-ME3 §8: *"启动超时：spawn 后 N 秒无 `initialize` 成功 → 按崩溃处理
/// (杀进程组+退避+计入熔断)"*. Treating it as a crash rather than as its own
/// outcome is what stops a module that starts and then hangs from occupying a
/// namespace forever while looking healthy.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// First restart delay. Doubles per consecutive failure; the breaker, not a
/// cap, ends the schedule (see [`RestartPolicy::failed`]).
pub const BASE_BACKOFF: Duration = Duration::from_millis(500);

/// How long to wait for a process to disappear after `SIGKILL` before reporting
/// that it did not.
///
/// A process CAN survive `SIGKILL` — while blocked in an uninterruptible state,
/// typically a stuck filesystem or device. Waiting forever for that is how a
/// supervisor becomes the thing that is stuck.
///
/// **The 5 seconds is an ASSUMPTION, not a measurement.** Nothing here has been
/// run against a genuinely stuck device; it is a number chosen to be longer than
/// any ordinary reap and shorter than a human's patience. Replace it with a
/// measured value if one ever exists, and treat a timeout as "report it", never
/// as "wait a bit more".
pub const REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// How many consecutive failures trip the breaker.
pub const BREAKER_THRESHOLD: u32 = 5;

/// A run this long counts as the module having worked, and resets the count.
///
/// Without it, a module that is fine but restarted once a week would eventually
/// trip the breaker on failures that are months apart — the count would be
/// measuring the module's lifetime rather than its current health.
pub const HEALTHY_RUN: Duration = Duration::from_secs(60);

/// What the supervisor should do after a module stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Wait this long, then start it again.
    RestartAfter(Duration),
    /// Stop trying. The namespace answers 503 until an operator intervenes.
    ///
    /// Carries the numbers so the message an operator sees does not require
    /// reading this file: SPEC's guidance was to keep the policy as constants
    /// rather than configuration, and to **print the constants in the failure
    /// text** — that is the half of configurability anyone actually uses.
    GiveUp { after: u32, within: Duration },
}

impl std::fmt::Display for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RestartAfter(d) => write!(f, "restarting in {}ms", d.as_millis()),
            Self::GiveUp { after, within } => write!(
                f,
                "giving up after {after} consecutive failures within {}s; \
                 the module's namespace will answer 503 until it is re-enabled",
                within.as_secs()
            ),
        }
    }
}

/// Why a module stopped. The policy treats these the same — SPEC says a startup
/// timeout is handled *as* a crash — but the caller logs them differently, and
/// collapsing them here would take that distinction away from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stopped {
    /// The process exited on its own.
    Exited,
    /// It was spawned but never completed `initialize` within
    /// [`STARTUP_TIMEOUT`].
    StartupTimeout,
}

/// Consecutive-failure tracking with time-based reset.
///
/// Holds no clock: every method takes `now`. A policy that read the clock itself
/// could only be tested by sleeping.
#[derive(Debug, Clone)]
pub struct RestartPolicy {
    consecutive: u32,
    first_failure_at: Option<Instant>,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl RestartPolicy {
    /// A policy that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self {
            consecutive: 0,
            first_failure_at: None,
        }
    }

    /// Record that a run which became ready lasted from `started` to `ended`.
    /// Call it when that run ENDS — before [`RestartPolicy::failed`] for the
    /// same exit.
    ///
    /// A run of at least [`HEALTHY_RUN`] clears the failure count. A shorter one
    /// does not: a module that starts, answers the handshake and dies two
    /// seconds later is crash-looping, and "it did become ready" must not be
    /// enough to reset the count — that is exactly the loop a breaker exists to
    /// stop.
    ///
    /// This was `ready(started_at, now)`, and the name invited the call at the
    /// moment of readiness — where `now - started_at` is the startup time,
    /// always far below [`HEALTHY_RUN`], so the count would never clear and a
    /// module restarted once a week would eventually trip the breaker, the very
    /// thing [`HEALTHY_RUN`] exists to prevent (planning review of ME3-SUP).
    pub fn ran(&mut self, started: Instant, ended: Instant) {
        if ended.duration_since(started) >= HEALTHY_RUN {
            self.consecutive = 0;
            self.first_failure_at = None;
        }
    }

    /// Record a failure and decide what to do.
    #[must_use]
    pub fn failed(&mut self, _why: Stopped, now: Instant) -> Decision {
        self.consecutive += 1;
        let first = *self.first_failure_at.get_or_insert(now);
        if self.consecutive >= BREAKER_THRESHOLD {
            return Decision::GiveUp {
                after: self.consecutive,
                within: now.duration_since(first),
            };
        }
        // 500ms, 1s, 2s, 4s — and then the breaker trips, so the schedule ends
        // there. **There is deliberately no cap.**
        //
        // The first version had `MAX_BACKOFF = 30s` and a `.min()`. A test
        // asserting the cap was reachable failed, and it was right: with
        // `BREAKER_THRESHOLD` at 5 the largest delay this can ever produce is
        // 4s, so the cap was unreachable code — and unreachable code that names
        // a hazard tells the next reader a bound is being enforced when nothing
        // is enforcing it. If the threshold ever rises, the cap comes back with
        // it; `the_schedule_is_bounded_by_the_breaker` ties the two constants so
        // that change is not silent.
        //
        // `saturating_mul` rather than `<<`: a shift would wrap silently at 32
        // consecutive failures and hand back a tiny delay — the opposite of a
        // backoff, at exactly the moment the module is misbehaving most. It
        // cannot happen today for the same reason the cap cannot; it is written
        // this way so it stays true if the threshold moves.
        let factor = 1u32.saturating_mul(1 << (self.consecutive - 1).min(16));
        Decision::RestartAfter(BASE_BACKOFF.saturating_mul(factor))
    }

    /// Consecutive failures so far. For logging; the decision is
    /// [`RestartPolicy::failed`]'s to make.
    #[must_use]
    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive
    }
}

/// A running module process: its child, its process group, and the generation
/// it was started for. **The only holder of either.**
///
/// FU-46 was that a [`crate::drain::KillPermit`] proved *some* generation had
/// been revoked, not that it was this process's — and that the child could be
/// killed without one. Both are closed here: the child is private, and
/// [`ModuleProcess::stop`] revokes *its own* generation before it signals the
/// group. Outside this crate nothing else can revoke or kill (see
/// [`crate::drain`] for the `compile_fail` proofs).
///
/// Dropping one without calling `stop` is also a kill path, and it follows the
/// same order: revoke, then `SIGKILL` the group — no grace, no waiting, because
/// a destructor cannot wait. That covers a supervisor that unwinds; it does not
/// cover a daemon that is itself killed with `SIGKILL` (no destructor runs).
#[derive(Debug)]
pub struct ModuleProcess {
    /// `None` once stopped (or while `stop` is running).
    child: Option<tokio::process::Child>,
    /// The group id, which is the leader's pid (the child was spawned with
    /// `process_group(0)`). Kept, because `Child::id` returns `None` once the
    /// leader has been reaped — and helpers can outlive the leader.
    group: Pid,
    generation: Arc<Generation>,
    token: String,
    /// Set once `stop` has revoked; kept across a failed kill so a retry has
    /// the permit and still reports what was abandoned.
    revocation: Option<Revocation>,
}

/// What stopping a module revoked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StopReport {
    /// Requests in flight and already sent when the generation was revoked:
    /// their outcome is unknown, and the caller must say so.
    pub abandoned: Vec<String>,
    /// Requests in flight but never sent: nothing ran.
    pub never_sent: Vec<String>,
}

/// [`ModuleProcess::stop`] could not make the group go away. The process comes
/// back — still revoked, still holding its permit — so the caller can retry.
#[derive(Debug)]
pub struct StopFailed {
    pub error: std::io::Error,
    pub process: Box<ModuleProcess>,
}

impl std::fmt::Display for StopFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for StopFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl ModuleProcess {
    pub(crate) fn new(
        child: tokio::process::Child,
        generation: Arc<Generation>,
        token: String,
    ) -> std::io::Result<Self> {
        let group = child
            .id()
            .and_then(|id| i32::try_from(id).ok())
            .and_then(Pid::from_raw)
            .ok_or_else(|| std::io::Error::other("the spawned child has no pid"))?;
        Ok(Self {
            child: Some(child),
            group,
            generation,
            token,
            revocation: None,
        })
    }

    /// The token this process was started with, for its handshake to compare
    /// against. A new process has a new token (FU-44).
    #[must_use]
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The leader's pid, which is also the process group id.
    #[must_use]
    pub fn pid(&self) -> i32 {
        self.group.as_raw_nonzero().get()
    }

    /// The generation this process was started for.
    #[must_use]
    pub fn generation(&self) -> &Arc<Generation> {
        &self.generation
    }

    /// Wait for the leader to exit. Cancel-safe, so it can be raced against a
    /// stop request. The group may outlive the leader: call
    /// [`ModuleProcess::stop`] afterwards regardless — it revokes the
    /// generation and reaps the helpers.
    ///
    /// # Errors
    ///
    /// The OS failed to report the child's status.
    pub async fn wait_exit(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self.child.as_mut() {
            Some(child) => child.wait().await,
            None => Err(std::io::Error::other("the process was already stopped")),
        }
    }

    /// Revoke this process's generation, then stop its whole process group:
    /// `SIGTERM`, up to `grace` for the leader to exit, then `SIGKILL`.
    ///
    /// SIGTERM first: a module with state to flush deserves the chance; one that
    /// ignores it must not get a veto. The group, not the pid: helpers outlive a
    /// leader killed alone — holding ports, holding the package directory, and
    /// invisible to a `disable` that reported success.
    ///
    /// # Errors
    ///
    /// Signalling or reaping failed — typically a process stuck in an
    /// uninterruptible state that survived `SIGKILL` for [`REAP_TIMEOUT`]. The
    /// process is handed back in [`StopFailed`], still revoked and holding its
    /// permit: revocation is one-shot, and a permit spent on a failed attempt
    /// would leave a revoked generation nobody could legally retry killing.
    pub async fn stop(mut self, grace: Duration) -> Result<StopReport, StopFailed> {
        let revocation = match self.revocation.take() {
            Some(r) => r,
            None => match self.generation.revoke() {
                Some(r) => r,
                // Only this type revokes a live process's generation, and it
                // does so once; reaching this means another in-crate caller
                // revoked it and took the permit.
                None => {
                    return Err(StopFailed {
                        error: std::io::Error::other(
                            "this process's generation was revoked by someone else, \
                             who holds its kill permit",
                        ),
                        process: Box::new(self),
                    });
                }
            },
        };
        let Revocation {
            permit,
            abandoned,
            never_sent,
        } = revocation;
        let Some(mut child) = self.child.take() else {
            return Err(StopFailed {
                error: std::io::Error::other("the process was already stopped"),
                process: Box::new(self),
            });
        };
        match terminate_group(&mut child, self.group, grace).await {
            Ok(()) => {
                let _spent = permit; // `self` now drops with no child: a no-op
                Ok(StopReport {
                    abandoned,
                    never_sent,
                })
            }
            Err(error) => {
                self.child = Some(child);
                self.revocation = Some(Revocation {
                    permit,
                    abandoned,
                    never_sent,
                });
                Err(StopFailed {
                    error,
                    process: Box::new(self),
                })
            }
        }
    }
}

impl Drop for ModuleProcess {
    fn drop(&mut self) {
        if self.child.take().is_none() {
            return; // stopped
        }
        // The same order as `stop`: revoke first. A permit taken here is spent
        // on the kill below; one already held (a failed stop) is spent too.
        let revoked = self
            .revocation
            .take()
            .or_else(|| self.generation.revoke())
            .is_some();
        let _ = signal_group(self.group, Signal::Kill);
        tracing::warn!(
            pid = self.group.as_raw_nonzero().get(),
            revoked,
            "a module process was dropped without being stopped; its group was killed \
             without grace"
        );
    }
}

/// Signal a process group. `ESRCH` (nothing there) is success: the goal state
/// is "that group is gone".
fn signal_group(group: Pid, sig: Signal) -> std::io::Result<()> {
    match rustix::process::kill_process_group(group, sig) {
        Err(rustix::io::Errno::SRCH) | Ok(()) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// SIGTERM the group, wait up to `grace` for the leader, SIGKILL the group, and
/// wait — bounded — for the leader to be reaped.
///
/// Private: the only caller is [`ModuleProcess::stop`], after it has revoked.
async fn terminate_group(
    child: &mut tokio::process::Child,
    group: Pid,
    grace: Duration,
) -> std::io::Result<()> {
    signal_group(group, Signal::Term)?;
    // An unrepresentable grace is treated by `timeout` as "very long", not as a
    // panic half-way through a kill.
    if let Ok(exited) = tokio::time::timeout(grace, child.wait()).await {
        exited?;
        // The leader is gone. Signal the group once more anyway — helpers
        // outlive their parent, and "the process we started exited" is not the
        // same claim as "the tree is gone".
        return signal_group(group, Signal::Kill);
    }
    signal_group(group, Signal::Kill)?;
    // Bounded, not an open-ended wait. Two reasons: with an unbounded wait, a
    // mutation that drops the SIGKILL above HANGS rather than failing — and a
    // hang is the one outcome a test run reports as silence; and SIGKILL does
    // not guarantee reaping — a process blocked in an uninterruptible state (a
    // stuck filesystem or device) stays until it unblocks.
    match tokio::time::timeout(REAP_TIMEOUT, child.wait()).await {
        Ok(exited) => exited.map(|_| ()),
        Err(_) => Err(std::io::Error::other(format!(
            "process group {} survived SIGKILL for {}s; it is most likely blocked in \
             an uninterruptible state (a stuck filesystem or device)",
            group.as_raw_nonzero(),
            REAP_TIMEOUT.as_secs()
        ))),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    /// Write an executable and **close the handle before anyone execs it**.
    ///
    /// The `drop` is the whole point. The first version of these tests held a
    /// `File` open for writing while spawning, and **Linux's `execve` returns
    /// `ETXTBSY` for a binary any process has open for writing** — macOS does not
    /// enforce that. So three tests were green on this machine and red in CI,
    /// with `Text file busy`.
    ///
    /// `std::fs::write` leaves no handle at all, which is why it is used here
    /// rather than remembering to drop one.
    fn exe(path: &std::path::Path, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The backoff schedule, as a table. Written out rather than computed,
    /// because a test that recomputes the formula passes for any formula.
    #[test]
    fn the_backoff_doubles_and_then_the_breaker_trips() {
        let mut p = RestartPolicy::new();
        let now = t0();
        let expected = [500u64, 1000, 2000, 4000];
        for (i, ms) in expected.iter().enumerate() {
            match p.failed(Stopped::Exited, now) {
                Decision::RestartAfter(d) => assert_eq!(
                    d,
                    Duration::from_millis(*ms),
                    "failure {} of {}",
                    i + 1,
                    expected.len()
                ),
                other => panic!("failure {} gave {other:?}", i + 1),
            }
        }
        // The fifth is the threshold.
        assert!(
            matches!(p.failed(Stopped::Exited, now), Decision::GiveUp { .. }),
            "the breaker did not trip at {BREAKER_THRESHOLD}"
        );
    }

    /// The schedule is bounded by the breaker, not by a separate cap — and this
    /// test is what ties the two constants together.
    ///
    /// It exists because the first version DID have a cap (`MAX_BACKOFF = 30s`)
    /// and a test asserting the cap was reachable. That test failed, correctly:
    /// with the breaker at 5, the largest delay this can produce is 4s, so the
    /// cap was unreachable — **a constant naming a hazard that nothing could
    /// ever hit, which reads to the next person as a bound being enforced.**
    ///
    /// If `BREAKER_THRESHOLD` rises, the largest delay grows with it and this
    /// test says so, rather than the growth being silent.
    #[test]
    fn the_schedule_is_bounded_by_the_breaker() {
        let now = t0();
        let mut p = RestartPolicy::new();
        let mut delays = Vec::new();
        while let Decision::RestartAfter(d) = p.failed(Stopped::Exited, now) {
            delays.push(d);
            // A bound on the test itself: if the breaker never tripped, this
            // would otherwise loop forever and be reported as silence.
            assert!(delays.len() < 64, "the breaker never tripped");
        }
        assert_eq!(
            delays.len() as u32,
            BREAKER_THRESHOLD - 1,
            "the number of restarts before giving up must follow the threshold"
        );
        let largest = *delays.iter().max().expect("at least one restart");
        assert_eq!(
            largest,
            BASE_BACKOFF.saturating_mul(1 << (BREAKER_THRESHOLD - 2)),
            "the largest delay is BASE * 2^(threshold-2); if this changed, the \
             schedule changed and a cap may now be worth having"
        );
        // Sanity on the actual numbers today, so the formula above cannot be
        // satisfied by a formula that happens to match itself.
        assert_eq!(largest, Duration::from_millis(4000));
    }

    /// A long healthy run clears the count; a short one does not.
    ///
    /// The second half is the load-bearing one: a module that starts, answers
    /// the handshake and dies two seconds later is crash-looping, and "it did
    /// become ready" must not reset the counter — that is precisely the loop the
    /// breaker exists to stop.
    #[test]
    fn only_a_long_enough_run_counts_as_healthy() {
        let now = t0();
        let mut p = RestartPolicy::new();
        for _ in 0..3 {
            let _ = p.failed(Stopped::Exited, now);
        }
        assert_eq!(p.consecutive_failures(), 3);

        // A two-second run: not healthy.
        p.ran(now, now + Duration::from_secs(2));
        assert_eq!(
            p.consecutive_failures(),
            3,
            "a two-second run reset the failure count"
        );

        // A run past the threshold: healthy.
        p.ran(now, now + HEALTHY_RUN);
        assert_eq!(p.consecutive_failures(), 0);
    }

    /// A startup timeout is handled AS a crash (SPEC-ME3 §8), so the policy must
    /// not treat it more leniently — a module that hangs on startup would
    /// otherwise be retried forever.
    #[test]
    fn a_startup_timeout_counts_the_same_as_a_crash() {
        let now = t0();
        let (mut a, mut b) = (RestartPolicy::new(), RestartPolicy::new());
        for _ in 0..BREAKER_THRESHOLD {
            let da = a.failed(Stopped::Exited, now);
            let db = b.failed(Stopped::StartupTimeout, now);
            assert_eq!(da, db, "the two outcomes diverged");
        }
        assert!(matches!(
            b.failed(Stopped::StartupTimeout, now),
            Decision::GiveUp { .. }
        ));
    }

    /// The operator must not have to read this file to learn the policy. SPEC's
    /// guidance was constants rather than configuration, **plus printing the
    /// constants in the failure text** — that is the half of configurability
    /// anyone actually uses.
    #[test]
    fn giving_up_says_what_the_policy_was() {
        let now = t0();
        let mut p = RestartPolicy::new();
        let mut last = None;
        for _ in 0..BREAKER_THRESHOLD {
            last = Some(p.failed(Stopped::Exited, now));
        }
        let text = last.unwrap().to_string();
        assert!(
            text.contains(&BREAKER_THRESHOLD.to_string()),
            "the threshold is not in the message: {text}"
        );
        assert!(text.contains("503"), "{text}");
    }

    fn start(dir: &std::path::Path, generation: Arc<Generation>) -> ModuleProcess {
        let spawn_cmd = agent24_domain::SpawnCommand {
            command: "bin/mod".to_owned(),
            args: vec![],
        };
        crate::launch::spawn(
            crate::launch::LaunchSpec {
                name: "t",
                command: &spawn_cmd,
                package_dir: dir,
                data_dir: dir,
                callback_sock: &dir.join("cb.sock"),
                listener: std::net::TcpListener::bind("127.0.0.1:0").unwrap(),
            },
            generation,
        )
        .expect("spawn")
    }

    /// A module that starts a helper which keeps touching `marker`, then sleeps.
    fn module_with_helper(dir: &std::path::Path, marker: &std::path::Path) {
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        exe(
            &dir.join("bin/mod"),
            &format!(
                "#!/bin/sh\n\
                 ( while : ; do touch {m} ; sleep 0.05 ; done ) &\n\
                 sleep 30\n",
                m = marker.display()
            ),
        );
    }

    /// Wait for the helper to prove it is running. Without this a test could
    /// "pass" by killing something that had not started yet.
    async fn helper_running(marker: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(
            marker.exists(),
            "the helper never started; nothing was proven"
        );
    }

    /// The helper stops touching the file — measured as "the mtime stops
    /// advancing", because the file itself remains.
    ///
    /// The baseline is taken a moment AFTER the kill: a signal is delivered
    /// asynchronously, so a `touch` already running when `stop` returned can
    /// still land. Taking the baseline at once made this read "alive" under
    /// load (the mutation harness saw it fail on runs where nothing about the
    /// kill had changed).
    async fn helper_stopped(marker: &std::path::Path) -> bool {
        tokio::time::sleep(Duration::from_millis(150)).await;
        let settle = std::time::SystemTime::now();
        tokio::time::sleep(Duration::from_millis(400)).await;
        !std::fs::metadata(marker)
            .and_then(|m| m.modified())
            .map(|t| t > settle)
            .unwrap_or(false)
    }

    /// **The reason the child is put in its own process group.**
    ///
    /// A module that starts a helper and is killed by pid alone leaves the
    /// helper running — holding ports, holding the package directory, and
    /// invisible to a `disable` that reported success.
    #[tokio::test]
    async fn stopping_kills_the_helper_too_not_just_the_module() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper-alive");
        module_with_helper(dir.path(), &marker);
        let p = start(dir.path(), Generation::starting());
        helper_running(&marker).await;

        p.stop(Duration::from_secs(2)).await.expect("stop");
        assert!(
            helper_stopped(&marker).await,
            "the helper outlived the module: only the process we held was killed"
        );
    }

    /// A helper that ignores SIGTERM outlives a leader that does not: the
    /// leader exits inside the grace, and the group is still killed. "The
    /// process we started exited" is not the claim "the tree is gone".
    #[tokio::test]
    async fn a_helper_that_ignores_sigterm_dies_even_when_the_leader_exits_in_time() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let marker = dir.path().join("helper-alive");
        // The helper's shell ignores TERM; its `sleep` does not, but the loop
        // just starts another. The leader keeps the default and exits on TERM.
        exe(
            &dir.path().join("bin/mod"),
            &format!(
                "#!/bin/sh\n\
                 ( trap '' TERM ; while : ; do touch {m} ; sleep 0.05 ; done ) &\n\
                 sleep 30\n",
                m = marker.display()
            ),
        );
        let p = start(dir.path(), Generation::starting());
        helper_running(&marker).await;

        let start = Instant::now();
        p.stop(Duration::from_secs(5)).await.expect("stop");
        // Precondition: the leader went on TERM, well inside the grace — so
        // this is the path where only the second signal can reach the helper.
        assert!(
            start.elapsed() < Duration::from_secs(4),
            "{:?}",
            start.elapsed()
        );
        assert!(
            helper_stopped(&marker).await,
            "a helper that ignores SIGTERM outlived a leader that exited in time"
        );
    }

    /// SIGTERM first, and a module that ignores it still goes away.
    #[tokio::test]
    async fn a_module_that_ignores_sigterm_is_still_killed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        // A BUSY loop, not `sleep`. The first version was `trap '' TERM;
        // sleep 30` — and it exited almost immediately, correctly: the trap
        // protects the shell, but `sleep` is a separate process in the same
        // group and does not ignore TERM, so the group signal killed it and the
        // shell fell through. The test's premise was wrong, not its assertion.
        // A shell builtin loop has no child to kill in its place.
        let ready = dir.path().join("trap-installed");
        exe(
            &dir.path().join("bin/mod"),
            &format!(
                "#!/bin/sh\ntrap '' TERM\ntouch {}\nwhile : ; do : ; done\n",
                ready.display()
            ),
        );
        let p = start(dir.path(), Generation::starting());
        let pid = Pid::from_raw(p.pid()).unwrap();

        // Wait until the trap is INSTALLED. Without this the test signals a
        // shell that has not run `trap` yet, the default disposition applies, and
        // the child dies immediately — measured: it returned in 30ms and the
        // assertion below failed. The test was racing the thing it was testing.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(ready.exists(), "the module never installed its trap");

        let start = Instant::now();
        p.stop(Duration::from_millis(300)).await.expect("stop");
        let took = start.elapsed();

        assert_eq!(
            rustix::process::test_kill_process(pid),
            Err(rustix::io::Errno::SRCH),
            "the child is still there after stop returned"
        );
        // It waited for the grace period rather than killing immediately —
        // otherwise "SIGTERM first" would be a claim with nothing behind it.
        assert!(took >= Duration::from_millis(250), "returned in {took:?}");
    }

    /// **FU-44's first half.** Every spawn mints a new token; a restart must not
    /// reuse the one a failed handshake already saw.
    #[tokio::test]
    async fn a_restart_does_not_reuse_the_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        exe(&dir.path().join("bin/mod"), "#!/bin/sh\nexit 1\n");

        let mut tokens = std::collections::BTreeSet::new();
        for _ in 0..5 {
            let p = start(dir.path(), Generation::starting());
            tokens.insert(p.token().to_owned());
            p.stop(Duration::from_millis(100)).await.expect("stop");
        }
        assert_eq!(
            tokens.len(),
            5,
            "a token was reused across restarts; each secret must be measurable \
             exactly once (FU-44)"
        );
    }

    /// `stop` revokes the process's OWN generation — the one it was started
    /// for — and reports what that revocation abandoned. This is FU-46: the
    /// permit that kills is the permit of that process's generation, not of
    /// whichever generation someone happened to revoke.
    #[tokio::test]
    async fn stop_revokes_its_own_generation_and_reports_what_it_abandoned() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper-alive");
        module_with_helper(dir.path(), &marker);
        let generation = Generation::starting();
        assert!(generation.ready());
        let sent = generation.admit_request("sent".to_owned()).unwrap();
        assert!(sent.dispatch());
        let _unsent = generation.admit_request("unsent".to_owned()).unwrap();
        // Another generation, to show which one is revoked.
        let other = Generation::starting();

        let p = start(dir.path(), generation.clone());
        let report = p.stop(Duration::from_secs(2)).await.expect("stop");

        assert_eq!(generation.state(), crate::drain::DrainState::Revoked);
        assert_eq!(
            other.state(),
            crate::drain::DrainState::Starting,
            "stop revoked a generation that was not its own"
        );
        assert_eq!(report.abandoned, vec!["sent".to_owned()]);
        assert_eq!(report.never_sent, vec!["unsent".to_owned()]);
        assert!(
            generation.revoke().is_none(),
            "the revocation was not the one-shot one: its permit is still to be had"
        );
    }

    /// Dropping a process without stopping it is a kill path too, and follows
    /// the same order: its generation is revoked, then its group killed.
    #[tokio::test]
    async fn dropping_a_process_revokes_its_generation_and_kills_its_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("helper-alive");
        module_with_helper(dir.path(), &marker);
        let generation = Generation::starting();
        let p = start(dir.path(), generation.clone());
        helper_running(&marker).await;

        drop(p);

        assert_eq!(generation.state(), crate::drain::DrainState::Revoked);
        assert!(
            helper_stopped(&marker).await,
            "the helper outlived a dropped module process"
        );
    }
}
