//! ME-3b-3 — keeping a module process alive, and deciding when to stop trying.
//!
//! Split in two on purpose:
//!
//! - [`RestartPolicy`] — a **pure** state machine over "it exited / it failed to
//!   become ready", answering `restart after D` or `give up`. No process, no
//!   clock of its own, no I/O. Table-testable, so the numbers are arguable
//!   before anything runs.
//! - [`Supervisor`] — owns the child, applies the policy, and kills the process
//!   **group**.
//!
//! The split is not tidiness. A restart policy tested through a real process is
//! tested by waiting, and a test that waits is a test that gets its timings
//! loosened until it passes.

use std::time::{Duration, Instant};

use crate::drain::KillPermit;

/// After this long without a successful handshake, a freshly spawned module is
/// treated as having crashed.
///
/// SPEC-ME3 §8: *"启动超时：spawn 后 N 秒无 `initialize` 成功 → 按崩溃处理
/// (杀进程组+退避+计入熔断)"*. Treating it as a crash rather than as its own
/// outcome is what stops a module that starts and then hangs from occupying a
/// namespace forever while looking healthy.
pub const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);

/// First restart delay. Doubles per consecutive failure, capped at
/// [`MAX_BACKOFF`].
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

    /// Record that the module became ready, after running since `started_at`.
    ///
    /// A run of at least [`HEALTHY_RUN`] clears the failure count. A shorter one
    /// does not: a module that starts, answers the handshake and dies two
    /// seconds later is crash-looping, and "it did become ready" must not be
    /// enough to reset the count — that is exactly the loop a breaker exists to
    /// stop.
    pub fn ready(&mut self, started_at: Instant, now: Instant) {
        if now.duration_since(started_at) >= HEALTHY_RUN {
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

/// Kill a process group and wait for the leader.
///
/// # Why the group and not the process
///
/// A module written in a scripting language routinely starts helpers. Killing
/// only the pid we hold leaves those running — holding ports, holding the
/// package directory, and invisible to a `disable` that reported success.
/// [`crate::launch::spawn`] puts the child in its own group precisely so this
/// can address the whole tree.
///
/// SIGTERM first, then SIGKILL after `grace`: a module that has state to flush
/// deserves the chance, and one that ignores SIGTERM must not get a veto.
///
/// # Only with a [`KillPermit`]
///
/// The permit comes from [`crate::drain::Generation::revoke`] and nowhere else,
/// so a call here is always preceded by a revocation (SPEC §4: revocation must
/// precede the kill, or the module can still write during the grace period).
/// It is consumed, so each call spends one. **It is not bound to `child`**: the
/// type proves that a revocation happened, not that it was this process's
/// generation — see [`crate::drain`] for what that leaves to the supervisor.
///
/// # Errors
///
/// A failure to signal or to reap, **with the permit handed back** in
/// [`TerminateFailed`]: revocation is one-shot, so a permit spent on an attempt
/// that failed would leave a revoked generation whose process nobody may legally
/// retry killing. `ESRCH` (nothing there) is **not** an error: the goal state is
/// "that group is gone".
pub fn terminate_group(
    permit: KillPermit,
    child: &mut std::process::Child,
    grace: Duration,
) -> Result<(), TerminateFailed> {
    terminate_group_inner(child, grace).map_err(|error| TerminateFailed { error, permit })
}

/// [`terminate_group`] failed. The permit comes back so the caller can retry.
#[derive(Debug)]
pub struct TerminateFailed {
    pub error: std::io::Error,
    pub permit: KillPermit,
}

impl std::fmt::Display for TerminateFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl std::error::Error for TerminateFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

fn terminate_group_inner(child: &mut std::process::Child, grace: Duration) -> std::io::Result<()> {
    use rustix::process::{Pid, Signal, kill_process_group};

    let raw = i32::try_from(child.id()).unwrap_or(0);
    let pid = Pid::from_raw(raw);
    let signal_group = |sig: Signal| match pid {
        Some(p) => match kill_process_group(p, sig) {
            Err(rustix::io::Errno::SRCH) => Ok(()),
            other => other.map_err(std::io::Error::from),
        },
        None => Ok(()),
    };

    signal_group(Signal::Term)?;

    // Poll rather than block: `wait` would hang exactly when the module is
    // ignoring SIGTERM, which is the case this function exists for.
    // `checked_add`: an unrepresentable grace must not panic half-way through a
    // kill (the permit is already spent by then). It is treated as "wait no
    // longer than the reap bound", not as forever.
    let deadline = Instant::now()
        .checked_add(grace)
        .unwrap_or_else(|| Instant::now() + REAP_TIMEOUT);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            // The leader is gone. Signal the group once more anyway — helpers
            // outlive their parent, and "the process we started exited" is not
            // the same claim as "the tree is gone".
            signal_group(Signal::Kill)?;
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    signal_group(Signal::Kill)?;

    // Bounded, not `child.wait()`. **Two reasons, and an earlier version of this
    // comment gave a third that does not hold.**
    //
    // What it said was that a blocking wait "can hang forever". Review settled
    // that with a 2×2: keep the `SIGKILL` above and put the blocking `wait()`
    // back, and the suite is green — the hang needs the `SIGKILL` to be MISSING,
    // i.e. it needs the code to be broken in the way the mutation broke it.
    // **That sentence attributed a mutant's failure to the shipped code.**
    //
    // The two reasons that do hold:
    //
    // 1. **Testability.** With a blocking wait, the mutation that drops the
    //    `SIGKILL` HANGS rather than failing — and a hang is the one outcome a
    //    test run reports as silence. Bounded polling turns that mutation red.
    // 2. **`SIGKILL` is not a guarantee of reaping.** A process blocked in an
    //    uninterruptible state (a stuck filesystem or device) stays until it
    //    unblocks. That is real, and it is NOT what the mutation showed.
    let hard_deadline = Instant::now() + REAP_TIMEOUT;
    while Instant::now() < hard_deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(std::io::Error::other(format!(
        "process group {} survived SIGKILL for {}s; it is most likely blocked in \
         an uninterruptible state (a stuck filesystem or device)",
        child.id(),
        REAP_TIMEOUT.as_secs()
    )))
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
        p.ready(now, now + Duration::from_secs(2));
        assert_eq!(
            p.consecutive_failures(),
            3,
            "a two-second run reset the failure count"
        );

        // A run past the threshold: healthy.
        p.ready(now, now + HEALTHY_RUN);
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

    /// **The reason the child is put in its own process group.**
    ///
    /// A module that starts a helper and is killed by pid alone leaves the
    /// helper running — holding ports, holding the package directory, and
    /// invisible to a `disable` that reported success.
    #[test]
    fn terminating_kills_the_helper_too_not_just_the_module() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        let marker = dir.path().join("helper-alive");
        // The module starts a helper that keeps touching a file, then sleeps.
        let script = format!(
            "#!/bin/sh\n\
             ( while : ; do touch {m} ; sleep 0.05 ; done ) &\n\
             sleep 30\n",
            m = marker.display()
        );
        exe(&dir.path().join("bin/mod"), &script);

        let spawn_cmd = agent24_domain::SpawnCommand {
            command: "bin/mod".to_owned(),
            args: vec![],
        };
        let mut launched = crate::launch::spawn(&spawn_cmd, dir.path(), dir.path()).expect("spawn");

        // Wait for the helper to prove it is running. Without this the test
        // could "pass" by killing something that had not started yet.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !marker.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            marker.exists(),
            "the helper never started; nothing was proven"
        );

        terminate_group(
            crate::drain::Generation::starting()
                .revoke()
                .expect("first revoke")
                .permit,
            &mut launched.child,
            Duration::from_secs(2),
        )
        .expect("terminate");

        // The helper stops touching the file. Measured as "the mtime stops
        // advancing", because the file itself remains.
        let settle = std::time::SystemTime::now();
        std::thread::sleep(Duration::from_millis(400));
        let touched_after_kill = std::fs::metadata(&marker)
            .and_then(|m| m.modified())
            .map(|t| t > settle)
            .unwrap_or(false);
        assert!(
            !touched_after_kill,
            "the helper outlived the module: only the process we held was killed"
        );
    }

    /// SIGTERM first, and a module that ignores it still goes away.
    #[test]
    fn a_module_that_ignores_sigterm_is_still_killed() {
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

        let spawn_cmd = agent24_domain::SpawnCommand {
            command: "bin/mod".to_owned(),
            args: vec![],
        };
        let mut launched = crate::launch::spawn(&spawn_cmd, dir.path(), dir.path()).expect("spawn");

        // Wait until the trap is INSTALLED. Without this the test signals a
        // shell that has not run `trap` yet, the default disposition applies, and
        // the child dies immediately — measured: it returned in 30ms and the
        // assertion below failed. The test was racing the thing it was testing.
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ready.exists(), "the module never installed its trap");

        let start = Instant::now();
        terminate_group(
            crate::drain::Generation::starting()
                .revoke()
                .expect("first revoke")
                .permit,
            &mut launched.child,
            Duration::from_millis(300),
        )
        .expect("terminate");
        let took = start.elapsed();

        assert!(
            launched.child.try_wait().unwrap().is_some(),
            "the child is still running after terminate_group returned"
        );
        // It waited for the grace period rather than killing immediately —
        // otherwise "SIGTERM first" would be a claim with nothing behind it.
        assert!(took >= Duration::from_millis(250), "returned in {took:?}");
    }

    /// **FU-44's first half.** Every spawn mints a new token; a restart must not
    /// reuse the one a failed handshake already saw.
    #[test]
    fn a_restart_does_not_reuse_the_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("bin")).unwrap();
        exe(&dir.path().join("bin/mod"), "#!/bin/sh\nexit 1\n");

        let spawn_cmd = agent24_domain::SpawnCommand {
            command: "bin/mod".to_owned(),
            args: vec![],
        };
        let mut tokens = std::collections::BTreeSet::new();
        for _ in 0..5 {
            let mut l = crate::launch::spawn(&spawn_cmd, dir.path(), dir.path()).expect("spawn");
            tokens.insert(l.token.clone());
            let _ = l.child.wait();
        }
        assert_eq!(
            tokens.len(),
            5,
            "a token was reused across restarts; each secret must be measurable \
             exactly once (FU-44)"
        );
    }
}
