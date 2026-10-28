//! Managed iDoris lifecycle. Setup, key storage and budget discovery stay injected.
//!
//! v0.4.0 (`20136552`) reads exactly 64 lowercase hex bytes plus LF from stdin,
//! uses IDORIS_OMLX_API_KEY for serve, and exposes contract_version in /health.
//! No readiness or error classification is inferred from diagnostic output.

mod identity;
mod logs;
#[cfg(test)]
mod tests;

use crate::{IdorisLock, install::InstalledVersion};
use identity::{Lease, ProcessRecord};
use nix::{
    sys::signal::{Signal, killpg},
    unistd::Pid,
};
use serde::Deserialize;
use std::{
    collections::VecDeque,
    fs::{self, File},
    future::Future,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::AsyncWriteExt,
    process::{Child, Command},
    sync::watch,
    task::JoinHandle,
    time::{Instant, sleep, timeout},
};
use tokio_util::sync::CancellationToken;
use zeroize::Zeroizing;

#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    #[error("the same database already has an instance, or process identity cannot be verified")]
    InstanceConflict,
    #[error("iDoris exited unsuccessfully ({code:?})")]
    Failed { code: Option<i32> },
    #[error("iDoris authentication failed")]
    Authentication,
    #[error("invalid launcher configuration")]
    Configuration,
    #[error("iDoris budget admission failed")]
    Budget,
    #[error("iDoris health or contract validation failed")]
    Contract,
    #[error("iDoris did not become ready before the deadline")]
    StartupTimeout,
    #[error("Admin token stdin handoff failed")]
    TokenHandoff,
    #[error("iDoris shutdown was requested")]
    Stopped,
    #[error("iDoris restart limit reached")]
    RestartLimit,
    #[error("iDoris supervisor I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("installed iDoris verification failed: {0}")]
    Installation(#[from] crate::install::InstallError),
}

