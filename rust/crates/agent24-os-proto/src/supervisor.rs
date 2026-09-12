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
//! Every way out of a run goes through [`ModuleProcess::stop`], so every way out
//! revokes the generation before anything is killed. A run whose callback
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
    /// The last run failed; the next starts after `delay`.
    Backoff { failures: u32, delay: Duration },
    /// The breaker tripped: no more runs until the supervisor is replaced.
    GaveUp { failures: u32, within: Duration },
    /// Stopped on request.
    Stopped,
}

/// Builds the callback methods for one generation. Called once per run, with
/// that run's generation, so every method it returns is bound to the
/// generation whose handshake the connection completed — not to whatever
/// `Current` holds when a callback arrives (FU-49).
pub type MethodsFor = Arc<dyn Fn(&Arc<Generation>) -> Methods + Send + Sync>;

/// A running supervisor. Dropping it without [`SupervisorHandle::stop`] aborts
/// the loop; the module process it held is dropped with it, which revokes its
/// generation and kills its group without grace.
#[derive(Debug)]
pub struct SupervisorHandle {
    stop: watch::Sender<bool>,
    status: watch::Receiver<Status>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl SupervisorHandle {
    /// Where the module is now.
    #[must_use]
    pub fn status(&self) -> Status {
        self.status.borrow().clone()
    }

    /// A receiver that sees every status change.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<Status> {
        self.status.clone()
    }

    /// Stop the module (revoke, SIGTERM, grace, SIGKILL) and the loop, and
    /// wait for both.
    pub async fn stop(mut self) {
        let _ = self.stop.send(true);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

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
#[must_use]
pub fn supervise(
    spec: ModuleSpec,
    dir: Arc<CallbackDir>,
    current: Arc<Current>,
    methods: MethodsFor,
    timings: Timings,
) -> SupervisorHandle {
    let (stop_tx, stop_rx) = watch::channel(false);
    let (status_tx, status_rx) = watch::channel(Status::Starting { attempt: 1 });
    let task = tokio::spawn(run_loop(
        spec, dir, current, methods, timings, stop_rx, status_tx,
    ));
    SupervisorHandle {
        stop: stop_tx,
        status: status_rx,
        task: Some(task),
    }
}

/// How one run ended.
enum Run {
    /// A stop was requested; the process has been stopped.
    StopRequested,
    /// The run is over: `why` for the policy, and when it became ready (if it
    /// did) for [`RestartPolicy::ran`].
    Ended {
        why: Stopped,
        ready_at: Option<std::time::Instant>,
    },
}

async fn run_loop(
    spec: ModuleSpec,
    dir: Arc<CallbackDir>,
    current: Arc<Current>,
    methods: MethodsFor,
    timings: Timings,
    mut stop: watch::Receiver<bool>,
    status: watch::Sender<Status>,
) {
    let mut policy = RestartPolicy::with_base(timings.backoff_base);
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        status.send_replace(Status::Starting { attempt });
        let (why, ready_at) = match run_once(
            &spec, &dir, &current, &methods, &timings, &mut stop, &status,
        )
        .await
        {
            Run::StopRequested => {
                status.send_replace(Status::Stopped);
                return;
            }
            Run::Ended { why, ready_at } => (why, ready_at),
        };
        let now = std::time::Instant::now();
        if let Some(ready_at) = ready_at {
            policy.ran(ready_at, now);
        }
        match policy.failed(why, now) {
            Decision::RestartAfter(delay) => {
                // Until the next run is handshaken the slot holds a generation
                // that was never started: requests get `503 module_not_ready`
                // (coming back), not the ended run's `module_stopping`. The one
                // taken out was revoked by its run's stop: nothing to do.
                // After a stop or a give-up the revoked generation stays — the
                // module is not coming back, and `module_stopping` says so.
                let _ended = current.replace(Generation::starting());
                tracing::warn!(module = %spec.name, ?why, delay_ms = delay.as_millis(), "module run ended; restarting");
                status.send_replace(Status::Backoff {
                    failures: policy.consecutive_failures(),
                    delay,
                });
                tokio::select! {
                    biased;
                    () = stop_requested(&mut stop) => {
                        status.send_replace(Status::Stopped);
                        return;
                    }
                    () = tokio::time::sleep(delay) => {}
                }
            }
            Decision::GiveUp { after, within } => {
                tracing::error!(module = %spec.name, "{}", Decision::GiveUp { after, within });
                status.send_replace(Status::GaveUp {
                    failures: after,
                    within,
                });
                stop_requested(&mut stop).await;
                status.send_replace(Status::Stopped);
                return;
            }
        }
    }
}

/// One run: spawn, handshake, serve, and stop. Returns how it ended; the
/// process is always stopped (or, if stopping failed, dropped — which kills
/// its group without grace) by the time it returns.
async fn run_once(
    spec: &ModuleSpec,
    dir: &CallbackDir,
    current: &Current,
    methods: &MethodsFor,
    timings: &Timings,
    stop: &mut watch::Receiver<bool>,
    status: &watch::Sender<Status>,
) -> Run {
    let failed = |why: Stopped| Run::Ended {
        why,
        ready_at: None,
    };
    // D4: a fresh port for every generation, so a connection pooled or queued
    // for the previous one can never reach this one.
    let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
        Ok(l) => l,
        Err(e) => {
            tracing::error!(module = %spec.name, "could not bind the module's port: {e}");
            return failed(Stopped::Exited);
        }
    };
    let callback = match dir.listen_next() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!(module = %spec.name, "could not listen for the callback: {e}");
            return failed(Stopped::Exited);
        }
    };
    let mut process = match launch::spawn(LaunchSpec {
        name: &spec.name,
        command: &spec.command,
        package_dir: &spec.package_dir,
        data_dir: &spec.data_dir,
        callback_sock: callback.path(),
        trampoline: &spec.trampoline,
        listener,
    })
    .await
    {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(module = %spec.name, "could not start the module: {e}");
            return failed(Stopped::Exited);
        }
    };
    let generation = process.generation().clone();
    // Out goes a placeholder (the caller's, or `run_loop`'s between runs),
    // which was never started.
    let _placeholder = current.replace(generation.clone());
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
            finish(process, timings, &spec.name).await;
            return Run::StopRequested;
        }
        exited = process.exited() => {
            tracing::warn!(module = %spec.name, ?exited, "the module exited before its handshake");
            finish(process, timings, &spec.name).await;
            return failed(Stopped::Exited);
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
            finish(process, timings, &spec.name).await;
            return failed(Stopped::StartupTimeout);
        }
    };
    if !generation.ready() {
        // Only a revocation moves a generation out of Starting, and only this
        // run stops its process — so this is not expected. It is still not a
        // ready module.
        tracing::error!(module = %spec.name, "the generation was revoked during its handshake");
        finish(process, timings, &spec.name).await;
        return failed(Stopped::StartupTimeout);
    }
    let ready_at = std::time::Instant::now();
    status.send_replace(Status::Running);
    let serve = rpc::serve_until(
        handshaken.reader,
        handshaken.writer,
        methods(&generation),
        rpc::Limits::default(),
        {
            let generation = generation.clone();
            async move { generation.revoked().await }
        },
    );
    let outcome = tokio::select! {
        biased;
        () = stop_requested(stop) => None,
        ended = serve => Some(format!("the callback connection ended: {ended:?}")),
        exited = process.exited() => Some(format!("the module exited: {exited:?}")),
    };
    finish(process, timings, &spec.name).await;
    match outcome {
        None => Run::StopRequested,
        Some(why) => {
            tracing::warn!(module = %spec.name, "{why}");
            Run::Ended {
                why: Stopped::Exited,
                ready_at: Some(ready_at),
            }
        }
    }
}

/// Resolves once a stop has been requested. The `watch::Ref` that `wait_for`
/// yields holds a read guard, which must not live across an await in a task
/// that is spawned: it is dropped here, and only `()` comes out.
async fn stop_requested(stop: &mut watch::Receiver<bool>) {
    let _ = stop.wait_for(|s| *s).await;
}

/// Stop a run's process. A stop that fails is logged and the process dropped,
/// which revokes (already done) and SIGKILLs its group without grace.
async fn finish(process: ModuleProcess, timings: &Timings, name: &str) {
    match process.stop(timings.stop_grace).await {
        Ok(report) => {
            if !report.abandoned.is_empty() {
                tracing::warn!(
                    module = name,
                    abandoned = ?report.abandoned,
                    "requests in flight when the module stopped: outcome unknown"
                );
            }
        }
        Err(failed) => {
            tracing::error!(
                module = name,
                "the module could not be stopped cleanly: {failed}"
            );
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
    /// start appends `<pid> <token>` to `<data>/starts`. `early`: exit without
    /// ever connecting.
    const MOCK: &str = r#"import json, os, socket, sys, time
mode, name, digest = sys.argv[1], sys.argv[2], sys.argv[3]
data = os.environ["A24_DATA_DIR"]
token = os.environ["A24_HANDSHAKE_TOKEN"]
with open(os.path.join(data, "starts"), "a") as f:
    f.write("%d %s\n" % (os.getpid(), token))
if mode == "silent":
    time.sleep(60)
    sys.exit(0)
if mode == "early":
    sys.exit(4)
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
        tokio::time::timeout(Duration::from_secs(60), rx.wait_for(|s| pred(s)))
            .await
            .unwrap_or_else(|_| panic!("never reached: {what}"))
            .unwrap()
            .clone()
    }

    /// The `<pid> <token>` lines the module wrote, one per start.
    fn starts(data: &std::path::Path) -> Vec<(i32, String)> {
        std::fs::read_to_string(data.join("starts"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(' '))
            .map(|(p, t)| (p.parse().unwrap(), t.to_owned()))
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

    /// Stop, bounded: a stop that hangs fails the test instead of the run.
    async fn stop(handle: SupervisorHandle) {
        tokio::time::timeout(Duration::from_secs(30), handle.stop())
            .await
            .expect("the supervisor did not stop");
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
        let handle = supervise(
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
        let handle = supervise(f.spec.clone(), f.dir.clone(), current, no_methods(), fast());
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
        let handle = supervise(
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
        stop(handle).await;

        let f = fixture("crash");
        let current = Current::new(Generation::starting());
        let handle = supervise(
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
        let handle = supervise(
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
        let handle = supervise(
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
        let handle = supervise(f.spec.clone(), f.dir.clone(), current, no_methods(), fast());
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
        let handle = supervise(f.spec.clone(), f.dir.clone(), current, methods, fast());
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

    /// Dropping the handle without stopping is still a kill path: the loop is
    /// aborted, the process it held is dropped, and dropping it kills the group.
    #[tokio::test]
    async fn dropping_the_handle_kills_the_module() {
        let f = fixture("normal");
        let current = Current::new(Generation::starting());
        let handle = supervise(f.spec.clone(), f.dir.clone(), current, no_methods(), fast());
        let mut rx = handle.subscribe();
        until(&mut rx, "Running", |s| *s == Status::Running).await;
        let (pid, _) = starts(f.data.path())[0].clone();
        drop(handle);
        assert!(gone(pid).await, "the module outlived a dropped supervisor");
    }
}