/// IL-05 returns this after setup; canonicalization rejects symlink escapes.
#[derive(Clone, Debug)]
pub struct ComponentsDir(PathBuf);
impl ComponentsDir {
    pub fn new(managed_root: &Path, directory: &Path) -> Result<Self, SupervisorError> {
        if !managed_root.is_absolute() || !directory.is_absolute() {
            return Err(SupervisorError::Configuration);
        }
        let root = fs::canonicalize(managed_root)?;
        let directory = fs::canonicalize(directory)?;
        if !directory.is_dir() || !directory.starts_with(root) {
            return Err(SupervisorError::Configuration);
        }
        Ok(Self(directory))
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
}

#[derive(Clone, Debug)]
pub struct DataPaths {
    pub db: PathBuf,
    pub state: PathBuf,
}
impl DataPaths {
    fn canonical(&self) -> Result<Self, SupervisorError> {
        if !self.db.is_absolute() || !self.state.is_absolute() {
            return Err(SupervisorError::Configuration);
        }
        fs::create_dir_all(&self.state)?;
        let parent = self.db.parent().ok_or(SupervisorError::Configuration)?;
        fs::create_dir_all(parent)?;
        let db = if self.db.exists() {
            fs::canonicalize(&self.db)?
        } else {
            fs::canonicalize(parent)?
                .join(self.db.file_name().ok_or(SupervisorError::Configuration)?)
        };
        Ok(Self {
            db,
            state: fs::canonicalize(&self.state)?,
        })
    }
}

/// Already computed by Agent24's budget owner. Zero is a real allocation.
#[derive(Clone, Copy, Debug)]
pub struct Budget {
    pub allocated_bytes: u64,
}

/// Never derive Debug: these values must not enter logs or task diagnostics.
pub struct LaunchInputs {
    pub budget: Budget,
    pub omlx_key: Option<Zeroizing<String>>,
}

#[derive(Clone, Copy, Debug)]
pub struct Ports {
    pub data: u16,
    pub admin: u16,
}
impl Default for Ports {
    fn default() -> Self {
        Self {
            data: 8740,
            admin: 8741,
        }
    }
}

/// Production defaults implement design §6 and the frozen §13.1 response.
#[derive(Clone, Debug)]
pub struct Timings {
    pub token_write: Duration,
    pub health_request: Duration,
    pub ready: Duration,
    pub poll: Duration,
    pub terminate: Duration,
    pub drain: Duration,
    pub restart_window: Duration,
    pub restart_delays: [Duration; 3],
}
impl Default for Timings {
    fn default() -> Self {
        Self {
            token_write: Duration::from_secs(10),
            health_request: Duration::from_secs(2),
            ready: Duration::from_secs(30),
            poll: Duration::from_millis(250),
            terminate: Duration::from_secs(17),
            drain: Duration::from_secs(30),
            restart_window: Duration::from_secs(300),
            restart_delays: [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
            ],
        }
    }
}

pub struct StartConfig {
    pub installed: InstalledVersion,
    pub components: ComponentsDir,
    pub data: DataPaths,
    pub ports: Ports,
    pub log_dir: PathBuf,
    /// Opt-in sanitized child output; default logs only controlled phases.
    pub diagnostic_output: bool,
    pub timings: Timings,
    /// Cancel even while start, input refresh or restart backoff is pending.
    pub shutdown: CancellationToken,
    pub lock: &'static IdorisLock,
}
impl StartConfig {
    pub fn new(
        installed: InstalledVersion,
        components: ComponentsDir,
        data: DataPaths,
    ) -> Result<Self, SupervisorError> {
        let home = std::env::var_os("HOME").ok_or(SupervisorError::Configuration)?;
        Ok(Self {
            installed,
            components,
            data,
            ports: Ports::default(),
            log_dir: PathBuf::from(home).join("Library/Logs/Agent24/idoris"),
            diagnostic_output: false,
            timings: Timings::default(),
            shutdown: CancellationToken::new(),
            lock: IdorisLock::embedded(),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StopReport {
    pub forced: bool,
    pub drain_timed_out: bool,
}

/// A single owner holds the process, lease, credentials and restart task.
/// Dropping the handle requests shutdown; explicit stop also awaits reaping.
/// The gateway must subscribe to accepting_requests before serving traffic.
pub struct Supervisor {
    accepting: watch::Receiver<bool>,
    shutdown: CancellationToken,
    task: Option<JoinHandle<Result<StopReport, SupervisorError>>>,
}
impl Supervisor {
    pub async fn start<F, Fut, D, Drain>(
        mut config: StartConfig,
        mut refresh: F,
        drain: D,
    ) -> Result<Self, SupervisorError>
    where
        F: FnMut() -> Fut + Send + 'static,
        Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
        D: FnOnce() -> Drain + Send + 'static,
        Drain: Future<Output = ()> + Send,
    {
        if config.ports.data == 0
            || config.ports.admin == 0
            || config.ports.data == config.ports.admin
        {
            return Err(SupervisorError::Configuration);
        }
        config.data = config.data.canonical()?;
        let lease = Lease::acquire(&config.data.state)?;
        lease
            .recover(
                &config.timings,
                &config.shutdown,
                &config.installed.binary(),
            )
            .await?;
        let config = Arc::new(config);
        let mut policy = RestartPolicy {
            attempts: VecDeque::new(),
        };
        let process = launch(&config, &lease, &mut refresh, &mut policy, Instant::now()).await?;
        let (accepting, receiver) = watch::channel(true);
        let shutdown = config.shutdown.clone();
        let task = tokio::spawn(async move {
            supervise(process, config, lease, refresh, drain, accepting, policy).await
        });
        Ok(Self {
            accepting: receiver,
            shutdown,
            task: Some(task),
        })
    }

    pub fn accepting_requests(&self) -> watch::Receiver<bool> {
        self.accepting.clone()
    }
    pub fn request_stop(&self) {
        self.shutdown.cancel();
    }
    pub async fn stop(mut self) -> Result<StopReport, SupervisorError> {
        self.shutdown.cancel();
        self.join().await
    }
    /// Await a terminal failure (or a stop requested via the supplied token).
    pub async fn wait(mut self) -> Result<StopReport, SupervisorError> {
        self.join().await
    }
    async fn join(&mut self) -> Result<StopReport, SupervisorError> {
        let task = self.task.take().ok_or(SupervisorError::Stopped)?;
        task.await
            .map_err(|_| io::Error::other("supervisor task failed"))?
    }
}
impl Drop for Supervisor {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct RestartPolicy {
    attempts: VecDeque<Instant>,
}
impl RestartPolicy {
    fn delay(
        &mut self,
        now: Instant,
        stable_since: Instant,
        timings: &Timings,
    ) -> Result<Duration, SupervisorError> {
        if now.duration_since(stable_since) >= timings.restart_window {
            self.attempts.clear();
        }
        while self
            .attempts
            .front()
            .is_some_and(|at| now.duration_since(*at) >= timings.restart_window)
        {
            self.attempts.pop_front();
        }
        let delay = *timings
            .restart_delays
            .get(self.attempts.len())
            .ok_or(SupervisorError::RestartLimit)?;
        self.attempts.push_back(now);
        Ok(delay)
    }
}

async fn supervise<F, Fut, D, Drain>(
    mut process: ManagedProcess,
    config: Arc<StartConfig>,
    lease: Lease,
    mut refresh: F,
    drain: D,
    accepting: watch::Sender<bool>,
    mut policy: RestartPolicy,
) -> Result<StopReport, SupervisorError>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
    D: FnOnce() -> Drain + Send,
    Drain: Future<Output = ()> + Send,
{
    let mut stable_since = Instant::now();
    loop {
        let status = tokio::select! {
            biased;
            _ = config.shutdown.cancelled() => {
                accepting.send_replace(false);
                let drain_timed_out = timeout(config.timings.drain, drain()).await.is_err();
                let forced = process.stop().await?;
                lease.clear()?;
                return Ok(StopReport { forced, drain_timed_out });
            }
            status = process.child.wait() => match status {
                Ok(status) => status,
                Err(error) => {
                    accepting.send_replace(false);
                    return Err(error.into());
                }
            },
        };
        accepting.send_replace(false);
        // Snapshot surviving workers at the reap itself, before the PGID
        // could be recycled; finish() then kills them with the group.
        process.adopt_survivors()?;
        process
            .output
            .event(&format!("exited code={:?}", status.code()));
        process.finish().await?;
        lease.clear()?;
        if config.shutdown.is_cancelled() {
            return Ok(StopReport::default());
        }
        if status.success() {
            return Ok(StopReport::default());
        }
        // v0.4.0 shares exit 1 between config/auth/budget/other failures. There
        // is no structured discriminator. Fail closed rather than retry any
        // of these. Only abnormal signal exits are provable crashes.
        if status.code().is_some() {
            return Err(exit_error(status));
        }
        let delay = policy.delay(Instant::now(), stable_since, &config.timings)?;
        tokio::select! {
            biased;
            _ = config.shutdown.cancelled() => return Ok(StopReport::default()),
            _ = sleep(delay) => {},
        }
        // Only confirmed signal crashes retry, including during readiness.
        // Reset the stability clock before attempting a new generation.
        process = match launch(&config, &lease, &mut refresh, &mut policy, Instant::now()).await {
            Err(SupervisorError::Stopped) => return Ok(StopReport::default()),
            other => other?,
        };
        stable_since = Instant::now();
        accepting.send_replace(true);
    }
}

async fn launch<F, Fut>(
    config: &Arc<StartConfig>,
    lease: &Lease,
    refresh: &mut F,
    policy: &mut RestartPolicy,
    stable_since: Instant,
) -> Result<ManagedProcess, SupervisorError>
where
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<LaunchInputs, SupervisorError>> + Send,
{
    loop {
        let inputs = tokio::select! {
            biased;
            _ = config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
            value = refresh() => value?,
        };
        match ManagedProcess::start(config.clone(), inputs, lease).await {
            Err(SupervisorError::Failed { code: None }) if !config.shutdown.is_cancelled() => {
                let delay = policy.delay(Instant::now(), stable_since, &config.timings)?;
                tokio::select! {
                    biased;
                    _ = config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
                    _ = sleep(delay) => {},
                }
            }
            other => return other,
        }
    }
}

fn exit_error(status: ExitStatus) -> SupervisorError {
    if status.code() == Some(3) {
        SupervisorError::InstanceConflict
    } else {
        SupervisorError::Failed {
            code: status.code(),
        }
    }
}

struct ManagedProcess {
    child: Child,
    group: Option<Pid>,
    members: Vec<ProcessRecord>,
    // Set once the leader's reap was observed and its survivors snapshotted.
    adopted: bool,
    config: Arc<StartConfig>,
    output: logs::Output,
    // Retained only in zeroizing memory for this generation and log redaction.
    _admin: Arc<Zeroizing<String>>,
}
impl ManagedProcess {
    async fn start(
        config: Arc<StartConfig>,
        inputs: LaunchInputs,
        lease: &Lease,
    ) -> Result<Self, SupervisorError> {
        if inputs.omlx_key.as_ref().is_some_and(|key| {
            key.is_empty() || key.chars().any(|ch| ch.is_whitespace() || ch.is_control())
        }) {
            return Err(SupervisorError::Configuration);
        }
        let token = Arc::new(mint_token()?);
        let mut output = logs::Output::new(
            &config.log_dir,
            token.clone(),
            inputs.omlx_key.map(Arc::new),
            config.diagnostic_output,
        )?;
        let mut command = serve_command(&config, &inputs.budget, output.omlx_key())?;
        let mut child = command.spawn()?;
        let spawned = Instant::now();
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("missing child PID"))?;
        output.attach(child.stdout.take(), child.stderr.take());
        let mut process = Self {
            child,
            group: Some(Pid::from_raw(pid as i32)),
            members: Vec::new(),
            adopted: false,
            config,
            output,
            _admin: token,
        };
        let result = async {
            let mut stdin = process
                .child
                .stdin
                .take()
                .ok_or(SupervisorError::TokenHandoff)?;
            let handoff = write_token(
                &mut stdin,
                &process._admin,
                spawned + process.config.timings.token_write,
                &process.config.shutdown,
            )
            .await;
            if let Err(error) = handoff {
                if matches!(error, SupervisorError::TokenHandoff)
                    && let Ok(status) =
                        timeout(Duration::from_millis(50), process.child.wait()).await
                {
                    let status = status?;
                    process.adopt_survivors()?;
                    return Err(exit_error(status));
                }
                return Err(error);
            }
            drop(stdin);
            // Capture kernel identity, not argv or a PID-only marker. The
            // durable record is written after the immediate token handoff.
            let record = match ProcessRecord::capture(pid) {
                Ok(record) => record,
                Err(error) => {
                    // A fast failure may disappear before the kernel query;
                    // preserve the contractual exit code, particularly L4.
                    if let Ok(status) =
                        timeout(Duration::from_millis(50), process.child.wait()).await
                    {
                        let status = status?;
                        process.adopt_survivors()?;
                        return Err(exit_error(status));
                    }
                    return Err(error.into());
                }
            };
            lease.write(&record)?;
            process
                .ready(spawned + process.config.timings.ready)
                .await?;
            process.remember_members()?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            process.output.event(match &error {
                SupervisorError::InstanceConflict => "failed: instance conflict",
                SupervisorError::Authentication => "failed: authentication",
                SupervisorError::Contract => "failed: health contract",
                SupervisorError::TokenHandoff => "failed: token handoff",
                SupervisorError::StartupTimeout => "failed: readiness timeout",
                SupervisorError::Stopped => "shutdown requested",
                _ => "failed: startup",
            });
            // Always reap before returning a failed start. Cleanup failure
            // preserves the record, allowing a later recovery to fail closed.
            process.stop().await?;
            lease.clear()?;
            return Err(error);
        }
        process.output.event("ready");
        Ok(process)
    }

    fn remember_members(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return self.adopt_survivors();
        }
        if let Some(group) = self.group {
            self.members = ProcessRecord::group_members(group)?;
        }
        Ok(())
    }

    /// Call right after any wait observes the leader's exit. tokio reaps only
    /// inside our own wait/try_wait calls, so the first observation is the
    /// reap. A PID that still names a process group is never reallocated, so
    /// members found now are this generation's workers (forked after ready,
    /// hence missing from the readiness snapshot). Later calls add nothing:
    /// once the group empties, its PGID may name an unrelated group.
    fn adopt_survivors(&mut self) -> io::Result<()> {
        if self.adopted {
            return Ok(());
        }
        self.adopted = true;
        if let Some(group) = self.group {
            for member in ProcessRecord::group_members(group)? {
                if !self.members.contains(&member) {
                    self.members.push(member);
                }
            }
        }
        Ok(())
    }

    /// Returns whether the signal was sent to a group proven to be ours.
    fn signal_owned_group(&mut self, signal: Signal) -> io::Result<bool> {
        let exited = self.child.try_wait()?.is_some();
        if exited {
            self.adopt_survivors()?;
        }
        if let Some(group) = self.group {
            // Once reaped, the numeric PGID alone proves no ownership. Only an
            // observed member with unchanged kernel identity can authorize
            // group cleanup. Unknown descendants are left alone.
            if !exited || self.members.iter().any(|member| member.in_group(group)) {
                signal_group(group, signal)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn ready(&mut self, deadline: Instant) -> Result<(), SupervisorError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.config.timings.health_request)
            .build()
            .map_err(|_| SupervisorError::Configuration)?;
        let url = format!("http://127.0.0.1:{}/health", self.config.ports.data);
        loop {
            let check = async {
                if let Ok(mut response) = client.get(&url).send().await {
                    if matches!(response.status().as_u16(), 401 | 403) {
                        return Err(SupervisorError::Authentication);
                    }
                    if response.status().is_success() {
                        let mut body = Vec::new();
                        while let Some(chunk) = response
                            .chunk()
                            .await
                            .map_err(|_| SupervisorError::Contract)?
                        {
                            if body.len() + chunk.len() > 16 * 1024 {
                                return Err(SupervisorError::Contract);
                            }
                            body.extend_from_slice(&chunk);
                        }
                        validate_health(&body, self.config.lock)?;
                        if !listeners_owned(&self.config, self.group).await? {
                            return Ok(false);
                        }
                        let admin_url = format!(
                            "http://127.0.0.1:{}/admin/api/v1/status",
                            self.config.ports.admin
                        );
                        if let Ok(response) = client
                            .get(admin_url)
                            .bearer_auth(self._admin.as_str())
                            .send()
                            .await
                        {
                            return match response.status().as_u16() {
                                200 => Ok(true),
                                401 | 403 => Err(SupervisorError::Authentication),
                                _ => Err(SupervisorError::Contract),
                            };
                        }
                        return Ok(false);
                    }
                    if response.status().is_redirection() || response.status().is_client_error() {
                        return Err(SupervisorError::Contract);
                    }
                }
                Ok(false)
            };
            let ready = tokio::select! {
                biased;
                _ = self.config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
                status = self.child.wait() => Err(status?),
                _ = tokio::time::sleep_until(deadline) => return Err(SupervisorError::StartupTimeout),
                value = check => Ok(value?),
            };
            let ready = match ready {
                Ok(ready) => ready,
                Err(status) => {
                    self.adopt_survivors()?;
                    return Err(exit_error(status));
                }
            };
            if ready {
                if let Some(status) = self.child.try_wait()? {
                    self.adopt_survivors()?;
                    return Err(exit_error(status));
                }
                return Ok(());
            }
            let exited = tokio::select! {
                biased;
                _ = self.config.shutdown.cancelled() => return Err(SupervisorError::Stopped),
                status = self.child.wait() => Some(status?),
                _ = tokio::time::sleep_until(deadline) => return Err(SupervisorError::StartupTimeout),
                _ = sleep(self.config.timings.poll) => None,
            };
            if let Some(status) = exited {
                self.adopt_survivors()?;
                return Err(exit_error(status));
            }
        }
    }

    /// `forced` is true only when SIGKILL was actually delivered to a group
    /// proven to be ours; a group we cannot prove is ours fails closed.
    async fn stop(&mut self) -> Result<bool, SupervisorError> {
        let group = self.group.ok_or(SupervisorError::Stopped)?;
        self.output.event("stopping");
        self.remember_members()?;
        self.signal_owned_group(Signal::SIGTERM)?;
        // The leader may exit before its runtime descendants. Their grace
        // period is part of the same 17 seconds, not a new budget.
        let deadline = Instant::now() + self.config.timings.terminate;
        let graceful = match tokio::time::timeout_at(deadline, self.child.wait()).await {
            Ok(status) => {
                status?;
                self.adopt_survivors()?;
                tokio::time::timeout_at(deadline, wait_group_gone(group))
                    .await
                    .map_or(Ok(false), |done| done.map(|()| true))?
            }
            Err(_) => false,
        };
        if !graceful {
            self.output.event("forced stop");
            if !self.signal_owned_group(Signal::SIGKILL)? {
                // Nothing provably ours to kill: report it, never forced=true.
                self.output.event("failed: unowned group survives");
                return Err(SupervisorError::InstanceConflict);
            }
            self.child.wait().await?;
            self.adopt_survivors()?;
            self.reclaimed(group).await?;
        }
        self.group = None;
        self.output.finish().await;
        Ok(!graceful)
    }
    /// Crash path: the leader is already reaped and its survivors adopted.
    async fn finish(&mut self) -> Result<(), SupervisorError> {
        if let Some(group) = self.group {
            if self.signal_owned_group(Signal::SIGKILL)? {
                self.reclaimed(group).await?;
            } else if group_exists(group)? {
                // A group with no proven member must not be signalled, and
                // must not be restarted beside either. Keep the record.
                return Err(SupervisorError::InstanceConflict);
            }
        }
        self.group = None;
        self.output.finish().await;
        Ok(())
    }
    /// SIGKILL'd members linger only until reaped by their new parent.
    async fn reclaimed(&self, group: Pid) -> Result<(), SupervisorError> {
        timeout(Duration::from_secs(5), wait_group_gone(group))
            .await
            .map_err(|_| SupervisorError::InstanceConflict)??;
        Ok(())
    }
}

async fn wait_group_gone(group: Pid) -> io::Result<()> {
    while group_exists(group)? {
        sleep(Duration::from_millis(10)).await;
    }
    Ok(())
}

/// F3/N1: loopback has no peer identity, so a 200 from a port proves
/// nothing. Before the Admin token leaves this process, every visible
/// LISTEN socket on both ports must be held by this generation's group.
async fn listeners_owned(
    config: &StartConfig,
    group: Option<Pid>,
) -> Result<bool, SupervisorError> {
    let group = group.ok_or(SupervisorError::InstanceConflict)?;
    for port in [config.ports.data, config.ports.admin] {
        let found = identity::listeners(port, config.timings.health_request).await;
        if !judge_listeners(found, group, |pid| {
            nix::unistd::getpgid(Some(Pid::from_raw(i32::try_from(pid).ok()?))).ok()
        })? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Only a listener confirmed to sit in another process group is a port
/// conflict (L4, never retried). A failed or timed-out query, a socket whose
/// holder is not visible, or a holder gone before its PGID was read proves
/// nothing either way: not ready yet, retried until the readiness deadline,
/// and the Admin token is still withheld.
fn judge_listeners(
    found: io::Result<identity::Listeners>,
    group: Pid,
    pgid_of: impl Fn(u32) -> Option<Pid>,
) -> Result<bool, SupervisorError> {
    let Ok(found) = found else {
        return Ok(false);
    };
    let mut proven = !found.unattributed && !found.pids.is_empty();
    for pid in found.pids {
        match pgid_of(pid) {
            Some(pgid) if pgid == group => {}
            Some(_) => return Err(SupervisorError::InstanceConflict),
            None => proven = false,
        }
    }
    Ok(proven)
}

/// A zombie-only group holds no ports, files or CPU and cannot be signalled
/// further, so it counts as gone; it vanishes once its parent reaps it.
fn group_exists(group: Pid) -> io::Result<bool> {
    let outcome = killpg_outcome(killpg(group, None), || {
        identity::group_has_live_member(group)
    })?;
    Ok(outcome == KillOutcome::Delivered)
}

#[derive(Debug, PartialEq, Eq)]
enum KillOutcome {
    Delivered,
    NoGroup,
    /// macOS only: every member is an unreaped zombie, so killpg (even with
    /// signal 0) fails with EPERM. Linux delivers to zombies and never gets
    /// here; there a zombie-only group reads as Delivered until reaped.
    ZombieOnly,
}

/// The one place EPERM is classified, shared by signal_group and
/// group_exists. EPERM is harmless only when no live member remains;
/// otherwise a member we may not signal survives, which stays an error.
fn killpg_outcome(
    result: nix::Result<()>,
    live_member: impl FnOnce() -> io::Result<bool>,
) -> io::Result<KillOutcome> {
    match result {
        Ok(()) => Ok(KillOutcome::Delivered),
        Err(nix::errno::Errno::ESRCH) => Ok(KillOutcome::NoGroup),
        Err(nix::errno::Errno::EPERM) if !live_member()? => Ok(KillOutcome::ZombieOnly),
        Err(error) => Err(io::Error::from_raw_os_error(error as i32)),
    }
}
impl Drop for ManagedProcess {
    fn drop(&mut self) {
        let _ = self.signal_owned_group(Signal::SIGKILL);
    }
}

/// A zombie-only group has nothing left to signal; callers then wait for it
/// to be reaped.
fn signal_group(group: Pid, signal: Signal) -> io::Result<()> {
    killpg_outcome(killpg(group, signal), || {
        identity::group_has_live_member(group)
    })?;
    Ok(())
}

fn serve_command(
    config: &StartConfig,
    budget: &Budget,
    key: Option<&str>,
) -> Result<Command, SupervisorError> {
    // macOS has no fexecve: verify the installed bytes immediately before each
    // spawn, accepting the remaining same-UID path-swap race after this check.
    config.installed.verify_for_launch()?;
    let binary = fs::canonicalize(config.installed.binary())?;
    let directory = fs::canonicalize(config.installed.directory())?;
    if !binary.starts_with(&directory) {
        return Err(SupervisorError::Configuration);
    }
    // Revalidate the capability at use time; setup's directory may have been
    // replaced with a symlink after it returned.
    if fs::canonicalize(config.components.path())? != config.components.0 {
        return Err(SupervisorError::Configuration);
    }
    let mut command = Command::new(binary);
    command
        .env_clear()
        .current_dir(&directory)
        .args(["serve", "--admin-token-stdin"])
        .process_group(0)
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // Fixed PATH prevents parent PATH injection into any runtime subprocess.
    command.env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin");
    for name in ["HOME", "TMPDIR", "LANG", "LC_ALL", "LC_CTYPE", "TZ"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("IDORIS_COMPONENTS_DIR", config.components.path())
        .env("IDORIS_DB_PATH", &config.data.db)
        .env("IDORIS_STATE_DIR", &config.data.state)
        .env("IDORIS_CATALOG", directory.join("config/catalog.yaml"))
        .env(
            "IDORIS_ROUTING_POLICY",
            directory.join("config/routing-policy.yaml"),
        )
        .env(
            "IDORIS_ALLOCATED_BUDGET_BYTES",
            budget.allocated_bytes.to_string(),
        )
        .env("IDORIS_PORT", config.ports.data.to_string())
        .env("IDORIS_ADMIN_PORT", config.ports.admin.to_string())
        .env("IDORIS_BIND_HOST", "127.0.0.1")
        .env("IDORIS_DEPLOY_MODE", "personal");
    if let Some(key) = key {
        command.env("IDORIS_OMLX_API_KEY", key);
    }
    Ok(command)
}

async fn write_token(
    writer: &mut (impl tokio::io::AsyncWrite + Unpin),
    token: &str,
    deadline: Instant,
    shutdown: &CancellationToken,
) -> Result<(), SupervisorError> {
    if Instant::now() >= deadline {
        return Err(SupervisorError::TokenHandoff);
    }
    let frame = Zeroizing::new(format!("{token}\n"));
    let handoff = async {
        writer.write_all(frame.as_bytes()).await?;
        writer.shutdown().await
    };
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => Err(SupervisorError::Stopped),
        result = tokio::time::timeout_at(deadline, handoff) => {
            match result { Ok(Ok(())) => Ok(()), _ => Err(SupervisorError::TokenHandoff) }
        }
    }
}

fn mint_token() -> io::Result<Zeroizing<String>> {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    File::open("/dev/urandom")?.read_exact(bytes.as_mut())?;
    let mut token = Zeroizing::new(String::with_capacity(64));
    const HEX: &[u8] = b"0123456789abcdef";
    for byte in bytes.iter() {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 15) as usize] as char);
    }
    Ok(token)
}

#[derive(Deserialize)]
struct Health {
    status: String,
    service: String,
    version: String,
    contract_version: String,
    instance_id: String,
    dev_no_key_enabled: bool,
}
fn validate_health(body: &[u8], lock: &IdorisLock) -> Result<(), SupervisorError> {
    let value: Health = serde_json::from_slice(body).map_err(|_| SupervisorError::Contract)?;
    if value.status != "ok"
        || value.service != "idoris"
        || value.version != lock.tag.trim_start_matches('v')
        || value.instance_id.trim().is_empty()
        || value.dev_no_key_enabled
        || semver::Version::parse(&value.contract_version).map_or(true, |v| v.major != 1)
        || lock.check_contract(&value.contract_version).is_err()
    {
        return Err(SupervisorError::Contract);
    }
    Ok(())
}
